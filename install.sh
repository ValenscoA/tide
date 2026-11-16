#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

REPO="ValenscoA/tide"
INSTALL_DIR="${TIDE_PREFIX:-$HOME/.local/bin}"
VERSION=""
FROM_SOURCE=0
VERBOSE=0

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--verbose) VERBOSE=1 ;;
		--from-source) FROM_SOURCE=1 ;;
		--version)
			shift
			[ $# -gt 0 ] || die "--version requires a tag argument (e.g. --version v0.1.0)"
			VERSION="$1"
			;;
		--version=*) VERSION="${1#--version=}" ;;
		-h|--help)
			cat <<EOF
Usage: install.sh [--version vX.Y.Z] [--from-source] [--verbose]

Downloads the latest (or pinned) tide release binary for the current
platform and installs it to \$TIDE_PREFIX (default: \$HOME/.local/bin).
Falls back to building from source with cargo when needed.

Options:
  --version vX.Y.Z   Pin a specific release tag instead of latest
  --from-source      Clone and build from source with cargo instead of downloading a release binary
  --verbose          Show download/build progress
  -h, --help         Show this help
EOF
			exit 0
			;;
		*) die "unknown argument: $1" ;;
	esac
	shift
done

mkdir -p "$INSTALL_DIR"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

build_from_source() {
	command -v git >/dev/null 2>&1 || die "git is required to build from source"
	command -v cargo >/dev/null 2>&1 || die "cargo is required to build from source (https://rustup.rs)"

	local clone_dir="$TMP_DIR/src"
	if [ "$VERBOSE" = 1 ]; then
		git clone --depth 1 ${VERSION:+--branch "$VERSION"} "https://github.com/$REPO.git" "$clone_dir"
		cargo build --release --bin tide --manifest-path "$clone_dir/Cargo.toml"
	else
		printf "Cloning and building tide from source... "
		git clone -q --depth 1 ${VERSION:+--branch "$VERSION"} "https://github.com/$REPO.git" "$clone_dir"
		cargo build --quiet --release --bin tide --manifest-path "$clone_dir/Cargo.toml"
		echo "done"
	fi

	install -m 755 "$clone_dir/target/release/tide" "$INSTALL_DIR/tide"
}

install_prebuilt() {
	local asset="$1"
	local url
	if [ -n "$VERSION" ]; then
		url="https://github.com/$REPO/releases/download/$VERSION/$asset"
	else
		url="https://github.com/$REPO/releases/latest/download/$asset"
	fi

	local tmp_bin="$TMP_DIR/tide"
	if command -v curl >/dev/null 2>&1; then
		if [ "$VERBOSE" = 1 ]; then
			echo "Downloading $url"
			curl -fL --retry 3 --retry-delay 2 -o "$tmp_bin" "$url" || return 1
		else
			printf "Downloading tide... "
			if ! curl -fsSL --retry 3 --retry-delay 2 -o "$tmp_bin" "$url"; then
				echo "unavailable"
				return 1
			fi
			echo "done"
		fi
	elif command -v wget >/dev/null 2>&1; then
		if [ "$VERBOSE" = 1 ]; then
			echo "Downloading $url"
			wget -O "$tmp_bin" "$url" || return 1
		else
			printf "Downloading tide... "
			if ! wget -q -O "$tmp_bin" "$url"; then
				echo "unavailable"
				return 1
			fi
			echo "done"
		fi
	else
		die "need curl or wget on PATH"
	fi

	[ -s "$tmp_bin" ] || return 1
	chmod +x "$tmp_bin"
	mv "$tmp_bin" "$INSTALL_DIR/tide"
}

if [ "$FROM_SOURCE" = 1 ]; then
	build_from_source
else
	os="$(uname -s)"
	arch="$(uname -m)"
	ASSET=""
	case "$os/$arch" in
		Linux/x86_64)                ASSET="tide-linux-x86_64" ;;
		Linux/aarch64|Linux/arm64)   ASSET="tide-linux-aarch64" ;;
		Darwin/x86_64)               ASSET="tide-macos-x86_64" ;;
		Darwin/arm64|Darwin/aarch64) ASSET="tide-macos-aarch64" ;;
	esac

	if [ -n "$ASSET" ] && install_prebuilt "$ASSET"; then
		:
	elif command -v cargo >/dev/null 2>&1 && command -v git >/dev/null 2>&1; then
		echo "No prebuilt binary available for $os/$arch; building from source..." >&2
		build_from_source
	else
		die "no prebuilt binary for $os/$arch and cargo/git not available — install Rust (https://rustup.rs) or grab a release from https://github.com/$REPO/releases"
	fi
fi

echo "Installed tide -> $INSTALL_DIR/tide"
if ! "$INSTALL_DIR/tide" --help >/dev/null 2>&1; then
	echo "warning: '$INSTALL_DIR/tide --help' exited nonzero — check shared library dependencies" >&2
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
