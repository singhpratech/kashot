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

  version "0.5.0"
  sha256  arm:   "4474ea5995edf2714288aa2a3381530e31486ff791730e28b99794a8e6013037",
          intel: "2462d15c2317cd1b7803e99307f4b3580a45d6c915203beb944dfb30843f5b26"

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
