# Accepted fix: visible progress and duplicate setup prevention

The user reports that their brother waited without useful feedback and could click the setup button
twice. Fix the app's setup experience rather than assuming that the delay was a network fault.

Show immediate feedback beside the setup button, distinct stages for checking the release, detecting
an old profile, downloading missing files, verifying files, copying mods/settings/worlds, saving the
archive, and handing off to Modrinth. Show stage progress and elapsed time where measurable. Do not
display native Modrinth installation as completed merely because its installer was opened.

Allow only one setup operation across launcher/updater processes. While preparing, disable the action
and show that work is ongoing. Persist the prepared import so another click or an app restart reopens
the same pending installer/archive rather than building a second full copy. Explain that the user
must confirm Install in Modrinth, and preserve retry/reopen behavior if that dialog was closed.

Keep the existing migration guarantees: preserve player settings/worlds, retain the old shared profile,
use native local import, no authentication bypass or private database writes. Test duplicate attempts,
pending resume, visible stage transitions, failure cleanup and source preservation with disposable
fixtures. No Git mutations or publication are authorized by this bug report; obtain approval for a
concrete follow-up release after validation.
