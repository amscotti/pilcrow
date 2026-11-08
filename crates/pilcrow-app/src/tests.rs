use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyWindowHandle, EntityInputHandler, TestAppContext, VisualTestContext};

use super::*;

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let base = std::env::temp_dir();
        loop {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(
                "pilcrow-app-test-{}-{}-{id}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("could not create {}: {error}", path.display()),
            }
        }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open_window(
    cx: &mut TestAppContext,
    text: &str,
    path: Option<PathBuf>,
    identity: Option<PathBuf>,
) -> WindowHandle<NoteWindow> {
    let disk = path
        .as_deref()
        .and_then(|path| file::stamp(path).ok().flatten());
    cx.update(|cx| {
        install(cx);
        open_note_window(cx, text, path, identity, disk, None, None, false, None)
    })
}

#[gpui::test]
fn title_and_dirty_are_derived_from_the_path_and_revision(cx: &mut TestAppContext) {
    let window = open_window(cx, "# A real heading\n\nbody", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    assert_eq!(
        note.read_with(&cx, |note, cx| note.title(cx)),
        "A real heading"
    );
    assert!(!note.read_with(&cx, |note, cx| note.is_dirty(cx)));
    assert_eq!(
        note.read_with(&cx, |note, cx| note.window_title(cx)),
        "A real heading"
    );

    cx.simulate_input("x");
    assert!(
        note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "typing must make the note dirty"
    );
    assert_eq!(
        note.read_with(&cx, |note, cx| note.title(cx)),
        "x# A real heading",
        "typing at the start changes the first meaningful line"
    );
    assert!(
        note.read_with(&cx, |note, cx| note.window_title(cx))
            .ends_with(" — Edited"),
        "the window title carries the edited marker"
    );
    assert_eq!(
        cx.window_title().as_deref(),
        Some("x# A real heading — Edited"),
        "render pushes the derived title to the platform"
    );

    let path = PathBuf::from("/tmp/research notes.md");
    let titled = cx.update(|_window, cx| {
        open_note_window(
            cx,
            "body text",
            Some(path),
            None,
            None,
            None,
            None,
            false,
            None,
        )
    });
    let titled = titled.root(&mut cx).expect("titled note root");
    assert_eq!(
        titled.read_with(&cx, |note, cx| note.title(cx)),
        "research notes",
        "a note with a path takes its filename stem"
    );

    let untitled = cx
        .update(|_window, cx| open_note_window(cx, "", None, None, None, None, None, false, None));
    let untitled = untitled.root(&mut cx).expect("untitled note root");
    assert_eq!(
        untitled.read_with(&cx, |note, cx| note.title(cx)),
        "Untitled"
    );
}

#[test]
fn title_skips_frontmatter_rules_and_blank_lines() {
    assert_eq!(note_title("# A short note\n\nbody", None), "A short note");
    assert_eq!(note_title("\n---\n\nbody", None), "body");
    assert_eq!(
        note_title("---\ntitle: Private\n---\n\n# A real heading", None),
        "A real heading"
    );
    assert_eq!(note_title("#nope first", None), "#nope first");
    assert_eq!(note_title("", None), "Untitled");
}

#[test]
fn startup_without_a_path_is_an_empty_untitled_note() {
    let note = initial_note_from(None);
    assert_eq!(note.text, "");
    assert_eq!(note.path, None);
    assert_eq!(note.identity, None);
    assert_eq!(note.disk, None);
    assert_eq!(note.failed_path, None);
    assert_eq!(note.error, None);
}

#[test]
fn startup_reads_the_configured_note() {
    let dir = TempDir::new();
    let path = dir.join("Launch.md");
    file::write_atomic(&path, b"# Launch\n\nbody").expect("fixture");

    let note = initial_note_from(Some(path.clone()));
    assert_eq!(note.text, "# Launch\n\nbody");
    assert_eq!(note.path, Some(path.clone()));
    assert_eq!(note.identity, file::identity(&path).ok());
    assert!(
        note.disk.is_some(),
        "the launch read keeps its on-disk stamp"
    );
    assert_eq!(note.failed_path, None);
    assert_eq!(note.error, None);
}

#[test]
fn startup_failures_do_not_pretend_the_note_was_empty() {
    let dir = TempDir::new();
    let missing = dir.join("missing.md");
    let note = initial_note_from(Some(missing.clone()));
    assert_eq!(note.text, "");
    assert_eq!(
        note.path, None,
        "an unreadable path is not adopted for saving"
    );
    assert_eq!(
        note.failed_path, None,
        "a missing file may be created by Save As"
    );
    let error = note.error.expect("missing file is visible");
    assert!(error.contains("Could not open"), "{error}");
    assert!(error.contains("missing.md"), "{error}");

    let invalid = dir.join("invalid.md");
    fs::write(&invalid, [0xff, 0xfe]).expect("fixture");
    let note = initial_note_from(Some(invalid.clone()));
    assert_eq!(note.text, "");
    assert_eq!(note.path, None);
    assert_eq!(note.failed_path, Some(invalid));
    assert!(
        note.error
            .expect("invalid UTF-8 is visible")
            .contains("UTF-8"),
        "the invalid encoding is named"
    );
}

#[gpui::test]
fn new_note_opens_a_blank_window_with_no_path(cx: &mut TestAppContext) {
    let first = open_window(cx, "existing", None, None);
    let first_id = AnyWindowHandle::from(first).window_id();
    let mut cx = VisualTestContext::from_window(first.into(), cx);

    let chord = if cfg!(target_os = "macos") {
        "cmd-n"
    } else {
        "ctrl-n"
    };
    cx.simulate_keystrokes(chord);

    let windows = cx.read(|cx| cx.windows());
    assert_eq!(windows.len(), 2, "New note opens another window");
    let opened = windows
        .into_iter()
        .find(|handle| handle.window_id() != first_id)
        .expect("the new note window");
    let (text, path, title, dirty) = opened
        .read::<NoteWindow, _, _>(&cx, |note, cx| {
            let note = note.read(cx);
            (
                note.editor().read(cx).text().to_owned(),
                note.path().map(Path::to_path_buf),
                note.title(cx),
                note.is_dirty(cx),
            )
        })
        .expect("read the new note");
    assert_eq!(text, "");
    assert_eq!(path, None);
    assert_eq!(title, "Untitled");
    assert!(!dirty);
}

#[gpui::test]
fn save_writes_the_snapshot_and_marks_the_note_clean(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("note.md");
    file::write_atomic(&path, b"hello").expect("fixture");
    let identity = file::identity(&path).expect("identity");

    let window = open_window(cx, "hello", Some(path.clone()), Some(identity));
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));

    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);

    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "a successful save must derive clean"
    );
    assert_eq!(
        note.read_with(&cx, |note, _| note.error().map(str::to_owned)),
        None
    );
    assert!(
        note.read_with(&cx, |note, _| note.disk_state().is_some()),
        "save records the disk stamp"
    );

    let (saved, _) = file::read(&path).expect("read saved note");
    assert_eq!(saved, "!hello");
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).text().to_owned()),
        saved
    );
}

