//! Which channel this copy of Kashot was installed from, and what the
//! updater is allowed to do about a newer release.
//!
//! Kashot ships through a dozen channels: GitHub tarballs / zips, the MSI,
//! the macOS `.dmg`, an AppImage, Snap, Flatpak, Homebrew, deb / rpm / AUR,
//! Scoop. Only some of them are ours to replace. Overwriting the binary of a
//! package-managed install is at best pointless (the package manager reverts
//! it on the next upgrade) and at worst breaks the install — a Snap's
//! read-only squashfs can't be written at all, and a swapped-out `/usr/bin`
//! binary makes dpkg / rpm checksums disagree with reality.
//!
//! So the dialog asks this module first. Detection is a pure function of
//! three observable things — a few environment variables, the path of the
//! running executable, and whether a couple of marker files exist — which
//! keeps it unit-testable for every channel on every host. The caller
//! ([`InstallProbe`] construction) is the only part that touches the OS.

/// The three platforms Kashot ships on. Carried in the probe rather than
/// read from `cfg!` inside the detection so a Linux test run can exercise
/// the Windows and macOS branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostOs {
    #[default]
    Linux,
    Windows,
    MacOs,
}

impl HostOs {
    /// The OS this binary was compiled for.
    pub fn host() -> Self {
        if cfg!(target_os = "windows") {
            HostOs::Windows
        } else if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else {
            HostOs::Linux
        }
    }
}

/// Everything the detection is allowed to look at. Fill it from the real
/// environment at the edge of the app; construct it by hand in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallProbe {
    pub os: HostOs,
    /// `std::env::current_exe()`, lossily stringified. Inside an AppImage
    /// this points into the mounted squashfs, not at the `.AppImage` file.
    pub exe_path: String,
    /// `$SNAP` — the snap's read-only mount root, e.g. `/snap/kashot/42`.
    pub snap_dir: Option<String>,
    /// `$FLATPAK_ID`.
    pub flatpak_id: Option<String>,
    /// Does `/.flatpak-info` exist? Present inside every Flatpak sandbox.
    pub flatpak_info_exists: bool,
    /// `$APPIMAGE` — absolute path of the running `.AppImage` file. This is
    /// the file the updater replaces, not `exe_path`.
    pub appimage_path: Option<String>,
    /// Does a Homebrew Caskroom entry for kashot exist? The cask drops
    /// `Kashot.app` into `/Applications`, so the app bundle itself carries
    /// no trace of brew — the Caskroom directory is the only marker.
    pub homebrew_cask_marker: bool,
    /// Contents of `/etc/os-release`, used only to name the right distro
    /// package-manager command. `None` degrades to a generic hint.
    pub os_release: Option<String>,
}

/// How this copy of Kashot got onto the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallChannel {
    Snap,
    Flatpak,
    AppImage,
    /// `brew install --cask kashot` (macOS app bundle managed by brew).
    HomebrewCask,
    /// A Homebrew formula install — the binary lives under `Cellar/`.
    HomebrewFormula,
    /// deb / rpm / AUR: a distro package that owns `/usr/bin/kashot`.
    LinuxSystemPackage,
    Scoop,
    /// Installed by `Kashot.msi` (directly, via winget, or via Chocolatey).
    WindowsMsi,
    /// Unpacked from `kashot-windows-x86_64.zip`.
    WindowsPortable,
    /// A `Kashot.app` bundle that no package manager claims (the `.dmg`).
    MacAppBundle,
    /// A loose binary from the release tarball / `install.sh`.
    Portable,
}

impl InstallChannel {
    /// Short human-readable name for the dialog.
    pub fn label(self) -> &'static str {
        match self {
            InstallChannel::Snap => "Snap",
            InstallChannel::Flatpak => "Flatpak",
            InstallChannel::AppImage => "AppImage",
            InstallChannel::HomebrewCask => "Homebrew cask",
            InstallChannel::HomebrewFormula => "Homebrew",
            InstallChannel::LinuxSystemPackage => "system package",
            InstallChannel::Scoop => "Scoop",
            InstallChannel::WindowsMsi => "Windows installer",
            InstallChannel::WindowsPortable => "portable build",
            InstallChannel::MacAppBundle => "app bundle",
            InstallChannel::Portable => "portable build",
        }
    }

    /// True when some package manager owns these files and the in-app
    /// updater must keep its hands off them.
    pub fn is_package_managed(self) -> bool {
        matches!(
            self,
            InstallChannel::Snap
                | InstallChannel::Flatpak
                | InstallChannel::HomebrewCask
                | InstallChannel::HomebrewFormula
                | InstallChannel::LinuxSystemPackage
                | InstallChannel::Scoop
        )
    }
}

