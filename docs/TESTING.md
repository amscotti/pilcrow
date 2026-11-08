# Pilcrow testing strategy

**Status:** current

**Updated:** 2026-11-08

**Audience:** anyone changing the Rust/GPUI app

**Companion docs:** `docs/ARCHITECTURE-GPUI.md`,
`docs/DEVELOPMENT-GUIDE.md`, `docs/GPUI-VERSION.md`,
`tests/e2e/README.md`

---

## 1. Why this document exists

The expensive bugs in a writing app are not usually model bugs. They are
interaction bugs that only appear when a real person opens a real window and
types:

- the caret drifts away from the glyphs;
- a keystroke reflows the page a frame late;
- the page freezes after an edit because layout invalidation ran in the wrong
  order;
- a click moves a hidden caret under an overlay;
- typing is smooth until the note grows, then hitches;
- a composition commits into the wrong undo step;
- a menu shortcut is written but never dispatched in a live window;
- a save reports success but the bytes on disk are stale;
- a crash loses an untitled note.

The model tests can be green while the app still feels wrong. The strategy is
therefore layered: prove the model headlessly, prove the view in-process, and
prove the real app through a real window.

---

## 2. Test layers

| Layer | What it covers | Location | Runs in |
|---|---|---|---|
| L1 core unit/property | buffer, blocks, inline, wrap, layout, style, history, search, display, export, stats | `crates/pilcrow-core`, `crates/pilcrow-ui` | every `cargo test` |
| L2 editor in-process | shaping, caret, selection, input, IME, find, reading, a11y, spell ranges | `crates/pilcrow-editor/src/tests.rs` | every `cargo test` |
| L3 app in-process | windows, saves, drafts, session, backups, prefs, registry, menus, identity | `crates/pilcrow-app/src/tests.rs` | every `cargo test` |
| L4 driver protocol | request parsing, auth, limits, text slicing, key chords | `crates/pilcrow-app/src/test_driver.rs` | every `cargo test` |
| L5 real-window E2E | launch, focus, type, layout, save, quit, trace, crash/recovery, client transport | `tests/e2e/` | GUI session, test-driver build |
| L6 accessibility | real platform tree, value, selection, edit actions | `tests/e2e/test_accessibility.py` | opt-in xa11y |
| L7 coverage | line/function/region coverage | `scripts/coverage.sh` | Linux CI and local |
| L8 manual/visual | type feel, page appearance, platform quirks, VoiceOver/Orca | release candidate | human |

Current local baseline:

- **495 Rust tests** pass across the workspace.
- **12 Python E2E tests** (5 smoke, 2 transport, 2 trace, 2 crash/recovery,
  1 accessibility).
- **87.03% line coverage** overall, above the 85% gate.
- Clippy and formatting are clean.

---

## 3. Commands

```sh
# Every Rust test, including GPUI in-process tests.
cargo test --workspace --locked

# The test-driver feature is part of the app's real surface.
cargo test -p pilcrow-app --features test-driver --locked

# Static checks.
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check

# Coverage gate, 85% lines by default.
./scripts/coverage.sh
PILCROW_COVERAGE_MIN=88 ./scripts/coverage.sh

# Build the driver binary and run the real-window suite.
cargo build -p pilcrow-app --features test-driver --locked
PILCROW_E2E_BIN=target/debug/pilcrow \
PILCROW_E2E_REQUIRE=1 \
python -m pytest tests/e2e -q -ra
```

The E2E suite needs a GUI session. On macOS, accessibility additionally needs
Accessibility and Screen Recording permission for the process that launches
pytest. On Linux, use X11 or Xvfb with a window manager, D-Bus, and AT-SPI2.

---

## 4. What each layer is for

### 4.1 Core unit/property tests

`pilcrow-core` has no GPUI dependency and no disk or global state. Tests use
fixed inputs and a fake measurer:

- buffer revisions, line index, UTF-8 boundaries;
- block and inline scanning, including malformed input;
- run-aware wrapping and incremental layout equality;
- history grouping, selection restore, byte caps;
- search ranges, display copy, HTML export;
- stats for prose, CJK, punctuation, and task markers;
- list continuation and format command rules.

