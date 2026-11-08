//! The Pilcrow application shell.
//!
//! One note is one window. A window owns an [`EditorView`] and the small amount
//! of file state a note needs: its path, the text that is safely on disk, an
//! error worth showing, and the title last sent to the platform. Disk work for
//! explicit open, save, drafts, and quit runs off the foreground; only the
//! launch note is read synchronously before the event loop starts.
//!
//! The save path is strict: it never writes a live IME preedit, never overwrites
//! a file that changed underneath it without saying so, and never lets a
//! successful write be lost because the window or app closed first. Quit
//! journals dirty notes before the window list disappears, then waits and
//! writes the final bytes on a bounded thread.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, ClipboardItem, Context, Entity, ExternalPaths,
    Focusable, Global, QuitMode, Render, Subscription, Task, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, point, px, size,
};
use pilcrow_commands::app_actions::*;
use pilcrow_commands::keymap::{self, Platform};
use pilcrow_commands::menus::{self, MenuAppearance, MenuFocus, MenuMeasure, MenuOptions};
use pilcrow_core::{BlockKind, content_hash, export, scan_blocks};
use pilcrow_editor::{EditorEvent, EditorView, FocusScope};
use pilcrow_ui::theme::Tokens;

use file::DiskState;
use registry::{
    ensure_registry, forget_window, lookup_open, register_open, release_reservation, reserve_open,
};
use save::{ActiveSave, SaveFailure, SaveGuard, SaveRequest, SaveState};

static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(not(test))]
static PREFERENCES_GENERATION: AtomicU64 = AtomicU64::new(0);

pub mod app_identity;
pub mod backups;
pub mod drafts;
mod ffi;
pub mod file;
#[cfg(feature = "goldens")]
pub mod goldens;
pub mod prefs;
pub mod registry;
pub mod save;
pub mod session;
pub mod test_driver;

/// The app-wide appearance preference. Every note resolves its page tokens
/// from this and the window's current system appearance.
#[derive(Clone, Copy, Debug)]
struct Preferences {
    appearance: prefs::Appearance,
    text_scale: f32,
    measure: prefs::Measure,
    contrast: prefs::Contrast,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            appearance: prefs::Appearance::Auto,
            text_scale: 1.0,
            measure: prefs::Measure::Standard,
            contrast: prefs::Contrast::Standard,
        }
    }
}

impl Global for Preferences {}

#[derive(Clone, Debug)]
struct AppDataRoot(PathBuf);

impl Global for AppDataRoot {}

#[derive(Clone, Debug, Default)]
struct RecentFiles(Vec<PathBuf>);

impl Global for RecentFiles {}

#[derive(Clone, Copy, Debug, Default)]
struct ActiveEditorModes {
    focus: Option<MenuFocus>,
    typewriter: bool,
    reading: bool,
}

impl Global for ActiveEditorModes {}

fn current_recent(cx: &App) -> Vec<PathBuf> {
    cx.try_global::<RecentFiles>()
        .map(|recent| recent.0.clone())
        .unwrap_or_default()
}

fn remember_recent(cx: &mut App, path: &Path) {
    cx.add_recent_document(path);
    let mut recent = current_recent(cx);
    session::remember(&mut recent, path);
    cx.set_global(RecentFiles(recent));
    install_menus(cx);
}

fn app_data_root(cx: &App) -> PathBuf {
    cx.try_global::<AppDataRoot>()
        .map(|root| root.0.clone())
        .unwrap_or_else(prefs::data_root)
}

fn current_prefs(cx: &App) -> Preferences {
    cx.try_global::<Preferences>().copied().unwrap_or_default()
}

fn sync_note_tokens(cx: &mut App) {
    let windows = cx.windows();
    for handle in windows {
        let Some(note) = handle.downcast::<NoteWindow>() else {
            continue;
        };
        note.update(cx, |note, window, cx| note.sync_tokens(window, cx))
            .ok();
    }
}

fn set_prefs(cx: &mut App, next: Preferences) {
    if cx.try_global::<Preferences>().is_none() {
        cx.set_global(next);
    } else {
        cx.update_global::<Preferences, _>(|prefs, _| *prefs = next);
    }
    cx.refresh_windows();
    sync_note_tokens(cx);
    install_menus(cx);
    #[cfg(not(test))]
    {
        let saved = prefs::Prefs {
            appearance: next.appearance,
            text_scale: next.text_scale,
            measure: next.measure,
            contrast: next.contrast,
        };
        let generation = PREFERENCES_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
        let executor = cx.background_executor().clone();
        cx.background_executor()
            .spawn(async move {
                // Coalesce a rapid Auto → Light → Dark sequence into one write.
                executor.timer(Duration::from_millis(250)).await;
                if PREFERENCES_GENERATION.load(Ordering::Relaxed) != generation {
                    return;
                }
                if let Err(error) = prefs::save(saved) {
                    eprintln!("pilcrow: could not save settings: {error:#}");
                }
            })
            .detach();
    }
}

/// The file identity a note was created with.
struct NoteFile {
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
    disk: Option<DiskState>,
    failed_path: Option<PathBuf>,
    draft_id: Option<String>,
    recovered: bool,
    error: Option<String>,
}

/// Everything needed to apply a finished background write.
struct SaveCompletion {
    path: PathBuf,
    identity: Option<PathBuf>,
    handle: Option<AnyWindowHandle>,
    generation: u64,
    snapshot: Arc<str>,
    from_conflict: bool,
    autosave: bool,
    result: Result<(DiskState, Option<PathBuf>), SaveFailure>,
}

/// The launch read, before the event loop starts.
struct InitialNote {
    text: String,
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
    disk: Option<DiskState>,
    failed_path: Option<PathBuf>,
    error: Option<String>,
}

/// The disk half of a note: where it lives, the bytes last read or saved, and
/// the Save As reservation/draft identity. The editor owns the live document.
pub struct NoteDocument {
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
    /// The exact text that is on disk. Dirty is a content comparison, not a
    /// revision comparison: undoing back to the saved text is clean again.
    saved_text: Arc<str>,
    /// The content hash of `saved_text`, kept in step by
    /// [`NoteDocument::set_saved`] so a dirty check can answer "changed"
    /// without comparing every byte of a long note.
    saved_hash: u64,
    /// The path whose read failed. Save As must not turn an unreadable note
    /// into an empty file.
    failed_path: Option<PathBuf>,
    /// The stamp of the version this window last read or saved.
    disk: Option<DiskState>,
    /// The draft key for a pathless note. Cleared when a real path is adopted.
    draft_id: Option<String>,
    /// A note restored from the draft store; it starts dirty so closing asks
    /// for a path instead of silently discarding the recovered words.
    recovered: bool,
}

impl NoteDocument {
    /// Records `text` as the bytes on disk, together with their hash. Every
    /// change to the saved text goes through here so the two cannot drift.
    fn set_saved(&mut self, text: Arc<str>) {
        self.saved_hash = content_hash(&text);
        self.saved_text = text;
    }
}

/// One note window.
pub struct NoteWindow {
    editor: Entity<EditorView>,
    tokens: Tokens,
    document: NoteDocument,
    /// `(revision, dirty)` for the last full comparison.
    dirty_cache: Cell<Option<(u64, bool)>>,
    /// `(revision, title)` for the last derived title.
    title_cache: Cell<Option<(u64, String)>>,
    error: Option<String>,
    last_title: String,
    last_edited: Option<bool>,
    last_document_path: Option<Option<PathBuf>>,
    /// Set after a save found that the file changed underneath the window.
    /// The next explicit save is allowed to replace exactly `conflict_disk`,
    /// and only while the document is still the revision that hit the conflict.
    conflict: bool,
    conflict_disk: Option<DiskState>,
    conflict_revision: Option<u64>,
    save: SaveState,
    /// The window asked to close while dirty; finish the save, then close.
    close_after_save: bool,
    /// The document revision when close was requested. Any later edit cancels
    /// the close intent.
    close_revision: Option<u64>,
    handle: Option<AnyWindowHandle>,
    /// Kept alive so the OS appearance callback stays registered.
    appearance_observer: Option<Subscription>,
    autosave_task: Option<Task<()>>,
    autosave_revision: Option<u64>,
    /// The revision an autosave already failed on. A new edit retries; an
    /// unchanged dirty revision does not hammer a full disk or bad volume.
    autosave_failed_revision: Option<u64>,
    draft_task: Option<Task<()>>,
    draft_revision: Option<u64>,
    draft_first_dirty: Option<std::time::Instant>,
    autosave_first_dirty: Option<std::time::Instant>,
    /// Offered after a close-save prompt on an untitled note is cancelled, so
    /// the person can discard the note without saving a dummy file.
    offer_discard: bool,
    /// Two-step confirmation before permanently deleting an unsaved draft.
    confirm_discard: bool,
}

impl std::ops::Deref for NoteWindow {
    type Target = NoteDocument;

    fn deref(&self) -> &Self::Target {
        &self.document
    }
}

impl std::ops::DerefMut for NoteWindow {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.document
    }
}

impl NoteWindow {
    pub fn new(cx: &mut Context<Self>, text: &str, tokens: Tokens, path: Option<PathBuf>) -> Self {
        Self::with_state(
            cx,
            text,
            tokens,
            NoteFile {
                path,
                identity: None,
                disk: None,
                failed_path: None,
                draft_id: None,
                recovered: false,
                error: None,
            },
        )
    }

    fn with_state(cx: &mut Context<Self>, text: &str, tokens: Tokens, file: NoteFile) -> Self {
        let editor = cx.new(|cx| EditorView::new(cx, text, tokens));
        // Typed editor events, not observe-every-notify: only an
        // actual edit may schedule autosave/draft work, and only the three
        // changes the window chrome renders from (text, selection stats,
        // word count) re-render the window. Scroll, spell results, and
        // find-bar state repaint inside the editor alone.
        cx.subscribe(&editor, |note, editor, event, cx| {
            match event {
                EditorEvent::Edited { .. } => {
                    note.offer_discard = false;
                    note.confirm_discard = false;
                    note.schedule_autosave(cx);
                    note.schedule_draft(cx);
                }
                EditorEvent::ModeChanged => {
                    let view = editor.read(cx);
                    let focus = view.focus_enabled().then(|| match view.focus_scope() {
                        FocusScope::Paragraph => MenuFocus::Paragraph,
                        FocusScope::Sentence => MenuFocus::Sentence,
                    });
                    let typewriter = view.typewriter_enabled();
                    let reading = view.view_state().3;
                    cx.set_global(ActiveEditorModes {
                        focus,
                        typewriter,
                        reading,
                    });
                    install_menus(cx);
                }
                EditorEvent::SelectionChanged | EditorEvent::StatsChanged => {}
            }
            cx.notify();
        })
        .detach();
        let saved_text: Arc<str> = if file.recovered {
            Arc::from("")
        } else {
            Arc::from(text)
        };
        let saved_hash = content_hash(&saved_text);
        let document = NoteDocument {
            path: file.path,
            identity: file.identity,
            saved_text,
            saved_hash,
            failed_path: file.failed_path,
            disk: file.disk,
            draft_id: file.draft_id,
            recovered: file.recovered,
        };
        Self {
            editor,
            tokens,
            document,
            dirty_cache: Cell::new(if file.recovered {
                None
            } else {
                Some((0, false))
            }),
            title_cache: Cell::new(None),
            error: file.error,
            last_title: String::new(),
            last_edited: None,
            last_document_path: None,
            conflict: false,
            conflict_disk: None,
            conflict_revision: None,
            save: SaveState::default(),
            close_after_save: false,
            close_revision: None,
            handle: None,
            appearance_observer: None,
            autosave_task: None,
            autosave_revision: None,
            autosave_failed_revision: None,
            draft_task: None,
            draft_revision: None,
            draft_first_dirty: None,
            autosave_first_dirty: None,
            offer_discard: false,
            confirm_discard: false,
        }
    }