/// What the Updates dialog offers once it knows a newer release exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateAction {
    /// Download the asset, verify it, swap it in, relaunch.
    SelfInstall,
    /// Hand the asset URL to the browser and let the user run the
    /// installer themselves.
    BrowserDownload,
    /// Someone else owns this install. Show `hint`, plus `command` when we
    /// can name the exact one-liner.
    Managed {
        hint: &'static str,
        command: Option<String>,
    },
}

/// Classify the running install. Pure — every input arrives in `probe`.
pub fn detect(probe: &InstallProbe) -> InstallChannel {
    match probe.os {
        HostOs::Linux => detect_linux(probe),
        HostOs::MacOs => detect_macos(probe),
        HostOs::Windows => detect_windows(probe),
    }
}

/// What the updater may do for `channel` on this host.
pub fn update_action(channel: InstallChannel, probe: &InstallProbe) -> UpdateAction {
    match channel {
        // The AppImage is a single file we own outright: download the new
        // one next to it and rename over the old one. Self-contained, no
        // package database to desynchronise.
        InstallChannel::AppImage => UpdateAction::SelfInstall,

        // MSI upgrades hand off to msiexec, which keeps Add/Remove Programs
        // honest; the plain Linux tarball layout is a straight binary swap.
        InstallChannel::WindowsMsi => UpdateAction::SelfInstall,
        InstallChannel::Portable if probe.os == HostOs::Linux => UpdateAction::SelfInstall,

        // Windows portable zips and macOS app bundles are unpacked by hand,
        // so we hand the download back to the user rather than guessing at
        // their layout (and, on macOS, at their code-signing state).
        InstallChannel::Portable
        | InstallChannel::WindowsPortable
        | InstallChannel::MacAppBundle => UpdateAction::BrowserDownload,

        InstallChannel::Snap => UpdateAction::Managed {
            hint: "Installed from the Snap Store - update with:",
            command: Some("sudo snap refresh kashot".to_owned()),
        },
        InstallChannel::Flatpak => UpdateAction::Managed {
            hint: "Installed as a Flatpak - update with:",
            command: Some("flatpak update org.kashot.Kashot".to_owned()),
        },
        InstallChannel::HomebrewCask => UpdateAction::Managed {
            hint: "Installed with Homebrew - update with:",
            command: Some("brew upgrade --cask kashot".to_owned()),
        },
        InstallChannel::HomebrewFormula => UpdateAction::Managed {
            hint: "Installed with Homebrew - update with:",
            command: Some("brew upgrade kashot".to_owned()),
        },
        InstallChannel::Scoop => UpdateAction::Managed {
            hint: "Installed with Scoop - update with:",
            command: Some("scoop update kashot".to_owned()),
        },
        InstallChannel::LinuxSystemPackage => match distro_family(probe.os_release.as_deref()) {
            DistroFamily::Debian => UpdateAction::Managed {
                hint: "Installed from a system package - update with:",
                command: Some(
                    "sudo apt update && sudo apt install --only-upgrade kashot".to_owned(),
                ),
            },
            DistroFamily::Fedora => UpdateAction::Managed {
                hint: "Installed from a system package - update with:",
                command: Some("sudo dnf upgrade kashot".to_owned()),
            },
            DistroFamily::Arch => UpdateAction::Managed {
                hint: "Installed from the AUR - update with your AUR helper:",
                command: Some("yay -Syu kashot-bin".to_owned()),
            },
            DistroFamily::Suse => UpdateAction::Managed {
                hint: "Installed from a system package - update with:",
                command: Some("sudo zypper update kashot".to_owned()),
            },
            DistroFamily::Unknown => UpdateAction::Managed {
                hint: "Installed from a system package - update it with your package manager.",
                command: None,
            },
        },
    }
}

/// Convenience: classify and resolve the action in one call.
pub fn detect_action(probe: &InstallProbe) -> (InstallChannel, UpdateAction) {
    let channel = detect(probe);
    let action = update_action(channel, probe);
    (channel, action)
}

// ── per-OS detection ────────────────────────────────────────────────────────

fn detect_linux(p: &InstallProbe) -> InstallChannel {
    if is_snap(p) {
        return InstallChannel::Snap;
    }
    if is_flatpak(p) {
        return InstallChannel::Flatpak;
    }
    if is_appimage(p) {
        return InstallChannel::AppImage;
    }
    // Linuxbrew keeps its binaries under <prefix>/Cellar/<formula>/<version>.
    if p.exe_path.contains("/Cellar/") {
        return InstallChannel::HomebrewFormula;
    }
    if is_system_path(&p.exe_path) {
        return InstallChannel::LinuxSystemPackage;
    }
    InstallChannel::Portable
}

