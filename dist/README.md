# Distribution package metadata

Each subfolder is a package manifest for a different distribution channel.
Files reference `v0.3.9` and use placeholder `REPLACE_WITH_ACTUAL_SHA256_AT_RELEASE_TIME`
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
| `rpm/`       | RPM SPEC for Fedora / RHEL / openSUSE (→ COPR)  | `sudo dnf install kashot` |
| `snap/`      | Snap Store (all distros with `snapd`)           | `sudo snap install kashot` |

Each first-time submission has its own gating process (winget reviews PRs,
Flathub reviews submissions, Homebrew Cask requires a clean RFC checklist,
etc.). After acceptance, the same manifest is bumped each release with the
new version + sha256.

> **Windows artifacts.** As of v0.3.9 the Release ships both
> `kashot-windows-x86_64.zip` (portable) and `Kashot.msi` (per-machine
> WiX-built installer with ffmpeg bundled). The `winget/` manifest
> targets the MSI; `scoop/` targets the portable zip; `chocolatey/`
> wraps the MSI.

> **Linux broad packaging.** `rpm/kashot.spec` and `snap/snapcraft.yaml`
> are buildable as-is against the v0.3.9 release tarball, but neither is
> activated yet: the RPM still needs a one-time Fedora COPR project
> submission before `dnf install kashot` works, and the snap needs a
> first `snapcraft upload --release=stable`. Until then, Fedora/RHEL/
> openSUSE users get the tarball via `install.sh`, and Snap users build
> locally with `snapcraft --use-lxd`.
