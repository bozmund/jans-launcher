# Microsoft sign-in setup for Jan's Launcher

The launcher owner completes this once. Friends then sign in with their own Microsoft accounts. They do not need Modrinth App. Pack downloads, the mod catalog and updates already work independently.

## Register the application

1. Open https://entra.microsoft.com/ and sign in using the account that should own the app. Use your own directory rather than an employer's directory without permission.
2. Open **Identity → Applications → App registrations → New registration**. You can also search the portal for **App registrations**.
3. Set the name to **Jan's Launcher**.
4. Choose **Personal Microsoft accounts only**. This launcher uses the `consumers` endpoint for personal Minecraft accounts.
5. Leave Redirect URI empty for this device-code flow and click **Register**.
6. Copy **Application (client) ID** from Overview. This identifier is public; it is not a password or client secret.
7. Under **Authentication → Advanced settings**, set **Allow public client flows = Yes** and save. Portal menu names can change; look for the public-client/device-code setting.

If application registration is unavailable or you do not have a directory, report the exact portal message. Registration depends on the permissions and directory available to your account; do not assume every account can create a new tenant for free.

## Request Minecraft API access

Entra registration and Minecraft Services access are separate. Open the official application review form at https://aka.ms/mce-reviewappid and follow its current instructions. If it is closed or redirected, ask official Minecraft support for the current process.

Prepare the client ID, application name and source link: https://github.com/bozmund/jans-launcher.

Suggested description: **Open-source Rust launcher for a Fabric modpack shared with friends. Players authenticate using their own Microsoft accounts through device-code flow.**

Approval and timing are not guaranteed. A Minecraft HTTP 403 after Microsoft sign-in requires checking the actual service response and app registration status; it is not proof of an incorrect password.

## Configure Jan's Launcher

For your test, open **Settings & account → Microsoft sign-in → Application (client) ID** and paste the ID. Click **Log in with Microsoft**, open the Microsoft page and enter the displayed code. Enter passwords only on Microsoft's website.

For all friends, configure the repository Actions variable `JANS_MSA_CLIENT_ID` and build a new official release. The public ID is then included in the executable so players do not need to enter it. Never include a client secret.

## Official references

- https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app
- https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code
- https://aka.ms/mce-reviewappid
