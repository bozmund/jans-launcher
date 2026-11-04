# Publishing the pack

Everything here runs on the PC that has the live profile (mony). You need `git` and the GitHub CLI
`gh` logged in (`gh auth login`), and the `jans-launcher` binary.

## One-time setup

1. Create an empty **public** GitHub repository for the pack, e.g. `bozmund/jans-modpack`
   (public, so friends can download without logging in).
2. Clone it and run publish once; it writes a `publish.toml` template and stops:

   ```bash
   git clone git@github.com:bozmund/jans-modpack.git ~/jans-modpack
   jans-launcher publish --profile ~/.local/share/ModrinthApp/profiles/"Fabric 26.3" --pack ~/jans-modpack
   ```

3. Check `~/jans-modpack/publish.toml`: pack `name`, `minecraft`, `fabric_loader` (must match the
   profile), `github_repo`, and the `include` / `exclude` / `once` lists.
   `*.bak*` and `*.disabled` files are excluded by default.

4. Before the first asset upload, the pack repository needs an initial commit pushed to `main`
   (for example its README, `.gitignore` and `publish.toml`). The `files` release tag cannot be
   created in an empty GitHub repository. Keep `/assets/` in the pack's `.gitignore`; local-asset
   test copies must never be committed. Publish the real manifest without `--local-assets` before
   committing it for friends.

The publisher scans only the configured top-level include folders; development folders such as
`local-mods/`, saves and logs are not traversed or shipped. Source-copy checks reject files that
change during publication. Do not edit the profile's pack files while publishing.

## Every new version

```bash
# 1. see what would change, without writing or uploading anything
jans-launcher publish --profile ~/.local/share/ModrinthApp/profiles/"Fabric 26.3" \
  --pack ~/jans-modpack --dry-run

# 2. publish (uploads new own jars to the "files" release, writes pack.json + CHANGELOG.md)
jans-launcher publish --profile ~/.local/share/ModrinthApp/profiles/"Fabric 26.3" \
  --pack ~/jans-modpack -m "Added Atum. Farlanders no longer spawn in oceans."

# 3. make it live
cd ~/jans-modpack
git add -A
git commit -m "Pack $(jq -r .version pack.json)"
git push
```

Friends see it the next time they open the launcher or click *Check for changes*.

Longer changelogs: write them to a file and use `--message-file notes.md`. Version label defaults
to today's date (`2026.11.03`, then `2026.11.03-2` for a second release on the same day); use
`--version 1.4` to pick your own.

## What goes where

| File | Goes to | Notes |
|---|---|---|
| Jar/zip that exists on Modrinth (same sha512) | Modrinth CDN | Name + version from Modrinth |
| Any other jar, or any file ≥ `asset_min_size_mb` | GitHub release `files` of the pack repo | Asset name = first 16 hex chars of sha512 + file name; uploaded only once |
| Everything else (configs, small packs) | `overrides/` in the pack repo | Folder is rebuilt every publish |

Do not delete old assets from the `files` release while someone might still be on an older pack
version. If you ever clean it up, publish first so the current `pack.json` no longer references
them.

## Rolling back

`git revert` the bad commit and push. The launcher always makes the instance match the current
`pack.json`, so friends go back to the old files on their next update (there is just no changelog
entry for it). If you then publish again, the build number continues from the reverted file, which
is fine.

## Releasing a new launcher version

Bump `version` in `Cargo.toml`, commit, then:

```bash
git tag v0.2.0 && git push origin v0.2.0
```

GitHub Actions builds Windows + Linux and creates the release; running launchers offer the update.