#[gpui::test]
fn a_failed_save_keeps_the_note_dirty_and_shows_the_error(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("missing").join("note.md");
    let window = open_window(cx, "hello", Some(path.clone()), None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    cx.dispatch_action(Save);

    assert!(
        note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "a failed write must never mark the note clean"
    );
    let error = note
        .read_with(&cx, |note, _| note.error().map(str::to_owned))
        .expect("the failure is visible");
    assert!(error.contains("Could not save"), "{error}");
    assert!(!path.exists());
}

#[gpui::test]
fn open_reads_a_fixture_and_opens_a_new_window(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("fixture.md");
    file::write_atomic(&path, b"# Fixture\n\ntext from disk\n").expect("fixture");

    let first = open_window(cx, "", None, None);
    let mut cx = VisualTestContext::from_window(first.into(), cx);
    assert_eq!(cx.read(|cx| cx.windows().len()), 1);

    cx.dispatch_action(Open);
    assert!(cx.did_prompt_for_paths(), "Open must ask for a note");
    cx.simulate_path_prompt_response(|_| Some(vec![path.clone()]));
    cx.run_until_parked();

    let windows = cx.read(|cx| cx.windows());
    assert_eq!(windows.len(), 2, "opening a different note adds a window");
    let opened = windows
        .into_iter()
        .find(|handle| handle.window_id() != first.window_id())
        .expect("the new note window");

    let (text, title, note_path, error) = opened
        .read::<NoteWindow, _, _>(&cx, |note, cx| {
            let note = note.read(cx);
            (
                note.editor().read(cx).text().to_owned(),
                note.title(cx),
                note.path().map(Path::to_path_buf),
                note.error().map(str::to_owned),
            )
        })
        .expect("read opened note");
    assert_eq!(text, "# Fixture\n\ntext from disk\n");
    assert_eq!(title, "fixture");
    assert_eq!(note_path, Some(path));
    assert_eq!(error, None);
}

#[gpui::test]
fn opening_the_same_path_raises_the_existing_window(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("same.md");
    file::write_atomic(&path, b"same note").expect("fixture");
    let identity = file::identity(&path).expect("identity");

    let first = open_window(cx, "same note", Some(path.clone()), Some(identity));
    let mut cx = VisualTestContext::from_window(first.into(), cx);

    cx.dispatch_action(Open);
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec![path]));
    cx.run_until_parked();

    assert_eq!(
        cx.read(|cx| cx.windows().len()),
        1,
        "a canonical path must not open twice"
    );
}

#[gpui::test]
fn save_without_a_path_prompts_and_adopts_the_chosen_path(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let destination = dir.join("saved note.md");

    let window = open_window(cx, "hello", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    cx.dispatch_action(Save);
    assert!(
        cx.did_prompt_for_new_path(),
        "an untitled note must ask where to save"
    );

    cx.simulate_new_path_selection(|_directory| Some(destination.clone()));
    cx.run_until_parked();

    let (saved, _) = file::read(&destination).expect("read saved note");
    assert_eq!(saved, "!hello");
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "adopting a new path still marks the written revision clean"
    );
    assert_eq!(
        note.read_with(&cx, |note, _| note.path().map(Path::to_path_buf)),
        Some(destination)
    );
    assert_eq!(note.read_with(&cx, |note, cx| note.title(cx)), "saved note");
    assert_eq!(
        note.read_with(&cx, |note, _| note.error().map(str::to_owned)),
        None
    );
}

#[gpui::test]
fn a_save_during_adoption_lands_on_the_adopted_path(cx: &mut TestAppContext) {
    // the single pending slot must not lose a plain Save whose path is
    // about to change. It is retargeted to the adopted path instead.
    let dir = TempDir::new();
    let destination = dir.join("adopted.md");

    let window = open_window(cx, "hello", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    cx.dispatch_action(Save);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_directory| Some(destination.clone()));
    // The adopting write is queued or in flight now. Type more and save
    // again before anything is parked: the second Save must not write to a
    // stale path or vanish.
    cx.simulate_input("?");
    cx.dispatch_action(Save);
    cx.run_until_parked();

    let (saved, _) = file::read(&destination).expect("read adopted note");
    assert_eq!(saved, "!?hello");
    assert_eq!(
        note.read_with(&cx, |note, _| note.path().map(Path::to_path_buf)),
        Some(destination)
    );
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "the retargeted save must leave the note clean"
    );
}

#[gpui::test]
fn a_replaced_pending_adoption_releases_its_reservation(cx: &mut TestAppContext) {
    // the other half of latest-intent-wins. A pending Save As replaced
    // by a newer one (Save with no path prompts again) must free the first
    // destination's reservation instead of wedging it forever.
    let dir = TempDir::new();
    let first = dir.join("first.md");
    let second = dir.join("second.md");
    file::write_atomic(&first, b"").expect("fixture first");
    file::write_atomic(&second, b"").expect("fixture second");
    let first_identity = file::identity(&first).expect("first identity");
    let second_identity = file::identity(&second).expect("second identity");

    let window = open_window(cx, "hello", None, None);
    let handle: AnyWindowHandle = window.into();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.update(|_window, cx| {
        assert!(reserve_open(cx, &first_identity, handle));
        assert!(reserve_open(cx, &second_identity, handle));
        note.update(cx, |note, cx| {
            note.queue_save(
                cx,
                SaveRequest::adopting(first.clone(), first_identity.clone(), handle, None),
            );
            note.queue_save(
                cx,
                SaveRequest::adopting(second.clone(), second_identity.clone(), handle, None),
            );
        });
        assert_eq!(
            lookup_open(cx, &first_identity),
            None,
            "the replaced adoption kept its reservation"
        );
        assert_eq!(lookup_open(cx, &second_identity), Some(handle));
    });
    let pending_path = note.read_with(&cx, |note, _| {
        note.save
            .pending
            .as_ref()
            .map(|pending| pending.path.clone())
    });
    assert_eq!(pending_path, Some(second));
}

#[gpui::test]
fn save_as_refuses_a_path_already_open_in_another_window(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("shared.md");
    file::write_atomic(&path, b"first note").expect("fixture");
    let identity = file::identity(&path).expect("identity");

    let first = open_window(cx, "first note", Some(path.clone()), Some(identity));
    let mut cx = VisualTestContext::from_window(first.into(), cx);

    let second = cx
        .update(|_window, cx| open_note_window(cx, "", None, None, None, None, None, false, None));
    let second_handle: AnyWindowHandle = second.into();
    let second = second.root(&mut cx).expect("second note root");
    cx.cx.simulate_input(second_handle, "second note");
    cx.cx.dispatch_action(second_handle, Save);
    assert!(cx.did_prompt_for_new_path());

    cx.simulate_new_path_selection(|_directory| Some(path.clone()));
    cx.run_until_parked();

    assert_eq!(
        second.read_with(&cx, |note, _| note.path().map(Path::to_path_buf)),
        None,
        "the untitled note must not adopt another window's path"
    );
    assert!(second.read_with(&cx, |note, cx| note.is_dirty(cx)));
    let error = second
        .read_with(&cx, |note, _| note.error().map(str::to_owned))
        .expect("the conflict is visible");
    assert!(error.contains("already open"), "{error}");

    let (text, _) = file::read(&path).expect("first note still readable");
    assert_eq!(text, "first note");
}

