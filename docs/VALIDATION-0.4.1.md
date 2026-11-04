# Profile creation button — 2026-11-04

The previously approved **0.4.0** was committed as `0a38c8409952ff396b1de2d7f0e4b5f893939b73`, pushed,
and published at https://github.com/bozmund/jans-launcher/releases/tag/v0.4.0. Tag workflow
`41902078801` passed Windows and Linux builds/tests and published all three application binaries.
Downloaded public Windows admin/updater binaries matched the release SHA256SUMS.

The follow-up **0.4.1** adds Create Modrinth profile to both player interfaces. It detects the known
old shared ID, managed pack state, or a strong mod-ID overlap that includes custom pack mods. It
does not infer pack identity from the displayed profile name alone. Multiple matches require a user
choice; an existing marked copy is reused, with incomplete native installation reported clearly.

Native import uses the documented mrpack format:
https://support.modrinth.com/en/articles/8802351-modrinth-modpack-format-mrpack

No Modrinth SQLite tables are written. The archive has a fresh local version ID, empty remote file
list and byte-identical overrides. It omits the old updater's operational metadata and adds only our
local migration marker. The user confirms installation in Modrinth. The updater accepts only a marked
completed local import whose source filename matches the marker and which has no remote reference
or shared/project link. Player mods/configs/worlds/options are retained; account/global/runtime
preferences continue to be owned by Modrinth.

Validation:

- Windows Rust suite: **47 passed** (23 launcher, 21 updater, 3 admin/shared).
- Tests verify ZIP contents, byte-preserved configs/options/worlds/mods, source preservation,
  identity-based detection, rejection of unfinished imports and rejection of remaining remote links.
- Native read-only CLI detected the actual old `Fabric 26.3` profile with Fabric loader 0.20.1 on
  this laptop. No production archive/import was created in that check.
- Native CLI exported a disposable fixture; Python independently verified the ZIP CRC, index,
  settings, config and saved-world bytes. Evidence: `target/modrinth-button-acceptance/verification.json`.
- Modrinth's registered executable was found through its Windows protocol registration. It is
  launched directly for local archives, without shell-evaluating the profile path.

Not yet exercised: confirmation in Modrinth's actual installer, its final settings reconciliation,
gameplay in the copied profile, and Linux runtime for this follow-up. The user reviews UI/gameplay.
Do not claim an installed production copy until the native installer has finished.

The new follow-up changes have not been committed or published. Separate explicit Git approval is
required for staging/commit/push and the v0.4.1 tag/release; the 0.4.0 approval covered its prepared
snapshot and was already fulfilled.
