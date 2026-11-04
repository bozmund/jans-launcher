# Jan's Pack Admin

Players install **Jan's Modpack Updater**. The pack owner uses **Jan's Pack Admin**.
The standalone Minecraft launcher remains separate while Microsoft API approval is pending.

## Publish from this laptop using Mony

Open `jans-pack-admin.exe`. Its defaults point at the current Mony SSH alias, live Fabric 26.3
profile, pack checkout and existing publisher executable. The source paths can be edited under
**Source and GitHub publishing setup**. Mony needs to be on and reachable over the existing SSH
connection. SSH uses your keys in batch mode and refuses unknown/changed host keys.

1. Enter release notes. Leave Version empty for automatic date/build numbering.
2. Click **Preview changes**. This scans the live profile and shows the publisher's added,
   updated and removed content. It does not upload, edit the pack or change Git.
3. Click **Publish update**. This explicitly authorizes uploading assets, staging only `pack.json`,
   `CHANGELOG.md` and `overrides/`, committing the pack version, then pushing `main` to the configured
   GitHub origin. It does not publish or tag the launcher application itself.
4. Wait for **Published**. Enrolled player updaters see it at their next check and install while
   Minecraft is closed. They do not need GitHub accounts or your SSH/GitHub credentials.

The admin checks that the source and repository still match the preview, the branch is `main`,
origin matches `github_repo` in `publish.toml`, and no unrelated or pre-staged work exists. It does
not switch branches, stage everything, force-push, or publish automatically when opened.

If upload fails, no commit is made. Uploaded hash-addressed files are harmless until a published
manifest references them. Preview again after resolving the error. If a commit hook fails after
preparation, the panel displays **Retry prepared commit & push**, which reuses the prepared update.
If the commit succeeds and push fails, **Retry push only** pushes that recorded commit without
uploading or committing again. If an external change makes recovery ambiguous, the panel stops and
shows the receipt/log path rather than guessing. It cannot repair arbitrary Git hooks or merge
conflicts itself.

The settings window retains at most 500 log lines. Complete logs, preview fingerprints and the
publication receipt live on the source machine under the user state folder `jans-pack-admin/`.
The player app has no publishing panel and needs no Python, Git or GitHub CLI.

## Requirements

Admin backend: Python **3.11+**, Git, the authenticated GitHub CLI (`gh auth login`), and the existing
`jans-launcher` publisher. The default Mony setup already has these. Python runs only on demand;
there is no admin web server or public port. To publish locally, disable **Work on Mony over SSH**
and set the local Python, publisher executable, source profile and pack checkout paths.

Read-only command access is available for diagnostics and acceptance tests:

```
jans-pack-admin --preview
jans-pack-admin --preview --settings admin-settings.json
```

Settings JSON fields: `remote`, `ssh`, `python`, `publisher`, `profile`, `pack`. Release notes and
version remain per-window inputs. No credentials are stored in this file.

## Validation boundaries

`python scripts/test-admin-backend.py` uses mocked Git/publisher commands to exercise stale previews,
unrelated changes, origin validation, hook failure, push failure and retry behavior without changing
real repositories. A real Mony preview checks SSH, publisher integration and the live source read-only.
Production publication and real hook/push recovery require an explicitly authorized release; do not
claim those were exercised merely because the mocked regressions passed.

`python scripts/test-background-windows.py target/release/jans-modpack-updater.exe` exercises a real
hidden worker and duplicate-worker exit with a disposable shared-profile fixture. It never registers
startup or downloads the production pack. Login startup, tray menu interaction and the full player
onboarding flow still need user acceptance testing.
