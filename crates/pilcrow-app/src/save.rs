//! One place that knows whether any note write is still in flight.
//!
//! Explicit saves run on the background executor. A window can close while a
//! write is still finishing, and the application can be asked to quit in that
//! same moment, so the process needs a count that is independent of any window
//! entity. The dedicated quit thread waits on this count, and the write itself
//! marks the guard done even if its window is already gone.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::AnyWindowHandle;

use crate::file::DiskState;

/// Why a background save did not replace the note.
#[derive(Debug)]
pub enum SaveFailure {
    /// The file on disk is no longer the version this window last read or
    /// saved. `None` means the file disappeared. The state is carried so an
    /// explicit overwrite can be bound to the version the person saw.
    Conflict(Option<DiskState>),
    /// The write failed for any other reason. The source keeps ENOSPC/EACCES
    /// and the rest of the chain available to diagnose and advise.
    Failed(anyhow::Error),
}

/// One explicit or automatic save request. Named fields replace the old
/// positional queue call, where five adjacent booleans were easy to transpose.
#[derive(Clone, Debug)]
pub(crate) struct SaveRequest {
    pub path: PathBuf,
    /// Present only for Save As: adopt this canonical identity on success.
    pub identity: Option<PathBuf>,
    /// The window that reserved `identity`, for release/register after the
    /// write even if the note entity no longer has a handle.
    pub handle: Option<AnyWindowHandle>,
    /// The exact disk version this write may replace. `None` means the note is
    /// new or the caller has no stamp, so an existing file conflicts.
    pub expected: Option<DiskState>,
    /// This write is the second Save after a conflict, bound to `expected`.
    pub from_conflict: bool,
    /// A clean Save may skip the write entirely when the stamp matches.
    pub skip_if_unchanged: bool,
    /// This write came from the idle autosave, not an explicit Save.
    pub autosave: bool,
}

impl SaveRequest {
    pub(crate) fn new(path: PathBuf, expected: Option<DiskState>) -> Self {
        Self {
            path,
            identity: None,
            handle: None,
            expected,
            from_conflict: false,
            skip_if_unchanged: false,
            autosave: false,
        }
    }

    pub(crate) fn adopting(
        path: PathBuf,
        identity: PathBuf,
        handle: AnyWindowHandle,
        expected: Option<DiskState>,
    ) -> Self {
        Self {
            identity: Some(identity),
            handle: Some(handle),
            ..Self::new(path, expected)
        }
    }

    pub(crate) fn autosave(mut self) -> Self {
        self.autosave = true;
        self
    }

    pub(crate) fn clean(mut self) -> Self {
        self.skip_if_unchanged = true;
        self
    }

    pub(crate) fn conflict(mut self) -> Self {
        self.from_conflict = true;
        self.skip_if_unchanged = false;
        self
    }
}

struct Tracker {
    pending: Mutex<usize>,
    done: Condvar,
}

impl Tracker {
    fn new() -> Self {
        Self {
            pending: Mutex::new(0),
            done: Condvar::new(),
        }
    }
}

fn tracker() -> &'static Tracker {
    static TRACKER: OnceLock<Tracker> = OnceLock::new();
    TRACKER.get_or_init(Tracker::new)
}

/// The write that is currently on the background executor. Kept so a quit can
/// tell whether the bytes now on disk are its own or an external edit.
#[derive(Clone, Debug)]
pub(crate) struct ActiveSave {
    pub path: PathBuf,
    pub text: Arc<str>,
}

/// One note's save state machine: at most one write in flight, at most one
/// coalesced replacement pending, and a generation so stale completions lose.
///
/// One pending slot is a decision, not an oversight. A request carries
/// no text: the snapshot is taken when the write starts, so for a given path
/// a newer request always writes at least what an older one would have, and
/// replacing it cannot lose words. The cases where replacing would lose an
/// *intent* are each handled where they arise instead of by queueing:
///
/// - autosave never queues over a pending request, a conflict, or an
///   adoption — it bails and reschedules;
/// - a plain Save while a Save As is landing sets `save_after_adopt`, so the
///   write retargets to the adopted path instead of being dropped;
/// - the Save As action refuses with a visible error while one is landing;
///   a Save As prompt that still completes queues behind the in-flight
///   write and adopts in turn, and a replaced pending adoption releases
///   its path reservation;
/// - close waits for the pending write instead of competing with it, and
///   quit takes the pending request into its synchronous flush.
///
/// A multi-intent queue would have to order writes bound to different disk
/// stamps against each other, which is strictly harder to keep conflict-safe
/// than latest-intent-wins. Drafts already have their own writer and are
/// only counted here through [`SaveGuard`] so quit can wait for them.
#[derive(Default, Debug)]
pub(crate) struct SaveState {
    pub in_flight: bool,
    pub active: Option<ActiveSave>,
    /// True while the in-flight write is a Save As adoption.
    pub adopting: bool,
    pub pending: Option<SaveRequest>,
    /// A plain Save arrived while a Save As was still landing. Apply it to the
    /// adopted path afterward instead of the old one.
    pub save_after_adopt: bool,
    /// Issued for every write, so a stale completion can never win.
    pub generation: u64,
    pub applied: u64,
}

