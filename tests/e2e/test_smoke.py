"""The first real-window E2E smoke for Pilcrow.

Both tests launch the actual binary, go through the test-only driver socket,
and skip with a clear reason when the driver build or a GUI session is missing.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.e2e


def _splice_utf8(text: str, start: int, end: int, insert: str) -> str:
    """Replace a byte range; driver offsets are UTF-8 bytes, not characters."""

    raw = text.encode("utf-8")
    return (raw[:start] + insert.encode("utf-8") + raw[end:]).decode("utf-8")


def test_window_and_driver(pilcrow):
    pilcrow.expect_title("short", contains=False)
    pilcrow.expect_text_contains("A short note")

    status = pilcrow.status()
    assert status, "status returned an empty object"
    assert status.get("anchor") is None, f"unselected status has an anchor: {status!r}"
    assert status.get("dirty") is False, f"a fresh note is dirty: {status!r}"
    path = status.get("path")
    assert isinstance(path, str) and path.endswith("short.md"), status

    before = pilcrow.text()
    caret = pilcrow.caret()
    caret_offset = caret.get("offset")
    assert isinstance(caret_offset, int), f"caret has no integer offset: {caret!r}"

    selection = pilcrow.selection()
    start = selection.get("start")
    end = selection.get("end")
    if not isinstance(start, int) or not isinstance(end, int):
        start = end = caret_offset
    replace_start = min(start, end)
    replace_end = max(start, end)

    pilcrow.type("hello")
    pilcrow.expect_text(_splice_utf8(before, replace_start, replace_end, "hello"))
    pilcrow.expect_caret(replace_start + len("hello"))
    pilcrow.expect_layout_done()

    pilcrow.quit()
    assert pilcrow.closed


def test_caret_is_visible(pilcrow):
    sentence = "The first hour is the one that counts."

    pilcrow.type(sentence)
    pilcrow.expect_text_contains(sentence)
    pilcrow.expect_layout_done()
    pilcrow.expect_caret_visible()


def test_lines_and_caret_report_layout_state(pilcrow):
    pilcrow.expect_layout_done()

    state = pilcrow.lines_result()
    assert state["layout_done"] is True, state
    assert state["stale"] is False, state
    assert isinstance(state["revision"], int), state
    assert isinstance(state["frame_seq"], int), state
    assert state["lines"], "layout is done but no lines were placed"
    for line in state["lines"]:
        assert "line" in line, line
        assert "block" not in line, line
        for key in ("start", "end", "x", "y", "h", "rule"):
            assert key in line, (key, line)

    caret = pilcrow.caret()
    assert caret["layout_done"] is True, caret
    assert isinstance(caret["revision"], int), caret
    assert isinstance(caret["frame_seq"], int), caret
    assert caret["visible"] is True, caret


def test_save_then_quit_flushes_the_write(pilcrow):
    status = pilcrow.status()
    path = status.get("path")
    assert isinstance(path, str), f"the launched note has no path: {status!r}"

    before = pilcrow.text()
    marker = "saved before quit\n\n"
    pilcrow.type(marker)
    pilcrow.expect_text_contains(marker)
    pilcrow.expect_dirty(True)

    chord = "cmd-s" if sys.platform == "darwin" else "ctrl-s"
    pilcrow.chord(chord)
    pilcrow.expect_dirty(False)

    pilcrow.quit()
    assert pilcrow.closed

    saved = Path(path).read_text(encoding="utf-8")
    assert marker in saved, "the explicit save did not reach disk before quit"
    assert saved.replace(marker, "", 1) == before, "the save changed other bytes"


def test_quit_flushes_a_save_requested_in_the_same_tick(pilcrow):
    status = pilcrow.status()
    path = status.get("path")
    assert isinstance(path, str), f"the launched note has no path: {status!r}"

    marker = "requested just before quit\n\n"
    pilcrow.type(marker)
    pilcrow.expect_dirty(True)

    # Do not poll for completion: the save is accepted and quit follows in the
    # same tick. The quit callback must write the latest committed text.
    chord = "cmd-s" if sys.platform == "darwin" else "ctrl-s"
    pilcrow.chord(chord)
    assert pilcrow.status().get("saving") is True, (
        "the test-only slow writer should leave the save in flight"
    )

    # Edit after the in-flight snapshot. The quit flush must write this newer
    # text, not just the bytes the background write already captured.
    late = "typed while saving\n\n"
    pilcrow.type(late)
    pilcrow.expect_text_contains(late)
    pilcrow.quit()

    saved = Path(path).read_text(encoding="utf-8")
    assert marker in saved, "quit lost a save that had just been accepted"
    assert late in saved, "quit wrote the in-flight snapshot, not the latest text"