    pub fn editor(&self) -> &Entity<EditorView> {
        &self.editor
    }

    pub fn tokens(&self) -> Tokens {
        self.tokens
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn disk_state(&self) -> Option<DiskState> {
        self.disk
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The note's name: the filename stem when it has a path, otherwise the
    /// first meaningful source line. Empty notes are "Untitled".
    pub fn title(&self, cx: &App) -> String {
        let editor = self.editor.read(cx);
        let revision = editor.revision();
        if let Some((cached_revision, title)) = self.title_cache.take()
            && cached_revision == revision
        {
            self.title_cache.set(Some((cached_revision, title.clone())));
            return title;
        }
        let title = if let Some(path) = self.path.as_deref()
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            && !stem.trim().is_empty()
        {
            stem.trim().to_owned()
        } else {
            editor.untitled_title()
        };
        self.title_cache.set(Some((revision, title.clone())));
        title
    }

    /// Dirty is a comparison with the bytes that are on disk, cached per
    /// revision so render does not re-compare a long note every frame.
    ///
    /// The buffer's incremental content hash answers most checks: a
    /// different hash proves the bytes differ, with no scan. An equal hash
    /// may just be reordered lines, so only then are the bytes compared —
    /// dirty stays exact, and undoing back to the saved bytes is clean.
    pub fn is_dirty(&self, cx: &App) -> bool {
        let editor = self.editor.read(cx);
        let revision = editor.revision();
        if let Some((cached_revision, dirty)) = self.dirty_cache.get()
            && cached_revision == revision
        {
            return dirty;
        }
        let dirty = editor.content_hash() != self.saved_hash || editor.text() != &*self.saved_text;
        self.dirty_cache.set(Some((revision, dirty)));
        dirty
    }

    /// The complete platform title, including the quiet edited marker.
    pub fn window_title(&self, cx: &App) -> String {
        let mut title = self.title(cx);
        if self.is_dirty(cx) {
            title.push_str(" — Edited");
        }
        title
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        let focus_handle = self.editor.read(cx).focus_handle(cx);
        window.focus(&focus_handle, cx);
    }

    fn has_pending_save(&self) -> bool {
        self.save.in_flight || self.save.pending.is_some()
    }

    /// Commit a live composition before an explicit file action. A preedit is
    /// never bytes the note owns yet.
    fn commit_ime(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.commit_composition(window);
            cx.notify();
        });
    }

    fn new_note(&mut self, _: &NewNote, _window: &mut Window, cx: &mut Context<Self>) {
        open_note_window(cx, "", None, None, None, None, None, false, None);
    }

