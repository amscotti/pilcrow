//! Durable drafts for notes that do not have a path yet.
//!
//! A draft is plain Markdown in the app's own data root, written atomically
//! with the same private temp-file path as a note. It is not a second
//! document: when the note gets a path, the draft is deleted. On launch,
//! drafts are offered as untitled notes so a crash cannot swallow the words.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub const DRAFTS_DIR: &str = "drafts";
pub const RECOVERY_DIR: &str = "recovery";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub id: String,
    pub text: String,
}

/// A stable, filename-safe draft id.
pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{counter}", std::process::id())
}

fn drafts_dir(root: &Path) -> PathBuf {
    root.join(DRAFTS_DIR)
}

fn recovery_dir(root: &Path) -> PathBuf {
    root.join(RECOVERY_DIR)
}

fn draft_path(root: &Path, id: &str) -> PathBuf {
    drafts_dir(root).join(format!("{id}.md"))
}

fn recovery_path(root: &Path, id: &str) -> PathBuf {
    recovery_dir(root).join(format!("{id}.md"))
}

pub fn save_to(root: &Path, id: &str, text: &str) -> Result<()> {
    let directory = drafts_dir(root);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    crate::file::write_atomic(&draft_path(root, id), text.as_bytes())
        .with_context(|| format!("could not save draft {id}"))?;
    let _ = std::fs::remove_file(recovery_path(root, id));
    Ok(())
}

pub fn read_from(root: &Path, id: &str) -> Result<String> {
    let primary = draft_path(root, id);
    let path = if primary.exists() {
        primary
    } else {
        let fallback = recovery_path(root, id);
        if fallback.exists() { fallback } else { primary }
    };
    let bytes = crate::file::read_bounded(&path, crate::file::MAX_NOTE_BYTES)
        .with_context(|| format!("could not read draft {}", path.display()))?;
    String::from_utf8(bytes).with_context(|| format!("draft {} is not UTF-8", path.display()))
}

pub fn remove_from(root: &Path, id: &str) {
    let _ = std::fs::remove_file(draft_path(root, id));
    let _ = std::fs::remove_file(recovery_path(root, id));
}

/// Drafts and quit recovery copies, newest first. Invalid or unreadable files
/// are skipped: a stray file must not stop the app from opening its real
/// drafts.
pub fn list_from(root: &Path) -> Vec<Draft> {
    let mut found = Vec::new();
    for directory in [drafts_dir(root), recovery_dir(root)] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            // Bounded like a note read: an oversized or stray file in the drafts
            // directory is skipped, never loaded whole during launch recovery.
            let Ok(bytes) = crate::file::read_bounded(&path, crate::file::MAX_NOTE_BYTES) else {
                continue;
            };
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            found.push((
                modified,
                Draft {
                    id: id.to_owned(),
                    text,
                },
            ));
        }
    }
    found.sort_by_key(|draft| std::cmp::Reverse(draft.0));
    let mut seen = Vec::new();
    let mut unique = Vec::with_capacity(found.len());
    for (_, draft) in found {
        if !seen.contains(&draft.id) {
            seen.push(draft.id.clone());
            unique.push(draft);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            loop {
                let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!("pilcrow-drafts-{}-{id}", std::process::id()));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("could not create {}: {error}", path.display()),
                }
            }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn drafts_round_trip_list_and_remove() {
        let dir = TempDir::new();
        let first = new_id();
        let second = new_id();
        save_to(&dir.0, &first, "first draft").expect("save first");
        save_to(&dir.0, &second, "# second draft\n").expect("save second");

        assert_eq!(read_from(&dir.0, &first).expect("read"), "first draft");
        let drafts = list_from(&dir.0);
        assert_eq!(drafts.len(), 2, "both drafts should be listed");
        assert!(drafts.iter().any(|draft| draft.id == first));
        assert!(drafts.iter().any(|draft| draft.text.contains("second")));

        remove_from(&dir.0, &first);
        assert_eq!(list_from(&dir.0).len(), 1);
    }

    #[test]
    fn recovery_copies_are_listed_and_removed_with_drafts() {
        let dir = TempDir::new();
        let recovery = dir.0.join(RECOVERY_DIR);
        std::fs::create_dir_all(&recovery).expect("recovery dir");
        crate::file::write_atomic(&recovery.join("quit-note-1.md"), b"recovered from quit")
            .expect("write recovery");

        assert_eq!(
            read_from(&dir.0, "quit-note-1").expect("read recovery"),
            "recovered from quit"
        );
        let listed = list_from(&dir.0);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "quit-note-1");
        assert_eq!(listed[0].text, "recovered from quit");

        remove_from(&dir.0, "quit-note-1");
        assert!(list_from(&dir.0).is_empty());
    }

    #[test]
    fn empty_and_invalid_drafts_are_ignored() {
        let dir = TempDir::new();
        save_to(&dir.0, "empty", "  \n").expect("save empty");
        save_to(&dir.0, "valid", "words").expect("save valid");
        assert_eq!(list_from(&dir.0).len(), 1);
        assert_eq!(list_from(&dir.0)[0].text, "words");
    }

    #[test]
    fn an_oversized_draft_is_refused_and_skipped() {
        let dir = TempDir::new();
        let drafts = dir.0.join(DRAFTS_DIR);
        std::fs::create_dir_all(&drafts).expect("drafts dir");
        let huge = drafts.join("huge.md");
        let file = std::fs::File::create(&huge).expect("fixture");
        file.set_len(crate::file::MAX_NOTE_BYTES + 1)
            .expect("sparse");
        drop(file);
        save_to(&dir.0, "valid", "words").expect("save valid");
        assert!(read_from(&dir.0, "huge").is_err());
        let listed = list_from(&dir.0);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].text, "words");
    }

    #[test]
    fn a_missing_root_has_no_drafts() {
        assert!(list_from(Path::new("/nonexistent/pilcrow")).is_empty());
    }
}