impl SaveState {
    pub(crate) fn replace_pending(&mut self, request: SaveRequest) -> Option<SaveRequest> {
        self.pending.replace(request)
    }

    pub(crate) fn take_pending(&mut self) -> Option<SaveRequest> {
        self.pending.take()
    }

    pub(crate) fn begin(&mut self, path: PathBuf, text: Arc<str>, adopting: bool) -> u64 {
        self.in_flight = true;
        self.active = Some(ActiveSave { path, text });
        self.adopting = adopting;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Returns `(is_current, was_adopting)`. A completion older than the last
    /// applied generation is stale and must not touch the note.
    pub(crate) fn finish(&mut self, generation: u64) -> Option<bool> {
        if generation <= self.applied {
            return None;
        }
        self.applied = generation;
        self.in_flight = false;
        self.active = None;
        let was_adopting = self.adopting;
        self.adopting = false;
        Some(was_adopting)
    }

    pub(crate) fn pending_expected(&mut self, path: &Path, stamp: DiskState) {
        if let Some(pending) = self.pending.as_mut()
            && pending.path == path
            && pending.identity.is_none()
        {
            pending.expected = Some(stamp);
        }
    }
}

/// Marks one background write as in flight until it is dropped.
pub struct SaveGuard {
    _private: (),
}

impl SaveGuard {
    /// Counts a new write. Call this before spawning the write.
    pub fn begin() -> Self {
        let mut pending = tracker()
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending += 1;
        Self { _private: () }
    }
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        let mut pending = tracker()
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = pending.saturating_sub(1);
        tracker().done.notify_all();
    }
}

/// Wait until every save that was started before this call has finished.
///
/// This is blocking by design and runs on the dedicated quit thread, not on the
/// foreground or the background executor: an explicit save may itself be a
/// task on that executor, so blocking there would wait on ourselves. A bounded
/// wait keeps a hung network volume from outliving GPUI's shutdown window; the
/// caller falls back to the recovery journal when it expires.
pub fn wait_idle_blocking(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut pending = tracker()
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while *pending > 0 {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let (next, result) = tracker()
            .done
            .wait_timeout(pending, deadline - now)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending = next;
        if result.timed_out() && *pending > 0 {
            return false;
        }
    }
    true
}

/// How many writes are in flight. Used by tests.
pub fn pending() -> usize {
    *tracker()
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_state_ignores_stale_generations_and_binds_queued_stamps() {
        let mut state = SaveState::default();
        let first = state.begin(PathBuf::from("a.md"), Arc::from("one"), false);
        state.pending = Some(SaveRequest::new(
            PathBuf::from("a.md"),
            Some(DiskState {
                size: 0,
                mtime: std::time::UNIX_EPOCH,
                digest: 0,
            }),
        ));
        let stamp = DiskState {
            size: 3,
            mtime: std::time::UNIX_EPOCH,
            digest: 7,
        };
        state.pending_expected(Path::new("a.md"), stamp);
        assert_eq!(state.pending.as_ref().unwrap().expected, Some(stamp));
        assert_eq!(state.finish(first), Some(false));
        assert_eq!(state.finish(first), None);
        assert!(!state.in_flight);
    }

    #[test]
    fn a_blocking_wait_returns_after_the_guard_drops() {
        let guard = SaveGuard::begin();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(guard);
        });
        assert!(wait_idle_blocking(Duration::from_secs(2)));
        assert_eq!(pending(), 0);
    }

    #[test]
    fn a_blocking_wait_times_out_with_a_live_guard() {
        let guard = SaveGuard::begin();
        assert!(!wait_idle_blocking(Duration::from_millis(20)));
        drop(guard);
        assert!(wait_idle_blocking(Duration::from_secs(1)));
    }
}
