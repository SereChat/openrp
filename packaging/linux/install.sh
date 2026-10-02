#!/bin/sh
# Installs OpenRP for the current user: the binary, plus the launcher entry
# and icon desktops use to show it in menus, docks and task switchers.
set -eu
cd "$(dirname "$0")"
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/256x256/apps"
cp openrp "$bin/openrp"
chmod 755 "$bin/openrp"
cp openrp.png "$data/icons/hicolor/256x256/apps/openrp.png"
# An absolute Exec works even when the bin folder is not on PATH.
sed "s|^Exec=.*|Exec=$bin/openrp|" openrp.desktop > "$data/applications/openrp.desktop"
echo "Installed OpenRP to $bin/openrp; it is now in your applications menu."
