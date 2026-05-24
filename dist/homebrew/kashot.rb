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

  version "0.3.8"
  sha256  arm:   "3ef68c09c92f022eb34594eacaa6262fc138e7cd5a3c9d4ac6d73fc39e9feedb",
          intel: "a5c41d65adf9a33c09bbc626004070e80aeeb0f444c401c3ececcb5249fb369f"

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