    fn open(&mut self, _: &Open, _window: &mut Window, cx: &mut Context<Self>) {
        open_note_dialog(cx);
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_ime(window, cx);
        if self.save.adopting
            || self
                .save
                .pending
                .as_ref()
                .is_some_and(|pending| pending.identity.is_some())
        {
            // The path is about to change; a write to the old one would be
            // wasted at best. Queue one save for the adopted path.
            self.save.save_after_adopt = true;
            return;
        }
        let Some(path) = self.path.clone() else {
            self.prompt_save_as(window, cx);
            return;
        };

        // The overwrite permit is bound to both the disk version that caused
        // the conflict and the document revision that saw it. Any edit makes
        // the next Save check the disk again.
        let revision = self.editor.read(cx).revision();
        if self.conflict && self.conflict_revision != Some(revision) {
            self.conflict = false;
            self.conflict_disk = None;
            self.conflict_revision = None;
        }
        self.autosave_failed_revision = None;
        self.autosave_revision = None;
        self.autosave_first_dirty = None;
        let from_conflict = self.conflict;
        let expected = if from_conflict {
            self.conflict_disk
        } else {
            self.disk
        };
        let clean = !self.is_dirty(cx);
        let mut request = SaveRequest::new(path, expected);
        request.from_conflict = from_conflict;
        request.skip_if_unchanged = clean && !from_conflict;
        self.queue_save(cx, request);
        self.pump_saves(cx);
    }

    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        if self.save.adopting
            || self
                .save
                .pending
                .as_ref()
                .is_some_and(|pending| pending.identity.is_some())
        {
            self.error =
                Some("Finish the current Save As before choosing another location.".into());
            cx.notify();
            return;
        }
        self.prompt_save_as(window, cx);
    }

    fn queue_save(&mut self, cx: &mut App, request: SaveRequest) {
        // Only the newest request matters: it snapshots the current text when
        // the in-flight write finishes. Release a replaced Save As claim so it
        // cannot block another window forever.
        if let Some(replaced) = self.save.replace_pending(request)
            && let (Some(identity), Some(handle)) = (replaced.identity, replaced.handle)
        {
            release_reservation(cx, &identity, handle);
        }
    }

    /// Start the oldest pending write if none is running. The snapshot is taken
    /// here, not when the request was queued, so a save always reflects the
    /// latest committed text.
    fn pump_saves(&mut self, cx: &mut Context<Self>) {
        if self.save.in_flight {
            return;
        }
        let Some(pending) = self.save.take_pending() else {
            return;
        };

        let text: Arc<str> = Arc::from(self.editor.read(cx).save_text());
        let expected = pending.expected;
        // Dirtiness can change while a write is in flight. Re-check it at the
        // moment the snapshot is taken; a queued clean Save must not skip a
        // later, different text.
        let skip_if_unchanged = pending.skip_if_unchanged && !self.is_dirty(cx);
        let adopting = pending.identity.is_some();
        let generation = self
            .save
            .begin(pending.path.clone(), text.clone(), adopting);

        let path = pending.path.clone();
        let identity = pending.identity.clone();
        let handle = pending.handle;
        let from_conflict = pending.from_conflict;
        let autosave = pending.autosave;
        let write_path = path.clone();
        let write_text = text.clone();
        let backup_root = (adopting || from_conflict).then(|| app_data_root(cx));
        let guard = SaveGuard::begin();
        let write = cx.background_executor().spawn(async move {
            let _guard = guard;
            if let Some(root) = backup_root
                && let Ok(previous) = file::read_bounded(&write_path, file::MAX_NOTE_BYTES)
                && !previous.is_empty()
            {
                let _ = backups::snapshot(&root, &write_path, &previous);
            }
            #[cfg(feature = "test-driver")]
            if let Some(millis) = std::env::var("PILCROW_TEST_SLOW_SAVE_MS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
            {
                std::thread::sleep(Duration::from_millis(millis.min(2_000)));
            }
            if skip_if_unchanged
                && let Some(expected) = expected
                && file::unchanged(&write_path, expected).unwrap_or(false)
            {
                return Ok((expected, None));
            }
            match file::write_atomic_checked(&write_path, write_text.as_bytes(), expected) {
                Ok(disk) => {
                    let canonical = if adopting {
                        file::identity(&write_path).ok()
                    } else {
                        None
                    };
                    Ok((disk, canonical))
                }
                Err(file::WriteFailure::Conflict(observed)) => Err(SaveFailure::Conflict(observed)),
                Err(file::WriteFailure::Failed(error)) => Err(SaveFailure::Failed(error)),
            }
        });

        let finish_path = path.clone();
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |note, cx| {
                note.finish_save(
                    SaveCompletion {
                        path: finish_path,
                        identity,
                        handle,
                        generation,
                        snapshot: text,
                        from_conflict,
                        autosave,
                        result,
                    },
                    cx,
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn finish_save(&mut self, completion: SaveCompletion, cx: &mut Context<Self>) {
        let SaveCompletion {
            path,
            identity,
            handle,
            generation,
            snapshot,
            from_conflict,
            autosave,
            result,
        } = completion;
        let Some(was_adopting) = self.save.finish(generation) else {
            return;
        };

        let adopting = identity.is_some();
        let mut adopted = false;
        match result {
            Ok((disk, canonical)) => {
                adopted = adopting;
                let root = app_data_root(cx);
                let backup_path = path.clone();
                let previous = self.saved_text.to_string();
                let backup_text = snapshot.clone();
                cx.background_executor()
                    .spawn(async move {
                        if !previous.is_empty()
                            && previous.as_str() != &*backup_text
                            && let Err(error) =
                                backups::snapshot(&root, &backup_path, previous.as_bytes())
                        {
                            eprintln!("pilcrow: could not record a backup: {error:#}");
                        }
                        if !backup_text.is_empty()
                            && let Err(error) =
                                backups::snapshot(&root, &backup_path, backup_text.as_bytes())
                        {
                            eprintln!("pilcrow: could not record a backup: {error:#}");
                        }
                    })
                    .detach();
                if let Some(identity) = identity {
                    let adopted_identity = canonical.unwrap_or(identity.clone());
                    self.path = Some(path.clone());
                    self.identity = Some(adopted_identity.clone());
                    self.recovered = false;
                    self.offer_discard = false;
                    self.confirm_discard = false;
                    self.draft_first_dirty = None;
                    if let Some(draft_id) = self.draft_id.take() {
                        let root = app_data_root(cx);
                        cx.background_executor()
                            .spawn(async move {
                                drafts::remove_from(&root, &draft_id);
                            })
                            .detach();
                    }
                    self.title_cache.set(None);
                    if let Some(handle) = handle {
                        if adopted_identity != identity {
                            release_reservation(cx, &identity, handle);
                        }
                        let owner = lookup_open(cx, &adopted_identity);
                        if owner.is_some_and(|owner| owner != handle) {
                            self.error = Some(format!(
                                "{} is already open in another window",
                                path.display()
                            ));
                        } else {
                            register_open(cx, adopted_identity, handle);
                        }
                    }
                }
                if self.path.as_deref() == Some(path.as_path()) {
                    self.disk = Some(disk);
                    // A request queued while this write was in flight carries
                    // the pre-write stamp. Rebind it to the bytes that just
                    // landed so its own checked write cannot false-conflict.
                    self.save.pending_expected(&path, disk);
                    self.set_saved(snapshot);
                    self.dirty_cache.set(None);
                    self.error = None;
                    self.conflict = false;
                    self.conflict_disk = None;
                    self.conflict_revision = None;
                    self.autosave_failed_revision = None;
                    self.autosave_first_dirty = None;
                }
            }
            Err(SaveFailure::Conflict(observed)) if !adopting => {
                self.conflict = true;
                self.conflict_disk = observed;
                self.conflict_revision = Some(self.editor.read(cx).revision());
                self.close_after_save = false;
                self.close_revision = None;
                self.error = Some(format!(
                    "{} changed on disk since it was opened. Save again to overwrite it.",
                    path.display()
                ));
                // A second Cmd+S that arrived while the first write was still
                // running must not silently become the overwrite. Drop it; the
                // person has to see the warning and press Save again.
                if self
                    .save
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.identity.is_none())
                {
                    self.save.pending = None;
                }
            }
            Err(SaveFailure::Conflict(_)) => {
                if let (Some(identity), Some(handle)) = (identity, handle) {
                    release_reservation(cx, &identity, handle);
                }
                self.close_after_save = false;
                self.close_revision = None;
                self.error = Some(format!(
                    "{} changed while saving. Nothing was written.",
                    path.display()
                ));
            }
            Err(SaveFailure::Failed(error)) => {
                if let (Some(identity), Some(handle)) = (identity, handle) {
                    release_reservation(cx, &identity, handle);
                }
                if autosave {
                    self.autosave_failed_revision = Some(self.editor.read(cx).revision());
                }
                self.error = Some(format!("Could not save {}: {error:#}", path.display()));
                self.close_after_save = false;
                self.close_revision = None;
            }
        }

        if from_conflict
            && !was_adopting
            && self
                .save
                .pending
                .as_ref()
                .is_some_and(|pending| pending.from_conflict && pending.identity.is_none())
        {
            // The second Save already did its job; a third queued request from
            // an impatient double-press must not run against the old stamp.
            self.save.pending = None;
        }

        if was_adopting && self.save.save_after_adopt {
            self.save.save_after_adopt = false;
            if adopted
                && self.path.is_some()
                && self.is_dirty(cx)
                && let Some(path) = self.path.clone()
            {
                self.queue_save(cx, SaveRequest::new(path, self.disk));
            }
        }
        self.pump_saves(cx);
        self.maybe_close(cx);
        self.schedule_autosave(cx);
        self.schedule_draft(cx);
    }

    fn prompt_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_ime(window, cx);
        let directory = default_save_directory(self.path.as_deref());
        let suggested_name = self.suggested_save_name(cx);
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested_name));
        let this_window = window.window_handle();
        let failed_path = self.failed_path.clone();

        cx.spawn_in(window, async move |this, cx| {
            let selected = match receiver.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => {
                    this.update(cx, |note, cx| {
                        if note.close_after_save && note.path.is_none() && note.is_dirty(cx) {
                            note.offer_discard = true;
                            note.confirm_discard = true;
                        }
                        note.close_after_save = false;
                        note.close_revision = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Ok(Err(error)) => {
                    this.update(cx, |note, cx| {
                        note.error = Some(format!("Could not choose a save location: {error}"));
                        note.close_after_save = false;
                        note.close_revision = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                Err(error) => {
                    this.update(cx, |note, cx| {
                        note.error = Some(format!("Could not choose a save location: {error}"));
                        note.close_after_save = false;
                        note.close_revision = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let identity_path = selected.clone();
            let failed_identity_path = failed_path.clone();
            let stamp_path = selected.clone();
            let (identity_result, failed_identity, target_stamp) = cx
                .background_executor()
                .spawn(async move {
                    let identity = file::identity(&identity_path);
                    let failed = failed_identity_path
                        .as_deref()
                        .and_then(|path| file::identity(path).ok());
                    let stamp = file::stamp(&stamp_path);
                    (identity, failed, stamp)
                })
                .await;
            let target_stamp = match target_stamp {
                Ok(stamp) => stamp,
                Err(error) => {
                    this.update(cx, |note, cx| {
                        note.error =
                            Some(format!("Could not examine {}: {error}", selected.display()));
                        note.close_after_save = false;
                        note.close_revision = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let identity = match identity_result {
                Ok(identity) => identity,
                Err(error) => {
                    this.update(cx, |note, cx| {
                        note.error =
                            Some(format!("Could not save {}: {error}", selected.display()));
                        note.close_after_save = false;
                        note.close_revision = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            if failed_identity.as_ref() == Some(&identity) {
                this.update(cx, |note, cx| {
                    note.error = Some(format!(
                        "{} could not be read when it was opened; refusing to replace it",
                        selected.display()
                    ));
                    note.close_after_save = false;
                    note.close_revision = None;
                    cx.notify();
                })
                .ok();
                return;
            }

            let reserved = cx
                .update(|_window, cx| reserve_open(cx, &identity, this_window))
                .unwrap_or(false);
            if !reserved {
                let existing = cx
                    .update(|_window, cx| lookup_open(cx, &identity))
                    .ok()
                    .flatten();
                if let Some(other) = existing {
                    cx.update(|_window, cx| {
                        focus_note(cx, other);
                    })
                    .ok();
                }
                this.update(cx, |note, cx| {
                    note.error = Some(format!(
                        "{} is already open in another window",
                        selected.display()
                    ));
                    note.close_after_save = false;
                    note.close_revision = None;
                    cx.notify();
                })
                .ok();
                return;
            }

            this.update(cx, |note, cx| {
                note.queue_save(
                    cx,
                    SaveRequest::adopting(selected, identity, this_window, target_stamp),
                );
                note.pump_saves(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Write a pathless note to the draft store after a short pause. A crash
    /// then loses at most the last few hundred milliseconds, not the note.
    fn schedule_draft(&mut self, cx: &mut Context<Self>) {
        let revision = self.editor.read(cx).revision();
        if self.draft_task.is_some() && self.draft_revision == Some(revision) {
            return;
        }
        self.draft_task = None;
        if self.path.is_some() || self.draft_id.is_none() || !self.is_dirty(cx) {
            self.draft_first_dirty = None;
            return;
        }
        if self.editor.read(cx).has_composition() {
            return;
        }
        let now = cx.background_executor().now();
        let first = *self.draft_first_dirty.get_or_insert(now);
        let remaining = (first + Duration::from_secs(5)).saturating_duration_since(now);
        let delay = Duration::from_millis(650).min(remaining);
        self.draft_revision = Some(revision);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |note, cx| note.draft_now(cx)).ok();
        });
        self.draft_task = Some(task);
    }

    fn draft_now(&mut self, cx: &mut Context<Self>) {
        self.draft_revision = None;
        self.draft_first_dirty = None;
        if self.path.is_some()
            || self.draft_id.is_none()
            || !self.is_dirty(cx)
            || self.editor.read(cx).has_composition()
        {
            return;
        }
        let text = self.editor.read(cx).save_text();
        let id = self.draft_id.clone().expect("checked above");
        let root = app_data_root(cx);
        let guard = save::SaveGuard::begin();
        cx.background_executor()
            .spawn(async move {
                let _guard = guard;
                if let Err(error) = drafts::save_to(&root, &id, &text) {
                    eprintln!("pilcrow: could not save draft: {error:#}");
                }
            })
            .detach();
    }

    /// Schedule one checked save after the typing pause. Autosave never
    /// replaces an explicit Save As, a conflict retry, or a write already
    /// waiting in the queue.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let revision = self.editor.read(cx).revision();
        // Stats refresh and selection notifications also notify the editor.
        // Only an actual revision change may restart the idle timer.
        if self.autosave_task.is_some() && self.autosave_revision == Some(revision) {
            return;
        }
        self.autosave_task = None;
        if self.path.is_none() || !self.is_dirty(cx) {
            self.autosave_first_dirty = None;
            return;
        }
        if self.conflict
            || self.save.adopting
            || self.save.pending.is_some()
            || self.autosave_failed_revision == Some(revision)
            || self.editor.read(cx).has_composition()
        {
            return;
        }
        let now = cx.background_executor().now();
        let first = *self.autosave_first_dirty.get_or_insert(now);
        let remaining = (first + Duration::from_secs(5)).saturating_duration_since(now);
        let delay = Duration::from_millis(1500).min(remaining);
        self.autosave_revision = Some(revision);
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |note, cx| note.autosave_now(cx)).ok();
        });
        self.autosave_task = Some(task);
    }

    fn autosave_now(&mut self, cx: &mut Context<Self>) {
        if self.path.is_none()
            || !self.is_dirty(cx)
            || self.conflict
            || self.has_pending_save()
            || self.editor.read(cx).has_composition()
        {
            return;
        }
        let path = self.path.clone().expect("checked above");
        self.queue_save(cx, SaveRequest::new(path, self.disk).autosave());
        self.pump_saves(cx);
    }

    fn cycle_appearance(
        &mut self,
        _: &CycleAppearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_appearance_now(window, cx);
    }

    fn on_appearance_click(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_appearance_now(window, cx);
    }

    fn discard_recovered(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_discard {
            self.confirm_discard = true;
            cx.notify();
            return;
        }
        if let Some(draft_id) = self.draft_id.take() {
            drafts::remove_from(&app_data_root(cx), &draft_id);
        }
        let current: Arc<str> = Arc::from(self.editor.read(cx).text());
        self.set_saved(current);
        self.dirty_cache.set(None);
        self.close_after_save = false;
        self.offer_discard = false;
        self.confirm_discard = false;
        window.remove_window();
    }

    fn keep_draft(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.offer_discard = false;
        self.confirm_discard = false;
        cx.notify();
        self.focus_editor(window, cx);
    }

    fn dismiss_error(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.conflict {
            self.error = None;
            cx.notify();
        }
        self.focus_editor(window, cx);
    }

    fn reload_conflict(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let read_path = path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { file::read(&read_path) })
                .await;
            this.update(cx, |note, cx| {
                match result {
                    Ok((text, disk)) => {
                        note.editor
                            .update(cx, |editor, cx| editor.replace_document(&text, cx));
                        note.set_saved(Arc::from(text));
                        note.dirty_cache.set(None);
                        note.disk = Some(disk);
                        note.conflict = false;
                        note.conflict_disk = None;
                        note.conflict_revision = None;
                        note.error = None;
                        note.close_after_save = false;
                        note.close_revision = None;
                    }
                    Err(error) => {
                        note.error = Some(format!("Could not reload {}: {error}", path.display()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn overwrite_conflict(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let expected = self.conflict_disk;
        self.queue_save(cx, SaveRequest::new(path, expected).conflict());
        self.pump_saves(cx);
    }

    fn save_as_conflict(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt_save_as(window, cx);
    }

    fn cycle_appearance_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut next = current_prefs(cx);
        next.appearance = next.appearance.next();
        set_prefs(cx, next);
        self.sync_tokens(window, cx);
        self.focus_editor(window, cx);
    }

    fn set_appearance_now(
        &mut self,
        appearance: prefs::Appearance,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = current_prefs(cx);
        next.appearance = appearance;
        set_prefs(cx, next);
        self.sync_tokens(window, cx);
    }

    fn appearance_auto(&mut self, _: &AppearanceAuto, window: &mut Window, cx: &mut Context<Self>) {
        self.set_appearance_now(prefs::Appearance::Auto, window, cx);
    }

    fn appearance_light(
        &mut self,
        _: &AppearanceLight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_appearance_now(prefs::Appearance::Light, window, cx);
    }

    fn appearance_dark(&mut self, _: &AppearanceDark, window: &mut Window, cx: &mut Context<Self>) {
        self.set_appearance_now(prefs::Appearance::Dark, window, cx);
    }

    fn toggle_high_contrast(
        &mut self,
        _: &ToggleHighContrast,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut next = current_prefs(cx);
        next.contrast = match next.contrast {
            prefs::Contrast::Standard => prefs::Contrast::High,
            prefs::Contrast::High => prefs::Contrast::Standard,
        };
        set_prefs(cx, next);
        self.sync_tokens(window, cx);
    }

    fn text_size_up(&mut self, _: &TextSizeUp, window: &mut Window, cx: &mut Context<Self>) {
        let next = prefs::text_scale_step(current_prefs(cx).text_scale, true);
        self.set_text_scale(next, window, cx);
    }

    fn text_size_down(&mut self, _: &TextSizeDown, window: &mut Window, cx: &mut Context<Self>) {
        let next = prefs::text_scale_step(current_prefs(cx).text_scale, false);
        self.set_text_scale(next, window, cx);
    }

    fn text_size_reset(&mut self, _: &TextSizeReset, window: &mut Window, cx: &mut Context<Self>) {
        self.set_text_scale(1.0, window, cx);
    }

    fn measure_narrow(&mut self, _: &MeasureNarrow, window: &mut Window, cx: &mut Context<Self>) {
        self.set_measure(prefs::Measure::Narrow, window, cx);
    }

    fn measure_standard(
        &mut self,
        _: &MeasureStandard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_measure(prefs::Measure::Standard, window, cx);
    }

    fn measure_wide(&mut self, _: &MeasureWide, window: &mut Window, cx: &mut Context<Self>) {
        self.set_measure(prefs::Measure::Wide, window, cx);
    }

    fn set_measure(&mut self, measure: prefs::Measure, window: &Window, cx: &mut Context<Self>) {
        let mut next = current_prefs(cx);
        next.measure = measure;
        set_prefs(cx, next);
        self.sync_tokens(window, cx);
    }

    fn set_text_scale(&mut self, scale: f32, window: &Window, cx: &mut Context<Self>) {
        let mut next = current_prefs(cx);
        next.text_scale = scale.clamp(0.75, 1.75);
        set_prefs(cx, next);
        self.sync_tokens(window, cx);
    }

    fn show_about(&mut self, _: &About, _window: &mut Window, _cx: &mut Context<Self>) {
        app_identity::show_about();
    }

    fn quit_app(&mut self, _: &QuitApp, _window: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        // Cmd-W must take the same path as the platform close button. Removing
        // the window directly skips the dirty prompt and pending save.
        if self.should_close(window, cx) {
            window.remove_window();
        }
    }

    fn open_recent_action(
        &mut self,
        action: &OpenRecent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_note_path(cx, action.path.clone());
    }

    fn next_note(&mut self, _: &NextNote, _window: &mut Window, cx: &mut Context<Self>) {
        cycle_note(cx, true);
    }

    fn previous_note(&mut self, _: &PreviousNote, _window: &mut Window, cx: &mut Context<Self>) {
        cycle_note(cx, false);
    }

    fn copy_html(&mut self, _: &CopyHtml, _window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title(cx);
        let html = export::document(self.editor.read(cx).text(), &title);
        cx.write_to_clipboard(ClipboardItem::new_string(html));
    }

    fn export_html(&mut self, _: &ExportHtml, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title(cx);
        let directory = default_save_directory(self.path.as_deref());
        let suggested = format!("{}.html", safe_file_stem(&title));
        let receiver = cx.prompt_for_new_path(&directory, Some(&suggested));
        let html = export::document(self.editor.read(cx).text(), &title);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let is_html = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
                });
            let identity = file::identity(&path).ok();
            let allowed = this
                .update(cx, |note, cx| {
                    if !is_html {
                        note.error = Some(
                            "Export writes an .html file; choose a path ending in .html."
                                .to_owned(),
                        );
                        cx.notify();
                        return false;
                    }
                    let already_open = identity
                        .as_ref()
                        .is_some_and(|identity| lookup_open(cx, identity).is_some());
                    if already_open || note.identity == identity {
                        note.error = Some(format!(
                            "Not exporting over the open note {}; choose another name.",
                            path.display()
                        ));
                        cx.notify();
                        return false;
                    }
                    true
                })
                .unwrap_or(false);
            if !allowed {
                return;
            }
            let write_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { file::write_atomic(&write_path, html.as_bytes()) })
                .await;
            this.update(cx, |note, cx| {
                note.error = result
                    .err()
                    .map(|error| format!("Could not export {}: {error}", path.display()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn suggested_save_name(&self, cx: &App) -> String {
        let title = self.title(cx);
        let cleaned = title
            .chars()
            .map(|character| match character {
                '/' | '\\' | ':' => '-',
                other => other,
            })
            .collect::<String>();
        let cleaned = cleaned.trim();
        if cleaned.is_empty() {
            "Untitled.md".to_owned()
        } else if cleaned.ends_with(".md") || cleaned.ends_with(".markdown") {
            cleaned.to_owned()
        } else {
            format!("{cleaned}.md")
        }
    }

    /// The platform asks whether the window may close. Dirty notes finish a
    /// save first; an untitled note asks where to put it. A failed, cancelled
    /// or conflicting save leaves the window open.
    fn should_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.handle = Some(window.window_handle());
        self.commit_ime(window, cx);
        if !self.has_pending_save() && !self.is_dirty(cx) {
            if self.path.is_none() {
                if let Some(draft_id) = self.draft_id.take() {
                    drafts::remove_from(&app_data_root(cx), &draft_id);
                }
                return true;
            }
            // A clean note still verifies its disk before closing: if the file
            // was deleted or replaced, the buffer must not disappear. The
            // writer skips a matching stamp, so an unchanged close does no I/O
            // beyond the check.
            self.close_after_save = true;
            let revision = self.editor.read(cx).revision();
            self.close_revision = Some(revision);
            let path = self.path.clone().expect("checked above");
            // A conflict permit is for an explicit Save, never for Close. A
            // clean buffer over an external edit must stay open and let the
            // person save deliberately.
            self.queue_save(cx, SaveRequest::new(path, self.disk).clean());
            self.pump_saves(cx);
            return false;
        }
        self.close_after_save = true;
        self.close_revision = Some(self.editor.read(cx).revision());
        if self.save.adopting
            || self
                .save
                .pending
                .as_ref()
                .is_some_and(|pending| pending.identity.is_some())
        {
            self.save.save_after_adopt = true;
            return false;
        }
        if self.has_pending_save() {
            // The existing write will finish; maybe_close decides whether one
            // more save is needed for edits made after it started.
            return false;
        }
        if let Some(path) = self.path.clone() {
            self.commit_ime(window, cx);
            self.queue_save(cx, SaveRequest::new(path, self.disk));
            self.pump_saves(cx);
        } else {
            self.prompt_save_as(window, cx);
        }
        false
    }

    /// Close only after the last requested write landed cleanly and the
    /// document still matches the revision the person asked to close.
    fn maybe_close(&mut self, cx: &mut Context<Self>) {
        if !self.close_after_save {
            return;
        }
        let revision = self.editor.read(cx).revision();
        if self.close_revision != Some(revision) {
            // An edit after the close request cancels it. A later Save must not
            // unexpectedly remove the window.
            self.close_after_save = false;
            self.close_revision = None;
            return;
        }
        if self.has_pending_save() {
            return;
        }
        if self.is_dirty(cx) {
            if let Some(path) = self.path.clone() {
                self.queue_save(cx, SaveRequest::new(path, self.disk));
                self.pump_saves(cx);
            } else {
                self.close_after_save = false;
                self.close_revision = None;
            }
            return;
        }
        self.close_after_save = false;
        self.close_revision = None;
        if let Some(handle) = self.handle {
            handle
                .update(cx, |_note, window, _cx| window.remove_window())
                .ok();
        }
    }

    /// Capture the latest committed text for the quit task. A pending Save As
    /// destination wins over the old path. No disk stamp is read here: a clean
    /// saved note is still captured so the background task can decide whether
    /// its file changed, and `journal` writes the recovery copy before the
    /// window list is cleared.
    fn quit_snapshot(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<QuitSnapshot> {
        self.commit_ime(window, cx);
        let dirty = self.is_dirty(cx);
        let pending = self.save.pending.take();
        let had_pending = pending.is_some();
        let active = self.save.active.as_ref().map(|active| ActiveSave {
            path: active.path.clone(),
            text: active.text.clone(),
        });
        let (path, disk) = match pending {
            Some(pending) => (Some(pending.path), pending.expected),
            None if self.save.adopting => (
                self.save.active.as_ref().map(|active| active.path.clone()),
                None,
            ),
            None => (self.path.clone(), self.disk),
        };
        let needs_write = dirty || had_pending || active.is_some();
        if !needs_write && path.is_none() {
            return None;
        }
        Some(QuitSnapshot {
            path,
            draft_id: self.draft_id.clone(),
            data_root: app_data_root(cx),
            text: self.editor.read(cx).snapshot().text,
            disk,
            dirty,
            needs_write,
            active,
            title: self.title(cx),
        })
    }
}

impl NoteWindow {
    /// Resolve the page palette outside draw. Appearance observers and the
    /// preference setter call this; render only reads the result.
    fn sync_tokens(&mut self, window: &Window, cx: &mut Context<Self>) {
        let want = prefs::tokens(
            prefs::Prefs {
                appearance: current_prefs(cx).appearance,
                text_scale: current_prefs(cx).text_scale,
                measure: current_prefs(cx).measure,
                contrast: current_prefs(cx).contrast,
            },
            window.appearance(),
        );
        if want != self.tokens {
            self.tokens = want;
            self.editor
                .update(cx, |editor, cx| editor.set_tokens(want, cx));
            cx.notify();
        }
        let scale = current_prefs(cx).text_scale;
        let measure = current_prefs(cx).measure.chars();
        self.editor.update(cx, |editor, cx| {
            editor.set_scale(scale, cx);
            editor.set_measure(measure, cx);
        });
    }
}

impl Render for NoteWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page_prefs = current_prefs(cx);
        let revision = self.editor.read(cx).revision();
        if self.conflict && self.conflict_revision != Some(revision) {
            // The person changed the document after the conflict. Do not carry
            // the old overwrite permit forward; the next Save checks again.
            self.conflict = false;
            self.conflict_disk = None;
            self.conflict_revision = None;
        }
        let title = self.title(cx);
        let dirty = self.is_dirty(cx);
        let desired = if dirty {
            format!("{title} — Edited")
        } else {
            title.clone()
        };

        // The platform title syncs here, in render, whichever typed event
        // (or save completion) brought us here. Seeding from
        // `window_title` keeps the title the window was opened with when
        // the platform (Linux) reports an empty string.
        if self.last_title.is_empty() {
            self.last_title = window.window_title();
        }
        if self.last_title != desired {
            window.set_window_title(&desired);
            self.last_title = desired;
        }
        if self.last_edited != Some(dirty) {
            window.set_window_edited(dirty);
            self.last_edited = Some(dirty);
        }
        if self.last_document_path.as_ref() != Some(&self.path) {
            window.set_document_path(self.path.as_deref());
            self.last_document_path = Some(self.path.clone());
        }

        let mut left = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w(px(0.0))
            .child(div().text_color(self.tokens.mid).child(title));
        if dirty {
            left = left.child(
                div()
                    .debug_selector(|| "note-status-dirty".into())
                    .text_color(self.tokens.dim)
                    .child(" — Edited"),
            );
        }
        if self.recovered {
            left = left.child(
                div()
                    .debug_selector(|| "note-status-recovered".into())
                    .text_color(self.tokens.accent)
                    .child("Recovered draft"),
            );
        }
        if self.recovered || (self.offer_discard && self.path.is_none() && dirty) {
            if self.confirm_discard {
                left = left
                    .child(
                        div()
                            .id("keep-draft")
                            .cursor_pointer()
                            .text_color(self.tokens.dim)
                            .child("Keep")
                            .on_click(cx.listener(Self::keep_draft)),
                    )
                    .child(
                        div()
                            .id("discard-draft")
                            .cursor_pointer()
                            .text_color(self.tokens.spell)
                            .child("Confirm discard")
                            .on_click(cx.listener(Self::discard_recovered)),
                    );
            } else {
                left = left.child(
                    div()
                        .id("discard-draft")
                        .cursor_pointer()
                        .text_color(self.tokens.dim)
                        .child("Discard")
                        .on_click(cx.listener(Self::discard_recovered)),
                );
            }
        }
        let editor = self.editor.read(cx);
        let stats = editor.selection_stats().map_or_else(
            || editor.stats().footer_label(),
            |selected| {
                format!(
                    "{} selected · {} · {}",
                    if selected.words == 1 {
                        "1 word".to_owned()
                    } else {
                        format!("{} words", selected.words)
                    },
                    if selected.chars == 1 {
                        "1 character".to_owned()
                    } else {
                        format!("{} characters", selected.chars)
                    },
                    selected.reading_label()
                )
            },
        );
        let stats = div()
            .debug_selector(|| "note-stats".into())
            .text_color(self.tokens.dim)
            .child(stats);
        let appearance = div()
            .id("appearance-toggle")
            .debug_selector(|| "appearance-toggle".into())
            .cursor_pointer()
            .text_color(self.tokens.dim)
            .child(page_prefs.appearance.label())
            .on_click(cx.listener(Self::on_appearance_click));

        let right = div()
            .flex()
            .items_center()
            .gap_3()
            .child(stats)
            .child(appearance);
        let status = div()
            .id("note-status")
            .debug_selector(|| "note-status".into())
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .h(px(24.0))
            .px(px(20.0))
            .text_size(px(11.0))
            .child(left)
            .child(right);

        let banner = self.error.clone().map(|error| {
            let mut row = div()
                .id("note-banner")
                .debug_selector(|| "note-banner".into())
                .flex()
                .items_center()
                .gap_3()
                .px(px(20.0))
                .py(px(8.0))
                .text_size(px(12.0))
                .bg(self.tokens.code_bg)
                .text_color(self.tokens.spell)
                .child(div().flex_1().child(error));
            if self.conflict {
                row = row
                    .child(
                        div()
                            .id("conflict-reload")
                            .cursor_pointer()
                            .text_color(self.tokens.accent)
                            .child("Reload")
                            .on_click(cx.listener(Self::reload_conflict)),
                    )
                    .child(
                        div()
                            .id("conflict-save-as")
                            .cursor_pointer()
                            .text_color(self.tokens.accent)
                            .child("Save As")
                            .on_click(cx.listener(Self::save_as_conflict)),
                    )
                    .child(
                        div()
                            .id("conflict-overwrite")
                            .cursor_pointer()
                            .text_color(self.tokens.spell)
                            .child("Overwrite")
                            .on_click(cx.listener(Self::overwrite_conflict)),
                    );
            } else {
                row = row.child(
                    div()
                        .id("banner-dismiss")
                        .cursor_pointer()
                        .text_color(self.tokens.dim)
                        .child("Dismiss")
                        .on_click(cx.listener(Self::dismiss_error)),
                );
            }
            row
        });

        div()
            .id("note-window")
            .key_context("NoteWindow")
            .size_full()
            .flex()
            .flex_col()
            .bg(self.tokens.paper)
            .on_drop::<ExternalPaths>(|paths, _window, cx| {
                for path in paths.paths() {
                    open_note_path(cx, path.clone());
                }
            })
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::show_about))
            .on_action(cx.listener(Self::quit_app))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::cycle_appearance))
            .on_action(cx.listener(Self::appearance_auto))
            .on_action(cx.listener(Self::appearance_light))
            .on_action(cx.listener(Self::appearance_dark))
            .on_action(cx.listener(Self::toggle_high_contrast))
            .on_action(cx.listener(Self::copy_html))
            .on_action(cx.listener(Self::export_html))
            .on_action(cx.listener(Self::open_recent_action))
            .on_action(cx.listener(Self::next_note))
            .on_action(cx.listener(Self::previous_note))
            .on_action(cx.listener(Self::text_size_up))
            .on_action(cx.listener(Self::text_size_down))
            .on_action(cx.listener(Self::text_size_reset))
            .on_action(cx.listener(Self::measure_narrow))
            .on_action(cx.listener(Self::measure_standard))
            .on_action(cx.listener(Self::measure_wide))
            .when_some(banner, |root, banner| root.child(banner))
            .child(
                div()
                    .id("note-page")
                    .debug_selector(|| "note-page".into())
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.editor.clone()),
            )
            .child(status)
    }
}

/// One note's final bytes at quit. The final write happens after every
/// in-flight save has drained, so it can tell an external edit from our own
/// write. The disk check and write run on a dedicated quit thread; the
/// recovery journal is written before the window list is cleared.
struct QuitSnapshot {
    path: Option<PathBuf>,
    draft_id: Option<String>,
    data_root: PathBuf,
    text: Arc<str>,
    disk: Option<DiskState>,
    dirty: bool,
    needs_write: bool,
    active: Option<ActiveSave>,
    title: String,
}

impl QuitSnapshot {
    /// The latest text first goes to a recovery file when the note is dirty.
    /// That file is removed only after a final write to the note succeeds, so a
    /// timeout or crash cannot leave the quit with nothing durable.
    /// Returns `(temporary recovery path, durable copy already exists)`.
    fn journal(&self) -> (Option<PathBuf>, bool) {
        if let Some(draft_id) = &self.draft_id
            && drafts::save_to(&self.data_root, draft_id, &self.text).is_ok()
        {
            return (None, true);
        }
        if !self.dirty {
            return (None, false);
        }
        (
            write_recovery(
                &self.text,
                &self.title,
                self.path.as_deref(),
                &self.data_root,
            ),
            false,
        )
    }

    /// A clean note whose on-disk bytes are no longer the version this window
    /// last saw. The stamp reads the file digest, so production calls this
    /// from the quit write task, never from the window update.
    fn disk_changed(&self) -> bool {
        !self.dirty
            && self
                .path
                .as_deref()
                .is_some_and(|path| match (self.disk, file::stamp(path)) {
                    (Some(expected), Ok(Some(current))) => !current.same_content(expected),
                    (None, Ok(None)) => false,
                    _ => true,
                })
    }

    /// Write the final bytes. Returns whether the note path now holds them.
    fn write(&self, drained: bool) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        if !drained {
            // An older write may still land after this one. Never race it.
            return false;
        }
        if !self.needs_write {
            // A clean note whose file vanished or changed only needs the
            // recovery copy; never overwrite the external version at quit.
            return false;
        }
        let mut expected = self.disk;
        if let Some(active) = &self.active
            && active.path == path
        {
            // After the drain, bytes equal to our own in-flight snapshot are
            // safe to replace with the final text. Keep the observed stamp so
            // an edit after this read is still caught by the checked write.
            if let Ok((on_disk, stamp)) = file::read(path)
                && on_disk.as_str() == &*active.text
            {
                expected = Some(stamp);
            }
        }
        match file::write_atomic_checked(path, self.text.as_bytes(), expected) {
            Ok(_) => true,
            Err(file::WriteFailure::Conflict(_)) => {
                eprintln!(
                    "pilcrow: {} changed on disk; keeping the recovery copy",
                    path.display()
                );
                false
            }
            Err(file::WriteFailure::Failed(error)) => {
                eprintln!(
                    "pilcrow: could not save {} on quit: {error:#}",
                    path.display()
                );
                false
            }
        }
    }
}

/// Leave most of GPUI's 200 ms shutdown window for the final target write.
/// A pending save that has not drained by then keeps its recovery journal.
#[cfg(not(test))]
const QUIT_SAVE_WAIT: Duration = Duration::from_millis(180);
/// Tests share the process-global save tracker and run in parallel; a short
/// production wait would make an unrelated test's guard look like a stuck save.
#[cfg(test)]
const QUIT_SAVE_WAIT: Duration = Duration::from_secs(5);

/// One note's captured text plus the recovery path to remove if the final
/// target write succeeds.
struct QuitWork {
    snapshot: QuitSnapshot,
    journal: Option<PathBuf>,
}

/// Capture every note and write the recovery journal for dirty ones. This is
/// the only quit work that must happen before GPUI clears the window list; the
/// disk stamp check, wait for in-flight saves, and final writes run on the
/// dedicated quit thread.
fn prepare_quit(cx: &mut App) -> (Vec<QuitWork>, Vec<String>) {
    let mut work = Vec::new();
    let mut clipboard = Vec::new();
    for handle in cx.windows() {
        let Some(note) = handle.downcast::<NoteWindow>() else {
            continue;
        };
        let Ok(Some(snapshot)) = note.update(cx, |note, window, cx| note.quit_snapshot(window, cx))
        else {
            continue;
        };
        if snapshot.dirty {
            let (journal, durable) = snapshot.journal();
            if !durable {
                // Even a full disk or read-only data directory should not make
                // the last copy disappear. Collect every note for one clipboard
                // fallback so two dirty windows cannot overwrite each other.
                clipboard.push(format!("--- {} ---\n{}\n", snapshot.title, snapshot.text));
            }
            work.push(QuitWork { snapshot, journal });
        } else {
            work.push(QuitWork {
                snapshot,
                journal: None,
            });
        }
    }
    (work, clipboard)
}

/// Finish quit work away from the foreground: wait for any explicit save that
/// was already in flight, then check clean notes for external changes and write
/// the final target bytes. A note whose final write cannot safely happen keeps
/// its recovery journal; a clean note whose file changed gets a recovery copy
/// but is never silently recreated.
fn finish_quit(work: Vec<QuitWork>) {
    // A quit with no dirty/pending note has nothing to drain and should not
    // spend the shutdown budget waiting on an unrelated writer.
    let needs_drain = work.iter().any(|work| work.snapshot.needs_write);
    let drained = !needs_drain || save::wait_idle_blocking(QUIT_SAVE_WAIT);
    if !drained {
        eprintln!("pilcrow: a save did not finish before quit; keeping recovery copies");
    }
    for QuitWork { snapshot, journal } in work {
        if !snapshot.dirty && !snapshot.needs_write {
            if snapshot.disk_changed() {
                let _ = write_recovery(
                    &snapshot.text,
                    &snapshot.title,
                    snapshot.path.as_deref(),
                    &snapshot.data_root,
                );
            }
            continue;
        }
        let wrote_target = snapshot.write(drained);
        if wrote_target && let Some(journal) = journal {
            let _ = fs::remove_file(journal);
        }
    }
}

fn write_clipboard_fallback(cx: &mut App, clipboard: Vec<String>) {
    if clipboard.is_empty() {
        return;
    }
    // Clipboard managers and Universal Clipboard can persist or sync the
    // contents. This last-resort copy is opt-in; recovery files are the
    // default.
    if std::env::var_os("PILCROW_RECOVERY_CLIPBOARD").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        cx.write_to_clipboard(ClipboardItem::new_string(clipboard.join("\n")));
        eprintln!("pilcrow: copied unsaved note text to the clipboard");
    } else {
        eprintln!(
            "pilcrow: recovery files failed; clipboard fallback is off                  (set PILCROW_RECOVERY_CLIPBOARD=1 to allow it)"
        );
    }
}

fn write_recovery(
    text: &str,
    title: &str,
    note_path: Option<&Path>,
    data_root: &Path,
) -> Option<PathBuf> {
    let name = recovery_name(title);
    let mut candidates = Vec::new();
    candidates.push(data_root.join("recovery"));
    candidates.push(std::env::temp_dir().join("pilcrow-next-recovery"));
    if let Some(parent) = note_path.and_then(Path::parent) {
        candidates.push(parent.to_path_buf());
    }
    if let Ok(current) = std::env::current_dir() {
        candidates.push(current);
    }

    for directory in candidates {
        if let Err(error) = fs::create_dir_all(&directory) {
            eprintln!(
                "pilcrow: could not create recovery directory {}: {error}",
                directory.display()
            );
            continue;
        }
        let path =
            if directory.ends_with("recovery") || directory.ends_with("pilcrow-next-recovery") {
                directory.join(&name)
            } else {
                // A sidecar next to the note is the fallback when the app data
                // directory is not writable.
                directory.join(format!(".{name}.recovery"))
            };
        if let Err(error) = file::write_atomic(&path, text.as_bytes()) {
            eprintln!(
                "pilcrow: could not write recovery copy {}: {error:#}",
                path.display()
            );
            continue;
        }
        eprintln!("pilcrow: wrote a recovery copy to {}", path.display());
        return Some(path);
    }
    eprintln!("pilcrow: could not preserve unsaved text at quit");
    None
}

fn recovery_name(title: &str) -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let name = title
        .chars()
        .filter(|character| character.is_alphanumeric() || matches!(character, ' ' | '-' | '_'))
        .take(48)
        .collect::<String>();
    let name = name.trim().replace(' ', "-");
    let name = if name.is_empty() {
        "untitled".to_owned()
    } else {
        name
    };
    let sequence = RECOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{stamp}-{}-{sequence}-{name}.md", std::process::id())
}

fn cycle_note(cx: &mut App, forward: bool) {
    let windows: Vec<AnyWindowHandle> = cx
        .windows()
        .into_iter()
        .filter(|handle| handle.downcast::<NoteWindow>().is_some())
        .collect();
    if windows.len() < 2 {
        return;
    }
    let active = cx.active_window();
    let index = active
        .and_then(|active| windows.iter().position(|handle| *handle == active))
        .unwrap_or(0);
    let target = if forward {
        (index + 1) % windows.len()
    } else {
        (index + windows.len() - 1) % windows.len()
    };
    let handle = windows[target];
    if let Some(note) = handle.downcast::<NoteWindow>() {
        note.update(cx, |note, window, cx| {
            window.activate_window();
            note.focus_editor(window, cx);
        })
        .ok();
    }
}

fn focus_note(cx: &mut App, handle: AnyWindowHandle) -> bool {
    let Some(handle) = handle.downcast::<NoteWindow>() else {
        return false;
    };
    handle
        .update(cx, |note, window, cx| {
            window.activate_window();
            note.focus_editor(window, cx);
        })
        .is_ok()
}

/// Window options restoring a saved frame when it still fits a connected
/// display, centered at the default size otherwise.
fn window_options_for(
    cx: &App,
    title: &str,
    frame: Option<(session::SavedFrame, bool)>,
) -> WindowOptions {
    let restored = frame.and_then(|(frame, maximized)| {
        let displays: Vec<session::DisplayRect> = cx
            .displays()
            .iter()
            .map(|display| {
                let visible = display.visible_bounds();
                session::DisplayRect {
                    x: visible.origin.x / px(1.0),
                    y: visible.origin.y / px(1.0),
                    width: visible.size.width / px(1.0),
                    height: visible.size.height / px(1.0),
                }
            })
            .collect();
        session::clamp_frame(frame, &displays).map(|frame| {
            let bounds = Bounds {
                origin: point(px(frame.x), px(frame.y)),
                size: size(px(frame.width), px(frame.height)),
            };
            if maximized {
                WindowBounds::Maximized(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }
        })
    });
    let bounds = restored.unwrap_or_else(|| {
        WindowBounds::Windowed(Bounds::centered(None, size(px(1000.0), px(720.0)), cx))
    });
    WindowOptions {
        window_bounds: Some(bounds),
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_owned().into()),
            ..Default::default()
        }),
        app_id: Some("dev.pilcrow.Pilcrow".to_owned()),
        // The measured column plus the left margin needs roughly 800pt at the
        // default size/measure. Keep the window from clipping a larger
        // reader-selected setting; the exact face-driven minimum comes with
        // packaging.
        window_min_size: Some(size(
            px(
                820.0 * current_prefs(cx).text_scale * current_prefs(cx).measure.chars() as f32
                    / 66.0,
            ),
            px(480.0 * current_prefs(cx).text_scale),
        )),
        ..Default::default()
    }
}

/// Where a Save As/Export dialog should start. A bundle launched from Finder
/// has `/` as its process working directory, so never use that as the default
/// destination. Prefer the note's real folder, then Documents, then home.
fn default_save_directory(note_path: Option<&Path>) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from));
    let cwd = std::env::current_dir().ok();
    choose_save_directory(note_path, home.as_deref(), cwd.as_deref())
}

fn choose_save_directory(
    note_path: Option<&Path>,
    home: Option<&Path>,
    cwd: Option<&Path>,
) -> PathBuf {
    if let Some(parent) = note_path.and_then(Path::parent)
        && !parent.as_os_str().is_empty()
        && parent != Path::new("/")
    {
        return parent.to_path_buf();
    }
    if let Some(home) = home {
        let documents = home.join("Documents");
        if documents.is_dir() {
            return documents;
        }
        if home.is_dir() {
            return home.to_path_buf();
        }
    }
    if let Some(cwd) = cwd
        && !cwd.as_os_str().is_empty()
        && cwd != Path::new("/")
    {
        return cwd.to_path_buf();
    }
    PathBuf::from(".")
}

/// A filesystem-safe name for export and Save As.
fn safe_file_stem(title: &str) -> String {
    let stem: String = title
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '\0' => '-',
            _ => ch,
        })
        .collect();
    let stem = stem.trim();
    if stem.is_empty() {
        "note".to_owned()
    } else {
        stem.to_owned()
    }
}

/// The note's display name. A path wins, because the file is the note's
/// identity on disk; an untitled note falls back to its first meaningful line.
fn note_title(text: &str, path: Option<&Path>) -> String {
    if let Some(path) = path
        && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
    {
        let stem = stem.trim();
        if !stem.is_empty() {
            return stem.to_owned();
        }
    }

    let mut blocks = Vec::new();
    scan_blocks(text, &mut blocks);
    for block in blocks {
        if matches!(
            block.kind,
            BlockKind::Frontmatter | BlockKind::Blank | BlockKind::Rule
        ) {
            continue;
        }
        let mut line = &text[block.start..block.end];
        if block.kind == BlockKind::Heading {
            line = &line[block.marker.min(line.len())..];
        }
        let line = line.trim();
        let line = line.lines().next().unwrap_or(line).trim();
        if !line.is_empty() {
            return line.to_owned();
        }
    }
    "Untitled".to_owned()
}

/// The frame to reopen `bounds` with. Fullscreen reopens windowed at its
/// restore size: relaunching into fullscreen because the last session
/// ended there is hostile.
fn saved_frame(bounds: WindowBounds) -> Option<(session::SavedFrame, bool)> {
    let (bounds, maximized) = match bounds {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        WindowBounds::Fullscreen(bounds) => (bounds, false),
    };
    let frame = session::SavedFrame {
        x: bounds.origin.x / px(1.0),
        y: bounds.origin.y / px(1.0),
        width: bounds.size.width / px(1.0),
        height: bounds.size.height / px(1.0),
    };
    (frame.width > 0.0 && frame.height > 0.0).then_some((frame, maximized))
}

fn capture_session(cx: &mut App) -> (PathBuf, session::Session) {
    let root = app_data_root(cx);
    let mut saved = session::Session {
        recent: current_recent(cx),
        ..session::Session::default()
    };
    for handle in cx.windows() {
        let Some(handle) = handle.downcast::<NoteWindow>() else {
            continue;
        };
        let Ok(captured) = handle.update(cx, |note, window, cx| {
            (
                note.path.clone(),
                note.editor.read(cx).view_state(),
                saved_frame(window.window_bounds()),
            )
        }) else {
            continue;
        };
        let (Some(path), (caret, anchor, scroll, reading, focus, typewriter), frame) = captured
        else {
            continue;
        };
        let (frame, maximized) = match frame {
            Some((frame, maximized)) => (Some(frame), maximized),
            None => (None, false),
        };
        let window = session::SavedWindow {
            path: path.clone(),
            caret,
            anchor,
            scroll,
            reading,
            focus,
            typewriter,
            frame,
            maximized,
        };
        session::remember(&mut saved.recent, &window.path);
        saved.windows.push(window);
    }
    (root, saved)
}

/// Save the paths and view positions of every open, titled note. No text is
/// written here: the file is still the only document.
fn persist_session(cx: &mut App) {
    let (root, saved) = capture_session(cx);
    if let Err(error) = session::save(&root, &saved) {
        eprintln!("pilcrow: could not save the session: {error:#}");
    }
}

fn persist_session_on_close(cx: &mut App) {
    #[cfg(test)]
    {
        persist_session(cx);
    }
    #[cfg(not(test))]
    {
        let (root, saved) = capture_session(cx);
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = session::save(&root, &saved) {
                    eprintln!("pilcrow: could not save the session: {error:#}");
                }
            })
            .detach();
    }
}

/// Reopen the last session's notes. Returns how many windows were restored.
/// Missing files are skipped; if none survive the caller opens an untitled
/// note, exactly like a first launch.
fn restore_session(cx: &mut App) -> usize {
    let root = app_data_root(cx);
    let session = session::load(&root);
    cx.set_global(RecentFiles(session.recent.clone()));
    let mut opened = 0;
    let mut seen = Vec::new();
    for saved in session.windows {
        let Ok((text, disk)) = file::read(&saved.path) else {
            continue;
        };
        let identity = file::identity(&saved.path).ok();
        let key = identity.clone().unwrap_or_else(|| saved.path.clone());
        if seen.contains(&key) {
            continue;
        }
        if identity
            .as_ref()
            .is_some_and(|identity| lookup_open(cx, identity).is_some())
        {
            continue;
        }
        seen.push(key);
        let path = saved.path.clone();
        let handle = open_note_window_framed(
            cx,
            &text,
            Some(path),
            identity,
            Some(disk),
            None,
            None,
            false,
            None,
            saved.frame.map(|frame| (frame, saved.maximized)),
        );
        let caret = saved.caret;
        let anchor = saved.anchor;
        let scroll = saved.scroll;
        let reading = saved.reading;
        let focus = saved.focus;
        let typewriter = saved.typewriter;
        handle
            .update(cx, |note, _, cx| {
                note.editor.update(cx, |editor, cx| {
                    editor
                        .restore_view_state(caret, anchor, scroll, reading, focus, typewriter, cx);
                });
            })
            .ok();
        opened += 1;
    }
    opened
}

/// Open a path from Open Recent without blocking the UI thread on disk.
fn open_note_path(cx: &mut App, path: PathBuf) {
    if let Ok(identity) = file::identity(&path)
        && let Some(existing) = lookup_open(cx, &identity)
        && focus_note(cx, existing)
    {
        return;
    }
    cx.spawn(async move |cx| {
        let read_path = path.clone();
        let identity = file::identity(&read_path).ok();
        let read = cx
            .background_executor()
            .spawn(async move { file::read(&read_path) })
            .await;
        cx.update(move |cx| {
            if let Some(identity) = identity.as_ref()
                && let Some(existing) = lookup_open(cx, identity)
                && focus_note(cx, existing)
            {
                return;
            }
            match read {
                Ok((text, disk)) => {
                    open_note_window(
                        cx,
                        &text,
                        Some(path),
                        identity,
                        Some(disk),
                        None,
                        None,
                        false,
                        None,
                    );
                }
                Err(error) => {
                    open_note_window(
                        cx,
                        "",
                        None,
                        None,
                        None,
                        Some(path.clone()),
                        None,
                        false,
                        Some(format!("Could not open {}: {error}", path.display())),
                    );
                }
            }
        });
    })
    .detach();
}

/// Decode a platform open URL into a note path. Only local file URLs and plain
/// paths are accepted; network schemes are ignored.
fn open_url_path(value: &str) -> Option<PathBuf> {
    let rest = if let Some(rest) = value.strip_prefix("file://") {
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        if !host.is_empty() && host != "localhost" {
            return None;
        }
        format!("/{path}")
    } else {
        if value.contains("://") {
            return None;
        }
        value.to_owned()
    };
    let rest = rest.as_str();
    let mut bytes_out = Vec::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                bytes_out.push(byte);
                index += 3;
                continue;
            }
        }
        bytes_out.push(bytes[index]);
        index += 1;
    }
    let decoded = String::from_utf8(bytes_out).ok()?;
    let decoded = if cfg!(target_os = "windows")
        && decoded.as_bytes().first() == Some(&b'/')
        && decoded.as_bytes().get(2) == Some(&b':')
    {
        decoded[1..].to_owned()
    } else {
        decoded
    };
    (!decoded.is_empty()).then(|| PathBuf::from(decoded))
}