fn open_window_with_disk(
    cx: &mut TestAppContext,
    text: &str,
    path: &Path,
) -> WindowHandle<NoteWindow> {
    let identity = file::identity(path).expect("identity");
    let disk = file::stamp(path).expect("stat").expect("fixture exists");
    cx.update(|cx| {
        install(cx);
        open_note_window(
            cx,
            text,
            Some(path.to_path_buf()),
            Some(identity),
            Some(disk),
            None,
            None,
            false,
            None,
        )
    })
}

#[gpui::test]
fn undoing_back_to_the_saved_text_is_clean(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("undo.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));

    let undo = if cfg!(target_os = "macos") {
        "cmd-z"
    } else {
        "ctrl-z"
    };
    cx.simulate_keystrokes(undo);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).text().to_owned()),
        "hello"
    );
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "undoing back to the saved bytes must derive clean"
    );
}

#[gpui::test]
fn retyping_the_saved_bytes_is_clean_and_reordering_lines_is_dirty(cx: &mut TestAppContext) {
    // Dirty is decided by the incremental content hash, confirmed by bytes:
    // returning to the exact saved bytes is clean however the person got
    // there, and the same lines in a different order still count as edited
    // even though their per-line hashes sum to the saved value.
    let dir = TempDir::new();
    let path = dir.join("dirty-hash.md");
    file::write_atomic(&path, b"alpha\nbeta\n").expect("fixture");

    let window = open_window_with_disk(cx, "alpha\nbeta\n", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("x");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));
    cx.simulate_keystrokes("backspace");
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "deleting back to the saved bytes must be clean"
    );

    cx.update(|_window, cx| {
        note.update(cx, |note, cx| {
            note.editor().update(cx, |editor, cx| {
                editor.replace_document("beta\nalpha\n", cx)
            });
        });
    });
    assert!(
        note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "the same lines in a different order are different bytes"
    );
}

#[gpui::test]
fn dirty_tracking_stays_exact_on_a_long_note(cx: &mut TestAppContext) {
    // Perf fixture: the incremental content hash must answer dirty exactly
    // at scale — typed-then-deleted returns to clean without a full compare
    // disguising an error either way.
    let mut text = String::new();
    for paragraph in 0..1_000 {
        text.push_str(&format!("para{paragraph}"));
        for _ in 1..100 {
            text.push_str(" word");
        }
        text.push_str("\n\n");
    }
    let dir = TempDir::new();
    let path = dir.join("long.md");
    file::write_atomic(&path, text.as_bytes()).expect("fixture");

    let window = open_window_with_disk(cx, &text, &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");
    assert!(!note.read_with(&cx, |note, cx| note.is_dirty(cx)));

    cx.simulate_input("x");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));
    cx.simulate_keystrokes("backspace");
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "deleting back to the saved bytes of a long note must be clean"
    );
}

#[gpui::test]
fn an_external_edit_is_not_overwritten_without_a_second_save(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("conflict.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    file::write_atomic(&path, b"someone else").expect("external edit");

    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);
    cx.run_until_parked();

    let error = note
        .read_with(&cx, |note, _| note.error().map(str::to_owned))
        .expect("the conflict is visible");
    assert!(error.contains("changed on disk"), "{error}");
    assert!(
        note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "a refused save must leave the note dirty"
    );
    let (on_disk, _) = file::read(&path).expect("read external text");
    assert_eq!(on_disk, "someone else", "the external edit was overwritten");

    // The explicit second Save is the overwrite path.
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read overwritten text");
    assert_eq!(on_disk, "!hello");
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "the overwrite did not mark the note clean"
    );
    assert_eq!(
        note.read_with(&cx, |note, _| note.error().map(str::to_owned)),
        None
    );
}

#[gpui::test]
fn saving_commits_a_live_composition_instead_of_writing_preedit(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("ime.md");
    file::write_atomic(&path, b"abc").expect("fixture");

    let window = open_window_with_disk(cx, "abc", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");
    let editor = note.read_with(&cx, |note, _| note.editor().clone());

    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_text_range(3..3, window, cx);
            editor.replace_and_mark_text_in_range(Some(3..3), "に", Some(1..1), window, cx);
        });
    });
    assert_eq!(
        editor.read_with(&cx, |editor, _| editor.text().to_owned()),
        "abcに"
    );

    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);
    cx.run_until_parked();

    let (on_disk, _) = file::read(&path).expect("read saved note");
    assert_eq!(
        on_disk, "abcに",
        "save wrote a preedit or failed to commit the composition"
    );
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "the committed composition should be clean after saving"
    );
}

#[gpui::test]
fn closing_a_dirty_note_finishes_its_save_before_removing_the_window(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("close.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    let allowed = cx.update(|window, cx| note.update(cx, |note, cx| note.should_close(window, cx)));
    assert!(!allowed, "a dirty note must not close immediately");

    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read saved note");
    assert_eq!(on_disk, "!hello", "closing lost the save");
    assert_eq!(
        cx.read(|cx| cx.windows().len()),
        0,
        "the window stayed open after its save landed"
    );
}

#[gpui::test]
fn quitting_writes_a_save_that_was_still_queued(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("quit.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    // A second explicit Save waiting behind an in-flight write has no guard of
    // its own. It must still be written by the quit flush.
    cx.update(|_window, cx| {
        note.update(cx, |note, cx| {
            note.queue_save(cx, SaveRequest::new(path.clone(), note.disk));
        });
    });

    // Hold the tracker open briefly so quit has to wait for something.
    let gate = save::SaveGuard::begin();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(75));
        drop(gate);
    });
    assert!(save::pending() >= 1);

    cx.quit();

    let (saved, _) = file::read(&path).expect("read after quit");
    assert_eq!(saved, "!hello", "quitting lost a queued save");
}

#[gpui::test]
fn a_clean_note_has_no_quit_work(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("clean.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    let snapshot = cx
        .update(|window, cx| note.update(cx, |note, cx| note.quit_snapshot(window, cx)))
        .expect("a saved note is checked for disk changes at quit");
    assert!(!snapshot.dirty);
    assert!(!snapshot.needs_write);
    assert!(
        !snapshot.disk_changed(),
        "an unchanged note needs no recovery copy"
    );
    assert_eq!(snapshot.journal(), (None, false));
}

#[gpui::test]
fn closing_a_clean_note_after_the_file_vanishes_recreates_it(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("vanished.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    fs::remove_file(&path).expect("external delete");
    let allowed = cx.update(|window, cx| note.update(cx, |note, cx| note.should_close(window, cx)));
    assert!(!allowed);
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| cx.windows().len()),
        1,
        "Close must not overwrite an external change without a Save"
    );
    assert!(note.read_with(&cx, |note, _| note.error.is_some()));

    // An explicit Save is the confirmation; it recreates the missing note.
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (saved, _) = file::read(&path).expect("recreated note");
    assert_eq!(saved, "hello");

    let allowed = cx.update(|window, cx| note.update(cx, |note, cx| note.should_close(window, cx)));
    assert!(!allowed);
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| cx.windows().len()), 0);
}

