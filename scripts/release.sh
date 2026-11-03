#!/usr/bin/env bash
# Builds the release tarball and its checksums into dist/.
#
#   scripts/release.sh              build, pack, checksum
#   COSIGN=1 scripts/release.sh     also sign SHA256SUMS with cosign
#
# Signing is keyless by default and needs a browser or an OIDC token.
# Set COSIGN_KEY to a key file to sign with a key instead.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
    echo "release.sh builds the x86_64 Linux tarball; this host is $(uname -sm)" >&2
    exit 1
fi

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$version" ]; then
    echo "could not read the version from Cargo.toml" >&2
    exit 1
fi
# The machine's own architecture: the workflow runs this on x86_64 and aarch64 runners.
arch="$(uname -m)"
name="ours-${version}-${arch}-linux"
dist="dist"
stage="${dist}/${name}"

cargo build --release --locked -p ours
bin="${CARGO_TARGET_DIR:-target}/release/ours"

rm -rf "$stage" "${dist}/${name}.tar.gz" "${dist}/SHA256SUMS" "${dist}/SHA256SUMS.bundle"
mkdir -p "$stage"
install -m 0755 "$bin" "$stage/ours"
strip "$stage/ours"
cp ours.example.toml README.md CHANGELOG.md CONTRIBUTING.md SECURITY.md "$stage/"
cp -r manifests deploy docs "$stage/"
if [ -f LICENSE ]; then
    cp LICENSE "$stage/"
fi

epoch="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${epoch}" \
    -C "$dist" -czf "${dist}/${name}.tar.gz" "$name"
rm -rf "$stage"

(cd "$dist" && sha256sum "${name}.tar.gz" > SHA256SUMS)
echo "wrote ${dist}/${name}.tar.gz and ${dist}/SHA256SUMS"

if [ -n "${COSIGN:-}" ]; then
    if [ -n "${COSIGN_KEY:-}" ]; then
        cosign sign-blob --yes --key "$COSIGN_KEY" \
            --bundle "${dist}/SHA256SUMS.bundle" "${dist}/SHA256SUMS"
    else
        cosign sign-blob --yes --bundle "${dist}/SHA256SUMS.bundle" "${dist}/SHA256SUMS"
    fi
    echo "signed: ${dist}/SHA256SUMS.bundle"
else
    cat <<MSG
not signed. To sign the checksums with cosign:

  COSIGN=1 scripts/release.sh

or by hand:

  cosign sign-blob --yes --bundle ${dist}/SHA256SUMS.bundle ${dist}/SHA256SUMS

and verify with:

  cosign verify-blob --bundle SHA256SUMS.bundle \\
    --certificate-identity <signer> --certificate-oidc-issuer <issuer> SHA256SUMS
MSG
fi
