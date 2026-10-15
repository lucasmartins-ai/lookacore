#!/bin/sh
# Installs lkc and looka from a GitHub release, verifying the archive's SHA-256.
#   curl -fsSL https://raw.githubusercontent.com/lucasmartins-ai/lookacore/main/install.sh | sh
#   LKC_VERSION=v0.2.0 LKC_BIN_DIR=/usr/local/bin sh install.sh
set -eu
repo="lucasmartins-ai/lookacore"
version="${LKC_VERSION:-latest}"
bin_dir="${LKC_BIN_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  *) echo "lkc: no prebuilt binary for $(uname -s) $(uname -m); build from source: cargo install --git https://github.com/$repo" >&2; exit 1 ;;
esac
if [ "$version" = latest ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$version" ] || { echo "lkc: could not resolve the latest release" >&2; exit 1; }
fi
case "$version" in v[0-9]*.[0-9]*.[0-9]*) ;; *) echo "lkc: unexpected version: $version" >&2; exit 1 ;; esac

archive="lookacore-$version-$target.tar.gz"
base="${LKC_BASE_URL:-https://github.com/$repo/releases/download/$version}"  # override: a mirror, or a local test
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL -o "$tmp/$archive" "$base/$archive"
curl -fsSL -o "$tmp/$archive.sha256" "$base/$archive.sha256"
expected="$(cut -d' ' -f1 < "$tmp/$archive.sha256")"
actual="$(shasum -a 256 "$tmp/$archive" 2>/dev/null || sha256sum "$tmp/$archive")"
[ "${actual%% *}" = "$expected" ] || { echo "lkc: checksum mismatch for $archive" >&2; exit 1; }

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$bin_dir"
install -m 0755 "$tmp/lkc" "$tmp/looka" "$bin_dir/"
echo "lkc $version installed to $bin_dir"
case ":$PATH:" in *":$bin_dir:"*) ;; *) echo "add it to PATH: export PATH=\"$bin_dir:\$PATH\"" ;; esac
