#!/bin/sh
# Skink installer for Linux and macOS.
#
#   curl -fsSL https://skink.cc/install.sh | sh
#
# What it does: downloads the right prebuilt binary for this machine, verifies its
# SHA-256, installs it to ~/.local/bin (no sudo), and adds that directory to your
# PATH in your shell's startup file if it is not there already.
#
# Options (environment variables, or flags after `sh -s --`):
#   SKINK_VERSION=v0.1.0    install a specific release (default: latest)
#   SKINK_INSTALL_DIR=DIR   install somewhere else (default: ~/.local/bin)
#   SKINK_NO_MODIFY_PATH=1  do not touch shell startup files   (flag: --no-modify-path)
#   --uninstall             remove skink and the PATH lines this script added
#   SKINK_BASE_URL=URL      download from URL instead of GitHub (testing, mirrors)
set -eu

REPO="xuckless/skink"
# minisign public key, filled in by the release workflow when releases are
# signed. With it and `minisign` installed, SHA256SUMS's signature is checked.
SKINK_PUBKEY=''

INSTALL_DIR="${SKINK_INSTALL_DIR:-$HOME/.local/bin}"
MODIFY_PATH=1
UNINSTALL=0
[ "${SKINK_NO_MODIFY_PATH:-}" = "1" ] && MODIFY_PATH=0
for a in "$@"; do
  case "$a" in
    --no-modify-path) MODIFY_PATH=0 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,16p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "skink-install: unknown option $a" >&2; exit 2 ;;
  esac
done

if [ -t 1 ]; then B='\033[1m'; G='\033[32m'; Y='\033[33m'; R='\033[31m'; N='\033[0m'; else B=; G=; Y=; R=; N=; fi
say()  { printf '%b\n' "${B}skink${N} $*"; }
warn() { printf '%b\n' "${Y}warning:${N} $*" >&2; }
die()  { printf '%b\n' "${R}error:${N} $*" >&2; exit 1; }

MARK_BEGIN="# >>> skink >>>"
MARK_END="# <<< skink <<<"

rc_files() {
  # Files to edit: every startup file that exists for the shells people use,
  # or a sensible default for the login shell if none exist.
  found=""
  for f in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile" "$HOME/.profile"; do
    [ -f "$f" ] && found="$found $f"
  done
  [ -d "$HOME/.config/fish" ] && found="$found $HOME/.config/fish/conf.d/skink.fish"
  if [ -z "$found" ]; then
    case "${SHELL:-}" in
      */zsh)  found="$HOME/.zshrc" ;;
      */fish) found="$HOME/.config/fish/conf.d/skink.fish" ;;
      */bash) [ "$(uname -s)" = Darwin ] && found="$HOME/.bash_profile" || found="$HOME/.bashrc" ;;
      *)      found="$HOME/.profile" ;;
    esac
  fi
  echo "$found"
}

if [ "$UNINSTALL" = 1 ]; then
  rm -f "$INSTALL_DIR/skink" && say "removed $INSTALL_DIR/skink"
  for f in $(rc_files); do
    [ -f "$f" ] || continue
    if grep -qF "$MARK_BEGIN" "$f"; then
      tmp="$f.skink-tmp"
      awk -v b="$MARK_BEGIN" -v e="$MARK_END" '$0==b{s=1;next} $0==e{s=0;next} !s' "$f" > "$tmp" && mv "$tmp" "$f"
      say "cleaned $f"
    fi
  done
  exit 0
fi

# --- detect platform -------------------------------------------------------
os=$(uname -s); arch=$(uname -m)
case "$os" in
  Linux)  os_part="unknown-linux-musl" ;;
  Darwin) os_part="apple-darwin" ;;
  *) die "unsupported OS '$os'. On Windows use PowerShell:  irm https://skink.cc/install.ps1 | iex" ;;
esac
case "$arch" in
  x86_64|amd64)  arch_part="x86_64" ;;
  aarch64|arm64) arch_part="aarch64" ;;
  *) die "unsupported CPU '$arch' (prebuilt: x86_64, aarch64). Build from source: cargo install --git https://github.com/$REPO skink" ;;
