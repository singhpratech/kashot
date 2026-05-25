# Security policy

## Reporting a vulnerability

If you believe you have found a security vulnerability in Kashot, please **do not** open a public GitHub issue. Use **private vulnerability reporting** instead so the issue can be triaged before exposure:

> [github.com/singhpratech/kashot/security/advisories/new](https://github.com/singhpratech/kashot/security/advisories/new)

Alternatively, email the maintainer (address on the GitHub profile). Please include:

- A description of the issue and the impact you observed.
- A minimal reproduction (steps, sample input, screenshots / video if relevant).
- The Kashot version (`kashot --version` or the binary's filename), and the OS + version you reproduced on.

You can expect:

- An acknowledgement within **3 business days**.
- A fix or mitigation plan within **14 days** for high-severity issues. Low-severity issues may roll into the next regular release.
- Credit in the release notes, unless you prefer to remain anonymous.

## Scope

In scope:

- The Kashot binary (any platform) and its bundled `ffmpeg` shim.
- The `kashot-core`, `kashot-platform`, and `kashot-app` crates in `kashot-rs/`.
- The `kashot.org` install scripts at `docs/install.sh` and `docs/install.ps1`.
- The release artifacts on [github.com/singhpratech/kashot/releases](https://github.com/singhpratech/kashot/releases).

Out of scope:

- Vulnerabilities that require physical access to an unlocked machine.
- Social-engineering attacks against the maintainer.
- Issues in upstream crates / packages — please report those directly to the upstream maintainer; Kashot will pick them up via Dependabot.
- The `kashot.org` GitHub Pages host itself — that's GitHub's infrastructure; report via [github.com/security](https://github.com/security).

## Supported versions

| Version | Status |
|---|---|
| `0.3.x` | ✅ Supported |
| `0.2.x` | ❌ End of life (replaced by 0.3.0) |
| `0.1.x` | ❌ End of life |

Security fixes land on the latest minor; older minor releases are not back-ported.

## Build provenance

Release artifacts are produced by GitHub-hosted runners running `build-rust.yml` on tag push. The full build log is public — every release is reproducible from the tagged commit:

```sh
git checkout v0.4.0
cd kashot-rs
cargo build --release --bin kashot
```

The shipped binary's SHA-256 should match what CI produced. If it doesn't, treat it as untrusted.

## Hardening status

The repository has the following hardening in place:

- **Branch protection on `main`**: PR required (no direct push), all CI checks must pass, force-push and deletion blocked, conversation resolution required before merge.
- **Dependabot vulnerability alerts** + **automated security updates** enabled.
- **Secret scanning** + **push protection** enabled (blocks pushes containing detected secrets).
- **Private vulnerability reporting** enabled.
- **CodeQL** scans the GitHub Actions workflows on every PR and push to `main`, plus a weekly safety run. (CodeQL Rust support is still in preview and is intentionally not enabled yet; C# was dropped when the legacy WinForms build was retired in v0.3.0.)
