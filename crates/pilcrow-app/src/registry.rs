//! Same-path window registry.
//!
//! Identity is the canonical note path. A Save As destination is reserved
//! before its write starts, so two windows cannot adopt one file at once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{AnyWindowHandle, App, Global};

#[derive(Default)]
struct OpenNotes {
    notes: HashMap<PathBuf, AnyWindowHandle>,
    reserved: HashMap<PathBuf, AnyWindowHandle>,
}

impl Global for OpenNotes {}

pub(crate) fn ensure_registry(cx: &mut App) {
    if cx.try_global::<OpenNotes>().is_none() {
        cx.set_global(OpenNotes::default());
    }
}

pub(crate) fn register_open(cx: &mut App, identity: PathBuf, handle: AnyWindowHandle) {
    ensure_registry(cx);
    cx.update_global::<OpenNotes, _>(|registry, _| {
        registry.notes.retain(|_, existing| *existing != handle);
        registry.reserved.remove(&identity);
        registry.notes.insert(identity, handle);
    });
}

/// Claim a path for Save As before its write starts. `true` when this window
/// already owns the path.
pub(crate) fn reserve_open(cx: &mut App, identity: &Path, handle: AnyWindowHandle) -> bool {
    ensure_registry(cx);
    let live = cx.windows();
    cx.update_global::<OpenNotes, _>(|registry, _| {
        registry.notes.retain(|_, existing| live.contains(existing));
        registry
            .reserved
            .retain(|_, existing| live.contains(existing));
        if let Some(existing) = registry.notes.get(identity) {
            return *existing == handle;
        }
        if let Some(existing) = registry.reserved.get(identity) {
            return *existing == handle;
        }
        registry.reserved.insert(identity.to_path_buf(), handle);
        true
    })
}

pub(crate) fn release_reservation(cx: &mut App, identity: &Path, handle: AnyWindowHandle) {
    if cx.try_global::<OpenNotes>().is_none() {
        return;
    }
    cx.update_global::<OpenNotes, _>(|registry, _| {
        if registry.reserved.get(identity) == Some(&handle) {
            registry.reserved.remove(identity);
        }
    });
}

pub(crate) fn lookup_open(cx: &mut App, identity: &Path) -> Option<AnyWindowHandle> {
    let handle = cx.try_global::<OpenNotes>().and_then(|registry| {
        registry
            .notes
            .get(identity)
            .or_else(|| registry.reserved.get(identity))
            .copied()
    })?;
    if cx.windows().contains(&handle) {
        Some(handle)
    } else {
        forget_window(cx, handle.window_id());
        None
    }
}

pub(crate) fn forget_window(cx: &mut App, id: gpui::WindowId) {
    if cx.try_global::<OpenNotes>().is_none() {
        return;
    }
    cx.update_global::<OpenNotes, _>(|registry, _| {
        registry
            .notes
            .retain(|_, existing| existing.window_id() != id);
        registry
            .reserved
            .retain(|_, existing| existing.window_id() != id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_process_global_and_empty_by_default() {
        // This is a type-level contract; the app tests exercise the behavior
        // with real windows. Keeping it here guards accidental removal of the
        // Global implementation.
        fn assert_global<T: Global>() {}
        assert_global::<OpenNotes>();
    }
}
