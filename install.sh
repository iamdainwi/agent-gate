#!/usr/bin/env sh
# AgentGate installer
# Usage: curl -fsSL https://raw.githubusercontent.com/iamdanwi/agentgate/main/install.sh | sh
set -e

REPO="iamdanwi/agentgate"
BIN="agentgate"
INSTALL_DIR="${AGENTGATE_INSTALL_DIR:-/usr/local/bin}"

# ── Detect OS / arch ──────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-unknown-linux-musl"  ;;
      aarch64) TARGET="aarch64-unknown-linux-musl"  ;;
      *)       echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  Darwin)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-apple-darwin"   ;;
      arm64)   TARGET="aarch64-apple-darwin"  ;;
      *)       echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    TARGET="x86_64-pc-windows-msvc"
    EXT="zip"
    ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

# ── Resolve latest release tag ────────────────────────────────────────────────
if [ -z "$AGENTGATE_VERSION" ]; then
  AGENTGATE_VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | sed 's/.*"tag_name": "\(.*\)".*/\1/')"
fi

if [ -z "$AGENTGATE_VERSION" ]; then
  echo "Could not determine latest release version." >&2
  exit 1
fi

echo "Installing AgentGate ${AGENTGATE_VERSION} for ${TARGET}…"

ARCHIVE="${BIN}-${AGENTGATE_VERSION}-${TARGET}.${EXT}"
URL="https://github.com/${REPO}/releases/download/${AGENTGATE_VERSION}/${ARCHIVE}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ── Download ──────────────────────────────────────────────────────────────────
curl -fsSL --output "${TMP}/${ARCHIVE}" "$URL"

# ── Extract ───────────────────────────────────────────────────────────────────
if [ "$EXT" = "tar.gz" ]; then
  tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"
else
  unzip -q "${TMP}/${ARCHIVE}" -d "$TMP"
fi

# ── Install ───────────────────────────────────────────────────────────────────
BINARY_SRC="$TMP/$BIN"
[ -f "$BINARY_SRC" ] || BINARY_SRC="$TMP/${BIN}.exe"

if [ ! -f "$BINARY_SRC" ]; then
  echo "Binary not found in archive." >&2
  exit 1
fi

chmod +x "$BINARY_SRC"

if [ -w "$INSTALL_DIR" ]; then
  mv "$BINARY_SRC" "${INSTALL_DIR}/${BIN}"
else
  echo "Requesting sudo to install to ${INSTALL_DIR}…"
  sudo mv "$BINARY_SRC" "${INSTALL_DIR}/${BIN}"
fi

echo "Installed ${BIN} to ${INSTALL_DIR}/${BIN}"
"${INSTALL_DIR}/${BIN}" --version