If a behavior can be decided from bytes alone, it belongs here.

### 4.2 Editor in-process tests

`pilcrow-editor` tests exercise the real `EditorView` and `ProseElement` through
GPUI's test context:

- typing, deletion, Enter/Tab, paste, IME composition;
- caret and selection geometry from the shaped line;
- grapheme/word/line/page/document movement;
- click, drag, double-click, triple-click;
- find/replace highlighting and replace-all undo;
- reading view navigation;
- focus and typewriter geometry;
- accessibility text metrics and actions;
- spell range filtering.

These tests are fast enough to run on every change and catch view logic that
does not need a platform window.

### 4.3 App in-process tests

`pilcrow-app` tests cover the shell with temporary roots:

- new/open/save/save-as/close/quit;
- autosave idle and maximum timers;
- drafts, recovery, discard, and Save As cleanup;
- quit journal, clean-changed recovery, and bounded final write;
- conflict banners and explicit overwrite;
- session restore, recent files, and window registry reservations;
- rolling backups and prefs round-trip;
- menu installation, keybinding dispatch, app identity;
- Save As and export destination selection.

Tests must never point at the developer's real `PilcrowNext` root. Use
`PILCROW_DATA_DIR` or an injected temporary root.

### 4.4 Driver protocol tests

The protocol types compile in every build; the socket and GPUI wiring only
exist behind the `test-driver` feature. Unit tests cover:

- JSON line decode, token rejection, close behavior;
- request size limits and invalid args;
- UTF-8-safe text slicing;
- key-chord construction and modifier validation;
- capability advertisement.

The token is never logged, echoed, or included in an error message.

### 4.5 Real-window E2E

`tests/e2e/` launches the actual binary, opens a real window, and drives it
through a Unix-socket test driver. The fixture:

1. creates a unique run directory and isolated `HOME`, `PILCROW_DATA_DIR`, and
   `TMPDIR`;
2. copies `tests/fixtures/short.md` into the run directory;
3. launches the binary with the test driver, a random token, and a light theme;
4. waits for the `0600` socket, validates the pid through `hello`, and yields a
   client;
5. on teardown, asks the app to quit, then terminates/kills if it does not.

Current coverage:

- launch, title, text, caret, selection, dirty state;
- type and verify UTF-8 byte splicing;
- layout completion and visible caret;
- explicit save + quit flush;
- save requested in the same tick as quit;
- frame trace settlement, no dropped input, layout done, caret visible, and a
  50 ms p99 over measured layout-and-paint work;
- SIGKILL crash/recovery with a complete note and autosaved words surviving;
- timeout/retry transport behavior;
- teardown failure when the app exits non-zero.

The full harness, environment variables, permissions, and artifacts are in
`tests/e2e/README.md`.

### 4.6 Accessibility

The page publishes one `MultilineTextInput` node with:

- a full text value and one synthetic `TextRun`;
- byte-accurate character lengths;
- caret and selection as AccessKit positions;
- `SetTextSelection`, `ReplaceSelectedText`, and `SetValue` actions.

The opt-in xa11y test attaches to the already-running app, reads the tree,
verifies the value matches the driver's text, selects a word, replaces it, and
sets the whole value. Linux CI runs this under X11/AT-SPI. macOS is best effort
because the runner needs TCC permissions. At the pinned GPUI revision neither
the macOS AccessKit adapter nor the Linux AT-SPI bridge exposes selected-text
insertion to xa11y, so that step drives real keystrokes after the accessibility
selection. `SetTextSelection` remains a direct tree action; the macOS adapter
also accepts `SetValue`, while the Linux bridge uses select-all and real
typing.

A manual VoiceOver/Orca pass remains a release-candidate checklist item.

---

## 5. Test driver

The driver exists only in a `--features test-driver` build. It listens on a
Unix socket, requires a random token on every request, and supports:

```text
hello, status, text, caret, selection, lines, frame,
type, key, chord, scroll, trace.start, trace.stop, quit
```

