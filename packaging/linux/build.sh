#!/bin/bash
# Builds the Linux release inside the build container (Ubuntu 22.04, so the binary runs on
# every current distro). From the repo root:
#   docker build -t micwright-build -f packaging/linux/Dockerfile.build packaging/linux
#   docker run --rm -v "$PWD:/src" -v mw-cargo-registry:/opt/cargo/registry \
#     -v mw-target:/target -e CARGO_TARGET_DIR=/target micwright-build bash packaging/linux/build.sh
# -> dist/MicWright-<version>-linux-x86_64.tar.gz
set -euo pipefail
cd "$(dirname "$0")/../.."
target=${CARGO_TARGET_DIR:-target}
version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
name="MicWright-$version-linux-x86_64"

echo "== Tests"
cargo test --release --quiet -p mw-dsp -p mw-engine -p mw-app

echo "== Third-party notices"
cargo run --release --quiet -p third-party-notices -- linux

echo "== App"
cargo build --release -p mw-app

echo "== Package"
stage=$(mktemp -d)/$name
mkdir -p "$stage"
cp "$target/release/micwright" "$stage/"
strip "$stage/micwright"
cp LICENSE packaging/linux/THIRD-PARTY-NOTICES.txt packaging/linux/README.txt packaging/linux/install.sh "$stage/"
cp packaging/linux/io.github.ryuu_x.MicWright.desktop "$stage/"
chmod +x "$stage/micwright" "$stage/install.sh"
mkdir -p dist
tar -C "$(dirname "$stage")" -czf "dist/$name.tar.gz" "$name"
rm -rf "$(dirname "$stage")"
echo "Done: dist/$name.tar.gz ($(du -h "dist/$name.tar.gz" | cut -f1))"
