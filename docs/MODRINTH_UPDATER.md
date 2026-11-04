# Jan's Modpack Updater

A separate small desktop application for players who keep using Modrinth App. It updates the
files inside an existing Fabric 26.3 Modrinth profile from Jan's published pack. No new Minecraft
client ID, Minecraft API approval, or additional account sign-in is needed. Modrinth still handles
Minecraft authentication, runtime installation and starting the game.

## Players

1. Save and close Minecraft. Keep the old shared Fabric 26.3 profile as a backup.
2. Click **Create Modrinth profile** in Jan's Launcher or Jan's Modpack Updater. It detects the
   existing Jan's pack by its old shared ID, updater state or a strong match of stable mod identities,
   rather than trusting the profile name. If several copies match, choose which one contains your
   settings/worlds. If no copy exists, it downloads the published pack first.
3. Confirm **Install** in Modrinth's native installer. The button builds and opens a supported local
   `.mrpack`; no empty-profile creation or manual copying is needed. Game settings, configs, worlds,
   mods and personal files are copied byte-for-byte. Old updater metadata and the shared-instance
   link are not carried across. Modrinth's own account, global settings and runtime preferences remain
   managed by Modrinth; its private database is not rewritten by our application.
4. Click **Refresh** in the updater. A single detected independent copy is selected automatically.
   If automatic discovery does not find a
   custom Modrinth data directory, use **Browse** and choose its `profiles/Fabric 26.3` folder.
5. Click **Check for updates**, review the pack version and download size, then **Update profile**.
6. Wait for **Up to date**, click **Open Modrinth**, and play the new profile in Modrinth.

Migration archives are stored locally under the app's `modrinth-imports/` folder. They can be large
because they include player worlds. **Do not share/upload these personal archives.** They are separate
from the public pack publisher. Unsafe symlinks/junctions are refused with a path. The source is checked
again after copying; a changing source causes failure rather than an inconsistent world copy.

Creating the independent profile stops native shared-instance updates. Jan's background updater
still intentionally manages its pack files; existing configs remain copy-only-if-missing.
The previous manual empty-local-profile/copy workflow remains under advanced one-time setup.

## Automatic updates on Windows

Closing either player window now keeps one lightweight worker in the Windows notification area.
While setup is running, the window hides and the work continues; after completion its renderer closes.
The tray worker can wait without a profile while native Modrinth installation finishes. It automatically
enrolls exactly one completed marked independent copy. Explicitly paused updates remain paused.
Use **Exit after current update** in the tray to stop the app, rather than the window X.

The player apps share the same worker lock. The hidden worker needs no settings window/graphics
context. Windows startup uses a versioned copy of the executable, so an updated app does not overwrite
an executable currently in use. If an older worker is already present, its single-worker lock still wins;
exit that older tray instance before starting the new version if its old UI remains visible.

Setup progress appears beside **Create Modrinth profile**, including file checks, copying, verification
and elapsed time. Once the archive is ready, the button changes to **Continue installation in Modrinth**.
Repeated clicks and app restarts reuse that pending copy instead of creating another archive. Native
installation still requires confirmation in Modrinth; it is not reported as complete at handoff.

To explicitly select a different independent local profile, click **Enable automatic updates at login**.
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
startup and pauses updating while the tray stays available. An update already installing finishes
safely before exit. Other profiles are not enrolled
implicitly. The worker verifies the enrolled profile ID and refuses shared/linked profiles and
arbitrary imported modpacks. Only our marked, fully installed local migration import is also accepted,
with its matching local filename and no remote references. It also refuses
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
