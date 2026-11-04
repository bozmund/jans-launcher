# Jan's Launcher

A standalone modpack updater and Minecraft launcher, written in Rust, for one Fabric modpack shared
between friends. Friends click **Update** and **Play**; the pack author publishes a new version with
one command. Runs on Windows and Linux as a single executable.

It replaces Modrinth App shared instances, which cap uploads at 500 MB and only show limited
information about what changed.

## How it works

```
 Jan's PC (mony)                        GitHub                               Friend's PC
 ───────────────                        ──────                               ───────────
 Modrinth profile ──publish──▶  pack repo (Git): pack.json,      ──fetch──▶  jans-launcher
                                CHANGELOG.md, overrides/ (configs)           • shows changelog + diff
                                release "files": own jars        ──────────▶ • downloads only changes
                     Modrinth CDN: every mod that is on Modrinth ──────────▶ • verifies sha512
                                                                              • starts Minecraft + Fabric
```

* **The pack is a Git repository** with a manifest (`pack.json`), not the jars themselves. Every
  commit is a pack version, so history stays tiny and you can see exactly what changed in a diff.
* **Mods on Modrinth** are downloaded from the Modrinth CDN (found by file hash at publish time).
* **Your own / patched jars** are uploaded once (by hash) as assets of a GitHub release in the pack
  repo. Unchanged jars are never uploaded twice.
* **Configs and other small files** are stored in the pack repo under `overrides/`.
* Every download is verified with sha512 and written atomically, so a broken connection never leaves
  a half-written mod behind.

### What the player sees

* **Check for changes**: the changelog of every version since theirs, plus a list of added /
  updated (old → new version) / removed mods, shaders and resource packs, and the download size.
* **Update**: downloads only what changed.
* **Play**: downloads Java (Mojang's own runtime), Minecraft, Fabric and assets on first start, then
  launches the game. Requires a Microsoft login (see below).
* Settings: memory, extra JVM args, custom Java, Microsoft account, English.
* The launcher updates itself from this repository's GitHub releases (checksum-verified).

### What is never touched

* Worlds (`saves/`), `options.txt`, `servers.dat`, screenshots.
* Mods and files the player added themselves: the launcher only changes files that a pack version
  listed. A mod you remove from the pack is moved to `.jans-launcher/trash/<date>/` in the instance,
  not deleted.
* Configs (`config/`, `defaultconfigs/`) are copied **only when missing**, so a player's own tweaks
  survive updates. (Configured with `once = [...]` in `publish.toml`.)

## For players

**Windows:** download `jans-launcher-windows-x86_64.exe` from the
[latest release](../../releases/latest), put it anywhere (e.g. Desktop) and double-click it.
Windows SmartScreen may warn because the exe is not signed: *More info → Run anyway*.

**Linux:** download `jans-launcher-linux-x86_64.tar.gz`, extract it and run `./install.sh` (adds it
to your app menu), or just run the `jans-launcher` binary. NixOS: `nix run github:bozmund/jans-launcher`.

There is exactly one app-owned instance: **Jan's Fabric 26.3**. There is no profile picker, external-launcher dependency or pack-source setting in the UI. Legacy profile folders are not deleted or modified when opening the dedicated launcher. **Overview → Download update** installs the current shared
pack. **Settings & account → Sign in with Microsoft** enables integrated Play once the launcher's
Microsoft application is registered and approved. This app never automatically opens Modrinth.
For the one-time owner setup, see [Registration steps](docs/MICROSOFT_SETUP.md).

**Discover mods** searches Modrinth for Fabric versions matching the active Minecraft version.
Preview resolves required dependencies before explicit installation. **Installed mods** lists actual
JAR files. **Mod updates** checks compatible upstream versions by installed file hash. Custom
unpublished ports are updated through pack Fetch; absence from the upstream API is not proof they
are current. Manually updating a pack-managed mod changes your local instance; the next pack Update
restores that mod to the published pack version. No mods are upgraded automatically.

Close Minecraft before updating. All downloads are verified and cached before any installed mod
is replaced. A failed download leaves the previous pack intact; a failed or interrupted file swap
is rolled back. Old files are retained under `.jans-launcher/trash/`. The first sync of an existing
profile moves earlier versions of the same Fabric mod IDs aside, leaving unrelated extra mods alone.
World imports skip an existing world entirely rather than mixing its chunks with another world.

The developer CLI retains explicit instance paths for isolated tests; these are not additional player profiles.

## For the pack author

See [docs/PUBLISHING.md](docs/PUBLISHING.md). In short, on the PC with the live profile:

```bash
jans-launcher publish --profile ~/.local/share/ModrinthApp/profiles/"Fabric 26.3" \
  --pack ~/jans-modpack -m "Added Atum, fixed Farlanders spawns"
cd ~/jans-modpack && git add -A && git commit -m "Pack 2026.11.03" && git push
```

Friends see the new version the next time they open the launcher.

Microsoft login needs a one-time Azure app registration and Mojang approval:
[docs/MICROSOFT_LOGIN.md](docs/MICROSOFT_LOGIN.md).

## Command line

The window is the main interface; these exist for the pack author and for testing:

| Command | What it does |
|---|---|
| `jans-launcher` | Open the window |
| `jans-launcher publish --profile P --pack R [-m TEXT] [--dry-run] [--local-assets]` | Build a new pack version |
| `jans-launcher check [--instance DIR] [--source SRC]` | Print what an update would change |
| `jans-launcher update [--instance DIR] [--source SRC]` | Update an instance |
| `jans-launcher play [--instance DIR]` | Start the game after Microsoft authentication and ownership verification |

`SRC` is `github:owner/repo@branch` or a local folder / URL containing `pack.json`.

Data lives in `%APPDATA%\jans-launcher\data` (Windows) or `~/.local/share/jans-launcher` (Linux):
`settings.json`, `account.json`, shared `libraries/`, `assets/`, `runtimes/` (Java), and the default
game folder `instance/`. Set `JANS_LAUNCHER_HOME` to use another folder.

## Building

```bash
cargo build --release
```

Linux needs `libgtk-3-dev libxkbcommon-dev libwayland-dev` (or `nix develop`). Pushing a `v*` tag
makes GitHub Actions build the Windows exe and Linux binary and publish a release with `SHA256SUMS`.

Build-time options (environment variables):

| Variable | Default | Meaning |
|---|---|---|
| `JANS_PACK_SOURCE` | `github:bozmund/jans-modpack@main` | Pack the app follows |
| `JANS_APP_REPO` | `bozmund/jans-launcher` | Where self-updates come from |
| `JANS_MSA_CLIENT_ID` | none | Azure app id for Microsoft login (Actions: repository variable) |

## License

MIT

## Authentication and affiliation

Jan's Launcher requires Microsoft sign-in and current Minecraft Java entitlement/profile verification
before starting gameplay. It has no offline-account or environment-variable authentication bypass.
The public client ID is `ebddfc17-e502-4c3e-9d64-92711868988f`; it is not a client secret. Minecraft API
review is pending and no authenticated end-to-end launch is claimed until the service accepts this app.

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.
