#!/bin/bash
# Build a release: format check, clippy, tests, then a static x86_64 musl binary built with
# CC=/bin/false CXX=/bin/false (any dependency that compiles C fails here), and a tarball
# with everything the target machine needs:
#
#   target/dist/chestcraft-<version>.tar.gz
#     chestcraft-<version>/chestcraft            the binary
#     chestcraft-<version>/install.sh            run there as root (see its header)
#     chestcraft-<version>/units/*               systemd units
#     chestcraft-<version>/config.example.toml
#     chestcraft-<version>/smartd.conf
#     chestcraft-<version>/SHA256SUMS
#
# Usage: deploy/release.sh [--skip-tests]
set -euo pipefail

cd "$(dirname "$0")/.."
skip_tests=0
for arg in "$@"; do
    case "$arg" in
        --skip-tests) skip_tests=1 ;;
        *) echo "usage: $0 [--skip-tests]" >&2; exit 2 ;;
    esac
done

target=x86_64-unknown-linux-musl
pkg=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
rev=$(git describe --always --dirty --exclude '*' 2>/dev/null || echo unknown)
version="$pkg+$rev"

step() { printf '\n== %s\n' "$*"; }

step "cargo fmt --check"
cargo fmt --check

step "cargo clippy"
cargo clippy --locked --all-targets -- -D warnings

if [[ $skip_tests == 0 ]]; then
    step "cargo test"
    cargo test --locked
fi

step "release build $version ($target, no C compiler)"
CC=/bin/false CXX=/bin/false CHESTCRAFT_VERSION="$version" \
    cargo build --locked --release --target "$target"
bin="target/$target/release/chestcraft"

# A static binary has no program interpreter (no dynamic loader) and no shared libraries.
if readelf --program-headers "$bin" | grep -q INTERP; then
    echo "error: $bin is dynamically linked" >&2
    exit 1
fi
if readelf --dynamic "$bin" | grep -q NEEDED; then
    echo "error: $bin needs shared libraries" >&2
    exit 1
fi
reported=$("$bin" --version)
if [[ $reported != "chestcraft $version" ]]; then
    echo "error: the binary reports \"$reported\", expected \"chestcraft $version\"" >&2
    exit 1
fi

step "tarball"
name="chestcraft-$version"
stage="target/dist/$name"
rm -rf -- "$stage"
mkdir -p "$stage/units"
install -m 755 "$bin" "$stage/chestcraft"
install -m 755 deploy/install.sh "$stage/install.sh"
install -m 644 deploy/units/*.service deploy/units/*.timer "$stage/units/"
install -m 644 config.example.toml deploy/smartd.conf "$stage/"
(cd "$stage" && find . -type f ! -name SHA256SUMS -printf '%P\n' | sort | xargs sha256sum > SHA256SUMS)
tar --owner=0 --group=0 --numeric-owner --sort=name -C target/dist -czf "target/dist/$name.tar.gz" "$name"
rm -rf -- "$stage"

step "done"
echo "$reported"
echo "binary:  $bin ($(du -h "$bin" | cut -f1))"
sha256sum "$bin" | cut -d' ' -f1 | sed 's/^/sha256:  /'
echo "tarball: target/dist/$name.tar.gz"
echo
echo "Copy it to the server and run the installer there, e.g.:"
echo "  scp target/dist/$name.tar.gz server:"
echo "  ssh server 'tar xzf $name.tar.gz && sudo $name/install.sh'"
