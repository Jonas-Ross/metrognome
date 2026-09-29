#!/usr/bin/env bash
# Usage: scripts/package-release.sh <version> <out-dir>
# Fuses the two macOS release builds into one universal binary and tars it.
set -euo pipefail

version=${1:?usage: package-release.sh <version> <out-dir>}
out=${2:?usage: package-release.sh <version> <out-dir>}
arm=target/aarch64-apple-darwin/release/metrognome
intel=target/x86_64-apple-darwin/release/metrognome
name="metrognome-${version}-universal-apple-darwin"
stage="${out}/${name}"

mkdir -p "$stage"
lipo -create -output "${stage}/metrognome" "$arm" "$intel"
# Apple Silicon refuses to run an unsigned binary; ad-hoc is enough outside a DMG.
codesign --force --sign - "${stage}/metrognome"
lipo "${stage}/metrognome" -verify_arch arm64 x86_64
cp LICENSE README.md "$stage/"

tar -C "$out" -czf "${out}/${name}.tar.gz" "$name"
(cd "$out" && shasum -a 256 "${name}.tar.gz" > "${name}.tar.gz.sha256")
rm -rf "$stage"
printf '%s\n' "${out}/${name}.tar.gz"
