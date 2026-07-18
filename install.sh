#!/bin/sh
# Install pengepul. Usage:
#   curl -fsSL https://raw.githubusercontent.com/gitshrl/pengepul/main/install.sh | sh
#
# Override the install directory with PENGEPUL_BIN_DIR, and the version with
# PENGEPUL_VERSION (defaults to the latest release).
set -eu

REPO="gitshrl/pengepul"
BIN_DIR="${PENGEPUL_BIN_DIR:-/usr/local/bin}"
VERSION="${PENGEPUL_VERSION:-latest}"

die() {
	echo "pengepul: $1" >&2
	exit 1
}

as_root() {
	if [ -n "$need_root" ]; then
		sudo "$@"
	else
		"$@"
	fi
}

case "$(uname -s)-$(uname -m)" in
	Linux-x86_64 | Linux-amd64) asset="pengepul-linux-x86_64.tar.gz" ;;
	Darwin-arm64 | Darwin-aarch64) asset="pengepul-macos-arm64.tar.gz" ;;
	*) die "no prebuilt binary for $(uname -s) $(uname -m). Build from source: cargo install --git https://github.com/$REPO.git --locked" ;;
esac

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

if [ "$VERSION" = "latest" ]; then
	base="https://github.com/$REPO/releases/latest/download"
else
	base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d)"
need_root=""
stage=""
cleanup() {
	rm -rf "$tmp"
	[ -z "$stage" ] || as_root rm -f "$stage"
}
trap cleanup EXIT INT TERM

echo "pengepul: downloading $asset"
curl -fsSL "$base/$asset" -o "$tmp/$asset" || die "download failed: $base/$asset"

# The checksum lives beside the asset, so this catches a truncated or corrupted
# download, not a compromised release.
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || die "could not fetch $base/checksums.txt"
if command -v sha256sum >/dev/null 2>&1; then
	got="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
elif command -v shasum >/dev/null 2>&1; then
	got="$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)"
else
	die "need sha256sum or shasum to verify the download"
fi
want="$(grep " $asset\$" "$tmp/checksums.txt" | cut -d' ' -f1)"
[ -n "$want" ] || die "no checksum for $asset in checksums.txt"
[ "$want" = "$got" ] || die "checksum mismatch for $asset"

tar xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
[ -f "$tmp/pengepul" ] || die "archive did not contain pengepul"

if [ ! -d "$BIN_DIR" ]; then
	mkdir -p "$BIN_DIR" 2>/dev/null || die "$BIN_DIR does not exist and could not be created"
fi

if [ -w "$BIN_DIR" ]; then
	need_root=""
elif command -v sudo >/dev/null 2>&1; then
	echo "pengepul: $BIN_DIR needs root"
	need_root=1
else
	die "$BIN_DIR is not writable. Retry with: PENGEPUL_BIN_DIR=\$HOME/.local/bin sh"
fi

# Stage inside $BIN_DIR so the last step is a same-filesystem rename: an abort
# can never leave a half-written binary at the destination.
stage="$BIN_DIR/.pengepul.$$"
as_root install -m 755 "$tmp/pengepul" "$stage" || die "could not install to $BIN_DIR"
as_root mv "$stage" "$BIN_DIR/pengepul" || die "could not install to $BIN_DIR"
stage=""

echo "pengepul: installed to $BIN_DIR/pengepul"
case ":$PATH:" in
	*":$BIN_DIR:"*) ;;
	*) echo "pengepul: note that $BIN_DIR is not on your PATH" ;;
esac
echo
echo "Next: pengepul login, then pengepul serve"
