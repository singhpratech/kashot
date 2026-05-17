# RPM SPEC for Kashot — binary repackage of the pre-built Linux release
# tarball. Targets Fedora / RHEL / CentOS Stream / Rocky / Alma / openSUSE.
#
# Build locally:
#   rpmbuild -bb dist/rpm/kashot.spec \
#     --define "_sourcedir $(pwd)/dist/rpm" \
#     --define "_topdir $(pwd)/rpmbuild"
#
# The tarball (Source0) is fetched from the GitHub release page rather than
# rebuilt from source — same artifact that ships everywhere else, no
# per-distro source compile, no per-distro toolchain pinning. Submission to
# Fedora COPR comes next; until then this is buildable by hand.

Name:           kashot
Version:        0.3.0
Release:        1%{?dist}
Summary:        Fast screenshots with annotations — tray-resident, hotkey-driven
License:        Apache-2.0
URL:            https://kashot.org/
Source0:        https://github.com/singhpratech/kashot/releases/download/v%{version}/kashot-linux-x86_64.tar.gz
Source1:        https://raw.githubusercontent.com/singhpratech/kashot/v%{version}/dist/aur/kashot.desktop
Source2:        https://raw.githubusercontent.com/singhpratech/kashot/v%{version}/icons/linux_hicolor/256x256/apps/kashot.png
Source3:        https://raw.githubusercontent.com/singhpratech/kashot/v%{version}/LICENSE

ExclusiveArch:  x86_64

# Binary repackage — no compiler, no headers, no pkg-config needed at build
# time. Everything heavy ran in the upstream GitHub Actions Linux build.
BuildRequires:  coreutils
BuildRequires:  tar
BuildRequires:  gzip

# Runtime libs the Rust binary dynamically links against on a stock Linux
# desktop. Names cover both the Fedora family and openSUSE — they happen to
# match here for every dep we actually need, with one alias handled below.
Requires:       gtk3
Requires:       dbus-libs
Requires:       libwayland-client
Requires:       libxkbcommon
Requires:       libxcb
Requires:       libxdo
Requires:       pulseaudio-libs

# Fedora / RHEL ship the indicator lib as `libayatana-appindicator-gtk3`;
# openSUSE ships it as `libayatana-appindicator3-1`. Let rpm pick either.
%if 0%{?suse_version}
Requires:       libayatana-appindicator3-1
%else
Requires:       libayatana-appindicator-gtk3
%endif

%description
Kashot is a tray-resident screenshot tool with a built-in annotation editor.
A global hotkey opens a region selector across all monitors; nine annotation
tools (pen, line, arrow, rectangle, ellipse, marker, text, numbered step,
blur/pixelate); four colour palettes; save, copy, or pin to screen.

This package is a repackage of the upstream pre-built x86_64 Linux binary
released on GitHub. It does not compile from source.

%prep
# The release tarball extracts to ./kashot/kashot (a single-binary layout).
%setup -q -c -n kashot-%{version}

%build
# Nothing to build — binary repackage.

%install
rm -rf %{buildroot}
install -Dm0755 kashot/kashot              %{buildroot}%{_bindir}/kashot
install -Dm0644 %{SOURCE1}                 %{buildroot}%{_datadir}/applications/kashot.desktop
install -Dm0644 %{SOURCE2}                 %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/kashot.png
install -Dm0644 %{SOURCE3}                 %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

%files
%license %{_datadir}/licenses/%{name}/LICENSE
%{_bindir}/kashot
%{_datadir}/applications/kashot.desktop
%{_datadir}/icons/hicolor/256x256/apps/kashot.png

%changelog
* Sun May 17 2026 Prateek Singh <singhpratech> - 0.3.0-1
- Windows screen recording shipped (ffmpeg gdigrab + dshow).
- Marker opacity slider in the editor; hotkey rebind widget in Settings.
- Native Linux arm64 release artifact added.
- C# / WinForms reference build retired; Rust is canonical on all platforms.
- License set to Apache-2.0 — installs LICENSE under %license.

* Sun May 17 2026 Prateek Singh <singhpratech> - 0.2.0-1
- Initial RPM packaging for Fedora / RHEL / openSUSE.
- Repackages the upstream kashot-linux-x86_64.tar.gz release asset.