Deliberately unsupported for now:

- `screenshot`;
- app/file commands such as `open`, `save`, `resize`, `click`;
- PNG output.

`frame` is a limited last-frame snapshot. `trace.start` begins a bounded frame
trace in the editor (records from paints and from each driver input, with
input sequence, revision, caret rect, scroll, layout done, frame work in
milliseconds (layout-and-paint duration for painted frames; input records
carry 0), and a paint hash); `trace.stop` returns the records in its
result, and the client writes them to `trace.jsonl`. The optional trace
`path` argument is echoed back, never written by the driver. `scroll` is
vertical only. The capability list stays honest; a command that returns
`unsupported` is not advertised.

Protocol: one JSON object per line.

```json
{"id": 1, "token": "...", "cmd": "status", "args": {}}
{"id": 1, "ok": true, "result": {...}}
{"id": 1, "ok": false, "error": {"code": "...", "message": "..."}}
```

---

## 6. Coverage gate

Line coverage runs with the test-driver feature because the driver is part of
the app's real surface:

```sh
./scripts/coverage.sh
```

The script runs:

```sh
cargo llvm-cov \
  --workspace --locked \
  --features pilcrow-app/test-driver \
  --summary-only \
  --fail-under-lines 85
```

Current result:

| Metric | Value |
|---|---|
| Lines | **87.03%** |
| Functions | **86.43%** |
| Regions | **88.10%** |
| Lines counted | 17,413 |

Core modules are mostly above 95%. The app shell and editor are lower because
GPUI event wiring and platform paths are exercised by real-window E2E. That is
acceptable only while the E2E jobs cover the gap.

Coverage is a smell detector, not a quality target. A line can be covered by a
test that asserts nothing. When raising coverage, add an assertion about
behavior, not a call that touches the line.

CI runs the gate on Linux after the normal tests. A drop below the floor is a
failure, not a warning.

---

## 7. CI topology

`.github/workflows/ci.yml`:

| Job | What it runs |
|---|---|
| `check` (macOS arm64, Linux x86_64) | fmt, clippy, build, workspace tests, test-driver tests and clippy; Linux runs the coverage gate |
| `driver-smoke` (Linux X11) | real-window E2E plus xa11y, `PILCROW_E2E_REQUIRE=1` |
| `driver-smoke-macos` | same suite on macOS, best effort |
| `windows-check` | workspace and test-driver compile, best effort |

`.github/workflows/nightly.yml` runs on a schedule and by manual dispatch:

| Job | What it runs |
|---|---|
| `e2e-linux` | full real-window suite on Linux X11: smoke, transport, trace, crash/recovery, accessibility |
| `perf` | the 100k/1M-word and long-note filters across core, editor, and app |

`.github/workflows/release.yml` runs the workspace tests on macOS and
Linux before packaging. It publishes unsigned artifacts and `SHA256SUMS`; no
signing secret is used.

CI installs the GPUI Linux dependencies (`fontconfig`, `freetype`, Wayland,
X11, Vulkan, ALSA) and caches Cargo. The E2E jobs use `xa11y/setup-a11y` for
X11, D-Bus, AT-SPI, and a window manager.

---

## 8. Writing a new test

### Pure logic

1. Put the test beside the module in `pilcrow-core` or `pilcrow-ui`.
2. Use fixed inputs and a fake measurer; no display.
3. Assert the exact bytes, ranges, or geometry, not just "no panic".

### Editor interaction

1. Use `#[gpui::test]` and `TestAppContext`.
2. Open a window with a known document.
3. Dispatch a key/action or call the real editor method.
4. Assert text, caret, selection, layout, and undo behavior together.

### App shell

1. Use `#[gpui::test]` and `VisualTestContext`.
2. Use a temporary data root and note directory.
3. Drive the real action; assert the save/draft/session result on disk.
4. Cover the failure path: conflict, read-only target, missing file, oversized
   file, or quit during a save.

### Real window

1. Add a test under `tests/e2e/` with `pytest.mark.e2e`.
2. Use `screenshot` or `input_sim` markers only when the driver advertises the
   capability.
