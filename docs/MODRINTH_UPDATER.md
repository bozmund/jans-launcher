# Jan's Modpack Updater

A separate small desktop application for players who keep using Modrinth App. It updates the
files inside an existing Fabric 26.3 Modrinth profile from Jan's published pack. No new Minecraft
client ID, Minecraft API approval, or additional account sign-in is needed. Modrinth still handles
Minecraft authentication, runtime installation and starting the game.

## Players

1. Save and close Minecraft. Keep the old shared Fabric 26.3 profile as a backup.
2. Once, create a **new local Fabric 26.3 profile** in Modrinth with the same Fabric loader as the
   old profile. Do not import or duplicate the shared pack: this could inherit its update link.
   Modrinth currently exposes no supported external profile-creation API, so this native step
   stays in Modrinth rather than rewriting its private database.
3. Open **Jan's Modpack Updater**. Under **One-time setup**, select the old profile as the source,
   then select the new empty profile as the destination and click **Copy everything**. The copy
   includes mods, worlds, configs, settings, screenshots and personal files; operational updater
   caches/locks are not copied. Symlinks/junctions are refused with a concrete path. The source
   remains intact; a destination containing worlds/mods/screenshots is refused. Use a fresh target.
4. Select the new profile. If automatic discovery does not find a
   custom Modrinth data directory, use **Browse** and choose its `profiles/Fabric 26.3` folder.
5. Click **Check for updates**, review the pack version and download size, then **Update profile**.
6. Wait for **Up to date**, click **Open Modrinth**, and play the new profile in Modrinth.

## Automatic updates on Windows

After selecting the independent local profile, click **Enable automatic updates at login** once.
This copies the updater to your Local AppData folder and registers it for your Windows account,
without administrator rights. It starts immediately and on subsequent logins, with a tray icon
and no settings window or graphics context. Closing the settings window leaves the worker running.

The worker checks every five minutes. While Minecraft is using that profile it waits, then checks
again every 30 seconds. It never closes the game. Download failures are retained in the status panel
and retried on the next interval. Installation uses verified staging and recoverable transactions.
An unchanged published revision avoids another whole-pack scan. This is a release watcher, not a
continuous integrity scan; use the manual Check button to diagnose locally changed/missing files.

Right-click the tray icon for **Open updater / status**, **Check now**, or **Exit after current
update**. Exit stops this run; **Disable automatic updates** in the window also removes login
startup. An update already installing finishes safely before exit. Other profiles are not enrolled
implicitly. The worker verifies the enrolled profile ID and refuses shared/imported/linked profiles
or a replacement at the same folder path. A loader change requires changing the loader in Modrinth.

Settings and the last check/update/error are kept in the updater's user-data directory as
`background-profile.json` and `background-status.json`. Startup is the current-user Run entry
`JansModpackUpdater`. Only one worker runs at once. This feature updates **pack content**; it does
not silently replace the updater EXE itself. Login/tray interaction still needs user acceptance
testing; automated tests never register startup on the developer's real Windows account.

The updater reads Modrinth's database without changing it or reading account tables. It checks the
registered game, loader and loader version, rejecting incompatible profiles. The current database
adapter is tested with Modrinth App 0.22's instances/content-set format. Older unsupported database
formats produce a clear error rather than guessing a profile.

If the published pack needs a different Fabric loader version, change the profile's Fabric loader
in Modrinth as instructed, then retry. This updater never rewrites Modrinth's runtime metadata.

A first check connects the profile to Jan's update channel. Existing matching JARs are reused, so
it does not download the full pack again. Downloaded changes are SHA512 checked and staged before
installation, interrupted installs are recovered, and replaced files stay in `.jans-launcher/trash`.
Existing configs are copied only if absent. Saves, options and unrelated personal mods are preserved.

Use this application for **Jan's pack updates**. It does not create a Modrinth release or change the
version shown by Modrinth's native shared-instance Update button. Do not run a Modrinth pack update
at the same time. The standalone Jan's Launcher remains a separate application with a single owned
instance.

## Author

Publish the Mony profile with the existing `jans-launcher publish` workflow in [PUBLISHING.md](PUBLISHING.md),
then explicitly commit/push the resulting manifest. Custom JARs are GitHub Release assets and
recognized upstream mods use Modrinth CDN links; the pack does not go through a 500 MB mrpack upload.
Players only need to re-run Check/Update, not download a new updater for each pack change.

## Command access

For diagnostics and isolated testing, the same executable supports:

```
jans-modpack-updater --list
jans-modpack-updater --check --profile ".../ModrinthApp/profiles/Fabric 26.3"
jans-modpack-updater --update --profile ".../ModrinthApp/profiles/Fabric 26.3"
jans-modpack-updater --copy-from ".../profiles/Old shared pack" --profile ".../profiles/New local pack"
jans-modpack-updater --background
```

It never launches Minecraft directly and does not contain the launcher's authentication module.
