#!/bin/sh
# Installs the latest treetop release for Linux:
#   curl -fsSL https://raw.githubusercontent.com/anthonyb8/treetop/main/install.sh | sh
# Set TREETOP_VERSION (e.g. v0.1.0) to install a specific release instead.
set -eu

REPO="anthonyb8/treetop"

if [ "$(uname -s)" != "Linux" ]; then
  echo "treetop ships Linux builds only; elsewhere: cargo install --git https://github.com/${REPO}" >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64 | amd64) ARCH="amd64" ;;
  aarch64 | arm64) ARCH="arm64" ;;
  *) echo "Unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

# Prefer ~/.local/bin when it is on PATH (no sudo needed), else /usr/local/bin.
if echo "$PATH" | tr ':' '\n' | grep -qx "$HOME/.local/bin"; then
  INSTALL_DIR="$HOME/.local/bin"
else
  INSTALL_DIR="/usr/local/bin"
fi

# The latest release, read from the redirect GitHub serves for it rather than
# the API, which rate-limits anonymous callers.
VERSION="${TREETOP_VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
  VERSION="${VERSION##*/}"
fi
case "$VERSION" in
  v*) ;;
  *) echo "Could not determine the latest treetop release" >&2; exit 1 ;;
esac

FILENAME="treetop-${VERSION}-linux-${ARCH}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${FILENAME}"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "Downloading treetop ${VERSION} for linux/${ARCH}..."
curl -fsSL "$URL" -o "${TMPDIR}/${FILENAME}"
curl -fsSL "${URL}.sha256" -o "${TMPDIR}/${FILENAME}.sha256"
(cd "$TMPDIR" && sha256sum -c "${FILENAME}.sha256" >/dev/null) || {
  echo "Checksum mismatch for ${FILENAME}; nothing was installed" >&2
  exit 1
}
tar xzf "${TMPDIR}/${FILENAME}" -C "$TMPDIR"

if [ -w "$INSTALL_DIR" ]; then
  install -m 0755 "${TMPDIR}/treetop" "${INSTALL_DIR}/treetop"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo install -d "$INSTALL_DIR"
  sudo install -m 0755 "${TMPDIR}/treetop" "${INSTALL_DIR}/treetop"
fi

echo "treetop ${VERSION} installed to ${INSTALL_DIR}/treetop"
