# Distribution package metadata

Each subfolder is a package manifest for a different distribution channel.
Files reference `v0.2.0` and use placeholder `REPLACE_WITH_ACTUAL_SHA256_AT_RELEASE_TIME`
strings — the CI workflow fills these in at tag time and submits the
manifests via PRs to each registry.

| Folder | Channel | User installs with |
|---|---|---|
| `winget/`    | Microsoft winget                                | `winget install singhpratech.Kashot` |
| `chocolatey/`| Chocolatey community repo                       | `choco install kashot` |
| `scoop/`     | Scoop bucket (`singhpratech/scoop-kashot`)      | `scoop install kashot` |
| `homebrew/`  | Homebrew Cask                                   | `brew install --cask kashot` |
| `aur/`       | Arch User Repository (`kashot-bin`)             | `yay -S kashot-bin` |
| `flatpak/`   | Flathub                                         | `flatpak install flathub org.kashot.Kashot` |
| `appimage/`  | AppImage (built by CI, attached to release)     | download + `chmod +x` + run |
| `debian/`    | `.deb` packaging (Debian / Ubuntu)              | `sudo apt install ./kashot.deb` |

Each first-time submission has its own gating process (winget reviews PRs,
Flathub reviews submissions, Homebrew Cask requires a clean RFC checklist,
etc.). After acceptance, the same manifest is bumped each release with the
new version + sha256.

> **Note on asset-name mismatches.** As of v0.2.0, the canonical Windows
> artifact on the Release is `kashot-windows-x86_64.zip` (built by
> `build-rust.yml`). The `winget/` and `chocolatey/` manifests still target
> `Kashot.msi` — the MSI now ships as a CI artifact on the
> `Build C# (Windows, legacy)` workflow run rather than on the Release.
> Before activating either channel, either (a) re-attach the MSI to the
> Release in `build-csharp.yml`, or (b) flip the manifest to a zip-style
> install pointing at `kashot-windows-x86_64.zip`. `scoop/` still targets
> the legacy `Kashot-portable.zip` for the same reason.
