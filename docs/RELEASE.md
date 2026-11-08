# Building and releasing Pilcrow without signing

**Status:** current policy

**Updated:** 2026-11-08

**Decision:** signing and notarization are **not required** for development, CI,
or public builds. The repo builds and packages without an Apple Developer ID,
Windows code-signing certificate, or CI secrets.

This is deliberate: the app should not be blocked on accounts, certificates, or
notarization. Signing can be added later behind an explicit switch without
changing the build shape.

## Defaults

| Platform | Default output | Signing |
|---|---|---|
| macOS | `Pilcrow.app`, zipped for release | unsigned; **no codesign invocation** |
| Linux | `.tar.gz` with binary, `.desktop`, and icon | unsigned |
| Windows | portable `.zip` with `pilcrow.exe` | unsigned; best effort |

- `PILCROW_SIGN=0` is the default and means **do not invoke `codesign` at
  all** — not Developer ID signing, not notarization, not ad-hoc signing.
  `codesign` can block on a keychain/password prompt; a build must never do
  that.
- `PILCROW_SIGN=1` is reserved for a future signed release. It must be opt-in
  and must fail with a clear message if the required identity is absent.
- CI must not require `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD`,
  `CODESIGN_IDENTITY`, or a Windows certificate. A release job that finds
  `PILCROW_SIGN=0` simply produces unsigned artifacts.
- Local builds are never signed by default. Do not add an unconditional
  `codesign` call to a build script.

## macOS unsigned build

Local debug bundle:

```sh
scripts/bundle-macos.sh
open target/Pilcrow.app
```

Release bundle:

```sh
PILCROW_PROFILE=release \
PILCROW_VERSION=1.0.0 \
scripts/bundle-macos.sh
```

The script builds the selected profile, copies the binary and `AppIcon.icns`
into `Pilcrow.app/Contents`, installs `Info.gpui.plist`, writes `PkgInfo`, and
optionally stamps `CFBundleShortVersionString`. It does not call `codesign`.

For a release artifact:

```sh
mkdir -p dist
ditto -c -k --sequesterRsrc --keepParent \
  target/release/Pilcrow.app \
  "dist/Pilcrow-<tag>-macos-arm64.app.zip"
```

If a downloaded artifact is quarantined:

```sh
xattr -d com.apple.quarantine Pilcrow.app
```

This is expected for an unsigned build; document it in the release notes. No
notarization. Gatekeeper warns on first open.

## Linux unsigned build

```sh
cargo build -p pilcrow-app --release --locked
mkdir -p dist/Pilcrow-<tag>-linux-x86_64
cp target/release/pilcrow dist/Pilcrow-<tag>-linux-x86_64/
cp packaging/linux/dev.pilcrow.Pilcrow.desktop dist/Pilcrow-<tag>-linux-x86_64/
cp macos/icon-1024.png dist/Pilcrow-<tag>-linux-x86_64/dev.pilcrow.Pilcrow.png
tar -C dist -czf "dist/Pilcrow-<tag>-linux-x86_64.tar.gz" \
  "Pilcrow-<tag>-linux-x86_64"
```

The desktop entry sets the app id, claims `text/markdown` and `text/plain`, and
passes selected files as argv. Linux binaries are unsigned by nature for this
project.

## Windows unsigned build

```sh
cargo build -p pilcrow-app --release --locked
```

Package `target/release/pilcrow.exe` as a portable `.zip`. SmartScreen may warn
on download; document it. Do not add a signing step until Windows is promoted
beyond best effort.

## GitHub Actions

`.github/workflows/ci.yml` is the CI gate:

- format, clippy, workspace build/tests;
- test-driver tests and clippy;
- Linux coverage gate;
- Linux X11 real-window E2E plus accessibility;
- macOS E2E best effort;
- Windows compile best effort.

`.github/workflows/release.yml` is the CD path. It runs on `v*` tags, or by
manual dispatch with a tag. It:

1. runs the workspace tests on macOS and Linux;
2. builds the release binary;
3. packages the unsigned macOS `.app.zip`, Linux `.tar.gz`, and Windows `.zip`
   (best effort);
4. writes `SHA256SUMS`;
5. creates or updates the GitHub Release.

Typical release:

```sh
git tag v1.0.0
git push origin v1.0.0
```

`PILCROW_SIGN=0` is set in the workflow. No signing secret is required.

## Future signing switch

When signing is wanted:

1. Add a signing path gated by `PILCROW_SIGN=1`; keep `PILCROW_SIGN=0` working.
2. macOS: import a Developer ID Application certificate, sign with
   `codesign --options runtime`, notarize with `notarytool`, staple.
3. Windows: sign with `signtool` and a certificate.
4. Add secrets only to the signed release job; never make normal PR/test jobs
   depend on them.
5. Update the release notes and quarantine instructions.

## Release checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo test -p pilcrow-app --features test-driver --locked`
- [ ] `./scripts/coverage.sh`
- [ ] Linux E2E plus xa11y
- [ ] macOS E2E and a local `target/Pilcrow.app` smoke
- [ ] Nightly E2E/crash/perf workflow green on the release commit
- [ ] Windows compile best effort
- [ ] Manual five-minute smoke
- [ ] VoiceOver/Orca manual pass
- [ ] `SHA256SUMS` and unsigned artifacts attached
- [ ] Quarantine/SmartScreen instructions in the release notes
