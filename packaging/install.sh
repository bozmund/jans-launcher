#!/bin/sh
# Installs jans-launcher for the current user (no root needed).
set -e
cd "$(dirname "$0")"
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/256x256/apps"
install -m 755 jans-launcher "$HOME/.local/bin/jans-launcher"
install -m 644 icon.png "$HOME/.local/share/icons/hicolor/256x256/apps/jans-launcher.png"
sed "s|^Exec=.*|Exec=$HOME/.local/bin/jans-launcher|" jans-launcher.desktop > "$HOME/.local/share/applications/jans-launcher.desktop"
echo "Installed. Start \"Jan's Launcher\" from your app menu or run ~/.local/bin/jans-launcher"