#[gpui::test]
fn an_unreadable_path_stays_protected_after_an_unrelated_save_as(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let broken = dir.join("broken.md");
    let original = [0xff, 0xfe, 0x00, 0x41];
    fs::write(&broken, original).expect("fixture");
    let initial = initial_note_from(Some(broken.clone()));

    let window = cx.update(|cx| {
        install(cx);
        open_note_window(
            cx,
            "",
            None,
            None,
            None,
            initial.failed_path,
            None,
            false,
            initial.error,
        )
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    let good = dir.join("good.md");
    cx.dispatch_action(Save);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(good.clone()));
    cx.run_until_parked();
    assert!(good.exists(), "the unrelated Save As did not land");

    cx.dispatch_action(SaveAs);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(broken.clone()));
    cx.run_until_parked();

    assert_eq!(
        fs::read(&broken).expect("unreadable file survives"),
        original,
        "a later Save As replaced the unreadable note"
    );
    let message = note
        .read_with(&cx, |note, _| note.error().map(str::to_owned))
        .expect("the refusal is visible");
    assert!(message.contains("refusing to replace"), "{message}");
}

#[gpui::test]
fn a_conflict_overwrite_is_bound_to_the_version_the_person_saw(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("bound.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };

    cx.simulate_input("!");
    file::write_atomic(&path, b"theirs-one").expect("external edit one");
    cx.simulate_keystrokes(save);
    cx.run_until_parked();

    // A second external edit after the warning must not be silently replaced
    // by the retry: the permit is bound to the version the person saw.
    file::write_atomic(&path, b"theirs-two").expect("external edit two");
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read");
    assert_eq!(
        on_disk, "theirs-two",
        "the retry overwrote an edit made after the conflict warning"
    );
    assert!(note.read_with(&cx, |note, _| note.conflict));

    // The third Save is now bound to theirs-two and is allowed to replace it.
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read");
    assert_eq!(on_disk, "!hello");
    assert!(!note.read_with(&cx, |note, cx| note.is_dirty(cx)));
}

#[gpui::test]
fn editing_after_a_conflict_requires_a_new_warning(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("edit-after-conflict.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };

    cx.simulate_input("!");
    file::write_atomic(&path, b"theirs").expect("external edit");
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    assert!(note.read_with(&cx, |note, _| note.conflict));

    // The conflict revision is invalidated by this edit, so the next Save
    // checks the disk again instead of using the old permit.
    cx.simulate_input("?");
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read");
    assert_eq!(
        on_disk, "theirs",
        "the edited text overwrote the external edit"
    );
}

#[gpui::test]
fn a_clean_buffer_can_overwrite_after_the_conflict_warning(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("clean-conflict.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    file::write_atomic(&path, b"theirs").expect("external edit");
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    assert!(note.read_with(&cx, |note, _| note.conflict));

    // The buffer is clean, so the second Save must actually write it rather
    // than skip as "unchanged against the conflict stamp".
    cx.simulate_keystrokes(save);
    cx.run_until_parked();
    let (on_disk, _) = file::read(&path).expect("read");
    assert_eq!(on_disk, "hello", "the clean retry was skipped");
    assert!(!note.read_with(&cx, |note, _| note.conflict));
    assert_eq!(
        note.read_with(&cx, |note, _| note.error().map(str::to_owned)),
        None
    );
}

#[gpui::test]
fn a_clean_note_whose_file_vanished_still_has_quit_work(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("quit-vanished.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    fs::remove_file(&path).expect("external delete");
    let snapshot =
        cx.update(|window, cx| note.update(cx, |note, cx| note.quit_snapshot(window, cx)));
    let snapshot = snapshot.expect("a missing file needs a recovery copy at quit");
    assert!(snapshot.disk_changed(), "the clean note looked unchanged");
    assert!(
        !snapshot.needs_write,
        "quit must not recreate the file silently"
    );
}

#[test]
fn the_quit_task_recovers_a_clean_note_changed_on_disk_without_rewriting_it() {
    let dir = TempDir::new();
    let path = dir.join("clean-changed.md");
    let root = dir.join("data");
    fs::write(&path, b"hello").expect("fixture");
    let disk = file::stamp(&path).expect("stat").expect("fixture exists");
    fs::write(&path, b"theirs").expect("external edit");

    finish_quit(vec![QuitWork {
        snapshot: QuitSnapshot {
            path: Some(path.clone()),
            draft_id: None,
            data_root: root.clone(),
            text: Arc::from("hello"),
            disk: Some(disk),
            dirty: false,
            needs_write: false,
            active: None,
            title: "clean-changed".to_owned(),
        },
        journal: None,
    }]);

    assert_eq!(fs::read(&path).expect("target"), b"theirs");
    let recovery = fs::read_dir(root.join("recovery"))
        .expect("recovery directory")
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_file())
        .expect("a clean changed note keeps a recovery copy");
    assert_eq!(fs::read(recovery.path()).expect("recovery"), b"hello");
}

#[gpui::test]
fn appearance_cycles_through_auto_light_and_dark(cx: &mut TestAppContext) {
    let window = open_window(cx, "hello", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    let light = pilcrow_ui::theme::light();
    let dark = pilcrow_ui::theme::dark();
    assert_eq!(
        note.read_with(&cx, |note, _| note.tokens().paper),
        light.paper,
        "a new test window starts light through Auto"
    );

    // Auto -> Light keeps the same page, then Light -> Dark changes it.
    cx.dispatch_action(CycleAppearance);
    cx.dispatch_action(CycleAppearance);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).tokens().paper),
        dark.paper,
        "the editor did not receive the dark tokens"
    );
    assert_eq!(
        cx.update(|_window, cx| prefs::current_for_test(cx).appearance),
        prefs::Appearance::Dark
    );

    cx.dispatch_action(CycleAppearance);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).tokens().paper),
        light.paper,
        "Auto did not resolve back to the light window"
    );
}

#[gpui::test]
fn high_contrast_toggles_the_page_tokens(cx: &mut TestAppContext) {
    let window = open_window(cx, "hello", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.dispatch_action(ToggleHighContrast);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).tokens()),
        pilcrow_ui::theme::light_high_contrast(),
        "the editor did not receive the high-contrast tokens"
    );
    assert_eq!(
        cx.update(|_window, cx| prefs::current_for_test(cx).contrast),
        prefs::Contrast::High
    );

    cx.dispatch_action(ToggleHighContrast);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor().read(cx).tokens()),
        pilcrow_ui::theme::light(),
        "toggling back did not restore the standard tokens"
    );
}

#[gpui::test]
fn the_menu_bar_exposes_file_edit_format_and_view(cx: &mut TestAppContext) {
    cx.update(install);
    let menus = cx.update(|cx| cx.get_menus()).expect("menus");
    let names = menus
        .iter()
        .map(|menu| menu.name.to_string())
        .collect::<Vec<_>>();
    assert!(names.contains(&"File".to_owned()), "{names:?}");
    assert!(names.contains(&"Format".to_owned()), "{names:?}");
    assert!(names.contains(&"View".to_owned()), "{names:?}");
}

