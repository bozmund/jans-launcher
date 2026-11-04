# Jan's Modpack Updater

A separate small desktop application for players who keep using Modrinth App. It updates the
files inside an existing Fabric 26.3 Modrinth profile from Jan's published pack. No new Minecraft
client ID, Minecraft API approval, or additional account sign-in is needed. Modrinth still handles
Minecraft authentication, runtime installation and starting the game.

## Players

1. Keep the existing Fabric 26.3 profile in Modrinth. Save and close Minecraft.
2. Open **Jan's Modpack Updater** and select that profile. If automatic discovery does not find a
   custom Modrinth data directory, use **Browse** and choose its `profiles/Fabric 26.3` folder.
3. Click **Check for updates**, review the pack version and download size, then **Update profile**.
4. Wait for **Up to date**, click **Open Modrinth**, and play that same profile in Modrinth.

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
```

It never launches Minecraft directly and does not contain the launcher's authentication module.
