#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

REPO="BreakLime/lumitide"
INSTALL_DIR="${LUMITIDE_PREFIX:-$HOME/.local/bin}"
VERSION=""
VERBOSE=0

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--verbose) VERBOSE=1 ;;
		--version)
			shift
			[ $# -gt 0 ] || die "--version requires a tag argument (e.g. --version v1.0.8)"
			VERSION="$1"
			;;
		--version=*) VERSION="${1#--version=}" ;;
		-h|--help)
			cat <<EOF
Usage: install.sh [--version vX.Y.Z] [--verbose]

Downloads the latest (or pinned) lumitide release binary for the current
platform and installs it to \$LUMITIDE_PREFIX (default: \$HOME/.local/bin).

Options:
  --version vX.Y.Z   Pin a specific release tag instead of latest
  --verbose          Show download progress
  -h, --help         Show this help
EOF
			exit 0
			;;
		*) die "unknown argument: $1" ;;
	esac
	shift
done

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
	Linux/x86_64)   ASSET="lumitide-linux" ;;
	Darwin/x86_64)  ASSET="lumitide-macos" ;;
	Darwin/arm64)
		ASSET="lumitide-macos"
		echo "note: Apple Silicon detected; the x86_64 binary will run under Rosetta 2." >&2
		;;
	*)
		die "no prebuilt binary for $os/$arch — build from source: https://github.com/$REPO#build-from-source"
		;;
esac

if [ -n "$VERSION" ]; then
	URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"
else
	URL="https://github.com/$REPO/releases/latest/download/$ASSET"
fi

mkdir -p "$INSTALL_DIR"
TMP="$(mktemp "$INSTALL_DIR/lumitide.XXXXXX")"
trap 'rm -f "$TMP"' EXIT

if command -v curl >/dev/null 2>&1; then
	if [ "$VERBOSE" = 1 ]; then
		echo "Downloading $URL"
		curl -fL --retry 3 --retry-delay 2 -o "$TMP" "$URL"
	else
		printf "Downloading lumitide... "
		if ! curl -fsSL --retry 3 --retry-delay 2 -o "$TMP" "$URL"; then
			echo "FAILED"
			die "download failed: $URL"
		fi
		echo "done"
	fi
elif command -v wget >/dev/null 2>&1; then
	if [ "$VERBOSE" = 1 ]; then
		echo "Downloading $URL"
		wget -O "$TMP" "$URL"
	else
		printf "Downloading lumitide... "
		if ! wget -q -O "$TMP" "$URL"; then
			echo "FAILED"
			die "download failed: $URL"
		fi
		echo "done"
	fi
else
	die "need curl or wget on PATH"
fi

[ -s "$TMP" ] || die "downloaded file is empty: $URL"

# --- Checksum verification ---
CHECKSUMS_URL="${URL%/*}/sha256sums.txt"
TMP_SUMS="$(mktemp)"
trap 'rm -f "$TMP" "$TMP_SUMS"' EXIT
GOT_SUMS=0
if command -v curl >/dev/null 2>&1; then
	curl -fsSL --retry 2 -o "$TMP_SUMS" "$CHECKSUMS_URL" 2>/dev/null && GOT_SUMS=1 || true
elif command -v wget >/dev/null 2>&1; then
	wget -q -O "$TMP_SUMS" "$CHECKSUMS_URL" 2>/dev/null && GOT_SUMS=1 || true
fi
if [ "$GOT_SUMS" = 1 ] && [ -s "$TMP_SUMS" ]; then
	EXPECTED="$(grep "  $ASSET\$" "$TMP_SUMS" | awk '{print $1}')"
	if [ -n "$EXPECTED" ]; then
		if command -v sha256sum >/dev/null 2>&1; then
			ACTUAL="$(sha256sum "$TMP" | awk '{print $1}')"
		elif command -v shasum >/dev/null 2>&1; then
			ACTUAL="$(shasum -a 256 "$TMP" | awk '{print $1}')"
		else
			echo "warning: no sha256 tool found — skipping checksum verification" >&2
			EXPECTED=""
		fi
		if [ -n "$EXPECTED" ]; then
			[ "$EXPECTED" = "$ACTUAL" ] || die "checksum mismatch for $ASSET (expected $EXPECTED, got $ACTUAL)"
			[ "$VERBOSE" = 1 ] && echo "Checksum OK: $ACTUAL"
		fi
	else
		echo "warning: $ASSET not found in sha256sums.txt — skipping verification" >&2
	fi
else
	echo "warning: could not fetch sha256sums.txt — skipping checksum verification" >&2
fi

chmod +x "$TMP"
mv "$TMP" "$INSTALL_DIR/lumitide"

# Smoke-test: `lumitide --help` exits 0 iff the binary loaded its shared
# libs successfully. Keep the outer exit 0 either way — the binary is
# already installed; a dynamic-linker failure will surface its own message.
echo "Installed lumitide → $INSTALL_DIR/lumitide"
if ! "$INSTALL_DIR/lumitide" --help >/dev/null 2>&1; then
	echo "warning: '$INSTALL_DIR/lumitide --help' exited nonzero — check shared library dependencies" >&2
fi

case ":$PATH:" in
	*":$INSTALL_DIR:"*) ;;
	*)
		echo ""
		echo "$INSTALL_DIR is not on your PATH. Add it with:"
		case "${SHELL:-}" in
			*/zsh)  echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.zshrc && exec zsh" ;;
			*/bash) echo "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc && exec bash" ;;
			*)      echo "  export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
		esac
		;;
esac

echo ""
echo "Run 'lumitide' to start."