fn appearance_item_checked(cx: &mut TestAppContext, label: &str) -> bool {
    cx.update(|cx| {
        cx.get_menus()
            .expect("menus")
            .into_iter()
            .find(|menu| menu.name == "View")
            .and_then(|menu| {
                menu.items.into_iter().find_map(|item| match item {
                    gpui::OwnedMenuItem::Submenu(menu) if menu.name == "Appearance" => {
                        menu.items.into_iter().find_map(|item| match item {
                            gpui::OwnedMenuItem::Action { name, checked, .. } if name == label => {
                                Some(checked)
                            }
                            _ => None,
                        })
                    }
                    _ => None,
                })
            })
            .unwrap_or(false)
    })
}

#[gpui::test]
fn appearance_menu_checkmarks_follow_the_choice(cx: &mut TestAppContext) {
    let window = open_window(cx, "hello", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    assert!(appearance_item_checked(&mut cx, "Follow System"));
    assert!(!appearance_item_checked(&mut cx, "Dark"));

    cx.dispatch_action(CycleAppearance);
    assert!(appearance_item_checked(&mut cx, "Light"));
    cx.dispatch_action(CycleAppearance);
    assert!(appearance_item_checked(&mut cx, "Dark"));
    assert!(!appearance_item_checked(&mut cx, "Light"));
}

#[gpui::test]
fn autosave_writes_after_the_typing_pause(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("autosave.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    let (before, _) = file::read(&path).expect("read before autosave");
    assert_eq!(before, "hello", "autosave wrote in the typing frame");

    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(1600));
    cx.run_until_parked();

    let (after, _) = file::read(&path).expect("read after autosave");
    assert_eq!(after, "!hello");
    assert!(
        !note.read_with(&cx, |note, cx| note.is_dirty(cx)),
        "a successful autosave must mark the written revision clean"
    );
}

#[gpui::test]
fn autosave_refuses_to_overwrite_an_external_edit(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("autosave-conflict.md");
    file::write_atomic(&path, b"hello").expect("fixture");

    let window = open_window_with_disk(cx, "hello", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("!");
    file::write_atomic(&path, b"theirs").expect("external edit");
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(1600));
    cx.run_until_parked();

    let (on_disk, _) = file::read(&path).expect("read");
    assert_eq!(on_disk, "theirs", "autosave overwrote an external edit");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));
    assert!(note.read_with(&cx, |note, _| note.error.is_some()));
}

fn open_window_in_root(
    cx: &mut TestAppContext,
    root: &Path,
    text: &str,
) -> WindowHandle<NoteWindow> {
    cx.update(|cx| {
        cx.set_global(AppDataRoot(root.to_path_buf()));
        install(cx);
        open_note_window(cx, text, None, None, None, None, None, false, None)
    })
}

#[gpui::test]
fn untitled_notes_write_a_draft_after_the_pause(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let window = open_window_in_root(cx, &root, "");
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("hello draft");
    assert!(
        drafts::list_from(&root).is_empty(),
        "the draft was written in the typing frame"
    );
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();

    let saved = drafts::list_from(&root);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].text, "hello draft");
    assert!(note.read_with(&cx, |note, cx| note.is_dirty(cx)));
}

#[gpui::test]
fn save_as_removes_the_untitled_draft(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let destination = dir.join("saved.md");
    let window = open_window_in_root(cx, &root, "");
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_input("draft words");

    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    assert_eq!(drafts::list_from(&root).len(), 1);

    cx.dispatch_action(Save);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(destination.clone()));
    cx.run_until_parked();
    assert!(
        drafts::list_from(&root).is_empty(),
        "saved draft was not removed"
    );
    assert_eq!(
        file::read(&destination).expect("saved note").0,
        "draft words"
    );
}

#[gpui::test]
fn startup_recovers_drafts_as_dirty_untitled_notes(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let id = drafts::new_id();
    drafts::save_to(&root, &id, "recovered words").expect("draft fixture");

    cx.update(|cx| {
        cx.set_global(AppDataRoot(root.clone()));
        install(cx);
        recover_drafts(cx);
    });
    let windows = cx.read(|cx| cx.windows());
    assert_eq!(windows.len(), 1);
    let (text, dirty, recovered, draft_id) = windows[0]
        .read::<NoteWindow, _, _>(cx, |note, cx| {
            let note = note.read(cx);
            (
                note.editor.read(cx).text().to_owned(),
                note.is_dirty(cx),
                note.recovered,
                note.draft_id.clone(),
            )
        })
        .expect("recovered note");
    assert_eq!(text, "recovered words");
    assert!(dirty, "a recovered draft must not look saved");
    assert!(recovered);
    assert_eq!(draft_id, Some(id));
}

#[gpui::test]
fn continuous_typing_still_hits_the_autosave_deadline(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("bounded.md");
    file::write_atomic(&path, b"start").expect("fixture");
    let window = open_window_with_disk(cx, "start", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    for _ in 0..6 {
        cx.simulate_input("a");
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(1000));
        cx.run_until_parked();
    }
    let (saved, _) = file::read(&path).expect("read after continuous typing");
    assert_ne!(
        saved, "start",
        "continuous typing never reached the autosave deadline"
    );
}

#[gpui::test]
fn quitting_an_untitled_note_keeps_a_draft(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let window = open_window_in_root(cx, &root, "");
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.simulate_input("kept through quit");
    cx.quit();

    let drafts = drafts::list_from(&root);
    assert_eq!(drafts.len(), 1, "quit did not keep the untitled draft");
    assert_eq!(drafts[0].text, "kept through quit");
}

#[gpui::test]
fn session_restores_windows_and_view_state(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let first = dir.join("first.md");
    let second = dir.join("second.md");
    file::write_atomic(&first, b"first words").expect("first");
    file::write_atomic(&second, b"second words").expect("second");
    let saved = session::Session {
        windows: vec![
            session::SavedWindow {
                path: first.clone(),
                caret: 5,
                anchor: Some(2),
                scroll: 9.0,
                reading: true,
                focus: true,
                typewriter: false,
                frame: Some(session::SavedFrame {
                    x: 40.0,
                    y: 30.0,
                    width: 900.0,
                    height: 600.0,
                }),
                maximized: false,
            },
            session::SavedWindow {
                path: second.clone(),
                ..session::SavedWindow::default()
            },
            session::SavedWindow {
                path: first.clone(),
                ..session::SavedWindow::default()
            },
        ],
        recent: vec![second.clone(), first.clone()],
    };
    session::save(&root, &saved).expect("save session");

    cx.update(|cx| {
        cx.set_global(AppDataRoot(root.clone()));
        install(cx);
        assert_eq!(restore_session(cx), 2);
    });
    let windows = cx.read(|cx| cx.windows());
    assert_eq!(windows.len(), 2);
    let restored = windows[0]
        .read::<NoteWindow, _, _>(cx, |note, cx| {
            let note = note.read(cx);
            (
                note.editor.read(cx).text().to_owned(),
                note.editor.read(cx).view_state(),
            )
        })
        .expect("first window");
    assert_eq!(restored.0, "first words");
    assert_eq!(restored.1.0, 5);
    assert_eq!(restored.1.1, Some(2));
    assert!(restored.1.3, "reading mode was not restored");

    // The saved frame is where the window actually opened.
    let opened_bounds = windows[0]
        .update(cx, |_note, window, _cx| window.window_bounds())
        .expect("first window bounds");
    let gpui::WindowBounds::Windowed(opened_bounds) = opened_bounds else {
        panic!("restored window is not windowed: {opened_bounds:?}");
    };
    assert_eq!(opened_bounds.origin.x, px(40.0));
    assert_eq!(opened_bounds.origin.y, px(30.0));
    assert_eq!(opened_bounds.size.width, px(900.0));
    assert_eq!(opened_bounds.size.height, px(600.0));

    cx.update(persist_session);
    let reloaded = session::load(&root);
    assert_eq!(reloaded.windows.len(), 2);
    assert_eq!(reloaded.windows[0].path, first);
    assert_eq!(
        reloaded.windows[0].frame,
        Some(session::SavedFrame {
            x: 40.0,
            y: 30.0,
            width: 900.0,
            height: 600.0,
        }),
        "persist did not capture the restored frame"
    );
    assert_eq!(reloaded.windows[1].path, second);
    assert_eq!(reloaded.recent[0], second);
}

