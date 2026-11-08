# Pilcrow development guide

**Status:** current

**Updated:** 2026-11-08

**Audience:** anyone changing the Rust/GPUI app

The README is the welcome page. This is the working manual: how to run, test,
change, package, and debug the app without breaking the writing loop.

Architecture diagrams and invariants live in `docs/ARCHITECTURE-GPUI.md`.
The testing contract is `docs/TESTING.md`.

---

## 1. Prerequisites

- Rust **1.98.0**; `rust-toolchain.toml` installs it through rustup.
- macOS: Xcode Command Line Tools.
- Linux: the GPUI build dependencies from the CI workflow:

```sh
sudo apt-get install -y \
  gcc g++ clang libfontconfig-dev libfreetype-dev \
  libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
  libx11-xcb-dev libssl-dev libzstd-dev libasound2-dev \
  libvulkan-dev
```

- Python 3.12 only for the real-window E2E suite.

No signing identity, Apple account, Windows certificate, or network service is
required.

---

## 2. Daily commands

```sh
cargo run -p pilcrow-app                 # debug window
cargo test --workspace --locked          # unit + GPUI tests
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

macOS: build the real bundle so the menu name, Dock icon, About panel, and
Markdown document association are correct:

```sh
scripts/bundle-macos.sh
open target/Pilcrow.app
```

After changes:

```sh
scripts/bundle-macos.sh && open target/Pilcrow.app
```

The bare debug binary is useful for `cargo run`/tests but macOS may show the
executable name rather than the product identity.

### Test-driver and E2E

```sh
cargo test -p pilcrow-app --features test-driver --locked
cargo build -p pilcrow-app --features test-driver --locked
PILCROW_E2E_BIN=target/debug/pilcrow \
PILCROW_E2E_REQUIRE=1 \
python -m pytest tests/e2e -q -ra
```

The real-window suite needs a GUI session. On macOS, accessibility can be
opt-in:

```sh
PILCROW_E2E_A11Y=1 python -m pytest tests/e2e/test_accessibility.py -q
```

### Coverage

```sh
./scripts/coverage.sh                    # 85% line floor
PILCROW_COVERAGE_MIN=88 ./scripts/coverage.sh
```

### Golden screenshots (macOS, manual gate)

```sh
./scripts/goldens.sh                       # compare against baselines
PILCROW_UPDATE_GOLDENS=1 ./scripts/goldens.sh   # rewrite baselines
```

Baselines live in `crates/pilcrow-app/goldens/`. Review the PNG diff before
committing an updated baseline, like any other visual change.

---

## 3. Code map by task

| I need to change… | Start here |
|---|---|
| typing, caret, selection | `crates/pilcrow-editor/src/lib.rs` |
| IME / platform text input | `crates/pilcrow-editor/src/input.rs` |
| render + paint | `crates/pilcrow-editor/src/paint.rs` |
| frozen editor API | `crates/pilcrow-editor/src/api.rs` |
| find/replace | `pilcrow-editor/src/find.rs` + find methods in `lib.rs` |
| reading view | `pilcrow-editor/src/reading.rs` |
| accessibility tree/actions | `pilcrow-editor/src/a11y.rs` + wrapper in `lib.rs` |
| spelling | `pilcrow-editor/src/spell.rs` |
| wrapping, line breaks, hangs | `pilcrow-core/src/wrap.rs`, `layout.rs` |
| type scale, gaps, measures | `pilcrow-core/src/style.rs`, `pilcrow-ui` |
| Markdown blocks/inline | `pilcrow-core/src/blocks.rs`, `inline.rs` |
| text commands/lists | `pilcrow-core/src/format.rs`, `lists.rs` |
| undo/redo | `pilcrow-core/src/history.rs` |
| search ranges/HTML/reading copy | `pilcrow-core/src/search.rs`, `export.rs`, `display.rs` |
| window lifecycle, drafts, sessions | `pilcrow-app/src/lib.rs` |
| save queue and atomic writes | `pilcrow-app/src/save.rs`, `file.rs` |
| ACL/xattr metadata FFI | `pilcrow-app/src/ffi.rs` |
| quit capture, journal, and final write | `quit_snapshot` / `prepare_quit` / `finish_quit` in `pilcrow-app/src/lib.rs` |
| backups | `pilcrow-app/src/backups.rs` |
| recent files/session | `pilcrow-app/src/session.rs` |
| appearance/text settings | `pilcrow-app/src/prefs.rs` |
| app name/icon/About/menu roles | `pilcrow-app/src/app_identity.rs` |
| actions, keymaps, menus | `pilcrow-commands/src/*` |
| app tests | `pilcrow-app/src/tests.rs` |
| editor tests | `pilcrow-editor/src/tests.rs` |
| E2E | `tests/e2e/` |

Before editing, read `docs/ARCHITECTURE-GPUI.md` §3 (typing frame), §5 (save),
and §13 (invariants).

---

## 4. Rules that are not negotiable

1. **The buffer is the file.** Do not introduce a rich-text tree or a second
   live document representation.
2. **Nothing expensive on the typing path.** Layout/shaping that affects the
   caret stays on the main thread, but must be incremental and bounded.
   Disk, backups, stats, and full-document work run elsewhere or after a pause.
3. **The caret is exact.** Every offset is snapped to a UTF-8 boundary; caret
   and selection geometry comes from the shaped line being painted.
4. **Undo is ours.** History groups edits, restores selection, and survives IME
   commit/cancel as one logical edit.
5. **Saves are checked.** A write must never silently replace an external edit.
   Success means the disk stamp returned by the atomic write.
6. **Drafts are not a second document.** They are recovery bytes, deleted when a
   path is adopted or a recovered note is explicitly discarded.
7. **No signing or network dependency** in normal build/test/CI paths.
8. **No data outside the app root.** Never read or modify another app's data.
9. **No animation while typing.** No layout jumps, popups, or suggestions in
   the input frame.

---

## 5. How to add a feature

### A new command/menu item

1. Add the action in `pilcrow-commands/src/app_actions.rs` or
   `editor_actions.rs`.
2. Add the handler in the owning view (`NoteWindow` or `EditorView`).
3. Bind it in `pilcrow-commands/src/keymap.rs`.
4. Add it to `pilcrow-commands/src/menus.rs`.
5. Add a test that dispatches the action and asserts the real result.
6. If it changes the visible page, test geometry or add a render assertion.

Never hardcode action names in menus; use the typed action values.

### A persistence/file change

1. Decide whether it is document truth, view state, or recovery state.
2. Document truth belongs in the byte buffer; view state in `NoteWindow`;
   recovery state in `drafts`/`backups`.
3. Route writes through `file::write_atomic` or `write_atomic_checked`.
4. Use `SaveRequest`/`SaveState`; do not add positional booleans.
5. Add failure tests: conflict, missing path, read-only target, oversized file,
   metadata-only touch.

### A layout/type change

1. Change `style::look`/`look_scaled` or `wrap.rs`.
2. Keep incremental equality: a suffix rewrap must equal a fresh layout.
3. Test with a fake measurer in `pilcrow-core`.
4. Check scale/measure invalidation and the cache key.
5. Redraw a real editor window in a GPUI test and assert caret/line geometry.

### An accessibility change

1. Keep exactly one text-input node: the page wrapper.
2. Use `a11y::text_metrics` for UTF-8/character lengths.
3. Update `publish_text_run` and the action handlers together.
4. Run the xa11y test with `PILCROW_E2E_A11Y=1` when the platform allows it.

---

## 6. Tests and definition of done

A task is done when:

- [ ] Pure logic has a core unit test.
- [ ] Interaction has a GPUI in-process test.
- [ ] A real-window path has an E2E test when the driver can observe it.
- [ ] Failure state has a test, not just the happy path.
- [ ] No new UI-thread disk I/O or full-document work while typing.
- [ ] `cargo fmt --all -- --check` and clippy pass.
- [ ] `./scripts/coverage.sh` passes.
- [ ] Relevant docs are updated.
- [ ] A review/adversarial review pass finds no unaddressed correctness issue.

Test layers: see `docs/TESTING.md` and `tests/e2e/README.md`.

---

## 7. CI and CD

CI (`.github/workflows/ci.yml`):

- format, clippy, workspace build/tests;
- test-driver tests and clippy;
- Linux coverage gate;
- Linux X11 real-window E2E + accessibility;
- macOS E2E best effort;
- Windows compile best effort.

Nightly (`.github/workflows/nightly.yml`) runs the full E2E/crash/a11y suite
and the 100k/1M-word long-note filters on Linux, plus a manual dispatch entry.

CD (`.github/workflows/release.yml`):

```sh
git tag v1.0.0
git push origin v1.0.0
```

or run **Release** manually with a tag. It publishes unsigned:

- `Pilcrow-<tag>-macos-arm64.app.zip`
- `Pilcrow-<tag>-linux-x86_64.tar.gz`
- `Pilcrow-<tag>-windows-x86_64.zip` (best effort)
- `SHA256SUMS`

`PILCROW_SIGN=0` is always set. See `docs/RELEASE.md`.

---

## 8. Environment and files

| Variable | Purpose |
|---|---|
| `PILCROW_OPEN` | open this note path |
| `PILCROW_DATA_DIR` | override the app data root; empty is an error |
| `PILCROW_THEME` | test override: `auto`, `light`, `dark` |
| `PILCROW_CONTRAST` | test override: `standard`, `high` |
| `PILCROW_TEST_DRIVER` | set `1` with the `test-driver` feature |
| `PILCROW_TEST_SOCKET` / `PILCROW_TEST_TOKEN` | driver transport |
| `PILCROW_E2E_BIN` | E2E binary path |
| `PILCROW_E2E_REQUIRE` | fail instead of skip on unavailable E2E capabilities |
| `PILCROW_E2E_A11Y` | opt in to the xa11y gate |
| `PILCROW_RECOVERY_CLIPBOARD` | opt in to last-resort clipboard recovery |

App data is under `PilcrowNext` (`~/Library/Application Support/PilcrowNext` on
macOS, `~/.local/share/pilcrow-next` on Linux, `%APPDATA%\PilcrowNext` on
Windows). `settings.json`, `session.json`, `drafts/`, `backups/`, and
`recovery/` live there. Never point a test at the real root.

---

## 9. Packaging and release

macOS local bundle:

```sh
scripts/bundle-macos.sh
```

Release bundle:

```sh
PILCROW_PROFILE=release PILCROW_VERSION=1.0.0 scripts/bundle-macos.sh
```

Linux release staging: release binary + `packaging/linux/*.desktop` + icon.
Windows: portable `.exe` zipped. All unsigned.

Gatekeeper/SmartScreen warnings are expected for unsigned downloads; document
the quarantine removal step, do not add code signing to a normal job.

---

## 10. Troubleshooting

| Symptom | Check |
|---|---|
| app menu says `pilcrow` | launch `target/Pilcrow.app`, not the bare binary |
| Save As opens `/` | fixed to prefer `~/Documents`; check `default_save_directory` |
| no window updates in E2E | GUI session/display must be awake; unit tests still run headless |
| memory spikes on open | 64 MiB note cap; check `file::MAX_NOTE_BYTES` |
| save says read-only | target parent is not writable; choose a writable folder |
| E2E skips | driver build/feature, Python deps, display, or Accessibility permission |
| settings not visible | app data root differs; check `PILCROW_DATA_DIR` |
| stale `.pilcrow-tmp` files | startup scavenges older than 24h; keep live temps |

If stuck, collect: platform, `cargo test` result, app/external log, app data
root, whether the bundle or bare binary was launched, and the last commit.