/// The note named by `PILCROW_OPEN` or the first file argument. Linux desktop
/// launchers and `open -a` shims pass a path; macOS bundle open events still
/// need a platform hook.
fn launch_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PILCROW_OPEN")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        return Some(path);
    }
    let path = PathBuf::from(std::env::args_os().nth(1)?);
    if path.as_os_str().to_string_lossy().starts_with('-') {
        return None;
    }
    let is_markdown = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md")
                || extension.eq_ignore_ascii_case("markdown")
                || extension.eq_ignore_ascii_case("mdown")
                || extension.eq_ignore_ascii_case("mkd")
                || extension.eq_ignore_ascii_case("txt")
                || extension.eq_ignore_ascii_case("text")
        });
    is_markdown.then_some(path)
}

/// Read the launch note before the event loop starts.
fn initial_note() -> InitialNote {
    initial_note_from(launch_path())
}

fn initial_note_from(path: Option<PathBuf>) -> InitialNote {
    let Some(path) = path else {
        return InitialNote {
            text: String::new(),
            path: None,
            identity: None,
            disk: None,
            failed_path: None,
            error: None,
        };
    };
    let path = if path.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    } else {
        path
    };

    let existed = match fs::metadata(&path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };

    match file::read(&path) {
        Ok((text, disk)) => {
            let identity = file::identity(&path).ok();
            InitialNote {
                text,
                path: Some(path),
                identity,
                disk: Some(disk),
                failed_path: None,
                error: None,
            }
        }
        Err(error) => InitialNote {
            text: String::new(),
            path: None,
            identity: None,
            disk: None,
            // A missing file may be created by Save As; an existing file that
            // could not be read must be protected from replacement.
            failed_path: existed.then_some(path.clone()),
            error: Some(format!("Could not open {}: {error}", path.display())),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn open_note_window(
    cx: &mut App,
    text: &str,
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
    disk: Option<DiskState>,
    failed_path: Option<PathBuf>,
    draft_id: Option<String>,
    recovered: bool,
    error: Option<String>,
) -> WindowHandle<NoteWindow> {
    open_note_window_framed(
        cx,
        text,
        path,
        identity,
        disk,
        failed_path,
        draft_id,
        recovered,
        error,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_note_window_framed(
    cx: &mut App,
    text: &str,
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
    disk: Option<DiskState>,
    failed_path: Option<PathBuf>,
    draft_id: Option<String>,
    recovered: bool,
    error: Option<String>,
    frame: Option<(session::SavedFrame, bool)>,
) -> WindowHandle<NoteWindow> {
    let title = note_title(text, path.as_deref());
    let draft_id = if path.is_none() && draft_id.is_none() {
        Some(drafts::new_id())
    } else {
        draft_id
    };
    let recovered = recovered && draft_id.is_some();
    let note_path = path.clone();
    let note_identity = identity.clone();
    let note_disk = disk;
    #[cfg(feature = "test-driver")]
    let driver_note = std::rc::Rc::new(std::cell::RefCell::new(None::<Entity<NoteWindow>>));
    #[cfg(feature = "test-driver")]
    let driver_note_in_window = driver_note.clone();
    let window = cx
        .open_window(window_options_for(cx, &title, frame), |window, cx| {
            let page = current_prefs(cx);
            let tokens = prefs::tokens(
                prefs::Prefs {
                    appearance: page.appearance,
                    text_scale: page.text_scale,
                    measure: page.measure,
                    contrast: page.contrast,
                },
                window.appearance(),
            );
            let note = cx.new(|cx| {
                NoteWindow::with_state(
                    cx,
                    text,
                    tokens,
                    NoteFile {
                        path: note_path,
                        identity: note_identity,
                        disk: note_disk,
                        failed_path,
                        draft_id,
                        recovered,
                        error,
                    },
                )
            });
            let scale = current_prefs(cx).text_scale;
            let measure = current_prefs(cx).measure.chars();
            note.update(cx, |note, cx| {
                note.editor.update(cx, |editor, cx| {
                    editor.set_scale(scale, cx);
                    editor.set_measure(measure, cx);
                });
            });
            let handle = window.window_handle();
            let appearance_note = note.downgrade();
            let appearance_observer = window.observe_window_appearance(move |window, cx| {
                appearance_note
                    .update(cx, |note, cx| note.sync_tokens(window, cx))
                    .ok();
                cx.refresh_windows();
            });
            note.update(cx, |note, _| {
                note.handle = Some(handle);
                note.appearance_observer = Some(appearance_observer);
            });
            #[cfg(feature = "test-driver")]
            {
                *driver_note_in_window.borrow_mut() = Some(note.clone());
            }
            let weak = note.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                let Some(note) = weak.upgrade() else {
                    return true;
                };
                note.update(cx, |note, cx| note.should_close(window, cx))
            });
            let focus_handle = note.read(cx).editor.read(cx).focus_handle(cx);
            window.focus(&focus_handle, cx);
            note
        })
        .expect("open a Pilcrow note window");

    if let Some(identity) = identity {
        register_open(cx, identity, window.into());
    }
    if let Some(path) = path.as_deref() {
        remember_recent(cx, path);
    }

    #[cfg(feature = "test-driver")]
    if std::env::var_os("PILCROW_TEST_DRIVER").as_deref() == Some(std::ffi::OsStr::new("1"))
        && let Some(note) = driver_note.borrow().clone()
    {
        test_driver::install(cx, window.into(), note);
    }

    window
}

/// Ask for one note, read it in the background, and open or raise its window.
fn open_note_dialog(cx: &mut App) {
    let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Open a note".into()),
    });

    cx.spawn(async move |cx| {
        let path = match receiver.await {
            Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                Some(path) => path,
                None => return,
            },
            Ok(Ok(None)) => return,
            Ok(Err(error)) => {
                cx.update(|cx| {
                    open_note_window(
                        cx,
                        "",
                        None,
                        None,
                        None,
                        None,
                        None,
                        false,
                        Some(format!("Could not open a note: {error}")),
                    );
                });
                return;
            }
            Err(error) => {
                cx.update(|cx| {
                    open_note_window(
                        cx,
                        "",
                        None,
                        None,
                        None,
                        None,
                        None,
                        false,
                        Some(format!("Could not open a note: {error}")),
                    );
                });
                return;
            }
        };

        let read_path = path.clone();
        let (identity, read_result) = cx
            .background_executor()
            .spawn(async move {
                let identity = file::identity(&read_path).ok();
                let read = file::read(&read_path);
                (identity, read)
            })
            .await;

        let path_for_ui = path.clone();
        cx.update(move |cx| {
            if let Some(identity) = identity.as_ref()
                && let Some(existing) = lookup_open(cx, identity)
                && focus_note(cx, existing)
            {
                return;
            }

            match read_result {
                Ok((text, disk)) => {
                    open_note_window(
                        cx,
                        &text,
                        Some(path_for_ui),
                        identity,
                        Some(disk),
                        None,
                        None,
                        false,
                        None,
                    );
                }
                Err(error) => {
                    open_note_window(
                        cx,
                        "",
                        None,
                        None,
                        None,
                        Some(path_for_ui.clone()),
                        None,
                        false,
                        Some(format!("Could not open {}: {error}", path_for_ui.display())),
                    );
                }
            }
        });
    })
    .detach();
}

