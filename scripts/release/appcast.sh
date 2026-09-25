#!/usr/bin/env bash
# Generate and sign the Fleet Sparkle feed for one release, then prove both
# signatures in it, the DMG's and the feed's own, verify under the public key
# Fleet builds pin.
#
#   bash scripts/release/appcast.sh <dmg> <generate_appcast> <version> <out-dir>
#
# Writes <out-dir>/appcast.xml with enclosures under this repository's release,
# and with BRIDGE=true also <out-dir>/bridge/appcast.xml with enclosures under
# the old repository's release of the same version, so the bridge is
# self-hosted.
#
# Environment:
#   PUB      true: KEY and PUBLIC are required, and the feed is refused unless
#            both of its signatures verify under PUBLIC. false: KEY and PUBLIC
#            are the throwaway pair the dry run's Fleet build pinned (Sparkle
#            signs only under the key the app pins); without them a fresh
#            throwaway key is made, which only a Fleet pinning no key accepts.
#   KEY      Sparkle EdDSA private key, base64 (SPARKLE_PRIVATE_ED_KEY).
#   PUBLIC   Sparkle EdDSA public key, base64 (SPARKLE_PUBLIC_ED_KEY).
#   BRIDGE   true: also write the bridge feed.
#   GITHUB_REPOSITORY  owner/name of the repository publishing the release.
set -euo pipefail

dmg="${1:?usage: appcast.sh <dmg> <generate_appcast> <version> <out-dir>}"
tool="${2:?usage: appcast.sh <dmg> <generate_appcast> <version> <out-dir>}"
version="${3:?usage: appcast.sh <dmg> <generate_appcast> <version> <out-dir>}"
out="${4:?usage: appcast.sh <dmg> <generate_appcast> <version> <out-dir>}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name the publishing repository}"
bridge_repo="stevengonsalvez/agents-in-a-box"

# macOS ships LibreSSL as /usr/bin/openssl, which lacks `pkeyutl -rawin`.
# Prefer Homebrew's OpenSSL 3 when it is installed, and fail early otherwise.
if command -v brew >/dev/null 2>&1 && [ -x "$(brew --prefix openssl@3 2>/dev/null)/bin/openssl" ]; then
  PATH="$(brew --prefix openssl@3)/bin:$PATH"
fi
openssl version | grep -q '^OpenSSL 3' || { echo "OpenSSL 3 is required, found: $(openssl version)" >&2; exit 1; }

test -f "$dmg" || { echo "no DMG at $dmg" >&2; exit 1; }
test -x "$tool" || { echo "generate_appcast is not executable at $tool" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
if [ "${PUB:-false}" = true ]; then
  test -n "${KEY:-}" || { echo "SPARKLE_PRIVATE_ED_KEY is required to publish" >&2; exit 1; }
  test -n "${PUBLIC:-}" || { echo "SPARKLE_PUBLIC_ED_KEY is required to publish" >&2; exit 1; }
  private="$KEY"
  public="$PUBLIC"
elif [ -n "${KEY:-}" ] && [ -n "${PUBLIC:-}" ]; then
  private="$KEY"
  public="$PUBLIC"
  echo "dry run: signing the feed with the throwaway key the build pinned"
else
  # A PKCS#8 Ed25519 key ends with its 32-byte seed, which is the form
  # generate_appcast reads from --ed-key-file.
  openssl genpkey -algorithm ed25519 -out "$work/key.pem"
  private="$(openssl pkey -in "$work/key.pem" -outform DER | tail -c 32 | base64)"
  public="$(openssl pkey -in "$work/key.pem" -pubout -outform DER | tail -c 32 | base64)"
  echo "dry run: signing the feed with a throwaway key"
fi
(printf '302a300506032b6570032100' | xxd -r -p; printf '%s' "$public" | base64 -d) \
  | openssl pkey -pubin -inform DER -out "$work/pub.pem"

feed() {
  # feed <repo> <dest>: generate the appcast for <repo>'s release into <dest>.
  local repo="$1" dest="$2" url sig
  url="https://github.com/${repo}/releases/download/v${version}/"
  mkdir -p "$work/$repo" "$dest"
  cp "$dmg" "$work/$repo/"
  printf '%s' "$private" | "$tool" --ed-key-file - --download-url-prefix "$url" "$work/$repo"
  test -s "$work/$repo/appcast.xml"
  grep -q "url=\"${url}$(basename "$dmg")\"" "$work/$repo/appcast.xml" \
    || { echo "appcast has no enclosure under $url" >&2; cat "$work/$repo/appcast.xml" >&2; exit 1; }
  sig="$(sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p' "$work/$repo/appcast.xml" | head -1)"
  test -n "$sig" || { echo "appcast carries no sparkle:edSignature" >&2; exit 1; }
  printf '%s' "$sig" | base64 -d > "$work/dmg.sig"
  # Every installed Fleet pins the public key, so a feed signed by any other key
  # would be rejected in the field. Refuse it here instead.
  openssl pkeyutl -verify -pubin -inkey "$work/pub.pem" -rawin -in "$dmg" -sigfile "$work/dmg.sig" \
    || { echo "the DMG signature does not verify under $public; refusing the feed" >&2; exit 1; }
  verify_feed "$work/$repo/appcast.xml"
  cp "$work/$repo/appcast.xml" "$dest/appcast.xml"
}

verify_feed() {
  # Fleet sets SURequireSignedFeed, so an installed Fleet also rejects a feed
  # whose own signature is missing or wrong. Sparkle signs every byte before
  # the trailing `<!-- sparkle-signatures:` comment, and records that count.
  local xml="$1" at block fsig flen
  at="$(perl -0777 -ne 'my $i = rindex($_, "<!-- sparkle-signatures:\n"); print $i if $i >= 0' "$xml")"
  test -n "$at" || { echo "appcast carries no feed signature (sparkle-signatures)" >&2; exit 1; }
  head -c "$at" "$xml" > "$work/feed.signed"
  block="$(tail -c +"$((at + 1))" "$xml")"
  fsig="$(sed -n 's/^edSignature: *//p' <<<"$block" | head -1)"
  flen="$(sed -n 's/^length: *//p' <<<"$block" | head -1)"
  test -n "$fsig" || { echo "the feed signature block has no edSignature" >&2; exit 1; }
  [ "$flen" = "$at" ] || { echo "the feed signature covers ${flen:-no} bytes, the feed has $at" >&2; exit 1; }
  printf '%s' "$fsig" | base64 -d > "$work/feed.sig"
  openssl pkeyutl -verify -pubin -inkey "$work/pub.pem" -rawin -in "$work/feed.signed" -sigfile "$work/feed.sig" \
    || { echo "the feed signature does not verify under $public; refusing the feed" >&2; exit 1; }
}

feed "$GITHUB_REPOSITORY" "$out"
if [ "${BRIDGE:-false}" = true ]; then
  feed "$bridge_repo" "$out/bridge"
fi
