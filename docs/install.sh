#!/usr/bin/env sh
# KAShot installer — Linux + macOS one-liner.
#
# Quick install (latest release):
#   curl -fsSL https://kashot.org/install.sh | sh
#
# Pin a specific version:
#   curl -fsSL https://kashot.org/install.sh | sh -s -- --tag v0.4.1
#
# Pick a custom install dir:
#   curl -fsSL https://kashot.org/install.sh | sh -s -- --dir /opt/kashot/bin
#
# Defaults: ~/.local/bin (user-local, no sudo).

set -eu

OWNER='singhpratech'
REPO='kashot'
TAG=''
DIR=''
DEFAULT_DIR="${HOME}/.local/bin"

while [ $# -gt 0 ]; do
  case "$1" in
    --tag) TAG="$2"; shift 2 ;;
    --dir) DIR="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,15p' "$0"
      exit 0 ;;
    *) echo "kashot: unknown argument: $1" >&2; exit 2 ;;
  esac
done

DIR="${DIR:-$DEFAULT_DIR}"

# ── OS + arch ────────────────────────────────────────────────────────────────
case "$(uname -s)" in
  Linux*)  OS='linux' ;;
  Darwin*) OS='macos' ;;
  MINGW*|MSYS*|CYGWIN*)
    echo 'kashot: Windows detected. Use the PowerShell installer instead:' >&2
    echo '  iwr -useb https://kashot.org/install.ps1 | iex' >&2
    exit 1 ;;
  *) echo "kashot: unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

# ── Suggest the native package manager when one is wired up ─────────────────
# Purely informational — the tarball install below still runs. We don't
# auto-switch to dnf/snap because (a) the user piped us into `sh` expecting
# a portable install, and (b) the RPM/COPR channel isn't activated yet.
if [ "$OS" = 'linux' ] && [ -r /etc/os-release ]; then
  # shellcheck disable=SC1091
  . /etc/os-release
  case "${ID:-}:${ID_LIKE:-}" in
    fedora*|*:*fedora*|rhel*|*:*rhel*|centos*|*:*centos*|rocky*|*:*rocky*|almalinux*|*:*almalinux*)
      echo 'kashot: Fedora/RHEL-family system detected.' >&2
      echo '  once the COPR repo is live you will be able to use:' >&2
      echo '    sudo dnf copr enable singhpratech/kashot && sudo dnf install kashot' >&2
      echo '  for now, continuing with the portable tarball install...' >&2
      echo >&2
      ;;
    opensuse*|*:*opensuse*|suse*|*:*suse*)
      echo 'kashot: openSUSE detected.' >&2
      echo '  once the OBS repo is live you will be able to use:' >&2
      echo '    sudo zypper install kashot' >&2
      echo '  for now, continuing with the portable tarball install...' >&2
      echo >&2
      ;;
  esac
fi

case "$(uname -m)" in
  x86_64|amd64)  ARCH='x86_64' ;;
  arm64|aarch64) ARCH='arm64' ;;
  *) echo "kashot: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

case "$OS-$ARCH" in
  linux-x86_64) ARTIFACT='kashot-linux-x86_64.tar.gz' ;;
  linux-arm64)  ARTIFACT='kashot-linux-arm64.tar.gz' ;;
  macos-arm64)  ARTIFACT='Kashot-macos-arm64' ;;
  macos-x86_64) ARTIFACT='Kashot-macos-x64' ;;
  *) echo "kashot: no release artifact for $OS-$ARCH" >&2; exit 1 ;;
esac

# ── Resolve tag ──────────────────────────────────────────────────────────────
if [ -z "$TAG" ]; then
  TAG=$(curl -fsSL "https://api.github.com/repos/${OWNER}/${REPO}/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' \
        | head -1)
fi

if [ -z "$TAG" ]; then
  echo 'kashot: could not resolve latest release tag (rate-limited or offline?)' >&2
  exit 1
fi

URL="https://github.com/${OWNER}/${REPO}/releases/download/${TAG}/${ARTIFACT}"

echo "→ KAShot ${TAG}"
echo "  platform:   ${OS}-${ARCH}"
echo "  artifact:   ${ARTIFACT}"
echo "  source:     ${URL}"
echo "  install:    ${DIR}/kashot"
echo

# ── Stop & clean any existing install ───────────────────────────────────────
# Reinstalls must replace the binary atomically. If the previous kashot is
# still running, kill it first so the new binary is what actually launches
# next; otherwise the old in-memory process keeps serving the tray icon.
if pgrep -x kashot >/dev/null 2>&1; then
  echo 'kashot: stopping running instance...' >&2
  pkill -TERM -x kashot 2>/dev/null || true
  for _ in 1 2 3 4 5; do
    pgrep -x kashot >/dev/null 2>&1 || break
    sleep 0.4
  done
  pgrep -x kashot >/dev/null 2>&1 && pkill -KILL -x kashot 2>/dev/null || true
fi

# Remove the old binary at our target path so the install never appends to
# or partially overwrites a stale file. Other kashot binaries elsewhere on
# PATH are flagged below but not auto-deleted (we don't own those paths).
if [ -e "${DIR}/kashot" ]; then
  echo "kashot: removing previous binary at ${DIR}/kashot" >&2
  rm -f "${DIR}/kashot"
fi

# Warn if another kashot lives elsewhere on PATH — PATH precedence might
# resolve `kashot` to the older copy after this install. Surface it loudly
# rather than silently shadowing.
OTHER=$(command -v kashot 2>/dev/null || true)
if [ -n "$OTHER" ] && [ "$OTHER" != "${DIR}/kashot" ]; then
  echo "kashot: heads up — another kashot is on your PATH at $OTHER" >&2
  echo "  remove it with: rm '$OTHER'" >&2
  echo "  (or ensure ${DIR} comes first in PATH so the new install wins)" >&2
fi

# ── Download + extract ───────────────────────────────────────────────────────
mkdir -p "$DIR"
TMP=$(mktemp -d 2>/dev/null || mktemp -d -t kashot)
trap 'rm -rf "$TMP"' EXIT INT TERM

if ! curl -fL --progress-bar -o "${TMP}/${ARTIFACT}" "$URL"; then
  echo 'kashot: download failed.' >&2
  exit 1
fi

cd "$TMP"
case "$ARTIFACT" in
  *.tar.gz)
    tar -xzf "$ARTIFACT"
    BIN=$(find . -type f -name 'kashot' -perm -u+x 2>/dev/null | head -1)
    [ -z "$BIN" ] && BIN=$(find . -type f -name 'kashot' | head -1)
    ;;
  *)
    chmod +x "$ARTIFACT"
    BIN="./${ARTIFACT}"
    ;;
esac

if [ -z "$BIN" ] || [ ! -f "$BIN" ]; then
  echo 'kashot: no kashot binary found in the download.' >&2
  exit 1
fi

# install(1) is the portable copy-with-mode utility.
install -m 0755 "$BIN" "${DIR}/kashot"

echo
echo "[ok] kashot installed -> ${DIR}/kashot"

case ":${PATH}:" in
  *":${DIR}:"*) ;;
  *)
    echo
    echo "  ${DIR} is not on your PATH. add this line to your shell rc:"
    echo "    export PATH=\"${DIR}:\$PATH\""
    ;;
esac

echo
echo '  run:        kashot'
echo '  uninstall:  rm '"${DIR}/kashot"
echo '  docs:       https://kashot.org'
