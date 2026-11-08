# Working in pilcrow

Pilcrow is a local-first writing app for macOS, Linux, and Windows. One note per
window, plain Markdown files on disk, no cloud, no account, no sidebar. The
product is the typing experience; everything else is in service of it.

The app is the Rust workspace in `crates/`, built on GPUI. macOS is primary,
Linux is first-class, Windows is best effort.

## Read first

- `docs/ARCHITECTURE-GPUI.md` — modules, runtime, typing frame, saves,
  persistence, and invariants.
- `docs/DEVELOPMENT-GUIDE.md` — setup, commands, code map, feature workflow,
  testing, CI/CD, and troubleshooting.
- `docs/TESTING.md` — test layers, test driver, E2E, coverage gate, release
  validation.
- `docs/RELEASE.md` — unsigned build and release policy.
- `docs/GPUI-VERSION.md` — pinned GPUI commit and upgrade procedure.

## Commands

```sh
cargo run -p pilcrow-app                 # debug window
cargo test --workspace --locked          # unit + GPUI tests
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

macOS bundle:

```sh
scripts/bundle-macos.sh
open target/Pilcrow.app
```

Test driver and real-window E2E:

```sh
cargo build -p pilcrow-app --features test-driver --locked
PILCROW_E2E_BIN=target/debug/pilcrow python -m pytest tests/e2e -q -ra
```

Coverage:

```sh
./scripts/coverage.sh                    # 85% line gate
```

Every change ends with the workspace tests, clippy, formatting, and the
coverage gate green. A visible behavior change also gets a real-window check
when the platform allows it.

**Build signing is off by default** (`PILCROW_SIGN=0`): no `codesign`, no
notarization, no keychain prompt. See `docs/RELEASE.md`.

**Git commit signing is disabled locally** (`commit.gpgsign=false`); do not
re-enable it, or agents will block on a password prompt.

## The rules that matter most

- **The buffer is the file.** No rich-text tree, no shadow copy. Style and
  layout are derived from the bytes; if a feature needs a second source of
  truth, it needs redesigning.
- **Nothing animates or does full-document work while typing.** A caret that
  jumps or a line that reflows a frame late is worse than no animation at all.
  Stats, spellcheck, drafts, autosaves, and backups run after a pause or off the
  typing thread.
- **A caret only ever sits on a UTF-8 boundary.** Movement helpers snap every
  externally supplied offset; caret and selection geometry come from the same
  shaped line that paints.
- **Visible layout is the main thread's**, because the caret must be exact in
  the frame being typed. Layout is incremental across frames on that same
  thread; a long paragraph rewraps only from the edited visual line.
- **One placed geometry per visual line.** Shaping is cached and limited to
  visible lines.
- **Undo is ours.** History groups edits, restores selection, and treats IME
  commit/cancel as one logical edit.
- **Undo back to the saved bytes is clean.** Dirty is a content comparison, not
  just a revision.
- **Saves are checked.** A write is bound to the exact disk version observed by
  the caller. An external edit is never silently overwritten.
- **Drafts are recovery bytes, not a second document.** They are deleted when a
  path is adopted or a recovered note is discarded.
- **No network, signing, or background service is required to build or write.**
- **Do not read or modify data outside the app's own root.**

## GPUI notes

- The GPUI commit in `Cargo.toml` is pinned; `gpui` and every `gpui_*` platform
  crate must stay on the same revision. See `docs/GPUI-VERSION.md`.
- Text shaping uses `WindowTextSystem` and must stay on the UI thread. Shape
  visible lines only, and cache against block/line/scale/measure changes.
- `pilcrow-core` has no GPUI dependency and no disk or global state. Keep it
  testable with a fake measurer and no display.
- Actions, keymaps, and menus live in `pilcrow-commands`; a view installs
  handlers but does not invent a parallel action vocabulary.
- `Pilcrow.app` is the macOS identity. The bare binary may still show the
  executable name; use the bundle for visual checks.

## Testing discipline

- Pure logic in `pilcrow-core`/`pilcrow-ui`: unit tests with fixed inputs and a
  fake measurer. No display.
- Interaction in `pilcrow-editor`/`pilcrow-app`: `#[gpui::test]` exercising the
  real view, input handler, or command.
- File and persistence behavior: temporary roots; failure, conflict, and
  crash-recovery paths get tests, not just the happy path.
- Real-window behavior: the test-driver build plus `tests/e2e` when the driver
  can observe it. Accessibility runs through the optional xa11y gate.
- A bug gets a regression test before the fix. A second occurrence of a bug
  class means the test belongs in the blocking gate.

The complete contract is `docs/TESTING.md`.

## Documentation

- Keep the README as the welcome page and product overview.
- Put architecture and diagrams in `docs/ARCHITECTURE-GPUI.md`.
- Put setup, workflow, and troubleshooting in `docs/DEVELOPMENT-GUIDE.md`.
- Update the relevant document when work lands or scope changes. Do not leave
  a completed item marked open.
