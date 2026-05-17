# Submit to homebrew/cask repo:
#   https://github.com/Homebrew/homebrew-cask
# under Casks/k/kashot.rb. The user installs with:
#   brew install --cask kashot
#
# As of v0.3.x we ship a real .app inside a .dmg (built in
# .github/workflows/build-rust.yml). Casks are happiest installing an
# `app` from a `.dmg`, so this file uses the `app` stanza instead of the
# old `binary` stanza that wrapped the raw Mach-O.
cask "kashot" do
  arch arm: "arm64", intel: "x64"

  version "0.3.0"
  sha256  arm:   "REPLACE_WITH_ARM64_DMG_SHA256_AT_RELEASE_TIME",
          intel: "REPLACE_WITH_X64_DMG_SHA256_AT_RELEASE_TIME"

  url       "https://github.com/singhpratech/kashot/releases/download/v#{version}/Kashot-macos-#{arch}.dmg",
            verified: "github.com/singhpratech/kashot/"
  name      "Kashot"
  desc      "Fast screenshots with annotations — tray-resident, hotkey-driven, free."
  homepage  "https://kashot.org/"

  app "Kashot.app"

  zap trash: [
    "~/Library/Application Support/Kashot",
    "~/Library/Preferences/org.kashot.app.plist",
  ]
end
