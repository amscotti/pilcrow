# Pilcrow real-window E2E

The first layer of L4 testing: launch the actual binary, open a real window,
drive it through the test-only app driver, and fail with a trace and artifacts
when the app misbehaves. The accessibility surface is available through the
optional `xa11y` fixture.

This directory is intentionally small and dependency-light. It is **not** a
replacement for the in-process GPUI tests or the headless golden tests; it is
the proof that the real app, real window and real event loop still work.

## Prerequisites

- Python 3.12 or newer.
- macOS with a GUI session, or Linux with X11 (or a working Wayland session).
  Windows is not supported by this first version: the client uses a Unix
  socket, while Windows will need a named pipe.
- A `test-driver` build of the app:

  ```sh
  cargo build -p pilcrow-app --features test-driver
  ```

  The default is `target/debug/pilcrow`. Set `PILCROW_E2E_BIN` to use another
  binary. A binary built without the feature does not create a socket, so the
  suite **skips** with the build command rather than passing. Until the driver
  exists, each test waits up to `PILCROW_E2E_LAUNCH_TIMEOUT` (15 s by default)
  and then skips.

- Install the Python test dependencies. From the repository root:

  ```sh
  python3.12 -m venv .venv
  . .venv/bin/activate
  python -m pip install -e tests/e2e
  ```

  The dependency ranges live in `pyproject.toml`; there is deliberately no
  lock file. `pillow` is not a dependency: the driver either writes PNG directly
  or returns base64, both of which the client handles without image decoding.

## Run

```sh
# everything in this directory
python -m pytest tests/e2e -m e2e -v

# just the launch/type/caret smoke
python -m pytest tests/e2e/test_smoke.py -v

# collect without launching anything
python -m pytest tests/e2e --collect-only
```

Run E2E serially. Input and the frontmost window are process-global; the suite
rejects `pytest-xdist` when processes are requested. Do not use
`pytest-xdist` for these tests.

Useful flags:

```sh
python -m pytest tests/e2e -m "e2e and not screenshot"
PILCROW_E2E_BIN=target/debug/pilcrow python -m pytest tests/e2e -v
PILCROW_E2E_KEEP=1 python -m pytest tests/e2e/test_smoke.py -v
```

## What the fixtures do

`pilcrow_binary` resolves `PILCROW_E2E_BIN` or `target/debug/pilcrow`.

`pilcrow`:

1. creates a unique temporary run directory (short `TMPDIR` path for the Unix
   socket) and a unique `HOME`;
2. copies `tests/fixtures/short.md` to `<run>/notes/short.md`, so the fixture in
   the repository is never edited or autosaved over;
3. starts the real binary with:

   | Variable | Value |
   |---|---|
   | `PILCROW_TEST_DRIVER` | `1` |
   | `PILCROW_TEST_SOCKET` | `<run>/driver.sock` |
   | `PILCROW_TEST_TOKEN` | 32 random bytes, never logged |
   | `PILCROW_DATA_DIR` | `<run>/data` |
   | `PILCROW_OPEN` | `<run>/notes/short.md` |
   | `PILCROW_THEME` | `light` |
   | `HOME` | `<run>/home` |
   | `TMPDIR`, `XDG_*` | isolated under `<run>` |

4. waits for the socket, verifies it is `0600`, sends `hello`, validates the
   reported pid, and yields a `Pilcrow` client;
5. on teardown asks the driver to quit, then terminates/kills the process if it
   does not exit. The token is never written to `app.log`, `driver.jsonl`,
   `environment.txt` or `report.json`.

`xa11y_app` attaches `xa11y.App.by_pid(pilcrow.pid)` to the same process. It is
optional: missing `xa11y`, missing Accessibility permission, or an attach
failure skips the test with the reason. It never starts a second app. The
suite defines this attach fixture itself because the app is already launched by
the driver fixture; it does not use `pytest-xa11y`'s `xa11y_launcher` path.

A test that cannot run skips; it never passes silently. Use `-ra` (already in
`addopts`) to see skip reasons.

## Permissions

### macOS

The process that runs pytest needs:

- **Accessibility** permission (System Settings → Privacy & Security →
  Accessibility) for AXUIElement access and input synthesis;
- **Screen & System Audio Recording** on macOS 26+ for window content and
  screenshots.

Grant permission to the terminal/app that launches pytest (Terminal, iTerm,
VS Code, etc.), then restart that app. If `xa11y_app` cannot attach, the test
skips with the permission hint.

In CI use `xa11y/setup-a11y` and point `macos-tcc-client` at the Python that
runs the tests:

```yaml
- uses: xa11y/setup-a11y@v1
  with:
    macos-tcc-client: /path/to/python3.12
```

### Linux

A real window needs a display server. Use X11 locally, or Xvfb + a window
manager + D-Bus + AT-SPI2 in CI:

```yaml
- uses: xa11y/setup-a11y@v1
  with:
    setup-display: true
    setup-atspi: true
    setup-window-manager: true
```

`DISPLAY` or `WAYLAND_DISPLAY` must be set; otherwise the suite skips with a
clear reason. Wayland input injection is platform-limited, so driver input is
the deterministic path. `xa11y` may still be unavailable under some
compositors; that fixture skips rather than failing unrelated driver tests.

## Markers and capability skips

All real-window tests are marked `e2e`:

```sh
python -m pytest tests/e2e -m e2e
```

`screenshot` and `input_sim` mark tests that need capabilities advertised by
the driver's `hello` response. If a test has one of those markers and the
capability is absent, the fixture skips with the advertised capability list.
Add the marker when writing a test:

```python
@pytest.mark.e2e
@pytest.mark.screenshot
def test_window_png(pilcrow):
    pilcrow.screenshot("window.png")
```

Capabilities are read from `hello.result.capabilities`; accepted shapes are a
list of names, a string, or an object of `name: bool`. The names this harness
checks for are `screenshot` and `input_sim`.

## Environment variables

### Read by the harness

| Variable | Meaning |
|---|---|
| `PILCROW_E2E_BIN` | Binary to launch; default `target/debug/pilcrow`. Relative paths are resolved from the repository root. A configured but missing/non-executable path fails; a missing default skips. |
| `PILCROW_E2E_KEEP` | `1`, `true`, `yes` or `on` keeps artifacts and the run directory even when the test passes. Failures always keep them. |
| `PILCROW_E2E_TIMEOUT` | Default assertion/poll timeout in seconds (default `5`). |
| `PILCROW_E2E_POLL` | Poll interval in seconds (default `0.02`); assertions poll, they do not sleep. |
| `PILCROW_E2E_LAUNCH_TIMEOUT` | Seconds to wait for the socket and `hello` (default `15`). |
| `TMPDIR` | Where the unique run directory lives. Keep it short enough for a Unix socket path. |

### Set for the app by the fixture

`PILCROW_TEST_DRIVER`, `PILCROW_TEST_SOCKET`, `PILCROW_TEST_TOKEN`,
`PILCROW_DATA_DIR`, `PILCROW_OPEN`, `PILCROW_THEME`, `HOME`, `TMPDIR` and the
`XDG_*` directories. The harness removes inherited `PILCROW_*` variables before
launching.

## Artifacts

Each test writes (or, on success without `PILCROW_E2E_KEEP=1`, removes):

```text
target/e2e/<test-id>-<hash>/
  report.json       outcome, skip/launch/quit errors, exit code
  app.log           stdout + stderr from the real app
  driver.jsonl      every request/response; token omitted
  trace.jsonl       frame records, when a trace was collected
  environment.txt   platform, binary, isolated dirs, permissions context
  screenshots/      PNGs from pilcrow.screenshot()
  run/              isolated HOME/data/note/socket (kept on failure or KEEP)
```

- Failure: everything is kept.
- Skip: logs and `report.json` are kept; the bulky `run/` directory is removed.
- Pass: the directory is removed unless `PILCROW_E2E_KEEP=1`.

A failing assertion message includes the last observed value, the last status
snapshot, and a tail of the frame trace when the driver can provide one.

## Client protocol assumptions

The client follows `docs/TESTING.md` §5, which leaves some argument and
result shapes open. This first version assumes:

- one JSON object per line, request
  `{"id": N, "token": "...", "cmd": "...", "args": {...}}`;
- response `{"id": N, "ok": true, "result": ...}` or
  `{"id": N, "ok": false, "error": {"code": "...", "message": "..."}}`;
- `hello.result` includes `version`, `pid`, and `capabilities`;
- `key` args are `{"name": "...", "mods": [...]}`;
- `resize` args are `{"width": W, "height": H}`;
- `scroll` args are `{"dx": DX, "dy": DY}`;
- `screenshot` receives `{"path": "<artifact path>"}` and either writes that
  PNG or returns `path` / `png_base64`;
- `trace.start` receives `{"path": "<artifact path>"}` and writes JSONL;
  `trace.stop` returns `{"frames": [...]}` or the client reads the JSONL file;
- `caret.result` has `offset` and `visible`; text/selection offsets are UTF-8
  byte offsets.

If the driver chooses different names, update `pilcrow.py` and this section
together. The client is deliberately tolerant about wrapper keys (`text`,
`frames`, `lines`, `selection`, `caret`) but strict about ids, tokens, `ok`,
and protocol framing.

## First tests

- `test_window_and_driver` — launch, check the title and status, type `hello`,
  assert the exact document and caret, finish layout, quit.
- `test_caret_is_visible` — type a sentence, assert the text, layout completion
  and caret visibility.

Both skip cleanly when the binary or driver socket is unavailable.
