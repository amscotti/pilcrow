"""Client-protocol tests that do not need a real window.

These exercise the transport itself: a timed-out request must not leave a
stale response in the receive buffer for the next command.
"""

from __future__ import annotations

import json
import shutil
import socket
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import pytest

from conftest import _Launch, _shutdown
from pilcrow import DriverTimeout, Pilcrow

pytestmark = pytest.mark.e2e


def _read_request(conn: socket.socket) -> dict[str, Any]:
    data = b""
    while b"\n" not in data:
        chunk = conn.recv(65536)
        if not chunk:
            raise EOFError("client closed")
        data += chunk
    line, _rest = data.split(b"\n", 1)
    return json.loads(line)


def _serve_delayed_then_fresh(
    listener: socket.socket, requests: list[dict[str, Any]]
) -> None:
    first, _ = listener.accept()
    with first:
        request = _read_request(first)
        requests.append(request)
        # Answer after the client's short deadline has expired.
        time.sleep(0.2)
        try:
            response = {"id": request["id"], "ok": True, "result": {"stale": True}}
            first.sendall(json.dumps(response).encode() + b"\n")
        except OSError:
            pass

    second, _ = listener.accept()
    with second:
        request = _read_request(second)
        requests.append(request)
        response = {
            "id": request["id"],
            "ok": True,
            "result": {"title": "fresh", "focused": True},
        }
        second.sendall(json.dumps(response).encode() + b"\n")


def test_timeout_reconnects_instead_of_reading_the_old_response(tmp_path: Path) -> None:
    # pytest's tmp_path can exceed the short AF_UNIX path limit; keep the socket
    # in its own short-lived directory and use tmp_path only for artifacts.
    socket_dir = Path(tempfile.mkdtemp(prefix="pc-sock-"))
    socket_path = socket_dir / "driver.sock"
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(socket_path))
    listener.listen(2)
    requests: list[dict[str, Any]] = []
    thread = threading.Thread(
        target=_serve_delayed_then_fresh,
        args=(listener, requests),
        daemon=True,
    )
    thread.start()

    client = Pilcrow.connect(
        socket_path,
        "test-token",
        artifacts=tmp_path / "artifacts",
        timeout=0.5,
        connect_timeout=1.0,
    )
    try:
        with pytest.raises(DriverTimeout):
            client.status(timeout=0.05)

        # The retry must travel on a new connection with a new id, not consume
        # the late response from the timed-out connection.
        result = client.status()
        assert result["title"] == "fresh"
        thread.join(timeout=1.0)
        assert len(requests) == 2, requests
        assert requests[0]["id"] == 1, requests
        assert requests[1]["id"] == 2, requests
    finally:
        client.close()
        listener.close()
        thread.join(timeout=1.0)
        shutil.rmtree(socket_dir, ignore_errors=True)


def test_a_passed_test_fails_teardown_when_the_app_exits_nonzero(
    tmp_path: Path,
) -> None:
    class FakeProc:
        def poll(self) -> int:
            return 3

        def wait(self, timeout: float | None = None) -> int:
            return 3

    class FakeClient:
        closed = True

        def close(self) -> None:
            pass

    class Report:
        outcome = "passed"
        duration = 0.1

    class Node:
        rep_call = Report()

    class Request:
        node = Node()

    artifacts = tmp_path / "artifacts"
    artifacts.mkdir()
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    state = _Launch(
        test_id="fake-test",
        artifacts=artifacts,
        run_dir=run_dir,
        home=run_dir,
        data=run_dir,
        tmp=run_dir,
        note=run_dir / "note.md",
        socket_path=run_dir / "driver.sock",
        proc=FakeProc(),
        client=FakeClient(),
    )

    problems = _shutdown(state, Request())  # type: ignore[arg-type]

    assert problems == ["app exited with status 3"], problems
    report = json.loads((artifacts / "report.json").read_text(encoding="utf-8"))
    assert report["outcome"] == "failed", report
    assert report["exit_code"] == 3, report
    assert report["teardown_error"] == "app exited with status 3", report