/// Open every draft left by a crash or an earlier untitled session. They stay
/// dirty and keep their draft ids, so closing them asks for a path unless the
/// person explicitly discards the recovery.
fn recover_drafts(cx: &mut App) {
    let root = app_data_root(cx);
    for draft in drafts::list_from(&root) {
        open_note_window(
            cx,
            &draft.text,
            None,
            None,
            None,
            None,
            Some(draft.id),
            true,
            None,
        );
    }
}

/// Install the native menu bar from the shared command model.
fn install_menus(cx: &mut App) {
    let appearance = match current_prefs(cx).appearance {
        prefs::Appearance::Auto => MenuAppearance::Auto,
        prefs::Appearance::Light => MenuAppearance::Light,
        prefs::Appearance::Dark => MenuAppearance::Dark,
    };
    let measure = match current_prefs(cx).measure {
        prefs::Measure::Narrow => MenuMeasure::Narrow,
        prefs::Measure::Standard => MenuMeasure::Standard,
        prefs::Measure::Wide => MenuMeasure::Wide,
    };
    let modes = cx
        .try_global::<ActiveEditorModes>()
        .copied()
        .unwrap_or_default();
    let options = MenuOptions::default()
        .with_appearance(appearance)
        .with_measure(measure)
        .with_high_contrast(current_prefs(cx).contrast == prefs::Contrast::High)
        .with_focus(modes.focus)
        .with_typewriter(modes.typewriter)
        .with_reading(modes.reading)
        .with_recent(recent_menu_labels(cx));
    cx.set_menus(menus::menus(&options));
    app_identity::set_app_menu_title("Pilcrow");
}

