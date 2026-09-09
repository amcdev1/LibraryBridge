#!/bin/sh
# Install LibraryBridge's desktop integration for the current user.
#
# The window's icon in the taskbar and app menu is provided by a desktop
# entry, not by the window itself: on Wayland compositors ignore the window
# icon property entirely and look up the entry by the window's app id
# ("librarybridge"). Without this file the desktop shows a generic icon.
#
# This copies the .desktop and the hicolor icons into ~/.local/share, and
# symlinks the binaries into ~/.local/bin so the desktop entry can find them
# when launched from the app grid and so the entry is considered launchable.
# It does not touch anything in the repository.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/linux/usr/share" && pwd)
BINROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor"
BIN="$HOME/.local/bin"

mkdir -p "$APPS" "$ICONS" "$BIN"

install -Dm644 "$ROOT/applications/librarybridge.desktop" \
    "$APPS/librarybridge.desktop"

for icon in "$ROOT"/icons/hicolor/*/apps/librarybridge.png; do
    size=$(dirname -- "$(dirname -- "$icon")")   # .../hicolor/<size>/apps
    size=${size##*/}
    install -Dm644 "$icon" "$ICONS/$size/apps/librarybridge.png"
done

# The release binaries live in the build tree; the desktop entry references
# them by name. Symlink them into a directory that is on PATH (the script
# assumes ~/.local/bin is, which most desktops put there by default), so the
# entry is launchable from the app grid as well as from a terminal.
for name in librarybridge librarybridge-gui; do
    src="$BINROOT/target/release/$name"
    if [ -x "$src" ]; then
        ln -sf "$src" "$BIN/$name"
    fi
done

# Refresh the app menu and icon caches if the tools are present. Nothing here
# failing is fatal: the entry is already in place.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPS"
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q --ignore-theme-index "$HOME/.local/share/icons/hicolor" 2>/dev/null \
        || true
fi

echo "Installed LibraryBridge desktop integration."
echo "  $APPS/librarybridge.desktop"
echo "  $ICONS/<size>/apps/librarybridge.png"
echo "  $BIN/librarybridge{,-gui} -> target/release"
echo "Restart LibraryBridge (and the app menu) to see the icon."