esac
# Rosetta: a shell running as x86_64 on Apple Silicon should still get the native build.
if [ "$os" = Darwin ] && [ "$arch_part" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch_part="aarch64"
fi
target="$arch_part-$os_part"
asset="skink-$target.tar.gz"

if [ -n "${SKINK_BASE_URL:-}" ]; then
  base="$SKINK_BASE_URL"
elif [ -n "${SKINK_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/$SKINK_VERSION"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

# --- download --------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --retry 3 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  die "need curl or wget"
fi
command -v tar >/dev/null 2>&1 || die "need tar"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t skink)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "installing for $target"
fetch "$base/$asset" "$tmp/$asset" || die "download failed: $base/$asset (is there a release for $target?)"
fetch "$base/$asset.sha256" "$tmp/$asset.sha256" || die "checksum download failed"

# --- verify ----------------------------------------------------------------
want=$(awk '{print $1}' "$tmp/$asset.sha256")
if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp/$asset" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1;  then got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
elif command -v openssl >/dev/null 2>&1; then got=$(openssl dgst -sha256 "$tmp/$asset" | awk '{print $NF}')
else die "no sha256 tool found (sha256sum, shasum or openssl)"; fi
[ -n "$want" ] && [ "$want" = "$got" ] || die "checksum mismatch for $asset (expected $want, got $got); refusing to install"
if [ -n "$SKINK_PUBKEY" ] && command -v minisign >/dev/null 2>&1; then
  fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "SHA256SUMS download failed"
  fetch "$base/SHA256SUMS.minisig" "$tmp/SHA256SUMS.minisig" || die "release is not signed; refusing to install"
  minisign -Vq -P "$SKINK_PUBKEY" -m "$tmp/SHA256SUMS" -x "$tmp/SHA256SUMS.minisig" \
    || die "signature check failed for SHA256SUMS; refusing to install"
  grep -q "^$got  *\*\{0,1\}$asset\$" "$tmp/SHA256SUMS" || die "$asset is not in the signed SHA256SUMS"
  say "signature verified (minisign)"
fi

# --- install ---------------------------------------------------------------
tar -xzf "$tmp/$asset" -C "$tmp"
bin="$tmp/skink-$target/skink"
[ -f "$bin" ] || die "archive did not contain skink"
mkdir -p "$INSTALL_DIR"
# Atomic replace so a running skink is not corrupted.
cp "$bin" "$INSTALL_DIR/.skink.new" && chmod 755 "$INSTALL_DIR/.skink.new" && mv -f "$INSTALL_DIR/.skink.new" "$INSTALL_DIR/skink"
[ "$os" = Darwin ] && xattr -d com.apple.quarantine "$INSTALL_DIR/skink" 2>/dev/null || true
ver=$("$INSTALL_DIR/skink" --version 2>/dev/null || echo "skink")
say "${G}installed${N} $ver -> $INSTALL_DIR/skink"

# --- PATH ------------------------------------------------------------------
on_path=0
case ":$PATH:" in *":$INSTALL_DIR:"*) on_path=1 ;; esac
if [ "$on_path" = 0 ] && [ "$MODIFY_PATH" = 1 ]; then
  for f in $(rc_files); do
    case "$f" in
      *.fish) line="fish_add_path -g \"$INSTALL_DIR\"" ;;
      *)      line="export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
    esac
    mkdir -p "$(dirname "$f")"
    if [ -f "$f" ] && grep -qF "$MARK_BEGIN" "$f"; then continue; fi
    printf '\n%s\n%s\n%s\n' "$MARK_BEGIN" "$line" "$MARK_END" >> "$f"
    say "added $INSTALL_DIR to PATH in $f"
  done
  say "open a new terminal, or run:  export PATH=\"$INSTALL_DIR:\$PATH\""
elif [ "$on_path" = 0 ]; then
  warn "$INSTALL_DIR is not on your PATH (add it, or re-run without --no-modify-path)"
fi

say "run:  skink FILE   (or  skink .  for a folder).  Press Ctrl-E then type 'help' for the docs."
say "icons use a Nerd Font in your terminal (Ghostty/Kitty/WezTerm have one built in); SKINK_ICONS=ascii turns them off."
