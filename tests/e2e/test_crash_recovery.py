"""Crash/recovery: a killed process never loses or corrupts saved words.

The app is SIGKILLed mid-session — no quit flush runs — and relaunched
against the exact state it left behind. The contracts under test:

- the note on disk is always a complete version (the saved bytes or a
  completed autosave), never a partial write;
- text the autosave landed before the kill is still there after relaunch,
  and the relaunched window agrees it is clean.
"""

from __future__ import annotations

import time

import pytest

pytestmark = pytest.mark.e2e

MARKER = "crash marker line\n\n"


def _wait_for_note_bytes(crashable, needle: bytes, *, timeout: float = 10.0) -> bytes:
    deadline = time.monotonic() + timeout
    data = crashable.note.read_bytes()
    while needle not in data and time.monotonic() < deadline:
        time.sleep(0.1)
        data = crashable.note.read_bytes()
    return data


def test_kill_before_autosave_leaves_a_complete_note(crashable):
    client = crashable.client
    assert client is not None
    original = crashable.note.read_bytes()

    client.type(MARKER)
    client.expect_dirty(True)
    client = crashable.kill_and_relaunch()

    on_disk = crashable.note.read_bytes()
    autosaved = MARKER.encode() + original
    assert on_disk in (original, autosaved), (
        "the killed process left a partial or unexpected note behind: "
        f"{on_disk[:80]!r}"
    )
    expected = on_disk.decode("utf-8")
    client.expect_text(expected)
    client.expect_dirty(False)


def test_autosaved_text_survives_a_kill(crashable):
    client = crashable.client
    assert client is not None

    client.type(MARKER)
    data = _wait_for_note_bytes(crashable, MARKER.strip().encode())
    assert MARKER.strip().encode() in data, "autosave never landed before the kill"

    client = crashable.kill_and_relaunch()
    client.expect_text_contains(MARKER.strip())
    client.expect_dirty(False)
    client.expect_layout_done()