3. Poll with the harness helpers; do not `sleep` and hope.
4. Keep the test serial; input and the frontmost window are process-global.
5. On failure, read `report.json`, `app.log`, `driver.jsonl`, and the trace
   before changing code.

### Accessibility

1. Keep exactly one text-input node: the page wrapper.
2. Use `a11y::text_metrics` for UTF-8/character lengths.
3. Update `publish_text_run` and action handlers together.
4. Run `PILCROW_E2E_A11Y=1` when the platform allows it.

---

## 9. What is not gated yet

These are known gaps:

- golden screenshots in CI (they run as a manual macOS gate via
  `scripts/goldens.sh`; baselines live in `crates/pilcrow-app/goldens/`);
- Windows test-driver transport (Unix socket only today);
- manual VoiceOver/Orca pass;
- visual diff review in CI.

The nightly workflow covers the long-running E2E, crash/recovery, trace, and
100k/1M-word filters; the PR gate keeps the fast checks.

Until those land, the release candidate gets the manual checks in section 10.

---

## 10. Release-blocking checks

Before tagging a release:

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] `cargo test -p pilcrow-app --features test-driver --locked`
- [ ] `./scripts/coverage.sh`
- [ ] Linux real-window E2E + xa11y
- [ ] macOS real-window E2E (best effort, run manually if CI cannot)
- [ ] Nightly E2E/crash/perf workflow green on the release commit
- [ ] Windows compile (best effort)
- [ ] Five-minute manual smoke: new, type, find, read, save, reopen, quit,
      recover
- [ ] VoiceOver/Orca manual pass
- [ ] Visual/design review at minimum width, light and dark
- [ ] Unsigned artifacts and `SHA256SUMS` produced
- [ ] Quarantine/SmartScreen instructions in the release notes

Signing and notarization are not required and stay off by default.

---

## 11. Debugging a failure

Failure artifacts live under `target/e2e/<test-id>-<hash>/`:

```text
report.json       outcome, skips, launch/quit errors, exit code
app.log           stdout + stderr from the real app
driver.jsonl      every request/response; token omitted
trace.jsonl       frame records, when available
environment.txt   platform, binary, isolated dirs, permissions context
screenshots/      PNGs from the harness
run/              isolated HOME/data/note/socket
```

Failures keep everything. Passes remove the bulky `run/` unless
`PILCROW_E2E_KEEP=1`. A skipped test keeps logs and `report.json` so the reason
is visible.

For a hang, check whether the socket exists, whether the app logged a panic,
and whether the window is focusable. For a stale frame, capture the driver
`frame` snapshot and the app log before touching the editor.

---

## 12. Flakiness rules

- Poll for a condition; never sleep for a fixed time and assume.
- Run E2E serially. Do not use `pytest-xdist`.
- Use unique temporary roots and short socket paths.
- Never point a test at the real app data root or the user's notes.
- Keep the token out of logs and failure reports.
- A skipped test is not a pass. Use `-ra` to see why.
- Do not weaken an assertion to make a flaky test green. Fix the timing or the
  app.
- A bug class that appears twice gets a blocking test.

---

## 13. Immediate next steps

1. Add a Windows named-pipe transport for the driver.
2. Record the manual VoiceOver/Orca result for the release candidate.
3. Tighten the frame p99 once the nightly Linux runner has a stable baseline.

Golden screenshots run as a manual macOS gate, not in CI, because pixels
depend on host fonts, OS version, and display scale:

```sh
./scripts/goldens.sh                       # compare against baselines
PILCROW_UPDATE_GOLDENS=1 ./scripts/goldens.sh   # rewrite baselines
```

The runner (`pilcrow-goldens`, feature `goldens`) renders the page
off-screen through `VisualTestAppContext` on the main thread — a binary,
not a `#[test]`, because the real macOS platform cannot be created on test
worker threads — and captures with direct texture readback. It fails past
0.5% of pixels differing by more than 8/255 per channel and writes actual
and diff images to `target/goldens/`.
