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

# `jade build` links two runtime archives into every executable it emits, so
# they are part of the toolchain, not optional extras. jade resolves them
# relative to its own location — <prefix>/bin/jade looks in <prefix>/lib/jade —
# so the two must be installed together. (Tarballs from before this layout
# carry only the binary; the `-d` check keeps this script working with them,
# in which case `jade build` needs JADE_RT_LIB/JADE_RUST_RT set by hand.)
LIB_DIR="$(dirname "$INSTALL_DIR")/lib/jade"

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

if [ -d "$TMP_DIR/lib" ]; then
  for archive in "$TMP_DIR"/lib/*.a; do
    [ -e "$archive" ] || continue
    install_file "$archive" "$LIB_DIR/$(basename "$archive")"
  done
fi

# Bundled inference providers (anthropic/openai .so's). They live under
# <prefix>/lib/jade/providers/ where `jade register` discovers them.
if [ -d "$TMP_DIR/lib/providers" ]; then
  for so in "$TMP_DIR"/lib/providers/*; do
    [ -e "$so" ] || continue
    install_file "$so" "$LIB_DIR/providers/$(basename "$so")"
  done
fi

echo ""
echo "jade $TAG installed to $INSTALL_DIR/$BINARY"
[ -d "$LIB_DIR" ] && echo "runtime archives installed to $LIB_DIR"
echo ""

# Set up an inference provider. Providers ship with jade; `jade register` picks
# one and stores your API key. When running via curl | sh, stdin is the pipe —
# read from /dev/tty so the prompt reaches the actual terminal.
if [ -e /dev/tty ]; then
  printf "Set up an inference provider (Anthropic / OpenAI) now? [Y/n] "
  read -r REPLY < /dev/tty
  case "$REPLY" in
    [nN]*)
      echo "Skipped. Run 'jade register' any time to choose a provider and set your API key."
      ;;
    *)
      echo ""
      # Don't fail the install if registration is cancelled or no provider shipped.
      "$INSTALL_DIR/$BINARY" register < /dev/tty || true
      ;;
  esac
else
  echo "Run 'jade register' to choose an inference provider and set your API key."
fi

echo ""
echo "Run 'jade --help' to get started."