#[gpui::test]
fn recent_notes_are_deduped_and_offer_a_menu_action(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let note = dir.join("note.md");
    cx.update(|cx| {
        cx.set_global(AppDataRoot(root.clone()));
        install(cx);
        remember_recent(cx, &note);
        remember_recent(cx, &note);
        assert_eq!(current_recent(cx), vec![note.clone()]);
        assert!(!recent_menu_labels(cx).is_empty());
    });
}

#[gpui::test]
fn copy_as_html_puts_a_standalone_document_on_the_clipboard(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("copy.md");
    file::write_atomic(&path, b"# Heading\n\nBody with *style*.").expect("fixture");
    let window = open_window_with_disk(cx, "# Heading\n\nBody with *style*.", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.dispatch_action(CopyHtml);
    let html = cx
        .update(|_, cx| cx.read_from_clipboard())
        .and_then(|item| item.text())
        .expect("clipboard html");
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<h1>Heading</h1>"));
    assert!(html.contains("<p>Body with style.</p>"));
}

#[gpui::test]
fn export_html_writes_a_file_without_changing_the_note(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("export.md");
    let destination = dir.join("export.html");
    file::write_atomic(&path, b"# Exported\n\nBody").expect("fixture");
    let window = open_window_with_disk(cx, "# Exported\n\nBody", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.dispatch_action(ExportHtml);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(destination.clone()));
    cx.run_until_parked();
    let html = file::read(&destination).expect("exported html").0;
    assert!(html.contains("<h1>Exported</h1>"));
    assert_eq!(
        file::read(&path).expect("original note").0,
        "# Exported\n\nBody"
    );
}

#[gpui::test]
fn saving_records_a_local_backup(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let data = dir.join("data");
    let path = dir.join("backed.md");
    file::write_atomic(&path, b"first").expect("fixture");
    cx.update(|cx| cx.set_global(AppDataRoot(data.clone())));
    let window = open_window_with_disk(cx, "first", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_input("!");
    cx.dispatch_action(Save);
    cx.run_until_parked();
    let versions = backups::versions(&data, &path);
    assert!(!versions.is_empty(), "the save produced no backup");
    assert_eq!(
        std::fs::read(&versions[0]).expect("backup bytes"),
        b"!first"
    );
}

#[gpui::test]
fn quitting_writes_the_session_paths(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let data = dir.join("data");
    let first = dir.join("first.md");
    let second = dir.join("second.md");
    file::write_atomic(&first, b"one").expect("first");
    file::write_atomic(&second, b"two").expect("second");
    cx.update(|cx| cx.set_global(AppDataRoot(data.clone())));
    open_window_with_disk(cx, "one", &first);
    open_window_with_disk(cx, "two", &second);
    cx.quit();
    let session = session::load(&data);
    let paths: Vec<PathBuf> = session
        .windows
        .into_iter()
        .map(|window| window.path)
        .collect();
    assert!(paths.contains(&first), "{paths:?}");
    assert!(paths.contains(&second), "{paths:?}");
    assert!(session.recent.contains(&first));
    assert!(session.recent.contains(&second));
}

#[gpui::test]
fn close_window_action_uses_the_dirty_prompt(cx: &mut TestAppContext) {
    let window = open_window(cx, "", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_input("not saved");
    cx.dispatch_action(CloseWindow);
    assert!(
        cx.did_prompt_for_new_path(),
        "Cmd-W discarded a dirty untitled note without asking"
    );
    assert_eq!(
        cx.read(|cx| cx.windows().len()),
        1,
        "the window closed anyway"
    );
}

#[gpui::test]
fn export_html_cannot_replace_the_open_note(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("source.md");
    let html_path = dir.join("source.html");
    file::write_atomic(&path, b"# Source\n\nbody").expect("fixture");
    let window = open_window_with_disk(cx, "# Source\n\nbody", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.dispatch_action(ExportHtml);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(path.clone()));
    cx.run_until_parked();
    assert_eq!(
        file::read(&path).expect("source note").0,
        "# Source\n\nbody",
        "export replaced the Markdown source"
    );
    assert!(
        !html_path.is_file(),
        "export silently wrote a normalized path the person did not approve"
    );
    let note = window.root(&mut cx).expect("root");
    assert!(
        note.read_with(&cx, |note, _| note.error.clone())
            .is_some_and(|error| error.contains(".html")),
        "the export did not explain the .html requirement"
    );
}

#[gpui::test]
fn save_as_backs_up_the_file_it_replaces(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let data = dir.join("data");
    let source = dir.join("source.md");
    let target = dir.join("target.md");
    file::write_atomic(&source, b"mine").expect("source");
    file::write_atomic(&target, b"target").expect("target");
    cx.update(|cx| cx.set_global(AppDataRoot(data.clone())));
    let window = open_window_with_disk(cx, "mine", &source);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.dispatch_action(SaveAs);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| Some(target.clone()));
    cx.run_until_parked();
    assert_eq!(file::read(&target).expect("target").0, "mine");
    let versions = backups::versions(&data, &target);
    assert!(
        versions
            .iter()
            .any(|version| std::fs::read(version).expect("backup") == b"target"),
        "Save As replaced target.md without backing it up: {versions:?}"
    );
}

#[test]
fn recovery_names_are_unique_even_within_one_clock_tick() {
    let first = recovery_name("same title");
    let second = recovery_name("same title");
    assert_ne!(first, second);
}

#[test]
fn open_urls_accept_local_files_and_reject_network_schemes() {
    assert_eq!(
        open_url_path("file:///tmp/My%20Note.md"),
        Some(PathBuf::from("/tmp/My Note.md"))
    );
    assert_eq!(
        open_url_path("/tmp/plain.md"),
        Some(PathBuf::from("/tmp/plain.md"))
    );
    assert_eq!(open_url_path("https://example.com/note.md"), None);
    assert_eq!(open_url_path("pilcrow://open?path=x"), None);
    assert_eq!(open_url_path("smb://host/share.md"), None);
    assert_eq!(open_url_path("file://server/share.md"), None);
    assert_eq!(
        open_url_path("file://localhost/tmp/local.md"),
        Some(PathBuf::from("/tmp/local.md"))
    );
}

#[gpui::test]
fn dragging_into_the_status_row_keeps_selecting(cx: &mut TestAppContext) {
    let window = open_window(cx, "first line\nsecond line", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    let (start, end) = note.read_with(&cx, |note, cx| {
        let editor = note.editor.read(cx);
        let caret = editor.caret_rect().expect("caret rect");
        let (_, height) = editor.viewport().expect("viewport");
        let status_y = caret.top() + px(height) + px(8.0);
        (
            caret.center(),
            gpui::point(caret.center().x + px(300.0), status_y),
        )
    });
    cx.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_move(
        end,
        Some(gpui::MouseButton::Left),
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
    let selection = note
        .read_with(&cx, |note, cx| note.editor.read(cx).selection())
        .expect("a selection");
    assert!(
        selection.end > "first line\n".len(),
        "dragging into the status row did not extend past the first line: {selection:?}"
    );
}

#[gpui::test]
fn the_window_publishes_its_document_path(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("published.md");
    file::write_atomic(&path, b"words").expect("fixture");
    let window = open_window_with_disk(cx, "words", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    assert_eq!(cx.document_path(), Some(path));
}

#[gpui::test]
fn text_size_actions_persist_and_reach_the_editor(cx: &mut TestAppContext) {
    let window = open_window(cx, "words", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    cx.dispatch_action(TextSizeUp);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor.read(cx).scale()),
        1.125
    );
    assert_eq!(
        cx.update(|_window, cx| prefs::current_for_test(cx).text_scale),
        1.125
    );
    cx.dispatch_action(TextSizeReset);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor.read(cx).scale()),
        1.0
    );
}

#[gpui::test]
fn measure_actions_persist_and_resize_the_column(cx: &mut TestAppContext) {
    let window = open_window(cx, "words", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    cx.dispatch_action(MeasureWide);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor.read(cx).measure_chars()),
        76
    );
    assert_eq!(
        cx.update(|_window, cx| prefs::current_for_test(cx).measure),
        prefs::Measure::Wide
    );
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let wide = note.read_with(&cx, |note, cx| note.editor.read(cx).laid_out_width());
    cx.dispatch_action(MeasureNarrow);
    assert_eq!(
        note.read_with(&cx, |note, cx| note.editor.read(cx).measure_chars()),
        56
    );
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
    });
    let narrow = note.read_with(&cx, |note, cx| note.editor.read(cx).laid_out_width());
    assert!(
        wide > narrow,
        "measure action did not resize the column: {wide} vs {narrow}"
    );
}

