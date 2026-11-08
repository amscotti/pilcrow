//! Quiet local version history.
//!
//! Every successful save drops the saved bytes into a per-note backup
//! directory under the app data root. Retention keeps the newest snapshot in
//! each of the last 24 hours and the newest in each of the last 14 days, so a
//! day of typing does not turn into thousands of files. This is not a sync
//! service and never touches the note's own directory.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

pub const BACKUPS_DIR: &str = "backups";
const HOUR_MS: u64 = 60 * 60 * 1000;
const DAY_MS: u64 = 24 * HOUR_MS;
const KEEP_HOURS: u64 = 24;
const KEEP_DAYS: u64 = 14;

/// Save one version of `path` with the bytes that were just written.
pub fn snapshot(root: &Path, path: &Path, bytes: &[u8]) -> Result<Option<PathBuf>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64);
    snapshot_at(root, path, bytes, now)
}

fn snapshot_at(root: &Path, path: &Path, bytes: &[u8], now_millis: u64) -> Result<Option<PathBuf>> {
    let directory = note_dir(root, path);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;
    let versions = versions_in(&directory);
    // Dedupe reads the newest backup only when its size already matches, and
    // never past the note limit: a stray huge file in a backup directory must
    // not be loaded whole on every save.
    if let Some(latest) = versions.first()
        && std::fs::metadata(latest).is_ok_and(|meta| meta.len() == bytes.len() as u64)
        && let Ok(current) = crate::file::read_bounded(latest, crate::file::MAX_NOTE_BYTES)
        && current == bytes
    {
        return Ok(None);
    }
    let mut stamp = now_millis;
    let mut target = directory.join(format!("{stamp}.md"));
    while target.exists() {
        stamp += 1;
        target = directory.join(format!("{stamp}.md"));
    }
    crate::file::write_atomic(&target, bytes)
        .with_context(|| format!("could not write backup {}", target.display()))?;
    prune(&directory);
    Ok(Some(target))
}

/// Newest first. Useful for a future Browse Versions view; the save path only
/// needs the newest entry for duplicate detection.
pub fn versions(root: &Path, path: &Path) -> Vec<PathBuf> {
    versions_in(&note_dir(root, path))
}

fn versions_in(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut versions: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                return None;
            }
            let stamp = path.file_stem()?.to_str()?.parse().ok()?;
            Some((stamp, path))
        })
        .collect();
    versions.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    versions.into_iter().map(|(_, path)| path).collect()
}

