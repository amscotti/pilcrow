"""Frame trace: typed input settles into painted frames, without gaps.

These are the R-32 frame assertions: every driver input has a record,
sequence numbers never skip, the trace settles on the live revision (not a
stale paint), the caret is visible, and layout reports done.

``frame_ms`` measures layout-and-paint work per painted frame (input records
carry 0), so the p99 budget judges frame cost, not client polling gaps. The
budget is 50 ms: roughly two orders of magnitude over the sub-millisecond
frames this fixture produces on a developer machine, which leaves room for
debug builds and shared CI runners while still catching a frame that starts
doing whole-document work.
"""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.e2e


def _require_trace(pilcrow) -> None:
    if not pilcrow.has_capability("trace.start"):
        pytest.skip("the test driver does not advertise trace.start")


def test_trace_settles_typed_input_into_frames(pilcrow):
    _require_trace(pilcrow)
    pilcrow.expect_layout_done()

    with pilcrow.trace() as trace:
        for piece in ("z", "z", "9"):
            pilcrow.type(piece)
        pilcrow.expect_text_contains("zz9")
        # Driver commands can all land inside one vsync interval, so wait
        # until the window has actually painted the typed revision and
        # finished laying it out before stopping the trace.
        pilcrow.expect_layout_done()

    frames = list(trace)
    assert frames, "trace returned no frame records"
    trace.expect_no_dropped_input()
    trace.expect_frame_p99(50.0)

    input_seqs = {int(frame["input_seq"]) for frame in frames}
    assert {1, 2, 3} <= input_seqs, f"each typed piece needs a record: {frames}"
    assert all(isinstance(frame["paint_hash"], int) for frame in frames)
    assert all(frame["frame_ms"] >= 0 for frame in frames)
    assert all(
        later["seq"] >= earlier["seq"]
        for earlier, later in zip(frames, frames[1:], strict=False)
    ), f"frame sequence went backwards: {frames}"

    painted = [frame for frame in frames if frame["seq"] > frames[0]["seq"]]
    assert painted, f"no painted frame after the input: {frames}"
    status = pilcrow.status()
    last = painted[-1]
    assert last["revision"] == status["revision"], (
        f"trace settled on revision {last['revision']}, live is {status['revision']}"
    )
    assert last["layout_done"] is True
    assert last["caret_visible"] is True
    assert last["input_seq"] == 3, "the settled paint must include the last input"


def test_trace_stop_without_start_returns_no_frames(pilcrow):
    _require_trace(pilcrow)
    assert pilcrow.trace_stop() == []