fn detect_macos(p: &InstallProbe) -> InstallChannel {
    if p.exe_path.contains("/Cellar/") {
        return InstallChannel::HomebrewFormula;
    }
    if in_app_bundle(&p.exe_path) {
        // The cask stages Kashot.app into /Applications, so the bundle looks
        // exactly like a hand-installed one — only the Caskroom entry tells
        // us brew is tracking it.
        return if p.homebrew_cask_marker {
            InstallChannel::HomebrewCask
        } else {
            InstallChannel::MacAppBundle
        };
    }
    InstallChannel::Portable
}

fn detect_windows(p: &InstallProbe) -> InstallChannel {
    let path = p.exe_path.to_ascii_lowercase().replace('/', "\\");
    // Scoop first: it unpacks the portable zip under <scoop>\apps\kashot\,
    // which is never inside Program Files, but checking it first keeps the
    // rule order obvious.
    if path.contains("\\scoop\\apps\\") {
        return InstallChannel::Scoop;
    }
    // Same heuristic the MSI handoff has always used: `perMachine` scope
    // puts kashot.exe in one of the two Program Files trees, and nothing
    // else does. winget and Chocolatey both install that same MSI.
    if path.contains("\\program files\\") || path.contains("\\program files (x86)\\") {
        return InstallChannel::WindowsMsi;
    }
    InstallChannel::WindowsPortable
}

fn is_snap(p: &InstallProbe) -> bool {
    // The snap's own binary always lives under the read-only /snap mount.
    if p.exe_path.starts_with("/snap/") {
        return true;
    }
    // $SNAP is inherited by every child process of a snap, so on its own it
    // proves nothing — a terminal snap launching a tarball build of Kashot
    // would set it too. Only trust it when the running binary is actually
    // inside that snap's tree.
    match p.snap_dir.as_deref() {
        Some(dir) if !dir.is_empty() => p.exe_path.starts_with(dir),
        _ => false,
    }
}

fn is_flatpak(p: &InstallProbe) -> bool {
    if p.flatpak_info_exists {
        return true;
    }
    matches!(p.flatpak_id.as_deref(), Some(id) if !id.is_empty())
}

fn is_appimage(p: &InstallProbe) -> bool {
    matches!(p.appimage_path.as_deref(), Some(path) if !path.is_empty())
}

/// A distro package owns `/usr/bin` and `/usr/lib`. `/usr/local` is
/// deliberately excluded — that tree is for hand-installed software, which
/// is exactly what `install.sh` produces when run as root.
fn is_system_path(exe: &str) -> bool {
    exe.starts_with("/usr/bin/")
        || exe.starts_with("/usr/lib/")
        || exe.starts_with("/usr/libexec/")
        || exe.starts_with("/usr/sbin/")
}

fn in_app_bundle(exe: &str) -> bool {
    exe.contains(".app/Contents/MacOS/")
}

// ── /etc/os-release ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DistroFamily {
    Debian,
    Fedora,
    Arch,
    Suse,
    Unknown,
}

