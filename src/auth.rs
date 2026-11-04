//! Microsoft account login (OAuth device code -> Xbox Live -> Minecraft).
//!
//! Needs an Azure app (client id) that Mojang has approved for the Minecraft API; see
//! docs/MICROSOFT_LOGIN.md.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEVICE_CODE: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const SCOPE: &str = "XboxLive.signin offline_access";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub uuid: String,
    pub access_token: String,
    pub user_type: String,
    pub xuid: String,
    /// Unix seconds when the Minecraft token expires.
    pub expires_at: u64,
    pub refresh_token: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn account_path() -> PathBuf {
    crate::config::data_dir().join("account.json")
}

impl Account {
    pub fn load() -> Option<Account> {
        serde_json::from_slice(&std::fs::read(account_path()).ok()?).ok()
    }

    pub fn save(&self) -> Result<()> {
        let path = account_path();
        crate::util::write_atomic(&path, &serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn forget() {
        let _ = std::fs::remove_file(account_path());
    }

    /// Offline account for local testing only (singleplayer; servers reject it).
    pub fn offline(name: &str) -> Account {
        use md5::Digest;
        let mut h: [u8; 16] = md5::Md5::digest(format!("OfflinePlayer:{name}").as_bytes()).into();
        h[6] = (h[6] & 0x0f) | 0x30;
        h[8] = (h[8] & 0x3f) | 0x80;
        Account {
            name: name.to_string(),
            uuid: hex::encode(h),
            access_token: "0".into(),
            user_type: "legacy".into(),
            xuid: "0".into(),
            expires_at: u64::MAX,
            refresh_token: None,
        }
    }

    pub fn needs_refresh(&self) -> bool {
        self.expires_at < now() + 300
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

pub async fn start_device_login(client: &reqwest::Client, client_id: &str) -> Result<DeviceCode> {
    let r = client
        .post(DEVICE_CODE)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await?;
    if !r.status().is_success() {
        bail!(
            "Microsoft refused the login request: {}",
            r.text().await.unwrap_or_default()
        );
    }
    Ok(r.json().await?)
}

/// Poll until the player has entered the code, then finish the whole chain.
pub async fn finish_device_login(
    client: &reqwest::Client,
    client_id: &str,
    code: &DeviceCode,
) -> Result<Account> {
    let deadline = now() + code.expires_in;
    let mut interval = code.interval.max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        if now() > deadline {
            bail!("the login code expired, try again");
        }
        let r: Value = client
            .post(TOKEN)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
            ])
            .send()
            .await?
            .json()
            .await?;
        match r["error"].as_str() {
            None => return minecraft_from_ms(client, &r).await,
            Some("authorization_pending") => continue,
            Some("slow_down") => interval += 5,
            Some(e) => bail!(
                "login failed: {e} {}",
                r["error_description"].as_str().unwrap_or("")
            ),
        }
    }
}

pub async fn refresh(
    client: &reqwest::Client,
    client_id: &str,
    account: &Account,
) -> Result<Account> {
    let rt = account
        .refresh_token
        .as_deref()
        .context("no refresh token, log in again")?;
    let r: Value = client
        .post(TOKEN)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", rt),
            ("scope", SCOPE),
        ])
        .send()
        .await?
        .json()
        .await?;
    if let Some(e) = r["error"].as_str() {
        bail!("session expired ({e}), log in again");
    }
    minecraft_from_ms(client, &r).await
}

async fn minecraft_from_ms(client: &reqwest::Client, ms: &Value) -> Result<Account> {
    let ms_token = ms["access_token"]
        .as_str()
        .context("no Microsoft access token")?;
    let refresh_token = ms["refresh_token"].as_str().map(str::to_string);

    let xbl: Value = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .json(&json!({
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={ms_token}")},
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        }))
        .send()
        .await?
        .error_for_status()
        .context("Xbox Live login failed")?
        .json()
        .await?;
    let xbl_token = xbl["Token"].as_str().context("no Xbox token")?;
    let uhs = xbl
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(|v| v.as_str())
        .context("no user hash")?;

    let xsts_resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .json(&json!({
            "Properties": {"SandboxId": "RETAIL", "UserTokens": [xbl_token]},
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        }))
        .send()
        .await?;
    let status = xsts_resp.status();
    let xsts: Value = xsts_resp.json().await?;
    if !status.is_success() {
        let msg = match xsts["XErr"].as_u64() {
            Some(2148916233) => {
                "this Microsoft account has no Xbox profile; create one at xbox.com first"
            }
            Some(2148916235) => "Xbox Live is not available in your country",
            Some(2148916236 | 2148916237) => "adult verification needed on xbox.com",
            Some(2148916238) => "child account: it must be added to a Microsoft family by an adult",
            _ => "Xbox authorization failed",
        };
        bail!("{msg}");
    }
    let xsts_token = xsts["Token"].as_str().context("no XSTS token")?;

    let mc_resp = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .json(&json!({"identityToken": format!("XBL3.0 x={uhs};{xsts_token}")}))
        .send()
        .await?;
    if mc_resp.status() == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "Minecraft refused this launcher's app id (403). The Azure app must be approved by Mojang first, see docs/MICROSOFT_LOGIN.md"
        );
    }
    let mc: Value = mc_resp
        .error_for_status()
        .context("Minecraft login failed")?
        .json()
        .await?;
    let mc_token = mc["access_token"]
        .as_str()
        .context("no Minecraft token")?
        .to_string();
    let expires_in = mc["expires_in"].as_u64().unwrap_or(86400);

    let profile_resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&mc_token)
        .send()
        .await?;
    if profile_resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("this account does not own Minecraft Java Edition");
    }
    let profile: Value = profile_resp.error_for_status()?.json().await?;
    Ok(Account {
        name: profile["name"]
            .as_str()
            .context("profile name")?
            .to_string(),
        uuid: profile["id"].as_str().context("profile id")?.to_string(),
        access_token: mc_token,
        user_type: "msa".into(),
        xuid: xsts
            .pointer("/DisplayClaims/xui/0/xid")
            .and_then(|v| v.as_str())
            .unwrap_or("0")
            .to_string(),
        expires_at: now() + expires_in,
        refresh_token,
    })
}
