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

  version "0.6.0"
  sha256  arm:   "c1bfdf05c1292dc0bb9b15445778f095073a917c97dcf27624dfbc15c63a0a9f",
          intel: "d29cbff7ae0d457102b79ee73986904251d511769d815e092aa4a142c56e7ed4"

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