fn prune(directory: &Path) {
    let versions = versions_in(directory);
    let Some((newest, _)) = versions.first().and_then(|path| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
            .map(|stamp| (stamp, path))
    }) else {
        return;
    };
    let mut hours = Vec::new();
    let mut days = Vec::new();
    let mut keep = Vec::new();
    for (index, path) in versions.iter().enumerate() {
        let Some(stamp) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<u64>().ok())
        else {
            continue;
        };
        let age = newest.saturating_sub(stamp);
        let hour = stamp / HOUR_MS;
        let day = stamp / DAY_MS;
        if index < 3 {
            if !hours.contains(&hour) {
                hours.push(hour);
            }
            if !days.contains(&day) {
                days.push(day);
            }
            keep.push(path.clone());
        } else if age <= KEEP_HOURS * HOUR_MS {
            if !hours.contains(&hour) {
                hours.push(hour);
                keep.push(path.clone());
            }
        } else if age <= KEEP_DAYS * DAY_MS && !days.contains(&day) {
            days.push(day);
            keep.push(path.clone());
        }
    }
    for path in versions {
        if !keep.contains(&path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn note_dir(root: &Path, path: &Path) -> PathBuf {
    let canonical = crate::file::identity(path).unwrap_or_else(|_| path.to_path_buf());
    let stem = canonical
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("note");
    let safe: String = stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ' ') {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    root.join(BACKUPS_DIR)
        .join(format!("{safe}-{:016x}", path_key(&canonical)))
}

fn path_key(path: &Path) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in path.as_os_str().to_string_lossy().as_bytes() {
        let folded = if cfg!(any(target_os = "macos", target_os = "windows")) {
            byte.to_ascii_lowercase()
        } else {
            *byte
        };
        hash ^= u64::from(folded);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            loop {
                let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!("pilcrow-backups-{}-{id}", std::process::id()));
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
    fn an_oversized_newest_backup_does_not_block_a_snapshot() {
        let dir = TempDir::new();
        let note = dir.0.join("note.md");
        snapshot_at(&dir.0, &note, b"one", 1_000).expect("first");
        let newest = versions(&dir.0, &note)[0].clone();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&newest)
            .expect("open newest");
        file.set_len(crate::file::MAX_NOTE_BYTES + 1)
            .expect("sparse");
        drop(file);
        let saved = snapshot_at(&dir.0, &note, b"two", 2_000).expect("second");
        assert!(saved.is_some());
        assert_eq!(
            std::fs::read(&versions(&dir.0, &note)[0]).expect("newest"),
            b"two"
        );
    }

    #[test]
    fn identical_content_is_not_snapshotted_twice() {
        let dir = TempDir::new();
        let note = dir.0.join("note.md");
        let first = snapshot_at(&dir.0, &note, b"one", 1_000).expect("first");
        assert!(first.is_some());
        assert!(
            snapshot_at(&dir.0, &note, b"one", 2_000)
                .expect("duplicate")
                .is_none()
        );
        assert_eq!(versions(&dir.0, &note).len(), 1);
        snapshot_at(&dir.0, &note, b"two", 3_000).expect("second");
        assert_eq!(versions(&dir.0, &note).len(), 2);
        assert_eq!(
            std::fs::read(&versions(&dir.0, &note)[0]).expect("newest"),
            b"two"
        );
    }

    #[test]
    fn retention_keeps_hourly_and_daily_representatives() {
        let dir = TempDir::new();
        let note = dir.0.join("note.md");
        let start = 10 * DAY_MS;
        for minute in 0..600 {
            let stamp = start + minute * 60_000;
            snapshot_at(&dir.0, &note, format!("v{minute}").as_bytes(), stamp).expect("snapshot");
        }
        let versions = versions(&dir.0, &note);
        assert!(versions.len() <= (KEEP_HOURS + KEEP_DAYS) as usize);
        assert!(
            versions.len() >= 10,
            "pruned too aggressively: {versions:?}"
        );
    }

    #[test]
    fn versions_are_per_note_and_survive_a_new_root() {
        let dir = TempDir::new();
        let first = dir.0.join("a.md");
        let second = dir.0.join("b.md");
        snapshot_at(&dir.0, &first, b"a", 1_000).expect("a");
        snapshot_at(&dir.0, &second, b"b", 1_000).expect("b");
        assert_eq!(versions(&dir.0, &first).len(), 1);
        assert_eq!(versions(&dir.0, &second).len(), 1);
        assert_ne!(versions(&dir.0, &first)[0], versions(&dir.0, &second)[0]);

        let dotted = dir.0.join(".").join("a.md");
        assert_eq!(versions(&dir.0, &dotted), versions(&dir.0, &first));
    }

    #[test]
    fn intra_hour_duplicates_do_not_consume_daily_retention() {
        let dir = TempDir::new();
        let note = dir.0.join("note.md");
        let base = 10 * DAY_MS;
        let older_hour = base - 2 * HOUR_MS;
        snapshot_at(&dir.0, &note, b"old-early", older_hour + 1_000).expect("old early");
        snapshot_at(&dir.0, &note, b"old-late", older_hour + 2_000).expect("old late");
        snapshot_at(&dir.0, &note, b"first", base + 1_000).expect("first");
        snapshot_at(&dir.0, &note, b"second", base + 2_000).expect("second");
        snapshot_at(&dir.0, &note, b"third", base + 3_000).expect("third");
        snapshot_at(&dir.0, &note, b"fourth", base + 4_000).expect("fourth");
        let kept = versions(&dir.0, &note);
        assert_eq!(
            kept.len(),
            4,
            "keeps the three latest plus one representative for the older hour: {kept:?}"
        );
        assert_eq!(std::fs::read(&kept[0]).expect("read"), b"fourth");
        assert_eq!(std::fs::read(&kept[1]).expect("read"), b"third");
        assert_eq!(std::fs::read(&kept[2]).expect("read"), b"second");
        assert_eq!(std::fs::read(&kept[3]).expect("read"), b"old-late");
    }
}