/// Map `ID` / `ID_LIKE` from `/etc/os-release` onto a package manager. Both
/// keys are checked because derivatives (Mint, Pop!_OS, EndeavourOS, Rocky…)
/// carry their own `ID` and name the parent in `ID_LIKE`.
fn distro_family(os_release: Option<&str>) -> DistroFamily {
    let Some(text) = os_release else {
        return DistroFamily::Unknown;
    };
    let mut ids: Vec<String> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key != "ID" && key != "ID_LIKE" {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        for word in value.split_whitespace() {
            ids.push(word.to_ascii_lowercase());
        }
    }
    for id in &ids {
        let family = match id.as_str() {
            "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" => DistroFamily::Debian,
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" => DistroFamily::Fedora,
            "arch" | "archlinux" | "manjaro" | "endeavouros" => DistroFamily::Arch,
            "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" | "sles" => {
                DistroFamily::Suse
            }
            _ => continue,
        };
        return family;
    }
    DistroFamily::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(exe: &str) -> InstallProbe {
        InstallProbe {
            os: HostOs::Linux,
            exe_path: exe.to_owned(),
            ..Default::default()
        }
    }

    fn windows(exe: &str) -> InstallProbe {
        InstallProbe {
            os: HostOs::Windows,
            exe_path: exe.to_owned(),
            ..Default::default()
        }
    }

    fn macos(exe: &str) -> InstallProbe {
        InstallProbe {
            os: HostOs::MacOs,
            exe_path: exe.to_owned(),
            ..Default::default()
        }
    }

    // ── channel detection, one test per channel ─────────────────────────

    #[test]
    fn detects_snap_from_env_and_path() {
        let mut p = linux("/snap/kashot/42/kashot");
        p.snap_dir = Some("/snap/kashot/42".to_owned());
        assert_eq!(detect(&p), InstallChannel::Snap);

        // $SNAP absent (e.g. launched by an odd wrapper) but the binary is
        // still inside the read-only snap mount.
        let bare = linux("/snap/kashot/current/kashot");
        assert_eq!(detect(&bare), InstallChannel::Snap);
    }

    #[test]
    fn snap_env_inherited_by_a_child_process_is_not_a_snap_install() {
        // A terminal snap launching the tarball build inherits $SNAP. The
        // exe path is the tiebreaker.
        let mut p = linux("/home/u/.local/bin/kashot");
        p.snap_dir = Some("/snap/some-terminal/17".to_owned());
        assert_eq!(detect(&p), InstallChannel::Portable);
    }

    #[test]
    fn detects_flatpak_from_marker_file() {
        let mut p = linux("/app/bin/kashot");
        p.flatpak_info_exists = true;
        assert_eq!(detect(&p), InstallChannel::Flatpak);
    }

    #[test]
    fn detects_flatpak_from_app_id() {
        let mut p = linux("/app/bin/kashot");
        p.flatpak_id = Some("org.kashot.Kashot".to_owned());
        assert_eq!(detect(&p), InstallChannel::Flatpak);
    }

    #[test]
    fn detects_appimage_from_env() {
        // Inside an AppImage, current_exe() points into the FUSE mount —
        // note it contains "/usr/bin/" but doesn't start with it, so the
        // system-package rule must not fire.
        let mut p = linux("/tmp/.mount_kashotAbCdEf/usr/bin/kashot");
        p.appimage_path = Some("/home/u/Apps/kashot-x86_64.AppImage".to_owned());
        assert_eq!(detect(&p), InstallChannel::AppImage);
    }

    #[test]
    fn detects_deb_rpm_aur_from_usr_bin() {
        assert_eq!(detect(&linux("/usr/bin/kashot")), InstallChannel::LinuxSystemPackage);
        assert_eq!(
            detect(&linux("/usr/lib/kashot/kashot")),
            InstallChannel::LinuxSystemPackage
        );
    }

    #[test]
    fn usr_local_is_not_a_system_package() {
        // install.sh run as root lands here; we still own that binary.
        assert_eq!(detect(&linux("/usr/local/bin/kashot")), InstallChannel::Portable);
    }

    #[test]
    fn detects_linuxbrew_formula() {
        assert_eq!(
            detect(&linux("/home/linuxbrew/.linuxbrew/Cellar/kashot/0.6.0/bin/kashot")),
            InstallChannel::HomebrewFormula
        );
    }

    #[test]
    fn detects_portable_tarball_install() {
        assert_eq!(detect(&linux("/home/u/.local/bin/kashot")), InstallChannel::Portable);
    }

    #[test]
    fn detects_homebrew_cask_only_with_the_caskroom_marker() {
        let exe = "/Applications/Kashot.app/Contents/MacOS/kashot";
        assert_eq!(detect(&macos(exe)), InstallChannel::MacAppBundle);

        let mut brewed = macos(exe);
        brewed.homebrew_cask_marker = true;
        assert_eq!(detect(&brewed), InstallChannel::HomebrewCask);
    }

    #[test]
    fn detects_macos_bare_binary_as_portable() {
        assert_eq!(detect(&macos("/Users/u/bin/kashot")), InstallChannel::Portable);
    }

    #[test]
    fn detects_windows_msi_install() {
        assert_eq!(
            detect(&windows("C:\\Program Files\\Kashot\\kashot.exe")),
            InstallChannel::WindowsMsi
        );
        assert_eq!(
            detect(&windows("C:\\Program Files (x86)\\Kashot\\kashot.exe")),
            InstallChannel::WindowsMsi
        );
    }

    #[test]
    fn detects_windows_portable_zip() {
        assert_eq!(
            detect(&windows("D:\\tools\\kashot\\kashot.exe")),
            InstallChannel::WindowsPortable
        );
    }

    #[test]
    fn detects_scoop_install() {
        assert_eq!(
            detect(&windows("C:\\Users\\u\\scoop\\apps\\kashot\\0.6.0\\kashot.exe")),
            InstallChannel::Scoop
        );
    }

    // ── actions ─────────────────────────────────────────────────────────

    #[test]
    fn package_managed_channels_never_self_install() {
        for channel in [
            InstallChannel::Snap,
            InstallChannel::Flatpak,
            InstallChannel::HomebrewCask,
            InstallChannel::HomebrewFormula,
            InstallChannel::LinuxSystemPackage,
            InstallChannel::Scoop,
        ] {
            assert!(channel.is_package_managed(), "{channel:?}");
            let action = update_action(channel, &linux("/usr/bin/kashot"));
            assert!(
                matches!(action, UpdateAction::Managed { .. }),
                "{channel:?} produced {action:?}"
            );
        }
    }

    #[test]
    fn snap_and_flatpak_name_their_own_commands() {
        let mut p = linux("/snap/kashot/42/kashot");
        p.snap_dir = Some("/snap/kashot/42".to_owned());
        let (channel, action) = detect_action(&p);
        assert_eq!(channel, InstallChannel::Snap);
        assert_eq!(
            action,
            UpdateAction::Managed {
                hint: "Installed from the Snap Store - update with:",
                command: Some("sudo snap refresh kashot".to_owned()),
            }
        );

        let mut f = linux("/app/bin/kashot");
        f.flatpak_info_exists = true;
        let (_, action) = detect_action(&f);
        let UpdateAction::Managed { command, .. } = action else {
            panic!("flatpak must be package-managed");
        };
        assert_eq!(command.as_deref(), Some("flatpak update org.kashot.Kashot"));
    }

    #[test]
    fn appimage_and_msi_self_install() {
        let mut p = linux("/tmp/.mount_kashotXy/usr/bin/kashot");
        p.appimage_path = Some("/opt/apps/kashot-x86_64.AppImage".to_owned());
        assert_eq!(detect_action(&p).1, UpdateAction::SelfInstall);

        let w = windows("C:\\Program Files\\Kashot\\kashot.exe");
        assert_eq!(detect_action(&w).1, UpdateAction::SelfInstall);
    }

    #[test]
    fn linux_tarball_self_installs_but_windows_and_macos_use_the_browser() {
        assert_eq!(
            detect_action(&linux("/home/u/.local/bin/kashot")).1,
            UpdateAction::SelfInstall
        );
        assert_eq!(
            detect_action(&windows("D:\\tools\\kashot\\kashot.exe")).1,
            UpdateAction::BrowserDownload
        );
        assert_eq!(
            detect_action(&macos("/Applications/Kashot.app/Contents/MacOS/kashot")).1,
            UpdateAction::BrowserDownload
        );
    }

    #[test]
    fn system_package_command_follows_the_distro() {
        let cmd_for = |os_release: Option<&str>| {
            let mut p = linux("/usr/bin/kashot");
            p.os_release = os_release.map(str::to_owned);
            match detect_action(&p).1 {
                UpdateAction::Managed { command, .. } => command,
                other => panic!("expected Managed, got {other:?}"),
            }
        };

        assert_eq!(
            cmd_for(Some("ID=ubuntu\nID_LIKE=debian\n")).as_deref(),
            Some("sudo apt update && sudo apt install --only-upgrade kashot")
        );
        assert_eq!(
            cmd_for(Some("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n")).as_deref(),
            Some("sudo dnf upgrade kashot")
        );
        assert_eq!(
            cmd_for(Some("ID=endeavouros\nID_LIKE=arch\n")).as_deref(),
            Some("yay -Syu kashot-bin")
        );
        assert_eq!(
            cmd_for(Some("ID=opensuse-tumbleweed\nID_LIKE=\"opensuse suse\"\n")).as_deref(),
            Some("sudo zypper update kashot")
        );
        // Unknown distro: no command, but still no self-install.
        assert_eq!(cmd_for(Some("ID=voidlinux\n")), None);
        assert_eq!(cmd_for(None), None);
    }

    #[test]
    fn os_release_id_wins_over_id_like_ordering() {
        // Mint declares ID=linuxmint before ID_LIKE=ubuntu; both map to apt,
        // but the parse must not choke on the quoted multi-word ID_LIKE.
        assert_eq!(
            distro_family(Some("NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n")),
            DistroFamily::Debian
        );
    }

    #[test]
    fn host_os_matches_the_build_target() {
        let expected = if cfg!(target_os = "windows") {
            HostOs::Windows
        } else if cfg!(target_os = "macos") {
            HostOs::MacOs
        } else {
            HostOs::Linux
        };
        assert_eq!(HostOs::host(), expected);
    }
}
