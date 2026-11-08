#!/usr/bin/env bash
# Install a Nerd Font for the current user (Linux/macOS). Skink draws file-type
# and status icons with Nerd Font glyphs; the FONT belongs to your terminal, so
# after installing, select it in the terminal's settings.
#
# Not needed in terminals that ship Nerd Font symbols (Ghostty, Kitty, WezTerm).
# Skink falls back to plain ASCII with SKINK_ICONS=ascii.
#
# Usage: scripts/install-nerd-font.sh [FontName]     (default: JetBrainsMono)
set -euo pipefail
font="${1:-JetBrainsMono}"
case "$(uname -s)" in
  Darwin) dest="$HOME/Library/Fonts" ;;
  *) dest="${XDG_DATA_HOME:-$HOME/.local/share}/fonts/NerdFonts" ;;
esac
url="https://github.com/ryanoasis/nerd-fonts/releases/latest/download/${font}.tar.xz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "Downloading $url"
curl -fL --proto '=https' -o "$tmp/font.tar.xz" "$url"
mkdir -p "$dest"
tar -xJf "$tmp/font.tar.xz" -C "$dest" --wildcards '*.ttf' '*.otf' 2>/dev/null || tar -xJf "$tmp/font.tar.xz" -C "$dest"
command -v fc-cache >/dev/null && fc-cache -f "$dest"
echo "Installed $font Nerd Font into $dest. Pick \"${font} Nerd Font Mono\" in your terminal."