/// Install the app's file actions for the host platform.
pub fn install_keybindings(cx: &mut App) {
    let platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else {
        Platform::Other
    };
    install_keybindings_for(cx, platform);
}

/// Install the app's file actions for a specific platform dialect. Tests can
/// install either map on any host.
pub fn install_keybindings_for(cx: &mut App, platform: Platform) {
    keymap::install_app_keybindings_for(cx, platform);
}

/// Install the process-wide pieces once, before the first window opens.
pub fn install(cx: &mut App) {
    pilcrow_editor::install_fonts(cx);
    ensure_registry(cx);
    let root = cx
        .try_global::<AppDataRoot>()
        .map(|root| root.0.clone())
        .unwrap_or_else(|| {
            #[cfg(test)]
            {
                std::env::temp_dir().join(format!("pilcrow-app-tests-{}", std::process::id()))
            }
            #[cfg(not(test))]
            {
                prefs::data_root()
            }
        });
    file::cleanup_stale_temps(&root);
    file::cleanup_stale_temps(&root.join(drafts::DRAFTS_DIR));
    file::cleanup_stale_temps(&root.join(drafts::RECOVERY_DIR));
    cx.set_global(AppDataRoot(root.clone()));
    #[cfg(not(test))]
    let loaded = prefs::Prefs::with_env_override(prefs::load());
    #[cfg(test)]
    let loaded = prefs::Prefs::with_env_override(prefs::Prefs::default());
    cx.set_global(Preferences {
        appearance: loaded.appearance,
        text_scale: loaded.text_scale.clamp(0.75, 1.75),
        measure: loaded.measure,
        contrast: loaded.contrast,
    });
    cx.set_global(RecentFiles(session::load(&root).recent));
    install_keybindings(cx);
    install_menus(cx);
    cx.on_window_closed(|cx, id| {
        forget_window(cx, id);
        persist_session_on_close(cx);
    })
    .detach();
    // GPUI calls this before it clears the window list and gives an async
    // future only 200 ms. Capture the notes and journal dirty ones here; the
    // stamp checks, wait for an in-flight save, and final target writes run on
    // a dedicated thread. A write that misses the shutdown window leaves its
    // recovery copy behind rather than blocking the foreground.
    cx.on_app_quit(|cx| {
        persist_session(cx);
        let (work, clipboard) = prepare_quit(cx);
        write_clipboard_fallback(cx, clipboard);
        // A dedicated thread, not the background executor: an explicit save
        // still in flight may be a task on that executor, and a blocking wait
        // there would deadlock the test dispatcher and starve a small pool.
        // The deterministic test executor does not drive that channel, so
        // tests run the same work inline.
        #[cfg(test)]
        {
            finish_quit(work);
            async {}
        }
        #[cfg(not(test))]
        {
            let (done, finished) = async_channel::bounded(1);
            std::thread::spawn(move || {
                finish_quit(work);
                let _ = done.send_blocking(());
            });
            async move {
                let _ = finished.recv().await;
            }
        }
    })
    .detach();
    cx.on_action(|_: &NewNote, cx| {
        open_note_window(cx, "", None, None, None, None, None, false, None);
    });
    cx.on_action(|_: &Open, cx| open_note_dialog(cx));
    cx.on_action(|action: &OpenRecent, cx| open_note_path(cx, action.path.clone()));
    cx.on_action(|_: &NextNote, cx| cycle_note(cx, true));
    cx.on_action(|_: &PreviousNote, cx| cycle_note(cx, false));
    cx.on_action(|_: &About, _cx| app_identity::show_about());
    cx.on_action(|_: &QuitApp, cx| cx.quit());
}

