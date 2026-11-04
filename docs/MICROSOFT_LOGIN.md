# Enabling Microsoft login

Minecraft accounts are Microsoft accounts. A launcher logs in through an **Azure app registration**
(a client id). Since 2023 Mojang only lets an app id use the Minecraft login API after it has been
**approved**; until then the last step returns HTTP 403 and the launcher says so.

Until approval arrives, friends can still use jans-launcher for updates: *Settings → Instance folder
→ Use an existing profile* (their Modrinth App profile), then update here and play from Modrinth App.

## 1. Register the Azure app (free, ~5 minutes)

1. Sign in at <https://portal.azure.com> with a Microsoft account (a free account is enough; no
   subscription is needed for app registrations).
2. Search for **App registrations** → **New registration**.
   * Name: `Jan's Launcher`
   * Supported account types: **Personal Microsoft accounts only**
   * Redirect URI: leave empty.
3. On the app's page copy the **Application (client) ID**.
4. **Authentication** → *Advanced settings* → **Allow public client flows: Yes** → Save.
   (The launcher uses the device-code flow: it shows a code, the player enters it on
   microsoft.com/link.)
5. No client secret and no API permissions are needed.

## 2. Ask Mojang for Minecraft API access

Fill in Mojang's form for third-party launchers (search "Minecraft Java Edition launcher app id
review form", or <https://aka.ms/mce-reviewappid>) with:

* the client id from step 1,
* the app name and a link to this repository,
* a short description: "Open-source modpack launcher for a group of friends; device-code login,
  launches the vanilla client with Fabric."

Approval takes from a few days to a few weeks. You get an email.

## 3. Put the client id into the launcher

Either:

* **For everyone (recommended):** in the GitHub repository go to *Settings → Secrets and variables
  → Actions → Variables* and add `JANS_MSA_CLIENT_ID` = the client id. The next release build bakes
  it in. (A client id is not a secret.)
* **For testing on one PC:** *Settings → Advanced → Azure client ID* in the launcher.

Then **Log in with Microsoft** shows a code, opens the Microsoft page, and after the player confirms,
the launcher stores a refresh token in `account.json` (in the launcher's data folder) so they stay
logged in.

## Error messages

| Message | Meaning |
|---|---|
| Minecraft refused this launcher's app id (403) | Mojang has not approved the app id yet |
| this Microsoft account has no Xbox profile | Log in once at xbox.com to create one |
| child account | The account must be added to a Microsoft family |
| this account does not own Minecraft Java Edition | Wrong account, or game not bought |
