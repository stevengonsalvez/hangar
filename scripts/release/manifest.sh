#!/usr/bin/env bash
# Assemble, sign and verify release-manifest.json for one release.
#
#   bash scripts/release/manifest.sh <artifacts-dir> <version>
#
# <artifacts-dir> is what download-artifact wrote: one directory per CLI target
# (`<triple>/ainb-<version>-<triple>.tar.gz`) and one per desktop target
# (`desktop-<triple>/ainb-desktop-<version>-<triple>.<ext>`). Writes
# release-manifest.json and release-manifest.sig into <artifacts-dir>, and with
# BRIDGE=true also bridge/release-manifest.{json,sig}.
#
# Environment:
#   PUB             true: KEY is required and its public half must equal the key
#                   every shipped client pins (update.rs). false: KEY is ignored
#                   and a throwaway key signs, so the signing path still runs.
#   KEY             Ed25519 private key, PEM.
#   BRIDGE          true: also write the old-repo bridge manifest, which drops
#                   `desktop` and names this repository's root as `next_root`.
#   DESKTOP_SIGNED  true when the macOS bundles carry the Developer ID.
#   GITHUB_REPOSITORY  owner/name of the repository publishing the release.
set -euo pipefail

dir="${1:?usage: manifest.sh <artifacts-dir> <version>}"
version="${2:?usage: manifest.sh <artifacts-dir> <version>}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name the publishing repository}"

pinned="$(sed -n 's/^const RELEASE_SIGNING_PUBLIC_KEY_B64: &str = "\(.*\)";$/\1/p' \
  "$repo_root/crates/ainb-app/src/cli/update.rs")"
test -n "$pinned" || { echo "RELEASE_SIGNING_PUBLIC_KEY_B64 not found in update.rs" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
key="$work/key.pem"
if [ "${PUB:-false}" = true ]; then
  test -n "${KEY:-}" || { echo "AINB_RELEASE_SIGNING_KEY is required to publish" >&2; exit 1; }
  printf '%s\n' "$KEY" > "$key"
else
  openssl genpkey -algorithm ed25519 -out "$key"
  echo "dry run: signing with a throwaway key"
fi
chmod 600 "$key"
openssl pkey -in "$key" -pubout -out "$work/pub.pem"
public="$(openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | base64)"
if [ "${PUB:-false}" = true ] && [ "$public" != "$pinned" ]; then
  echo "the signing key's public half $public is not the pinned $pinned; refusing to sign" >&2
  exit 1
fi

sha_of() {
  local file
  file="$(find "$dir/$1" -maxdepth 1 -name "$2" -print -quit 2>/dev/null)"
  test -n "$file" || { echo "missing $1/$2" >&2; return 1; }
  sha256sum "$file" | awk '{print $1}'
}

cli="[]"
for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu; do
  archive="ainb-${version}-${target}.tar.gz"
  sha="$(sha_of "$target" "$archive")"
  cli="$(jq -c --arg t "$target" --arg a "$archive" --arg s "$sha" \
    '. + [{target: $t, archive: $a, sha256: $s}]' <<<"$cli")"
done

# The desktop bundles go under their own key, never into assets[]: a shipped
# CLI matches assets[] on target alone and takes the first hit, so a .dmg there
# would be installed as the ainb binary. `publish` needs every desktop leg, so a
# missing bundle here is an error, not an omission.
signed="${DESKTOP_SIGNED:-false}"
desktop="[]"
add_desktop() {
  # add_desktop <target> <format> <extension> <signed>
  local archive="ainb-desktop-${version}-$1.$3" sha
  sha="$(sha_of "desktop-$1" "$archive")"
  desktop="$(jq -c --arg t "$1" --arg f "$2" --arg a "$archive" --arg s "$sha" --argjson g "$4" \
    '. + [{target: $t, format: $f, archive: $a, sha256: $s, signed: $g}]' <<<"$desktop")"
}
add_desktop aarch64-apple-darwin dmg dmg "$signed"
add_desktop x86_64-apple-darwin dmg dmg "$signed"
add_desktop x86_64-unknown-linux-gnu appimage AppImage false
add_desktop x86_64-unknown-linux-gnu deb deb false

jq -n --arg v "$version" --argjson a "$cli" --argjson d "$desktop" \
  '{version: $v, assets: $a, desktop: $d}' > "$dir/release-manifest.json"

# Read it back before signing: every assets[] entry a CLI archive, every
# desktop entry a bundle of a known format.
jq -e --arg v "$version" '
  (.assets | length == 3) and all(.assets[]; .archive | startswith("ainb-" + $v + "-"))
  and (.desktop | length == 4)
  and all(.desktop[]; (.archive | startswith("ainb-desktop-" + $v + "-"))
                      and (.format | IN("dmg", "appimage", "deb"))
                      and (.signed | type == "boolean"))
' "$dir/release-manifest.json" >/dev/null \
  || { echo "release manifest has an entry under the wrong key" >&2; cat "$dir/release-manifest.json" >&2; exit 1; }

sign() {
  # sign <manifest.json>: writes the base64 signature beside it, then verifies it.
  local json="$1" sig="${1%.json}.sig"
  openssl pkeyutl -sign -rawin -inkey "$key" -in "$json" -out "$work/raw.sig"
  base64 < "$work/raw.sig" | tr -d '\n' > "$sig"
  base64 -d < "$sig" > "$work/check.sig"
  openssl pkeyutl -verify -pubin -inkey "$work/pub.pem" -rawin -in "$json" -sigfile "$work/check.sig"
}
sign "$dir/release-manifest.json"
cat "$dir/release-manifest.json"

if [ "${BRIDGE:-false}" = true ]; then
  mkdir -p "$dir/bridge"
  jq --arg r "https://github.com/${GITHUB_REPOSITORY}/releases/latest/download" \
    'del(.desktop) + {next_root: $r}' "$dir/release-manifest.json" > "$dir/bridge/release-manifest.json"
  sign "$dir/bridge/release-manifest.json"
  cat "$dir/bridge/release-manifest.json"
fi
