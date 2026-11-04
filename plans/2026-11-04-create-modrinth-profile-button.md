# Accepted amendment: Create Modrinth profile button

The user requests that, while standalone Minecraft API approval is pending, the launcher create a
new Modrinth profile, and explicitly asks to make this a button.

Add **Create Modrinth profile** to the launcher and companion updater. Prepare a consistent local
copy of the selected existing profile, including player files, or install the published pack into
the app-owned instance when no old profile exists. Keep the original profile and worlds intact.

Use the supported local `.mrpack` import mechanism to open Modrinth's native installer. The user
confirms installation there; remove the earlier manual create-empty-profile/copy workflow from the
primary setup instructions. Do not write Modrinth's private SQLite database. Mark only these locally
created archives so the background updater can recognize their imported profiles without accepting
arbitrary linked modpacks or inheriting the old shared-instance update association.

Keep archives local: migration archives may contain worlds and personal configuration. Validate
paths, reject unsafe links, require Minecraft to be closed, verify copied bytes and preserve a failed
export for diagnosis without reporting success. Expose a CLI archive operation for disposable tests.

Track the already authorized 0.4.0 release independently. These new changes are outside that prepared
commit/tag and require separate explicit Git approval before a follow-up release.