fn close_startup_untitled_if_pristine(
    cx: &mut App,
    startup_blank: &mut Option<WindowHandle<NoteWindow>>,
) {
    let Some(blank) = startup_blank.take() else {
        return;
    };
    let is_pristine = blank
        .read_with(cx, |note, cx| {
            note.path.is_none()
                && !note.recovered
                && !note.is_dirty(cx)
                && !note.has_pending_save()
                && note.error.is_none()
                && note.editor.read(cx).text().is_empty()
        })
        .unwrap_or(false);
    if is_pristine {
        let _ = blank.update(cx, |note, window, cx| {
            if let Some(draft_id) = note.draft_id.take() {
                drafts::remove_from(&app_data_root(cx), &draft_id);
            }
            window.remove_window();
        });
    }
}

/// Launch the application.
pub fn run() {
    let has_open = std::env::var_os("PILCROW_OPEN").is_some_and(|path| !path.is_empty());
    let note = initial_note();
    let (url_sender, url_receiver) = async_channel::unbounded();
    let application = gpui_platform::application().with_quit_mode(QuitMode::LastWindowClosed);
    application.on_open_urls(move |urls| {
        let _ = url_sender.try_send(urls);
    });
    application.run(move |cx: &mut App| {
        app_identity::apply();
        install(cx);
        let mut opened_from_url = false;
        while let Ok(urls) = url_receiver.try_recv() {
            for url in urls {
                if let Some(path) = open_url_path(&url) {
                    open_note_path(cx, path);
                    opened_from_url = true;
                }
            }
        }
        let mut startup_blank = if has_open || (!opened_from_url && restore_session(cx) == 0) {
            let is_blank_fallback =
                !has_open && note.path.is_none() && note.text.is_empty() && note.error.is_none();
            let handle = open_note_window(
                cx,
                &note.text,
                note.path,
                note.identity,
                note.disk,
                note.failed_path,
                None,
                false,
                note.error,
            );
            is_blank_fallback.then_some(handle)
        } else {
            None
        };
        cx.spawn(async move |cx| {
            while let Ok(urls) = url_receiver.recv().await {
                cx.update(|cx| {
                    let mut opened_any = false;
                    for url in urls {
                        if let Some(path) = open_url_path(&url) {
                            open_note_path(cx, path);
                            opened_any = true;
                        }
                    }
                    if opened_any {
                        close_startup_untitled_if_pristine(cx, &mut startup_blank);
                    }
                });
            }
        })
        .detach();
        recover_drafts(cx);
        cx.activate(true);
    });
}

/// Items for File → Open Recent. An empty list is one disabled placeholder so
/// the submenu never appears broken.
fn recent_menu_labels(cx: &App) -> Vec<(String, PathBuf)> {
    current_recent(cx)
        .into_iter()
        .map(|path| {
            let label = path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            (label, path)
        })
        .collect()
}

#[cfg(test)]
mod tests;
