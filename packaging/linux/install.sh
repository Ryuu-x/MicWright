#!/bin/bash
# Installs MicWright for the current user (no root needed):
#   ./install.sh              app in ~/.local/share/MicWright, `micwright` in ~/.local/bin,
#                             and a menu entry
#   ./install.sh --uninstall  removes those and the start-at-login entry; your settings,
#                             presets and sounds in ~/.config/MicWright stay
set -euo pipefail
app_id=io.github.ryuu_x.MicWright
data=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
dir="$data/MicWright"
bin="$HOME/.local/bin/micwright"
entry="$data/applications/$app_id.desktop"

if [ "${1:-}" = "--uninstall" ]; then
    "$dir/micwright" --quit 2>/dev/null || true
    rm -rf "$dir" "$bin" "$entry" "$config/autostart/$app_id.desktop"
    echo "MicWright removed. Your settings are still in $config/MicWright."
    exit 0
fi

here=$(cd "$(dirname "$0")" && pwd)
# A running copy keeps the old file busy.
"$bin" --quit 2>/dev/null && sleep 1 || true
mkdir -p "$dir" "$(dirname "$bin")" "$(dirname "$entry")"
cp "$here/micwright" "$here/LICENSE" "$here/THIRD-PARTY-NOTICES.txt" "$here/README.txt" "$dir/"
ln -sf "$dir/micwright" "$bin"
sed "s|^Exec=.*|Exec=\"$dir/micwright\"|" "$here/$app_id.desktop" > "$entry"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" 2>/dev/null || true
echo "MicWright installed. Start it from your app menu, or run: micwright"
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo "(~/.local/bin isn't on your PATH; the menu entry works either way.)" ;; esac
