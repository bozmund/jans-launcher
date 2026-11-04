# Accepted fix: close player windows to the Windows system tray

The user reports that closing the app does not leave it in the Windows notification area and asks
for that background behaviour. Apply consistently to the launcher and companion player updater.

Closing either player window should leave one lightweight user-owned background/tray worker. During
setup, keep the running preparation alive until it finishes; do not abandon work when the UI hides.
The worker waits if native Modrinth installation is incomplete and automatically recognizes a single
completed marked independent profile. Existing explicit pause/disable choices must remain respected.
Expose Open, Check now and explicit Exit; do not silently claim active updates without a valid profile.

Keep the existing startup/update safeguards, remote-link checks and game-running guard. Test in
isolated state; no real startup registration or game/world changes merely for testing. Git commits,
pushes and a new application release require separate explicit authorization after validation.
