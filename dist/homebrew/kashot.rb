# Submitted to homebrew/cask repo at:
#   https://github.com/Homebrew/homebrew-cask/blob/main/Casks/k/kashot.rb
# Users install with:
#   brew install --cask kashot
#
# This file is the in-repo source of truth — sync changes here AND open a PR
# against homebrew-cask with the same content. Per-release: bump version + the
# two sha256 lines (arm + intel DMG) from the published SHA256SUMS.
cask "kashot" do
  arch arm: "arm64", intel: "x64"

  version "0.4.0"
  sha256  arm:   "ad3900f7d1c811d1a37b1b20e684089405e103302ae30f23ce1cca7270671535",
          intel: "978dbfd0aee33dc1fe728bcb6fb64c4abb596398df8ea28760d68b028c2dd9ca"

  url       "https://github.com/singhpratech/kashot/releases/download/v#{version}/Kashot-macos-#{arch}.dmg",
            verified: "github.com/singhpratech/kashot/"
  name      "Kashot"
  desc      "Fast screenshots with annotations — tray-resident, hotkey-driven, free"
  homepage  "https://kashot.org/"

  app "Kashot.app"

  zap trash: [
    "~/Library/Application Support/Kashot",
    "~/Library/Preferences/org.kashot.app.plist",
  ]
end
