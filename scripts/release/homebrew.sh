#!/usr/bin/env bash
# Write the Homebrew formula and the Fleet cask for one stable release into a
# checkout of the tap repository. Every URL and the homepage name the
# repository publishing the release, so the tap follows the release root.
#
#   bash scripts/release/homebrew.sh <tap-dir> <version> <artifacts-dir>
#
# <artifacts-dir> holds `<triple>/ainb-<version>-<triple>.tar.gz.sha256` for the
# three CLI targets and `fleet-dmg/AINBFleet-<version>.dmg`.
set -euo pipefail

tap="${1:?usage: homebrew.sh <tap-dir> <version> <artifacts-dir>}"
version="${2:?usage: homebrew.sh <tap-dir> <version> <artifacts-dir>}"
dir="${3:?usage: homebrew.sh <tap-dir> <version> <artifacts-dir>}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must name the publishing repository}"

sha_of() {
  local file="$dir/$1/ainb-${version}-$1.tar.gz.sha256" sha
  sha="$(awk '{print $1}' "$file" 2>/dev/null || true)"
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo "no sha256 in $file" >&2; return 1; }
  printf '%s' "$sha"
}
mac_sha="$(sha_of aarch64-apple-darwin)"
intel_sha="$(sha_of x86_64-apple-darwin)"
linux_sha="$(sha_of x86_64-unknown-linux-gnu)"
fleet_dmg="$dir/fleet-dmg/AINBFleet-${version}.dmg"
test -f "$fleet_dmg" || { echo "no Fleet DMG at $fleet_dmg" >&2; exit 1; }
fleet_sha="$(sha256sum "$fleet_dmg" | awk '{print $1}')"

mkdir -p "$tap/Formula" "$tap/Casks"
cat > "$tap/Formula/ainb.rb" << 'FORMULA'
class Ainb < Formula
  desc "Terminal-based development environment manager for Claude Code agents"
  homepage "https://github.com/REPO_PLACEHOLDER"
  license "MIT"
  version "VERSION_PLACEHOLDER"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/REPO_PLACEHOLDER/releases/download/vVERSION_PLACEHOLDER/ainb-VERSION_PLACEHOLDER-aarch64-apple-darwin.tar.gz"
      sha256 "MAC_SHA_PLACEHOLDER"
    else
      url "https://github.com/REPO_PLACEHOLDER/releases/download/vVERSION_PLACEHOLDER/ainb-VERSION_PLACEHOLDER-x86_64-apple-darwin.tar.gz"
      sha256 "INTEL_SHA_PLACEHOLDER"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/REPO_PLACEHOLDER/releases/download/vVERSION_PLACEHOLDER/ainb-VERSION_PLACEHOLDER-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "LINUX_SHA_PLACEHOLDER"
    end
  end

  def install
    # Real binary + bundled first-party plugins live in libexec;
    # bin gets a thin env wrapper pointing the plugin runtime at
    # them. `ainb plugin install` is not shipped yet, so without
    # this a brew install has no analytics plugins at all. A
    # user-set AINB_PLUGIN_ROOT still wins.
    libexec.install "ainb"
    libexec.install "plugins" if File.directory?("plugins")
    # ainb(1). Guarded so this formula still installs an older
    # tarball that predates the man page.
    man1.install "share/man/man1/ainb.1" if File.exist?("share/man/man1/ainb.1")
    (bin/"ainb").write <<~WRAPPER
      #!/bin/bash
      # This value names a keg-versioned path, so an older install's
      # export outlives the keg. Inherited from a long-lived shell or
      # a tmux server environment it survives `brew upgrade` and then
      # either points at a directory the upgrade deleted (every plugin
      # screen comes up empty) or, until `brew cleanup` runs and
      # removes the old keg, still resolves, pairing this binary with
      # the PREVIOUS release's plugin binaries.
      #
      # Drop any inherited value naming a different ainb keg, whether
      # or not it still exists. A genuine user override (any path not
      # under an ainb Cellar keg) is untouched.
      case "${AINB_PLUGIN_ROOT}" in
        "" ) ;;
        "#{libexec}/plugins" ) ;;
        */Cellar/ainb/* ) unset AINB_PLUGIN_ROOT ;;
        * ) [ -d "${AINB_PLUGIN_ROOT}" ] || unset AINB_PLUGIN_ROOT ;;
      esac
      export AINB_PLUGIN_ROOT="${AINB_PLUGIN_ROOT:-#{libexec}/plugins}"
      exec "#{libexec}/ainb" "$@"
    WRAPPER
    (bin/"ainb").chmod 0755
  end

  test do
    assert_match "ainb", shell_output("#{bin}/ainb --version")
  end
end
FORMULA

cat > "$tap/Casks/ainb-fleet.rb" << 'CASK'
cask "ainb-fleet" do
  version "VERSION_PLACEHOLDER"
  sha256 "FLEET_SHA_PLACEHOLDER"

  url "https://github.com/REPO_PLACEHOLDER/releases/download/v#{version}/AINBFleet-#{version}.dmg"
  name "AINB Fleet"
  desc "Menu bar control plane for AINB sessions"
  homepage "https://github.com/REPO_PLACEHOLDER"

  auto_updates true
  depends_on macos: ">= :sonoma"

  app "AINBFleet.app"

  caveats <<~EOS
    AINB Fleet is unsigned. On first launch, right-click AINBFleet.app and choose Open.
    Or choose Open Anyway in System Settings > Privacy & Security.
  EOS
end
CASK

sed -i.bak \
  -e "s|REPO_PLACEHOLDER|${GITHUB_REPOSITORY}|g" \
  -e "s|VERSION_PLACEHOLDER|${version}|g" \
  -e "s|MAC_SHA_PLACEHOLDER|${mac_sha}|g" \
  -e "s|INTEL_SHA_PLACEHOLDER|${intel_sha}|g" \
  -e "s|LINUX_SHA_PLACEHOLDER|${linux_sha}|g" \
  -e "s|FLEET_SHA_PLACEHOLDER|${fleet_sha}|g" \
  "$tap/Formula/ainb.rb" "$tap/Casks/ainb-fleet.rb"
rm -f "$tap/Formula/ainb.rb.bak" "$tap/Casks/ainb-fleet.rb.bak"

if grep -n 'PLACEHOLDER' "$tap/Formula/ainb.rb" "$tap/Casks/ainb-fleet.rb"; then
  echo "unresolved placeholder in the tap files" >&2
  exit 1
fi
cat "$tap/Formula/ainb.rb" "$tap/Casks/ainb-fleet.rb"
