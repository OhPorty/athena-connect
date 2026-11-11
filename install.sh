#!/bin/sh
# Installs athena-connect on Linux:
#   curl -fsSL https://raw.githubusercontent.com/OhPorty/athena-connect/main/install.sh | sh
# Downloads the latest release for this computer, checks it against the
# release's SHA256SUMS, and puts it in ~/.local/bin (or $ATHENA_CONNECT_DIR).
set -eu

REPO="OhPorty/athena-connect"
DIR="${ATHENA_CONNECT_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux) ;;
  *) echo "athena-connect: this installer is for Linux. On Windows, use install.ps1." >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) ARCH=x86_64 ;;
  aarch64 | arm64) ARCH=aarch64 ;;
  *) echo "athena-connect: there's no build for $(uname -m) yet." >&2; exit 1 ;;
esac
ASSET="athena-connect-linux-$ARCH"
BASE="${ATHENA_CONNECT_RELEASES:-https://github.com/$REPO/releases/latest/download}"

fetch() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$2" "$1"
  else echo "athena-connect: needs curl or wget to download." >&2; exit 1
  fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "Downloading $ASSET..."
fetch "$BASE/$ASSET" "$TMP/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"

EXPECTED="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | cut -d' ' -f1)"
ACTUAL="$(sha256sum "$TMP/$ASSET" | cut -d' ' -f1)"
if [ -z "$EXPECTED" ] || [ "$EXPECTED" != "$ACTUAL" ]; then
  echo "athena-connect: the download doesn't match the release's checksum; not installing it." >&2
  exit 1
fi

mkdir -p "$DIR"
install -m 755 "$TMP/$ASSET" "$DIR/athena-connect"
echo "Installed $("$DIR/athena-connect" --version) to $DIR/athena-connect"

case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "Note: $DIR isn't on your PATH. Add this to your shell's startup file:  export PATH=\"$DIR:\$PATH\"" ;;
esac
cat <<EOF

Next:
  athena-connect login https://your-athena     (once: pairs this computer)
  cd /the/folder && athena-connect             (gives Athena that folder until you close the terminal)
EOF