#[gpui::test]
fn a_queued_clean_save_cannot_mark_newer_disk_text_clean(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("race.md");
    file::write_atomic(&path, b"T1").expect("fixture");
    let window = open_window_with_disk(cx, "T0", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    cx.update(|_window, cx| {
        note.update(cx, |note, cx| {
            // Simulate: a write of T1 landed, then the person undid to T0 and
            // pressed Save while that write was still in flight.
            note.set_saved(Arc::from("T1"));
            note.dirty_cache.set(None);
            note.queue_save(cx, SaveRequest::new(path.clone(), note.disk).clean());
            note.pump_saves(cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        file::read(&path).expect("read").0,
        "T0",
        "the queued save skipped the visible text"
    );
    assert!(!note.read_with(&cx, |note, cx| note.is_dirty(cx)));
}

#[gpui::test]
fn a_metadata_only_touch_does_not_create_quit_work(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let path = dir.join("touched-clean.md");
    file::write_atomic(&path, b"clean").expect("fixture");
    let window = open_window_with_disk(cx, "clean", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open timestamp");
    file.set_modified(SystemTime::now() + std::time::Duration::from_secs(3600))
        .expect("touch");
    let snapshot = cx
        .update(|window, cx| note.update(cx, |note, cx| note.quit_snapshot(window, cx)))
        .expect("a saved note is checked for disk changes at quit");
    assert!(!snapshot.disk_changed(), "a touch produced a recovery copy");
}

#[gpui::test]
fn the_registry_reserves_dedupes_and_releases_paths(cx: &mut TestAppContext) {
    let first_window = open_window(cx, "first", None, None);
    let second_window = open_window(cx, "second", None, None);
    let first: AnyWindowHandle = first_window.into();
    let second: AnyWindowHandle = second_window.into();
    let identity = Path::new("/tmp/pilcrow-registry-test.md");
    cx.update(|cx| {
        assert!(reserve_open(cx, identity, first));
        assert!(!reserve_open(cx, identity, second));
        assert_eq!(lookup_open(cx, identity), Some(first));
        forget_window(cx, first.window_id());
        assert_eq!(lookup_open(cx, identity), None);
        assert!(reserve_open(cx, identity, second));
        assert_eq!(lookup_open(cx, identity), Some(second));
    });
}

#[test]
fn save_dialogs_never_default_to_the_filesystem_root() {
    let dir = TempDir::new();
    let home = dir.join("home");
    fs::create_dir_all(home.join("Documents")).expect("documents");
    let note = dir.join("note.md");
    assert_eq!(
        choose_save_directory(Some(&note), Some(&home), Some(Path::new("/"))),
        dir.0
    );
    assert_eq!(
        choose_save_directory(
            Some(Path::new("/note.md")),
            Some(&home),
            Some(Path::new("/"))
        ),
        home.join("Documents")
    );
    assert_eq!(
        choose_save_directory(None, Some(&home), Some(Path::new("/"))),
        home.join("Documents")
    );
    assert_eq!(
        choose_save_directory(None, None, Some(Path::new("/"))),
        PathBuf::from(".")
    );
}

#[gpui::test]
fn save_as_prompt_does_not_start_at_the_filesystem_root(cx: &mut TestAppContext) {
    let window = open_window(cx, "draft", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.dispatch_action(Save);
    assert!(cx.did_prompt_for_new_path());
    let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen_in_prompt = seen.clone();
    cx.simulate_new_path_selection(move |directory| {
        *seen_in_prompt.borrow_mut() = Some(directory.to_path_buf());
        None
    });
    let directory = seen.borrow().clone().expect("prompt directory");
    assert_ne!(
        directory,
        PathBuf::from("/"),
        "Save As started at the read-only filesystem root"
    );
}

#[gpui::test]
fn cancelling_close_on_dirty_untitled_note_offers_discard_and_closes_when_confirmed(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let window = open_window_in_root(cx, &root, "");
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    cx.simulate_input("throwaway thoughts");
    cx.background_executor
        .advance_clock(std::time::Duration::from_millis(700));
    cx.run_until_parked();
    assert_eq!(drafts::list_from(&root).len(), 1);

    cx.dispatch_action(CloseWindow);
    assert!(cx.did_prompt_for_new_path());
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();

    assert!(
        note.read_with(&cx, |note, _| note.offer_discard && note.confirm_discard),
        "cancelling the close-save prompt should offer to discard the untitled note"
    );
    assert_eq!(cx.read(|cx| cx.windows().len()), 1);

    cx.update(|window, cx| {
        note.update(cx, |note, cx| {
            note.discard_recovered(&ClickEvent::default(), window, cx);
        });
    });
    assert_eq!(
        cx.read(|cx| cx.windows().len()),
        0,
        "confirming discard should close the untitled note"
    );
    assert!(
        drafts::list_from(&root).is_empty(),
        "discarding should remove the draft file"
    );
}

#[gpui::test]
fn discarding_a_recovered_draft_requires_confirmation(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let id = drafts::new_id();
    drafts::save_to(&root, &id, "precious draft").expect("draft fixture");

    let window = cx.update(|cx| {
        cx.set_global(AppDataRoot(root.clone()));
        install(cx);
        open_note_window(
            cx,
            "precious draft",
            None,
            None,
            None,
            None,
            Some(id.clone()),
            true,
            None,
        )
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("note root");

    // First click only arms confirmation; the draft and window remain intact.
    cx.update(|window, cx| {
        note.update(cx, |note, cx| {
            note.discard_recovered(&ClickEvent::default(), window, cx);
        });
    });
    assert!(note.read_with(&cx, |note, _| note.confirm_discard));
    assert_eq!(cx.read(|cx| cx.windows().len()), 1);
    assert_eq!(drafts::list_from(&root).len(), 1);

    // Keep cancels the armed discard.
    cx.update(|window, cx| {
        note.update(cx, |note, cx| {
            note.keep_draft(&ClickEvent::default(), window, cx);
        });
    });
    assert!(!note.read_with(&cx, |note, _| note.confirm_discard));

    // Arm and confirm on the second click.
    cx.update(|window, cx| {
        note.update(cx, |note, cx| {
            note.discard_recovered(&ClickEvent::default(), window, cx);
            note.discard_recovered(&ClickEvent::default(), window, cx);
        });
    });
    assert_eq!(cx.read(|cx| cx.windows().len()), 0);
    assert!(drafts::list_from(&root).is_empty());
}

#[gpui::test]
fn startup_recovers_quit_recovery_journals(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let recovery = write_recovery("rescued at quit", "rescued", None, &root).expect("recovery");
    assert!(recovery.exists());

    cx.update(|cx| {
        cx.set_global(AppDataRoot(root.clone()));
        install(cx);
        recover_drafts(cx);
    });
    let windows = cx.read(|cx| cx.windows());
    assert_eq!(windows.len(), 1);
    let (text, recovered) = windows[0]
        .read::<NoteWindow, _, _>(cx, |note, cx| {
            let note = note.read(cx);
            (note.editor.read(cx).text().to_owned(), note.recovered)
        })
        .expect("recovered window");
    assert_eq!(text, "rescued at quit");
    assert!(recovered);
}

#[gpui::test]
fn continuous_typing_in_untitled_note_hits_draft_deadline(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let root = dir.join("data");
    let window = open_window_in_root(cx, &root, "");
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    for _ in 0..12 {
        cx.simulate_input("a");
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(500));
        cx.run_until_parked();
    }
    let saved = drafts::list_from(&root);
    assert_eq!(
        saved.len(),
        1,
        "continuous typing never reached the draft deadline"
    );
    assert!(!saved[0].text.is_empty());
}

#[gpui::test]
fn conflict_overwrite_snapshots_external_file_into_backups(cx: &mut TestAppContext) {
    let dir = TempDir::new();
    let data = dir.join("data");
    let path = dir.join("conflict-backup.md");
    file::write_atomic(&path, b"mine").expect("fixture");
    cx.update(|cx| cx.set_global(AppDataRoot(data.clone())));
    let window = open_window_with_disk(cx, "mine", &path);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.simulate_input("!");
    file::write_atomic(&path, b"external version").expect("external edit");
    cx.dispatch_action(Save);
    cx.run_until_parked();
    // Second Save confirms the overwrite.
    cx.dispatch_action(Save);
    cx.run_until_parked();

    let versions = backups::versions(&data, &path);
    assert!(
        versions
            .iter()
            .any(|version| fs::read(version).expect("backup") == b"external version"),
        "conflict overwrite did not back up the external file: {versions:?}"
    );
}

#[gpui::test]
fn view_mode_menu_checkmarks_follow_focus_typewriter_and_reading(cx: &mut TestAppContext) {
    use pilcrow_commands::editor_actions::{FocusSentence, ToggleReading, ToggleTypewriter};

    let window = open_window(cx, "First sentence. Second sentence.", None, None);
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    cx.dispatch_action(FocusSentence);
    cx.dispatch_action(ToggleTypewriter);
    cx.dispatch_action(ToggleReading);

    let (focus_checked, sentence_checked, typewriter_checked, reading_checked) =
        cx.update(|_window, cx| {
            let view_menu = cx
                .get_menus()
                .expect("menus")
                .into_iter()
                .find(|menu| menu.name == "View")
                .expect("View menu");
            let mut focus_checked = false;
            let mut sentence_checked = false;
            let mut typewriter_checked = false;
            let mut reading_checked = false;
            for item in view_menu.items {
                match item {
                    gpui::OwnedMenuItem::Action { name, checked, .. } => match name.as_str() {
                        "Typewriter" => typewriter_checked = checked,
                        "Reading View" => reading_checked = checked,
                        _ => {}
                    },
                    gpui::OwnedMenuItem::Submenu(submenu) if submenu.name == "Focus" => {
                        for sub in submenu.items {
                            if let gpui::OwnedMenuItem::Action { name, checked, .. } = sub {
                                match name.as_str() {
                                    "Focus Mode" => focus_checked = checked,
                                    "Sentence" => sentence_checked = checked,
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            (
                focus_checked,
                sentence_checked,
                typewriter_checked,
                reading_checked,
            )
        });
    assert!(focus_checked, "Focus Mode menu item should be checked");
    assert!(
        sentence_checked,
        "Sentence scope menu item should be checked"
    );
    assert!(
        typewriter_checked,
        "Typewriter Scrolling menu item should be checked"
    );
    assert!(reading_checked, "Reading View menu item should be checked");
}

#[gpui::test]
fn error_banner_can_be_dismissed(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        install(cx);
        open_note_window(
            cx,
            "",
            None,
            None,
            None,
            None,
            None,
            false,
            Some("Could not open missing.md".to_owned()),
        )
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    let note = window.root(&mut cx).expect("root");
    assert!(note.read_with(&cx, |note, _| note.error().is_some()));

    cx.update(|window, cx| {
        note.update(cx, |note, cx| {
            note.dismiss_error(&ClickEvent::default(), window, cx);
        });
    });
    assert_eq!(
        note.read_with(&cx, |note, _| note.error().map(str::to_owned)),
        None
    );
}

#[gpui::test]
fn pristine_startup_blank_window_is_closed_when_external_url_opens(cx: &mut TestAppContext) {
    let blank = open_window(cx, "", None, None);
    let _other = open_window(cx, "real note", None, None);
    assert_eq!(cx.read(|cx| cx.windows().len()), 2);

    let mut startup_blank = Some(blank);
    cx.update(|cx| close_startup_untitled_if_pristine(cx, &mut startup_blank));
    assert!(startup_blank.is_none());
    assert_eq!(cx.read(|cx| cx.windows().len()), 1);
}
