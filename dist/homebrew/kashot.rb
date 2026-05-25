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

  version "0.4.2"
  sha256  arm:   "edb7a495af2cf548f3ad695d77869e9340841a5c8212a43eb0a530e4c8fea58a",
          intel: "c55c089c9559dc85a1ebe97b4a43d4a0eb482b949bf09140a746101ab4512e5b"

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
