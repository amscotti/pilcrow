"""Accessibility gate: the page must be a real editable text control.

This test talks to the platform accessibility API, not the test driver. It is
opt-in in the local suite because macOS requires Accessibility permission for
the Python process. CI enables it after ``xa11y/setup-a11y``; a failure there is
a release blocker.
"""

from __future__ import annotations

import os
import sys
import time

import pytest

try:
    import xa11y
except Exception as exc:  # noqa: BLE001 - optional dependency
    if os.environ.get("PILCROW_E2E_REQUIRE", "").strip().lower() in {"1", "true", "yes", "on"}:
        pytest.fail(
            f"xa11y is required for the accessibility gate: {type(exc).__name__}: {exc}",
            pytrace=False,
        )
    pytest.skip(f"xa11y is not installed: {type(exc).__name__}: {exc}")

ENABLED = os.environ.get("PILCROW_E2E_A11Y", "").strip().lower() in {
    "1",
    "true",
    "yes",
    "on",
}

pytestmark = [
    pytest.mark.a11y,
    pytest.mark.skipif(
        not ENABLED,
        reason="set PILCROW_E2E_A11Y=1 to run the platform accessibility test",
    ),
]


def _note_role(tree: dict) -> str | None:
    if tree.get("name") == "Note":
        return tree.get("role")
    for child in tree.get("children", []):
        found = _note_role(child)
        if found:
            return found
    return None


def _wait_value(locator, expected: str, timeout: float = 15.0) -> str:
    deadline = time.monotonic() + timeout
    value = ""
    while time.monotonic() < deadline:
        value = locator.element().value or ""
        if value == expected:
            return value
        time.sleep(0.05)
    return value


def test_the_page_is_editable_through_the_accessibility_tree(pilcrow, xa11y_app) -> None:
    xa11y.set_default_timeout(15.0)
    deadline = time.monotonic() + 20.0
    windows = []
    role = None
    tree = {}
    while time.monotonic() < deadline and role is None:
        windows = xa11y_app.windows()
        if windows:
            tree = windows[0].tree()
            role = _note_role(tree)
        if role is None:
            time.sleep(0.1)
    assert windows, "the app exposed no accessibility window"
    assert role, f"no note control in the accessibility tree: {tree}"

    note = xa11y_app.locator(f'{role}[name="Note"]')
    note.wait_visible()
    # Accessibility text insertion is delivered to the active window. The
    # driver launch may leave another app frontmost on macOS, so activate the
    # note window and wait for the editor to report focus before typing.
    windows[0].activate()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        status = pilcrow.status()
        if status.get("focused") is True:
            break
        time.sleep(0.05)
    else:
        pytest.fail(f"the note editor never reported focus: {pilcrow.status()}")
    text = pilcrow.text()
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline and note.element().value != text:
        time.sleep(0.05)
    assert note.element().value == text, "the accessibility value diverged from the file"

    # SetTextSelection: select a word and verify the driver sees the same range.
    start = text.index("Pilcrow")
    end = start + len("Pilcrow")
    byte_start = len(text[:start].encode("utf-8"))
    byte_end = len(text[:end].encode("utf-8"))
    note.select_text(start, end)
    deadline = time.monotonic() + 15.0
    while time.monotonic() < deadline:
        selection = pilcrow.selection()
        if selection.get("start") == byte_start and selection.get("end") == byte_end:
            break
        time.sleep(0.05)
    else:
        pytest.fail(
            f"SetTextSelection did not reach the document: {pilcrow.selection()}"
        )

    # The selected word must be the one replaced. Neither the macOS AccessKit
    # adapter nor the Linux AT-SPI bridge at the pinned GPUI revision exposes
    # the selected-text insertion action to xa11y, so drive real keystrokes
    # after the accessibility selection. SetTextSelection and SetValue remain
    # direct tree actions.
    expected = text[:start] + "X" + text[end:]
    xa11y.input_sim().type_text("X")
    assert _wait_value(note, expected) == expected
    pilcrow.expect_text(expected)

    # SetValue replaces the whole document and resets the caret. The macOS
    # AccessKit adapter accepts it; the Linux AT-SPI bridge does not expose a
    # writable text value, so that platform uses select-all and real typing.
    if sys.platform == "darwin":
        note.set_value("fresh text")
    else:
        xa11y.input_sim().chord("a", held=["Ctrl"])
        xa11y.input_sim().type_text("fresh text")
    assert _wait_value(note, "fresh text") == "fresh text"
    pilcrow.expect_text("fresh text")
