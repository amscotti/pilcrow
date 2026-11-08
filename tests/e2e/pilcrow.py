"""Playwright-like client for Pilcrow's test-only app driver.

The driver is a newline-delimited JSON service on a Unix socket. It is enabled
only in a ``test-driver`` build. See ``docs/TESTING.md`` §5 and
``tests/e2e/README.md`` for the command contract this client assumes.

The token is never written to the driver log, the app log, artifact metadata,
or exception messages.
"""

from __future__ import annotations

import base64
import binascii
import json
import math
import os
import shutil
import socket
import sys
import time
from collections.abc import Iterator, Mapping, Sequence
from contextlib import contextmanager
from pathlib import Path
from threading import Lock
from typing import Any

DEFAULT_TIMEOUT = 5.0
POLL_INTERVAL = 0.02
MAX_LINE_BYTES = 64 * 1024 * 1024

_NOT_SET = object()
_TRACE_TAIL = 10

_REDACT_KEY_FRAGMENTS = ("token", "secret", "password", "authorization")
_BULKY_KEY_NAMES = {"png_base64", "base64", "data"}

_CAPABILITY_ALIASES = {
    "screenshot": {"screenshot", "screenshots"},
    "input_sim": {"input_sim", "input_simulation", "os_input"},
}


def _env_float(name: str, default: float) -> float:
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    try:
        value = float(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be a number, got {raw!r}") from exc
    if not math.isfinite(value) or value <= 0:
        raise ValueError(
            f"{name} must be a finite number greater than zero, got {raw!r}"
        )
    return value


DEFAULT_TIMEOUT = _env_float("PILCROW_E2E_TIMEOUT", DEFAULT_TIMEOUT)
POLL_INTERVAL = _env_float("PILCROW_E2E_POLL", POLL_INTERVAL)


class PilcrowError(RuntimeError):
    """A failed assertion or protocol operation, with diagnostic context."""

    def __init__(self, message: str, *, diagnostics: Mapping[str, Any] | None = None):
        super().__init__(message)
        self.diagnostics = dict(diagnostics or {})


class DriverError(PilcrowError):
    """The driver answered with ``{"ok": false, ...}``."""

    def __init__(
        self,
        command: str,
        code: str,
        message: str,
        *,
        response: Mapping[str, Any] | None = None,
    ):
        self.command = command
        self.code = code
        self.response = dict(response or {})
        super().__init__(f"driver command {command!r} failed: {code}: {message}")


class DriverTimeout(DriverError):
    """The driver did not answer before the client deadline."""


class DriverClosed(DriverError):
    """The driver socket was closed while a request was in flight."""


class DriverProtocolError(PilcrowError):
    """The driver socket produced data that is not the documented protocol."""


class CapabilityError(PilcrowError):
    """A test requested a capability the driver did not advertise."""


def _redact_and_clip(value: Any) -> Any:
    """Make a value safe for JSONL logs: no secrets, no huge base64 blobs."""

    if isinstance(value, Mapping):
        cleaned: dict[str, Any] = {}
        for key, item in value.items():
            key_text = str(key)
            lowered = key_text.lower()
            if any(fragment in lowered for fragment in _REDACT_KEY_FRAGMENTS):
                cleaned[key_text] = "<redacted>"
            elif (
                lowered in _BULKY_KEY_NAMES
                and isinstance(item, str)
                and len(item) > 128
            ):
                cleaned[key_text] = f"<{len(item)} chars>"
            else:
                cleaned[key_text] = _redact_and_clip(item)
        return cleaned
    if isinstance(value, list):
        return [_redact_and_clip(item) for item in value]
    if isinstance(value, tuple):
        return [_redact_and_clip(item) for item in value]
    if isinstance(value, str) and len(value) > 8192:
        return value[:8192] + "...<truncated>"
    return value


def _summarize_text(text: str, limit: int = 240) -> str:
    if len(text) <= limit:
        return repr(text)
    return f"{text[:limit]!r}... <{len(text)} chars>"


def _format_diagnostics(data: Mapping[str, Any], limit: int = 6000) -> str:
    try:
        rendered = json.dumps(data, indent=2, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        rendered = repr(data)
    if len(rendered) > limit:
        rendered = rendered[:limit] + "\n... <diagnostics truncated>"
    return rendered


def _as_mapping(name: str, value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise DriverProtocolError(
            f"{name} result must be a JSON object, got {type(value).__name__}"
        )
    return dict(value)


def _as_int(name: str, value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise DriverProtocolError(f"{name} must be an integer, got {value!r}")
    return int(value)


def _rect_xywh(value: Any) -> tuple[float, float, float, float] | None:
    if isinstance(value, Mapping):
        try:
            x = float(value["x"])
            y = float(value["y"])
            w = float(value.get("w", value.get("width", 0.0)))
            h = float(value.get("h", value.get("height", 0.0)))
        except (KeyError, TypeError, ValueError):
            return None
        return (x, y, w, h)
    if (
        isinstance(value, Sequence)
        and not isinstance(value, (str, bytes))
        and len(value) >= 2
    ):
        try:
            x = float(value[0])
            y = float(value[1])
            w = float(value[2]) if len(value) > 2 else 0.0
            h = float(value[3]) if len(value) > 3 else 0.0
        except (TypeError, ValueError):
            return None
        return (x, y, w, h)
    return None


def _caret_rect(caret: Mapping[str, Any]) -> tuple[float, float, float, float] | None:
    for key in ("screen_rect", "rect", "local_rect", "caret_rect"):
        rect = _rect_xywh(caret.get(key))
        if rect is not None:
            return rect
    return None


def _scroll_y(status: Mapping[str, Any]) -> float | None:
    scroll = status.get("scroll")
    if isinstance(scroll, bool):
        return None
    if isinstance(scroll, (int, float)):
        return float(scroll)
    if isinstance(scroll, Mapping):
        try:
            return float(scroll["y"])
        except (KeyError, TypeError, ValueError):
            return None
    return None


def _input_seq(frame: Mapping[str, Any]) -> int | None:
    value = frame.get("input_seq")
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float, str)):
        raise PilcrowError(f"frame has a non-numeric input_seq: {value!r}")
    try:
        return int(value)
    except (TypeError, ValueError) as exc:
        raise PilcrowError(f"frame has a non-numeric input_seq: {value!r}") from exc


def _expect_frame_p99(frames: Sequence[Mapping[str, Any]], max_ms: float) -> float:
    if not frames:
        raise PilcrowError("frame trace is empty; cannot compute a p99 frame time")

    times: list[float] = []
    for index, frame in enumerate(frames):
        if not isinstance(frame, Mapping):
            raise PilcrowError(
                f"frame {index} is not an object: {type(frame).__name__}"
            )
        value = frame.get("frame_ms")
        if value is None:
            raise PilcrowError(f"frame {index} has no frame_ms field")
        try:
            times.append(float(value))
        except (TypeError, ValueError) as exc:
            raise PilcrowError(
                f"frame {index} has a non-numeric frame_ms: {value!r}"
            ) from exc

    times.sort()
    rank = max(0, math.ceil(0.99 * len(times)) - 1)
    p99 = times[rank]
    if p99 > max_ms:
        raise PilcrowError(
            f"p99 frame time is {p99:.2f} ms, over the {max_ms:.2f} ms budget "
            f"({len(times)} frames, worst {times[-1]:.2f} ms)"
        )
    return p99


def _expect_no_dropped_input(frames: Sequence[Mapping[str, Any]]) -> int:
    if not frames:
        raise PilcrowError("frame trace is empty; cannot check for dropped input")

    seqs: list[int] = []
    for index, frame in enumerate(frames):
        if not isinstance(frame, Mapping):
            raise PilcrowError(
                f"frame {index} is not an object: {type(frame).__name__}"
            )
        seq = _input_seq(frame)
        if seq is not None:
            seqs.append(seq)

    if not seqs:
        raise PilcrowError(
            "frame trace contains no input_seq values; cannot check for dropped input"
        )

    for previous, current in zip(seqs, seqs[1:]):
        if current < previous:
            raise PilcrowError(
                f"input_seq went backwards ({previous} -> {current}); the trace is not ordered"
            )

    seen = set(seqs)
    missing = [seq for seq in range(min(seqs), max(seqs) + 1) if seq not in seen]
    if missing:
        preview = ", ".join(str(seq) for seq in missing[:12])
        suffix = "" if len(missing) <= 12 else f", ... ({len(missing)} total)"
        raise PilcrowError(
            f"input sequence has gaps, which means input was dropped: {preview}{suffix}"
        )
    return len(seen)


class FrameTrace(Sequence[dict[str, Any]]):
    """A list-like view of the frame records returned by ``trace.stop``."""

    def __init__(self, frames: Sequence[Mapping[str, Any]] = ()):
        self._frames: list[dict[str, Any]] = [dict(frame) for frame in frames]

    @property
    def frames(self) -> list[dict[str, Any]]:
        return list(self._frames)

    def __len__(self) -> int:
        return len(self._frames)

    def __getitem__(self, index: int | slice) -> Any:
        return self._frames[index]

    def __iter__(self) -> Iterator[dict[str, Any]]:
        return iter(self._frames)

    def tail(self, count: int = _TRACE_TAIL) -> list[dict[str, Any]]:
        if count <= 0:
            return []
        return self._frames[-count:]

    def _replace(self, frames: Sequence[Mapping[str, Any]]) -> None:
        self._frames = [dict(frame) for frame in frames]

    def expect_frame_p99(self, max_ms: float) -> float:
        return _expect_frame_p99(self._frames, max_ms)

    def expect_no_dropped_input(self) -> int:
        return _expect_no_dropped_input(self._frames)


class Pilcrow:
    """A live connection to one Pilcrow test-driver process."""

    def __init__(
        self,
        sock: socket.socket,
        token: str,
        *,
        artifacts: Path | str,
        driver_log: Path | str | None = None,
        timeout: float | None = None,
        pid: int | None = None,
        socket_path: Path | str | None = None,
        connect_timeout: float = 0.25,
    ):
        self.artifacts = Path(artifacts).resolve()
        self.driver_log = (
            Path(driver_log).resolve()
            if driver_log is not None
            else self.artifacts / "driver.jsonl"
        )
        self.timeout = DEFAULT_TIMEOUT if timeout is None else float(timeout)
        self.pid = pid
        self.process: Any | None = None
        self.hello_result: dict[str, Any] | None = None
        self.version: str | None = None
        self.last_trace: FrameTrace | None = None

        self._sock = sock
        self._token = token
        self._socket_path = (
            os.fspath(socket_path) if socket_path is not None else None
        )
        self._connect_timeout = float(connect_timeout)
        self._lock = Lock()
        self._buffer = b""
        self._next_id = 0
        self._closed = False
        self._log = None
        self._screenshot_seq = 0
        self._trace_path: Path | None = None
        self._last_status: dict[str, Any] | None = None
        self._capabilities: set[str] = set()

        self.artifacts.mkdir(parents=True, exist_ok=True)
        (self.artifacts / "screenshots").mkdir(parents=True, exist_ok=True)
        self.driver_log.parent.mkdir(parents=True, exist_ok=True)
        self._log = open(self.driver_log, "a", encoding="utf-8", buffering=1)
        self._sock.settimeout(self.timeout)

    # -- construction and identity ---------------------------------------

    @classmethod
    def connect(
        cls,
        socket_path: Path | str,
        token: str,
        *,
        artifacts: Path | str,
        driver_log: Path | str | None = None,
        timeout: float | None = None,
        pid: int | None = None,
        connect_timeout: float = 0.25,
    ) -> "Pilcrow":
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(connect_timeout)
        try:
            sock.connect(os.fspath(socket_path))
        except BaseException:
            sock.close()
            raise
        sock.settimeout(DEFAULT_TIMEOUT if timeout is None else float(timeout))
        return cls(
            sock,
            token,
            artifacts=artifacts,
            driver_log=driver_log,
            timeout=timeout,
            pid=pid,
            socket_path=socket_path,
            connect_timeout=connect_timeout,
        )

    def __repr__(self) -> str:
        return (
            f"<Pilcrow pid={self.pid!r} artifacts={str(self.artifacts)!r} "
            "token=<redacted>>"
        )

    @property
    def closed(self) -> bool:
        return self._closed

    @property
    def capabilities(self) -> frozenset[str]:
        return frozenset(self._capabilities)

    def has_capability(self, name: str) -> bool:
        aliases = _CAPABILITY_ALIASES.get(name, {name})
        return bool(self._capabilities.intersection(aliases))

    def require_capability(self, name: str) -> None:
        if not self.has_capability(name):
            raise CapabilityError(
                f"driver did not advertise the {name!r} capability "
                f"(advertised: {sorted(self._capabilities) or 'none'})"
            )

    # -- transport -------------------------------------------------------

    def _read_line(self, timeout: float) -> str:
        deadline = time.monotonic() + timeout
        while b"\n" not in self._buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise socket.timeout("driver response deadline expired")
            self._sock.settimeout(remaining)
            try:
                chunk = self._sock.recv(65536)
            except socket.timeout:
                raise
            except (ConnectionResetError, BrokenPipeError) as exc:
                self._closed = True
                raise EOFError("driver closed the socket") from exc
            if not chunk:
                self._closed = True
                raise EOFError("driver closed the socket")
            self._buffer += chunk
            if len(self._buffer) > MAX_LINE_BYTES:
                raise DriverProtocolError(
                    f"driver response exceeded {MAX_LINE_BYTES} bytes without a newline"
                )

        raw, self._buffer = self._buffer.split(b"\n", 1)
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise DriverProtocolError("driver response is not valid UTF-8") from exc

    def _recover_after_timeout(self) -> None:
        """Drop the timed-out connection so a retry cannot read its response.

        The request may still be executing in the app. Closing the socket makes
        that response undeliverable; the next request opens a fresh connection
        with a new id and a clean receive buffer.
        """

        try:
            self._sock.close()
        except OSError:
            pass
        self._buffer = b""
        if self._socket_path is None:
            self._closed = True
            return

        try:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        except OSError:
            self._closed = True
            return
        sock.settimeout(self._connect_timeout)
        try:
            sock.connect(self._socket_path)
            sock.settimeout(self.timeout)
        except OSError:
            sock.close()
            self._closed = True
            return
        self._sock = sock
        self._closed = False

    def _log_exchange(
        self,
        request: Mapping[str, Any],
        response: Mapping[str, Any] | None,
        error: BaseException | None,
    ) -> None:
        record: dict[str, Any] = {
            "ts_ms": round(time.monotonic() * 1000.0, 1),
            "request": _redact_and_clip(request),
        }
        if response is not None:
            record["response"] = _redact_and_clip(response)
        if error is not None:
            record["error"] = f"{type(error).__name__}: {error}"
        try:
            if self._log is not None:
                line = json.dumps(record, ensure_ascii=False, default=str)
                if self._token:
                    line = line.replace(self._token, "<redacted>")
                self._log.write(line + "\n")
                self._log.flush()
        except OSError as exc:
            print(f"pilcrow: cannot write driver log: {exc}", file=sys.stderr)

    def _request(
        self,
        command: str,
        args: Mapping[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> Any:
        if self._closed:
            raise DriverClosed(command, "closed", "the driver connection is closed")

        request_timeout = self.timeout if timeout is None else float(timeout)
        if request_timeout <= 0:
            raise ValueError("timeout must be greater than zero")

        with self._lock:
            self._next_id += 1
            request_id = self._next_id
            payload = {
                "id": request_id,
                "token": self._token,
                "cmd": command,
                "args": dict(args or {}),
            }
            logged_request = {
                key: value for key, value in payload.items() if key != "token"
            }
            wire = (
                json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode(
                    "utf-8"
                )
                + b"\n"
            )

            try:
                self._sock.sendall(wire)
                line = self._read_line(request_timeout)
            except socket.timeout as exc:
                error = DriverTimeout(
                    command,
                    "timeout",
                    f"no response within {request_timeout:.2f}s",
                )
                self._log_exchange(logged_request, None, error)
                self._recover_after_timeout()
                raise error from exc
            except EOFError as exc:
                error = DriverClosed(command, "closed", "driver closed the socket")
                self._log_exchange(logged_request, None, error)
                raise error from exc
            except (BrokenPipeError, ConnectionResetError) as exc:
                self._closed = True
                error = DriverClosed(command, "closed", "driver closed the socket")
                self._log_exchange(logged_request, None, error)
                raise error from exc
            except DriverProtocolError as exc:
                self._log_exchange(logged_request, None, exc)
                raise

            try:
                response = json.loads(line)
            except json.JSONDecodeError as exc:
                error = DriverProtocolError(
                    f"driver sent invalid JSON for {command!r}: {line[:200]!r}"
                )
                self._log_exchange(logged_request, None, error)
                raise error from exc

            self._log_exchange(logged_request, response, None)

            if not isinstance(response, Mapping):
                raise DriverProtocolError(
                    f"driver response for {command!r} is not an object"
                )
            response_id = response.get("id")
            if response_id is not None and str(response_id) != str(request_id):
                raise DriverProtocolError(
                    f"driver response id {response_id!r} does not match request {request_id}"
                )
            if "ok" not in response:
                raise DriverProtocolError(
                    f"driver response for {command!r} has no 'ok' field"
                )

            if not response["ok"]:
                error_value = response.get("error")
                if isinstance(error_value, Mapping):
                    code = str(error_value.get("code", "error"))
                    message = str(
                        error_value.get("message", "driver did not explain the error")
                    )
                elif isinstance(error_value, str):
                    code = "error"
                    message = error_value
                else:
                    code = "error"
                    message = "driver did not explain the error"
                message = message.replace(self._token, "<redacted>")
                raise DriverError(command, code, message[:1000], response=response)

            result = response.get("result", {})
            if command == "status" and isinstance(result, Mapping):
                self._last_status = dict(result)
            return result

    def close(self) -> None:
        with self._lock:
            if self._closed and self._log is None:
                return
            self._closed = True
            try:
                self._sock.close()
            finally:
                if self._log is not None:
                    try:
                        self._log.flush()
                    finally:
                        self._log.close()
                        self._log = None

    # -- commands --------------------------------------------------------

    def hello(self) -> dict[str, Any]:
        result = self._request("hello", {})
        hello = _as_mapping("hello", result)
        self.hello_result = hello
        self.version = str(hello["version"]) if "version" in hello else None

        hello_pid = hello.get("pid")
        if hello_pid is not None:
            self.pid = _as_int("hello.pid", hello_pid)

        raw_capabilities = hello.get("capabilities", [])
        capabilities: set[str] = set()
        if isinstance(raw_capabilities, Mapping):
            capabilities = {
                str(name) for name, enabled in raw_capabilities.items() if bool(enabled)
            }
        elif isinstance(raw_capabilities, str):
            capabilities = {raw_capabilities}
        elif isinstance(raw_capabilities, Sequence):
            capabilities = {str(name) for name in raw_capabilities}
        else:
            raise DriverProtocolError(
                "hello.capabilities must be an object, string, or list"
            )
        self._capabilities = capabilities
        return hello

    def status(self, *, timeout: float | None = None) -> dict[str, Any]:
        return _as_mapping("status", self._request("status", {}, timeout=timeout))

    def text(self, start: int | None = None, end: int | None = None) -> str:
        args: dict[str, Any] = {}
        if start is not None:
            args["start"] = int(start)
        if end is not None:
            args["end"] = int(end)
        result = self._request("text", args)
        if isinstance(result, str):
            return result
        if isinstance(result, Mapping):
            for key in ("text", "value"):
                value = result.get(key)
                if isinstance(value, str):
                    return value
        raise DriverProtocolError(
            f"text result must be a string or an object with text/value, got {type(result).__name__}"
        )

    def caret(self) -> dict[str, Any]:
        """Caret offset and rect plus the layout revision/frame it came from."""

        result = self._request("caret", {})
        if isinstance(result, (int, float)) and not isinstance(result, bool):
            return {"offset": int(result)}
        if isinstance(result, Mapping):
            data = dict(result)
            nested = data.get("caret")
            if isinstance(nested, Mapping):
                merged = dict(nested)
                merged.update(
                    {key: value for key, value in data.items() if key != "caret"}
                )
                return merged
            return data
        raise DriverProtocolError(
            f"caret result must be an offset or an object, got {type(result).__name__}"
        )

    def selection(self) -> dict[str, Any]:
        result = self._request("selection", {})
        if isinstance(result, Mapping):
            data = dict(result)
            nested = data.get("selection")
            if isinstance(nested, Mapping):
                merged = dict(nested)
                merged.update(
                    {key: value for key, value in data.items() if key != "selection"}
                )
                return merged
            return data
        if isinstance(result, Sequence) and not isinstance(result, (str, bytes)):
            if len(result) == 2:
                return {"start": result[0], "end": result[1]}
        raise DriverProtocolError(
            f"selection result must be an object or [start, end], got {type(result).__name__}"
        )

    def lines_result(self) -> dict[str, Any]:
        """The placed-line response plus its layout freshness metadata.

        Each entry is a source line: ``line`` (line index), ``start``/``end``
        (UTF-8 byte offsets), ``x``/``y``/``h`` and ``rule``. Until markdown
        ``Placed`` blocks land there is no ``block`` field. The top-level
        object always carries ``layout_done``, ``revision`` and ``frame_seq``;
        ``lines`` is empty and ``stale`` is true when the layout is stale.
        """

        result = self._request("lines", {})
        if isinstance(result, Sequence) and not isinstance(result, (str, bytes)):
            data = {
                "lines": [dict(line) for line in result if isinstance(line, Mapping)],
                "layout_done": None,
                "revision": None,
                "frame_seq": None,
                "stale": None,
            }
        elif isinstance(result, Mapping):
            data = dict(result)
            raw_lines = data.get("lines", data.get("visible_lines"))
            if not isinstance(raw_lines, Sequence) or isinstance(
                raw_lines, (str, bytes)
            ):
                raise DriverProtocolError("lines result has no lines/visible_lines array")
            data["lines"] = [
                dict(line) for line in raw_lines if isinstance(line, Mapping)
            ]
        else:
            raise DriverProtocolError(
                f"lines result must be an array or object, got {type(result).__name__}"
            )

        for key in ("layout_done", "revision", "frame_seq"):
            if data.get(key) is None:
                raise DriverProtocolError(f"lines result has no {key} field")
        for index, line in enumerate(data["lines"]):
            if "line" not in line:
                raise DriverProtocolError(
                    f"lines[{index}] has no source 'line' field: {line!r}"
                )
        return data

    def lines(self) -> list[dict[str, Any]]:
        return self.lines_result()["lines"]

    def frame(self, *, timeout: float | None = None) -> dict[str, Any]:
        result = self._request("frame", {}, timeout=timeout)
        if isinstance(result, Mapping):
            nested = result.get("frame")
            if isinstance(nested, Mapping):
                return dict(nested)
            return dict(result)
        raise DriverProtocolError(
            f"frame result must be an object, got {type(result).__name__}"
        )

    def trace_start(self, path: Path | str | None = None) -> dict[str, Any]:
        trace_path = self._artifact_path(
            path,
            default_name="trace.jsonl",
            subdir=None,
            suffix=".jsonl",
        )
        self._trace_path = trace_path
        try:
            trace_path.write_text("", encoding="utf-8")
        except OSError as exc:
            print(f"pilcrow: cannot reset trace artifact: {exc}", file=sys.stderr)
        result = self._request("trace.start", {"path": str(trace_path)})
        return dict(result) if isinstance(result, Mapping) else {"result": result}

    def trace_stop(self, *, timeout: float | None = None) -> list[dict[str, Any]]:
        result = self._request("trace.stop", {}, timeout=timeout)
        frames = self._extract_frames(result)
        trace_path = self._trace_path

        if isinstance(result, Mapping) and not frames:
            returned_path = result.get("path")
            if isinstance(returned_path, str):
                candidate = Path(returned_path)
                if not candidate.is_absolute():
                    candidate = self.artifacts / candidate
                if candidate.is_file():
                    trace_path = candidate

        if not frames and trace_path is not None and trace_path.is_file():
            frames = self._read_jsonl(trace_path)

        if frames and trace_path is not None:
            try:
                if not trace_path.is_file() or trace_path.stat().st_size == 0:
                    trace_path.write_text(
                        "".join(
                            json.dumps(frame, ensure_ascii=False, default=str) + "\n"
                            for frame in frames
                        ),
                        encoding="utf-8",
                    )
            except OSError as exc:
                print(f"pilcrow: cannot write trace artifact: {exc}", file=sys.stderr)

        return frames

    @contextmanager
    def trace(self, path: Path | str | None = None) -> Iterator[FrameTrace]:
        """Collect frame records for the duration of the block.

        The yielded :class:`FrameTrace` is filled on exit, so it is list-like
        both inside and after the ``with`` block.
        """
        trace = FrameTrace()
        self.last_trace = None
        self.trace_start(path)
        failed = False
        try:
            yield trace
        except BaseException:
            failed = True
            raise
        finally:
            try:
                frames = self.trace_stop()
            except Exception:
                if not failed:
                    raise
            else:
                trace._replace(frames)
                self.last_trace = trace

    def type(self, text: str) -> Any:
        return self._request("type", {"text": text})

    def key(self, name: str, mods: Sequence[str] = ()) -> Any:
        if isinstance(mods, str):
            mods = (mods,)
        return self._request("key", {"name": name, "mods": list(mods)})

    def chord(self, chord: str) -> Any:
        return self._request("chord", {"chord": chord})

    def click(self, x: float, y: float) -> Any:
        return self._request("click", {"x": float(x), "y": float(y)})

    def scroll(self, dx: float = 0.0, dy: float = 0.0) -> Any:
        return self._request("scroll", {"dx": float(dx), "dy": float(dy)})

    def resize(self, w: int, h: int) -> Any:
        return self._request("resize", {"width": int(w), "height": int(h)})

    def open(self, path: Path | str) -> Any:
        return self._request("open", {"path": os.fspath(Path(path).resolve())})

    def save(self) -> Any:
        return self._request("save", {})

    def screenshot(self, path: Path | str | None = None) -> Path:
        target = self._artifact_path(
            path,
            default_name=f"step-{self._screenshot_seq + 1:02d}.png",
            subdir="screenshots",
            suffix=".png",
        )
        self._screenshot_seq += 1
        result = self._request("screenshot", {"path": str(target)})

        if target.is_file() and target.stat().st_size > 0:
            return target

        candidates: list[str] = []
        if isinstance(result, str):
            candidates.append(result)
        elif isinstance(result, Mapping):
            for key in ("path", "png_path", "file"):
                value = result.get(key)
                if isinstance(value, str):
                    candidates.append(value)
            for key in ("png_base64", "base64", "data", "png", "image"):
                value = result.get(key)
                if not isinstance(value, str):
                    continue
                if key in ("png_base64", "base64", "data"):
                    if self._write_base64_png(target, value):
                        return target
                else:
                    candidates.append(value)

        for candidate in candidates:
            candidate_path = Path(candidate)
            if not candidate_path.is_absolute():
                candidate_path = self.artifacts / candidate_path
            if candidate_path.is_file() and candidate_path.stat().st_size > 0:
                if candidate_path.resolve() != target.resolve():
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(candidate_path, target)
                return target
            if self._write_base64_png(target, candidate):
                return target

        raise PilcrowError(
            "driver acknowledged screenshot but produced no PNG; "
            f"expected it to write args.path={str(target)!r} or return "
            f"path/png_base64 (result keys: {sorted(result) if isinstance(result, Mapping) else type(result).__name__})"
        )

    def quit(self) -> Any:
        if self._closed:
            self.close()
            return None
        try:
            result = self._request("quit", {}, timeout=min(self.timeout, 3.0))
        except DriverClosed:
            result = None
        finally:
            self.close()
        if self.process is not None:
            # The driver acknowledges quit before the app has finished flushing
            # and exiting. Tests read the note file after quit, so wait for the
            # process rather than racing the final write.
            try:
                self.process.wait(timeout=min(self.timeout + 5.0, 30.0))
            except Exception as exc:  # noqa: BLE001 - surfaced as a test failure
                raise DriverError(
                    "quit",
                    "timeout",
                    f"the app did not exit after quit: {exc}",
                ) from exc
        return result

    # -- assertions ------------------------------------------------------

    def expect_title(
        self,
        expected: str,
        *,
        contains: bool = True,
        timeout: float | None = None,
    ) -> str:
        def probe() -> str:
            return str(self.status().get("title", ""))

        def matches(title: str) -> bool:
            return expected in title if contains else title == expected

        description = (
            f"window title never contained {expected!r}"
            if contains
            else f"window title never equaled {expected!r}"
        )
        return self._poll(description, probe, matches, timeout, observed=repr)

    expect_window_title = expect_title

    def expect_dirty(self, expected: bool, *, timeout: float | None = None) -> dict[str, Any]:
        def probe() -> dict[str, Any]:
            return self.status()

        def matches(status: dict[str, Any]) -> bool:
            return status.get("dirty") is expected

        description = f"note dirty state never became {expected!r}"
        return self._poll(description, probe, matches, timeout, observed=str)

    def expect_text(
        self,
        expected: str,
        *,
        contains: bool = False,
        timeout: float | None = None,
    ) -> str:
        def probe() -> str:
            return self.text()

        def matches(actual: str) -> bool:
            return expected in actual if contains else actual == expected

        description = (
            f"document text never contained {_summarize_text(expected)}"
            if contains
            else f"document text never equaled {_summarize_text(expected)}"
        )
        return self._poll(
            description,
            probe,
            matches,
            timeout,
            observed=_summarize_text,
        )

    def expect_text_contains(
        self, expected: str, *, timeout: float | None = None
    ) -> str:
        return self.expect_text(expected, contains=True, timeout=timeout)

    def expect_caret(
        self,
        offset: int | None = None,
        at: tuple[float, float] | None = None,
        *,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        if offset is None and at is None:
            raise ValueError("expect_caret needs an offset, a screen position, or both")

        def probe() -> dict[str, Any]:
            return self.caret()

        def matches(caret: Mapping[str, Any]) -> bool:
            if offset is not None:
                actual = caret.get("offset")
                if actual is None:
                    return False
                if _as_int("caret.offset", actual) != int(offset):
                    return False
            if at is not None:
                rect = _caret_rect(caret)
                if rect is None:
                    return False
                if abs(rect[0] - float(at[0])) > 0.75:
                    return False
                if abs(rect[1] - float(at[1])) > 0.75:
                    return False
            return True

        description = f"caret never reached offset={offset!r} at={at!r}"
        return self._poll(description, probe, matches, timeout, observed=repr)

    def expect_caret_visible(self, *, timeout: float | None = None) -> dict[str, Any]:
        def probe() -> dict[str, Any]:
            return self.caret()

        def matches(caret: Mapping[str, Any]) -> bool:
            visible = caret.get("visible")
            if visible is None:
                raise PilcrowError(
                    "driver caret result has no 'visible' field; visibility cannot be proven"
                )
            return bool(visible)

        return self._poll(
            "caret never became visible",
            probe,
            matches,
            timeout,
            observed=repr,
        )

    def expect_selection(
        self,
        start: int | None = None,
        end: int | None = None,
        *,
        timeout: float | None = None,
    ) -> dict[str, Any]:
        if start is None and end is None:
            raise ValueError("expect_selection needs a start, an end, or both")

        def probe() -> dict[str, Any]:
            return self.selection()

        def matches(selection: Mapping[str, Any]) -> bool:
            if start is not None:
                actual = selection.get("start")
                if actual is None or _as_int("selection.start", actual) != int(start):
                    return False
            if end is not None:
                actual = selection.get("end")
                if actual is None or _as_int("selection.end", actual) != int(end):
                    return False
            return True

        description = f"selection never became start={start!r} end={end!r}"
        return self._poll(description, probe, matches, timeout, observed=repr)

    def expect_scroll(
        self, y: float, *, timeout: float | None = None
    ) -> dict[str, Any]:
        def probe() -> dict[str, Any]:
            return self.status()

        def matches(status: Mapping[str, Any]) -> bool:
            actual = _scroll_y(status)
            return actual is not None and abs(actual - float(y)) <= 0.5

        return self._poll(
            f"scroll never reached y={y!r}",
            probe,
            matches,
            timeout,
            observed=repr,
        )

    def expect_layout_done(self, *, timeout: float | None = None) -> dict[str, Any]:
        def probe() -> dict[str, Any]:
            return self.status()

        def matches(status: Mapping[str, Any]) -> bool:
            done = status.get("layout_done", status.get("layoutDone"))
            if done is None:
                raise PilcrowError("status result has no layout_done field")
            return bool(done)

        return self._poll(
            "layout never finished",
            probe,
            matches,
            timeout,
            observed=repr,
        )

    def expect_frame_p99(
        self,
        max_ms: float,
        frames: Sequence[Mapping[str, Any]] | FrameTrace | None = None,
    ) -> float:
        selected = self._select_frames(frames)
        try:
            return _expect_frame_p99(selected, max_ms)
        except PilcrowError as exc:
            raise self._annotate_failure(exc) from exc

    def expect_no_dropped_input(
        self,
        frames: Sequence[Mapping[str, Any]] | FrameTrace | None = None,
    ) -> int:
        selected = self._select_frames(frames)
        try:
            return _expect_no_dropped_input(selected)
        except PilcrowError as exc:
            raise self._annotate_failure(exc) from exc

    def _annotate_failure(self, exc: PilcrowError) -> PilcrowError:
        try:
            context = self._format_failure_context(_NOT_SET, exc)
        except Exception:
            return exc
        return PilcrowError(f"{exc}\n{context}", diagnostics=exc.diagnostics)

    # -- polling and diagnostics ----------------------------------------

    def _select_frames(
        self,
        frames: Sequence[Mapping[str, Any]] | FrameTrace | None,
    ) -> Sequence[Mapping[str, Any]]:
        if isinstance(frames, FrameTrace):
            return frames.frames
        if frames is not None:
            return list(frames)
        if self.last_trace is not None:
            return self.last_trace.frames
        raise PilcrowError(
            "no frame trace available; collect one with "
            "`with pilcrow.trace() as trace:` before asserting frame budgets"
        )

    def _poll(
        self,
        description: str,
        probe: Any,
        predicate: Any,
        timeout: float | None,
        *,
        observed: Any = None,
    ) -> Any:
        poll_timeout = self.timeout if timeout is None else float(timeout)
        if poll_timeout <= 0:
            raise ValueError("timeout must be greater than zero")

        deadline = time.monotonic() + poll_timeout
        last: Any = _NOT_SET
        last_error: BaseException | None = None

        while True:
            try:
                last = probe()
            except (DriverClosed, DriverProtocolError) as exc:
                raise PilcrowError(
                    f"{description}: driver became unavailable: {exc}\n"
                    f"{self._format_failure_context(last, last_error)}"
                ) from exc
            except DriverError as exc:
                last_error = exc
            else:
                try:
                    if predicate(last):
                        return last
                except PilcrowError as exc:
                    raise PilcrowError(
                        f"{description}: {exc}\n"
                        f"{self._format_failure_context(last, last_error)}"
                    ) from exc
                except Exception as exc:
                    raise PilcrowError(
                        f"{description}: probe failed: {type(exc).__name__}: {exc}\n"
                        f"{self._format_failure_context(last, last_error)}"
                    ) from exc

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(POLL_INTERVAL, remaining))

        last_text = "<no observation>"
        if last is not _NOT_SET:
            try:
                last_text = observed(last) if observed is not None else repr(last)
            except Exception:
                last_text = repr(last)

        message = (
            f"{description} (waited {poll_timeout:.2f}s; last observed: {last_text})"
        )
        if last_error is not None:
            message += f"\nlast error: {type(last_error).__name__}: {last_error}"
        message += "\n" + self._format_failure_context(last, last_error)
        raise PilcrowError(message)

    def _format_failure_context(
        self,
        last: Any,
        last_error: BaseException | None,
    ) -> str:
        diagnostics = self._diagnostics(last, last_error)
        return "diagnostics:\n" + _format_diagnostics(diagnostics)

    def _diagnostics(
        self,
        last: Any,
        last_error: BaseException | None,
    ) -> dict[str, Any]:
        data: dict[str, Any] = {
            "last_observed": "<no observation>" if last is _NOT_SET else last,
            "last_status": self._last_status,
            "last_error": (
                None
                if last_error is None
                else f"{type(last_error).__name__}: {last_error}"
            ),
        }
        if self._closed:
            data["driver"] = "closed"
            return data

        diagnostic_timeout = min(self.timeout, 1.0)
        try:
            data["last_status"] = self.status(timeout=diagnostic_timeout)
        except Exception as exc:
            data["status_error"] = f"{type(exc).__name__}: {exc}"

        try:
            data["frame"] = self.frame(timeout=diagnostic_timeout)
        except Exception as exc:
            data["frame_error"] = f"{type(exc).__name__}: {exc}"

        try:
            frames = self.trace_stop(timeout=diagnostic_timeout)
            data["trace_tail"] = frames[-_TRACE_TAIL:]
        except Exception as exc:
            data["trace_error"] = f"{type(exc).__name__}: {exc}"
        return data

    # -- artifacts -------------------------------------------------------

    def _artifact_path(
        self,
        path: Path | str | None,
        *,
        default_name: str,
        subdir: str | None,
        suffix: str,
    ) -> Path:
        root = self.artifacts.resolve()
        base = root if subdir is None else root / subdir

        if path is None:
            target = base / default_name
        else:
            target = Path(path)
            if not target.is_absolute():
                target = base / target
            if suffix and not target.suffix:
                target = target.with_suffix(suffix)
            target = target.resolve()

        if not target.is_relative_to(root):
            raise ValueError(
                f"artifact path must stay under {str(root)!r}, got {str(target)!r}"
            )
        target.parent.mkdir(parents=True, exist_ok=True)
        return target

    @staticmethod
    def _read_jsonl(path: Path) -> list[dict[str, Any]]:
        frames: list[dict[str, Any]] = []
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            raise PilcrowError(f"cannot read trace artifact {path}: {exc}") from exc
        for number, line in enumerate(lines, start=1):
            if not line.strip():
                continue
            try:
                value = json.loads(line)
            except json.JSONDecodeError as exc:
                raise PilcrowError(
                    f"trace artifact {path}:{number} is not valid JSON: {exc}"
                ) from exc
            if not isinstance(value, Mapping):
                raise PilcrowError(
                    f"trace artifact {path}:{number} is not a frame object"
                )
            frames.append(dict(value))
        return frames

    @staticmethod
    def _extract_frames(result: Any) -> list[dict[str, Any]]:
        if isinstance(result, FrameTrace):
            return result.frames
        if isinstance(result, Sequence) and not isinstance(result, (str, bytes)):
            return [dict(frame) for frame in result if isinstance(frame, Mapping)]
        if isinstance(result, Mapping):
            for key in ("frames", "records", "trace"):
                value = result.get(key)
                if isinstance(value, Sequence) and not isinstance(value, (str, bytes)):
                    return [
                        dict(frame) for frame in value if isinstance(frame, Mapping)
                    ]
                if isinstance(value, Mapping):
                    nested = value.get("frames")
                    if isinstance(nested, Sequence) and not isinstance(
                        nested, (str, bytes)
                    ):
                        return [
                            dict(frame)
                            for frame in nested
                            if isinstance(frame, Mapping)
                        ]
        return []

    @staticmethod
    def _write_base64_png(target: Path, value: str) -> bool:
        text = value.strip()
        if text.startswith("data:"):
            _, separator, text = text.partition(",")
            if not separator:
                return False
        text = "".join(text.split())
        try:
            raw = base64.b64decode(text, validate=True)
        except (ValueError, binascii.Error):
            return False
        if not raw.startswith(b"\x89PNG\r\n\x1a\n"):
            return False
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(raw)
        return True
