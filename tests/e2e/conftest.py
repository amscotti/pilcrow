"""Pytest fixtures for Pilcrow's real-window E2E suite.

Every test gets a fresh app process, a throwaway ``HOME`` and data directory,
and a fresh test-driver socket. The app is never pointed at the person's real
data, and the driver token is never written to a log or an assertion message.

If the binary or the ``test-driver`` socket is unavailable the test is skipped
with the reason and the build command. A skip is never treated as a pass.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import platform
import re
import secrets
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

from pilcrow import (  # noqa: E402 - conftest may be loaded before pytest adds this dir
    DEFAULT_TIMEOUT,
    DriverClosed,
    DriverError,
    DriverTimeout,
    Pilcrow,
)

REPO_ROOT = HERE.parents[1]
FIXTURES = REPO_ROOT / "tests" / "fixtures"
ARTIFACT_ROOT = REPO_ROOT / "target" / "e2e"
BUILD_HINT = "cargo build -p pilcrow-app --features test-driver"

DEFAULT_LAUNCH_TIMEOUT = 15.0
SOCKET_POLL_INTERVAL = 0.05

_MARKER_CAPABILITIES = {
    "screenshot": "screenshot",
    "input_sim": "input_sim",
}


class DriverUnavailable(RuntimeError):
    """The binary started but never produced a usable driver socket."""


class LaunchError(RuntimeError):
    """The app or driver failed after launch; this is a failure, not a skip."""


@dataclass
class _Launch:
    test_id: str
    artifacts: Path
    run_dir: Path
    home: Path
    data: Path
    tmp: Path
    note: Path
    socket_path: Path
    proc: subprocess.Popen[bytes] | None = None
    log_handle: Any = None
    client: Pilcrow | None = None
    skip_reason: str | None = None
    launch_error: str | None = None
    quit_error: str | None = None
    token: str = field(default="", repr=False)
    extra: dict[str, Any] = field(default_factory=dict)


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        value = float(raw)
    except ValueError as exc:
        raise pytest.UsageError(f"{name} must be a number, got {raw!r}") from exc
    if not math.isfinite(value) or value <= 0:
        raise pytest.UsageError(
            f"{name} must be a finite number greater than zero, got {raw!r}"
        )
    return value


LAUNCH_TIMEOUT = _env_float("PILCROW_E2E_LAUNCH_TIMEOUT", DEFAULT_LAUNCH_TIMEOUT)


def _keep_artifacts() -> bool:
    return os.environ.get("PILCROW_E2E_KEEP", "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def _require_e2e() -> bool:
    """In require mode an unavailable driver/display is a failure, not a skip."""

    return os.environ.get("PILCROW_E2E_REQUIRE", "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def _skip_or_fail(reason: str) -> None:
    if _require_e2e():
        pytest.fail(f"PILCROW_E2E_REQUIRE=1: {reason}", pytrace=False)
    pytest.skip(reason)


def _redact(text: str, token: str) -> str:
    return text.replace(token, "<redacted>") if token else text


def pytest_configure(config: pytest.Config) -> None:
    config.addinivalue_line(
        "markers",
        "e2e: real-window end-to-end test (needs a GUI session and a test-driver build)",
    )
    config.addinivalue_line(
        "markers",
        "screenshot: requires the driver's screenshot capability",
    )
    config.addinivalue_line(
        "markers",
        "input_sim: requires the driver's platform input-simulation capability",
    )
    config.addinivalue_line(
        "markers",
        "a11y: talks to the platform accessibility API (opt-in locally)",
    )


@pytest.hookimpl(wrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo[Any]) -> Any:
    report = yield
    setattr(item, f"rep_{report.when}", report)
    return report


def pytest_collection_modifyitems(
    config: pytest.Config,
    items: list[pytest.Item],
) -> None:
    if config.pluginmanager.hasplugin("xdist"):
        numprocesses = getattr(config.option, "numprocesses", None)
        if numprocesses:
            raise pytest.UsageError(
                "Pilcrow E2E input and window state are process-global; "
                "run without pytest-xdist (no -n/--dist)."
            )


def _artifact_dir(test_id: str) -> Path:
    slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", test_id).strip("-")[:80] or "e2e"
    digest = hashlib.sha1(test_id.encode("utf-8")).hexdigest()[:8]
    return ARTIFACT_ROOT / f"{slug}-{digest}"


def _platform_skip_reason() -> str | None:
    if sys.platform == "win32":
        return (
            "the first E2E harness uses a Unix-socket driver; Windows uses a "
            "named pipe and is not wired up yet"
        )
    if sys.platform not in {"darwin", "linux"}:
        return (
            f"real-window E2E is only supported on macOS and Linux (got {sys.platform})"
        )
    if sys.platform.startswith("linux") and not (
        os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")
    ):
        return (
            "no DISPLAY or WAYLAND_DISPLAY; a real window needs X11 or a Wayland "
            "session (CI: xa11y/setup-a11y, local: run inside the desktop session)"
        )
    return None


def _tail(path: Path, limit: int = 4000) -> str:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        return f"<cannot read {path}: {exc}>"
    if len(text) > limit:
        return "..." + text[-limit:]
    return text


def _prepare(request: pytest.FixtureRequest) -> _Launch:
    test_id = request.node.nodeid
    artifacts = _artifact_dir(test_id)
    if artifacts.exists():
        shutil.rmtree(artifacts, ignore_errors=True)
    artifacts.mkdir(parents=True, exist_ok=True)
    (artifacts / "screenshots").mkdir(exist_ok=True)

    run_dir = Path(tempfile.mkdtemp(prefix="pc-e2e-")).resolve()
    home = run_dir / "home"
    data = run_dir / "data"
    tmp = run_dir / "tmp"
    notes = run_dir / "notes"
    for directory in (home, data, tmp, notes):
        directory.mkdir(parents=True, exist_ok=True)

    fixture = FIXTURES / "short.md"
    if not fixture.is_file():
        shutil.rmtree(run_dir, ignore_errors=True)
        shutil.rmtree(artifacts, ignore_errors=True)
        pytest.fail(f"missing test fixture: {fixture}", pytrace=False)

    note = notes / "short.md"
    shutil.copyfile(fixture, note)

    socket_path = run_dir / "driver.sock"
    if len(os.fsencode(str(socket_path))) >= 100:
        shutil.rmtree(run_dir, ignore_errors=True)
        shutil.rmtree(artifacts, ignore_errors=True)
        _skip_or_fail(
            f"driver socket path is too long for a Unix socket: {socket_path}; "
            "set TMPDIR to a shorter directory"
        )

    return _Launch(
        test_id=test_id,
        artifacts=artifacts,
        run_dir=run_dir,
        home=home,
        data=data,
        tmp=tmp,
        note=note,
        socket_path=socket_path,
    )


def _write_environment(state: _Launch, binary: Path) -> None:
    xdg = {
        name: str(state.run_dir / name.lower().replace("_", "-"))
        for name in (
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "XDG_STATE_HOME",
        )
    }
    lines = [
        f"test: {state.test_id}",
        f"platform: {platform.platform()}",
        f"python: {sys.version.splitlines()[0]}",
        f"binary: {binary}",
        f"artifacts: {state.artifacts}",
        f"run_dir: {state.run_dir}",
        f"home: {state.home}",
        f"data_dir: {state.data}",
        f"note: {state.note}",
        f"driver_socket: {state.socket_path}",
        "driver_token: <set; never logged>",
        "PILCROW_TEST_DRIVER: 1",
        "PILCROW_THEME: light",
        f"DISPLAY: {os.environ.get('DISPLAY', '')}",
        f"WAYLAND_DISPLAY: {os.environ.get('WAYLAND_DISPLAY', '')}",
        f"XDG_SESSION_TYPE: {os.environ.get('XDG_SESSION_TYPE', '')}",
        f"CI: {os.environ.get('CI', '')}",
        f"PILCROW_E2E_KEEP: {os.environ.get('PILCROW_E2E_KEEP', '')}",
        f"PILCROW_E2E_LAUNCH_TIMEOUT: {LAUNCH_TIMEOUT}",
        f"PILCROW_E2E_TIMEOUT: {DEFAULT_TIMEOUT}",
        "xdg:",
    ]
    lines.extend(f"  {name}: {value}" for name, value in xdg.items())
    (state.artifacts / "environment.txt").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )


def _child_env(state: _Launch, token: str) -> dict[str, str]:
    env = os.environ.copy()
    for key in list(env):
        if key.startswith("PILCROW_"):
            del env[key]

    for name in (
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ):
        path = state.run_dir / name.lower().replace("_", "-")
        path.mkdir(parents=True, exist_ok=True)
        env[name] = str(path)

    env.update(
        {
            "HOME": str(state.home),
            "TMPDIR": str(state.tmp),
            "PILCROW_DATA_DIR": str(state.data),
            "PILCROW_OPEN": str(state.note),
            "PILCROW_THEME": "light",
            "PILCROW_TEST_DRIVER": "1",
            "PILCROW_TEST_SLOW_SAVE_MS": "150",
            "PILCROW_TEST_SOCKET": str(state.socket_path),
            "PILCROW_TEST_TOKEN": token,
            "RUST_BACKTRACE": "1",
        }
    )
    return env


def _connect_socket(path: Path) -> socket.socket:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(0.25)
    try:
        sock.connect(os.fspath(path))
    except BaseException:
        sock.close()
        raise
    return sock


def _launch(state: _Launch, binary: Path) -> Pilcrow:
    _write_environment(state, binary)
    state.token = secrets.token_hex(32)
    env = _child_env(state, state.token)

    log_path = state.artifacts / "app.log"
    log_path.write_text(
        "\n".join(
            [
                f"# pilcrow E2E: {state.test_id}",
                f"# binary: {binary}",
                f"# started: {time.strftime('%Y-%m-%dT%H:%M:%S%z')}",
                f"# command: {binary}",
                "",
            ]
        ),
        encoding="utf-8",
    )
    state.log_handle = open(log_path, "ab", buffering=0)

    try:
        state.proc = subprocess.Popen(
            [os.fspath(binary)],
            cwd=os.fspath(state.run_dir),
            env=env,
            stdout=state.log_handle,
            stderr=subprocess.STDOUT,
        )
    except OSError as exc:
        raise LaunchError(f"could not launch {binary}: {exc}") from exc

    deadline = time.monotonic() + LAUNCH_TIMEOUT
    last_error: BaseException | None = None
    sock: socket.socket | None = None

    while True:
        exited = state.proc.poll()
        if exited is not None:
            raise LaunchError(
                f"app exited with status {exited} before creating the driver socket.\n"
                f"app log ({log_path}):\n{_tail(log_path)}"
            )

        try:
            sock = _connect_socket(state.socket_path)
            break
        except OSError as exc:
            last_error = exc

        if time.monotonic() >= deadline:
            raise DriverUnavailable(
                f"test-driver socket did not appear within {LAUNCH_TIMEOUT:.1f}s "
                f"({state.socket_path}: {type(last_error).__name__}: {last_error}). "
                f"Build the app with `{BUILD_HINT}` and retry. "
                f"App log: {log_path}"
            )
        time.sleep(SOCKET_POLL_INTERVAL)

    try:
        mode = stat.S_IMODE(os.stat(state.socket_path).st_mode)
    except OSError as exc:
        sock.close()
        raise LaunchError(
            f"driver socket disappeared before it could be inspected: {exc}"
        ) from exc
    if mode & 0o077:
        sock.close()
        raise LaunchError(
            f"driver socket is not private: mode {mode:04o}, expected 0600 "
            f"({state.socket_path})"
        )

    client = Pilcrow(
        sock,
        state.token,
        artifacts=state.artifacts,
        driver_log=state.artifacts / "driver.jsonl",
        timeout=DEFAULT_TIMEOUT,
        pid=state.proc.pid,
    )
    state.client = client
    client.process = state.proc

    hello_deadline = time.monotonic() + LAUNCH_TIMEOUT
    hello_error: BaseException | None = None
    while True:
        try:
            hello = client.hello()
            break
        except (DriverTimeout, DriverClosed) as exc:
            hello_error = exc
        except DriverError as exc:
            raise LaunchError(f"driver handshake failed: {exc}") from exc

        if client.closed:
            raise LaunchError(
                f"driver closed the socket during the handshake: {hello_error}\n"
                f"app log ({log_path}):\n{_tail(log_path)}"
            ) from hello_error
        if state.proc.poll() is not None:
            raise LaunchError(
                f"app exited during the driver handshake.\n"
                f"app log ({log_path}):\n{_tail(log_path)}"
            )
        if time.monotonic() >= hello_deadline:
            raise LaunchError(
                f"driver socket accepted the connection but `hello` did not succeed: "
                f"{type(hello_error).__name__}: {hello_error}\n"
                f"app log ({log_path}):\n{_tail(log_path)}"
            )
        time.sleep(SOCKET_POLL_INTERVAL)

    hello_pid = hello.get("pid")
    if hello_pid is not None:
        try:
            hello_pid = int(hello_pid)
        except (TypeError, ValueError) as exc:
            raise LaunchError(
                f"driver hello.pid is not an integer: {hello_pid!r}"
            ) from exc
        if hello_pid != state.proc.pid:
            raise LaunchError(
                f"driver reported pid {hello_pid}, but the launched process is "
                f"{state.proc.pid}; refusing to attach to the wrong app"
            )
    client.pid = state.proc.pid

    return client


def _stopped_exit_code(proc: subprocess.Popen[bytes] | None) -> int | None:
    if proc is None:
        return None
    try:
        return proc.poll()
    except OSError:
        return None


def _scrub_token_from_log(path: Path, token: str) -> None:
    if not token or not path.is_file():
        return
    try:
        raw = path.read_bytes()
    except OSError:
        return
    needle = token.encode("utf-8")
    if needle in raw:
        try:
            path.write_bytes(raw.replace(needle, b"<redacted>"))
        except OSError:
            pass


def _shutdown(state: _Launch, request: pytest.FixtureRequest) -> list[str]:
    """Stop the app and return teardown problems for a passed test."""
    client = state.client
    proc = state.proc

    if client is not None:
        if not client.closed:
            try:
                client.quit()
            except Exception as exc:  # noqa: BLE001 - report, then force-stop
                state.quit_error = f"{type(exc).__name__}: {exc}"
        client.close()

    if proc is not None and proc.poll() is None:
        if client is None:
            proc.terminate()
        else:
            try:
                proc.wait(timeout=3.0)
            except subprocess.TimeoutExpired:
                proc.terminate()
        try:
            proc.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            try:
                proc.wait(timeout=5.0)
            except subprocess.TimeoutExpired:
                pass

    if state.log_handle is not None:
        try:
            state.log_handle.flush()
            state.log_handle.close()
        except OSError:
            pass
        state.log_handle = None

    _scrub_token_from_log(state.artifacts / "app.log", state.token)
    _scrub_token_from_log(state.artifacts / "driver.jsonl", state.token)

    report = getattr(request.node, "rep_call", None)
    if report is None:
        report = getattr(request.node, "rep_setup", None)
    if report is not None:
        outcome = report.outcome
        duration = report.duration
    elif state.launch_error is not None:
        outcome = "failed"
        duration = None
    elif state.skip_reason is not None:
        outcome = "skipped"
        duration = None
    else:
        outcome = "failed"
        duration = None

    exit_code = _stopped_exit_code(proc)
    teardown_problems: list[str] = []
    if outcome == "passed":
        if state.quit_error is not None:
            teardown_problems.append(f"quit failed: {state.quit_error}")
        if proc is not None and exit_code != 0:
            teardown_problems.append(f"app exited with status {exit_code}")
        if teardown_problems:
            outcome = "failed"

    keep = _keep_artifacts() or outcome == "failed"
    final_run_dir: Path | None = state.run_dir

    if keep and state.run_dir.exists():
        destination = state.artifacts / "run"
        try:
            if destination.exists():
                shutil.rmtree(destination, ignore_errors=True)
            shutil.move(os.fspath(state.run_dir), os.fspath(destination))
            final_run_dir = destination
        except OSError as exc:
            state.extra["run_move_error"] = f"{type(exc).__name__}: {exc}"
            final_run_dir = state.run_dir

    payload = {
        "test": state.test_id,
        "outcome": outcome,
        "duration_s": duration,
        "skip_reason": state.skip_reason,
        "launch_error": state.launch_error,
        "quit_error": state.quit_error,
        "teardown_error": "; ".join(teardown_problems) or None,
        "exit_code": exit_code,
        "run_dir": str(final_run_dir) if final_run_dir is not None else None,
        "artifacts": str(state.artifacts),
        "keep": keep,
        **state.extra,
    }
    try:
        (state.artifacts / "report.json").write_text(
            _redact(json.dumps(payload, indent=2), state.token) + "\n",
            encoding="utf-8",
        )
    except OSError as exc:
        print(f"pilcrow: cannot write report.json: {exc}", file=sys.stderr)

    if not keep:
        if state.run_dir.exists():
            shutil.rmtree(state.run_dir, ignore_errors=True)
        if outcome == "passed":
            shutil.rmtree(state.artifacts, ignore_errors=True)
        else:
            print(
                f"\npilcrow E2E diagnostics kept for this skip: {state.artifacts}",
                file=sys.stderr,
            )
    else:
        print(f"\npilcrow E2E artifacts kept: {state.artifacts}", file=sys.stderr)

    return teardown_problems


def _capability_skip_reason(
    node: pytest.Item,
    client: Pilcrow,
) -> str | None:
    for marker_name, capability in _MARKER_CAPABILITIES.items():
        if node.get_closest_marker(marker_name) and not client.has_capability(
            capability
        ):
            return (
                f"test is marked {marker_name!r}, but the driver did not advertise "
                f"the {capability!r} capability (advertised: "
                f"{sorted(client.capabilities) or 'none'})"
            )
    return None


@pytest.fixture(scope="session")
def pilcrow_binary() -> Path:
    reason = _platform_skip_reason()
    if reason is not None:
        _skip_or_fail(reason)

    configured = os.environ.get("PILCROW_E2E_BIN")
    if configured:
        binary = Path(configured).expanduser()
        if not binary.is_absolute():
            binary = (REPO_ROOT / binary).resolve()
        else:
            binary = binary.resolve()
        if not binary.is_file():
            pytest.fail(
                f"PILCROW_E2E_BIN points at a missing file: {binary}", pytrace=False
            )
        if not os.access(binary, os.X_OK):
            pytest.fail(f"PILCROW_E2E_BIN is not executable: {binary}", pytrace=False)
        return binary

    binary = REPO_ROOT / "target" / "debug" / "pilcrow"
    if not binary.is_file():
        _skip_or_fail(
            f"{binary} not found; build with `{BUILD_HINT}` "
            "(or set PILCROW_E2E_BIN to another driver-enabled binary)"
        )
    if not os.access(binary, os.X_OK):
        _skip_or_fail(f"{binary} is not executable; rebuild it with `{BUILD_HINT}`")
    return binary


@pytest.fixture
def pilcrow(
    request: pytest.FixtureRequest,
    pilcrow_binary: Path,
) -> Pilcrow:
    state = _prepare(request)
    try:
        try:
            client = _launch(state, pilcrow_binary)
        except DriverUnavailable as exc:
            reason = _redact(str(exc), state.token)
            state.skip_reason = reason
            _skip_or_fail(reason)
        except LaunchError as exc:
            message = _redact(str(exc), state.token)
            state.launch_error = message
            pytest.fail(message, pytrace=False)

        capability_reason = _capability_skip_reason(request.node, client)
        if capability_reason is not None:
            state.skip_reason = capability_reason
            _skip_or_fail(capability_reason)

        yield client
    finally:
        teardown_problems = _shutdown(state, request)
        if teardown_problems:
            pytest.fail(
                "teardown: " + "; ".join(teardown_problems), pytrace=False
            )


class Crashable:
    """A launched app that can be SIGKILLed and relaunched on the same root.

    Crash/recovery tests use this instead of the plain `pilcrow` fixture:
    the first launch is identical (isolated HOME, data dir, and note), and
    `kill_and_relaunch` starts a fresh process against exactly the state the
    killed process left behind. Teardown quits the final process normally.
    """

    def __init__(self, state: _Launch, binary: Path) -> None:
        self._state = state
        self._binary = binary
        self.client: Pilcrow | None = state.client

    @property
    def note(self) -> Path:
        return self._state.note

    @property
    def data(self) -> Path:
        return self._state.data

    def kill(self) -> None:
        """Hard-kill the app process; no quit flush, no cleanup handlers."""
        proc = self._state.proc
        if proc is not None and proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5.0)
        if self.client is not None:
            self.client.close()
            self.client = None
        if self._state.log_handle is not None:
            try:
                self._state.log_handle.close()
            except OSError:
                pass
            self._state.log_handle = None

    def relaunch(self) -> Pilcrow:
        """Launch again against the same isolated dirs and note."""
        try:
            self.client = _launch(self._state, self._binary)
        except DriverUnavailable as exc:
            reason = _redact(str(exc), self._state.token)
            self._state.skip_reason = reason
            _skip_or_fail(reason)
        except LaunchError as exc:
            message = _redact(str(exc), self._state.token)
            self._state.launch_error = message
            pytest.fail(message, pytrace=False)
        return self.client

    def kill_and_relaunch(self) -> Pilcrow:
        self.kill()
        return self.relaunch()


@pytest.fixture
def crashable(
    request: pytest.FixtureRequest,
    pilcrow_binary: Path,
) -> Crashable:
    state = _prepare(request)
    try:
        try:
            _launch(state, pilcrow_binary)
        except DriverUnavailable as exc:
            reason = _redact(str(exc), state.token)
            state.skip_reason = reason
            _skip_or_fail(reason)
        except LaunchError as exc:
            message = _redact(str(exc), state.token)
            state.launch_error = message
            pytest.fail(message, pytrace=False)
        yield Crashable(state, pilcrow_binary)
    finally:
        teardown_problems = _shutdown(state, request)
        if teardown_problems:
            pytest.fail(
                "teardown: " + "; ".join(teardown_problems), pytrace=False
            )


@pytest.fixture
def xa11y_app(pilcrow: Pilcrow) -> Any:
    """Attach xa11y to the same process, or skip with the permission reason."""

    try:
        import xa11y  # type: ignore[import-not-found]
    except Exception as exc:  # noqa: BLE001 - optional test dependency
        _skip_or_fail(
            f"xa11y is not available ({type(exc).__name__}: {exc}); install the "
            "E2E dependencies with `python -m pip install -e tests/e2e`"
        )

    app_class = getattr(xa11y, "App", None)
    if app_class is None or not hasattr(app_class, "by_pid"):
        _skip_or_fail(
            "the installed xa11y has no App.by_pid; install the version range in "
            "tests/e2e/pyproject.toml"
        )

    try:
        app = app_class.by_pid(pilcrow.pid)
    except Exception as exc:  # noqa: BLE001 - platform permission/capability varies
        _skip_or_fail(
            f"xa11y could not attach to pid {pilcrow.pid}: "
            f"{type(exc).__name__}: {exc}. Grant Accessibility (and, on macOS 26+, "
            "Screen & System Audio Recording) to the test client, or run "
            "xa11y/setup-a11y in CI."
        )

    yield app
