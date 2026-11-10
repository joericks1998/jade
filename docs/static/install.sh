#!/usr/bin/env sh
# Jade installer — downloads the prebuilt binary for your platform from GitHub Releases
# Usage: curl -fsSL https://jadelang.org/install.sh | sh

set -e

REPO="joericks1998/jade"
INSTALL_DIR="${JADE_INSTALL_DIR:-/usr/local/bin}"
BINARY="jade"

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64) ARCHIVE="jade-macos-arm64.tar.gz" ;;
      *)
        echo "Unsupported macOS architecture: $ARCH"
        echo "Jade ships an Apple Silicon (arm64) build only; Intel macOS is not supported."
        echo "On an Intel Mac, run the arm64 build under Rosetta 2, or build from source:"
        echo "  https://github.com/$REPO"
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      x86_64|amd64) ARCHIVE="jade-linux-x86_64.tar.gz" ;;
      *)
        echo "Unsupported Linux architecture: $ARCH"
        echo "Download manually from: https://github.com/$REPO/releases"
        exit 1
        ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS"
    echo "Jade supports macOS and Linux only. On Windows, install inside WSL2."
    exit 1
    ;;
esac

# Resolve latest release tag via GitHub API
TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep '"tag_name"' \
  | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')

if [ -z "$TAG" ]; then
  echo "Could not determine latest release. Check https://github.com/$REPO/releases"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$TAG/$ARCHIVE"

echo "Installing jade $TAG for $OS/$ARCH..."
echo "  From: $URL"
echo "  To:   $INSTALL_DIR/$BINARY"
echo ""

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

CHECKSUMS_URL="https://github.com/$REPO/releases/download/$TAG/checksums.txt"

echo "Downloading checksums..."
curl -fsSL --progress-bar "$CHECKSUMS_URL" -o "$TMP_DIR/checksums.txt"
echo "Downloading archive..."
curl -fsSL --progress-bar "$URL" -o "$TMP_DIR/$ARCHIVE"

echo "Verifying checksum..."
case "$OS" in
  Darwin) SHASUM_CMD="shasum -a 256" ;;
  *)      SHASUM_CMD="sha256sum" ;;
esac
(cd "$TMP_DIR" && grep "$ARCHIVE" checksums.txt | $SHASUM_CMD -c -)

tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"
chmod +x "$TMP_DIR/$BINARY"

install_file() {
  src="$1"; dest="$2"; dest_dir="$(dirname "$dest")"
  if [ -w "$dest_dir" ] 2>/dev/null || [ ! -e "$dest_dir" ] && mkdir -p "$dest_dir" 2>/dev/null; then
    mv "$src" "$dest"
  else
    sudo mkdir -p "$dest_dir"
    sudo mv "$src" "$dest"
  fi
}

install_file "$TMP_DIR/$BINARY" "$INSTALL_DIR/$BINARY"

echo ""
echo "jade $TAG installed to $INSTALL_DIR/$BINARY"

# Warn if a *different* jade is earlier in PATH — it would shadow the one we just
# installed, so `jade` keeps running the old binary and the install looks like it
# did nothing. (This is exactly the trap a ~/.local/bin/jade sets ahead of
# /usr/local/bin.)
RESOLVED=$(command -v jade 2>/dev/null || true)
if [ -n "$RESOLVED" ] && [ "$RESOLVED" != "$INSTALL_DIR/$BINARY" ]; then
  echo ""
  echo "Warning: another 'jade' is earlier in your PATH and will shadow this install:"
  echo "           $RESOLVED"
  echo "         Remove it (rm '$RESOLVED') or re-run with JADE_INSTALL_DIR=$(dirname "$RESOLVED")."
fi
echo ""

# A program that dereferences a prompt runs a model the dovata process holds;
# see https://github.com/jadeinc/dovata to install it and add a model.
echo ""
echo "Run 'jade --help' to get started."
