//! Last-session paths and recent notes.
//!
//! The session is a small JSON file in the app data root. It stores only
//! paths and view positions, never document text: the note file remains the
//! only copy of the words. Missing or malformed entries are ignored so a
//! stale session cannot stop the app from opening.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SESSION_FILE: &str = "session.json";
pub const MAX_RECENT: usize = 10;
/// A session file holds paths and view positions only; anything past this
/// size is not a session this app wrote and is treated as corrupt.
const MAX_SESSION_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub windows: Vec<SavedWindow>,
    pub recent: Vec<PathBuf>,
}

/// A window's frame in global display coordinates. Stored so a restored
/// session reopens windows where they were left; validated against the
/// connected displays at restore time, since monitors change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedFrame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The visible rectangle of one connected display, in frame coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedWindow {
    pub path: PathBuf,
    pub caret: usize,
    pub anchor: Option<usize>,
    pub scroll: f32,
    pub reading: bool,
    pub focus: bool,
    pub typewriter: bool,
    /// Where the window was. `None` in sessions written before frames were
    /// recorded.
    pub frame: Option<SavedFrame>,
    /// The window was maximized; `frame` is then its restore size.
    pub maximized: bool,
}

pub fn path(root: &Path) -> PathBuf {
    root.join(SESSION_FILE)
}

pub fn load(root: &Path) -> Session {
    let file = path(root);
    let Ok(bytes) = crate::file::read_bounded(&file, MAX_SESSION_BYTES) else {
        return Session::default();
    };
    let mut session: Session = serde_json::from_slice(&bytes).unwrap_or_default();
    session.windows.retain(|window| window.path.is_file());
    let mut seen_windows = Vec::new();
    session.windows.retain(|window| {
        if seen_windows.contains(&window.path) {
            false
        } else {
            seen_windows.push(window.path.clone());
            true
        }
    });
    session.recent.retain(|recent| recent.is_file());
    dedupe(&mut session.recent);
    session.recent.truncate(MAX_RECENT);
    // A frame no display could ever show (hand-edited file, nonsense
    // numbers) is dropped here; display fit is judged again at restore.
    for window in &mut session.windows {
        if window.frame.is_some_and(|frame| !sane_frame(frame)) {
            window.frame = None;
            window.maximized = false;
        }
    }
    session
}

fn sane_frame(frame: SavedFrame) -> bool {
    frame.x.is_finite()
        && frame.y.is_finite()
        && frame.width.is_finite()
        && frame.height.is_finite()
        && frame.x.abs() <= 32_768.0
        && frame.y.abs() <= 32_768.0
        && (100.0..=16_384.0).contains(&frame.width)
        && (100.0..=16_384.0).contains(&frame.height)
}

fn overlap(frame: SavedFrame, display: DisplayRect) -> f32 {
    let width = (frame.x + frame.width).min(display.x + display.width) - frame.x.max(display.x);
    let height = (frame.y + frame.height).min(display.y + display.height) - frame.y.max(display.y);
    width.max(0.0) * height.max(0.0)
}

/// Adjust a saved frame for the displays connected now. `None` when the
/// frame intersects no display (its monitor is gone): the caller falls
/// back to default placement. Otherwise the frame is nudged fully inside
/// the display it overlaps most, keeping as much of its size as fits.
pub fn clamp_frame(frame: SavedFrame, displays: &[DisplayRect]) -> Option<SavedFrame> {
    let best = displays
        .iter()
        .copied()
        .max_by(|a, b| overlap(frame, *a).total_cmp(&overlap(frame, *b)))?;
    if overlap(frame, best) <= 0.0 {
        return None;
    }
    let width = frame.width.min(best.width);
    let height = frame.height.min(best.height);
    Some(SavedFrame {
        x: frame
            .x
            .clamp(best.x, (best.x + best.width - width).max(best.x)),
        y: frame
            .y
            .clamp(best.y, (best.y + best.height - height).max(best.y)),
        width,
        height,
    })
}

pub fn save(root: &Path, session: &Session) -> Result<()> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("could not create {}", root.display()))?;
    let file = path(root);
    let bytes = serde_json::to_vec_pretty(session)
        .with_context(|| format!("could not serialize {}", file.display()))?;
    crate::file::write_atomic(&file, &bytes)
        .with_context(|| format!("could not save {}", file.display()))?;
    Ok(())
}

/// Put `path` first, removing duplicates and keeping the list bounded.
pub fn remember(recent: &mut Vec<PathBuf>, path: &Path) {
    let path = path.to_path_buf();
    recent.retain(|entry| entry != &path);
    recent.insert(0, path);
    recent.truncate(MAX_RECENT);
}

fn dedupe(recent: &mut Vec<PathBuf>) {
    let mut seen = Vec::new();
    recent.retain(|path| {
        if seen.contains(path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
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
                let path = base.join(format!("pilcrow-session-{}-{id}", std::process::id()));
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
    fn missing_and_corrupt_sessions_are_empty() {
        let dir = TempDir::new();
        assert_eq!(load(&dir.0), Session::default());
        std::fs::write(path(&dir.0), b"not json").expect("fixture");
        assert_eq!(load(&dir.0), Session::default());
    }

    #[test]
    fn an_oversized_session_is_treated_as_corrupt() {
        let dir = TempDir::new();
        let file = std::fs::File::create(path(&dir.0)).expect("fixture");
        file.set_len(MAX_SESSION_BYTES + 1).expect("sparse");
        drop(file);
        assert_eq!(load(&dir.0), Session::default());
    }

    #[test]
    fn round_trip_keeps_windows_and_prunes_missing_paths() {
        let dir = TempDir::new();
        let existing = dir.0.join("note.md");
        std::fs::write(&existing, "words").expect("fixture");
        let missing = dir.0.join("gone.md");
        let session = Session {
            windows: vec![
                SavedWindow {
                    path: existing.clone(),
                    caret: 3,
                    anchor: Some(1),
                    scroll: 12.5,
                    reading: true,
                    focus: false,
                    typewriter: true,
                    frame: Some(SavedFrame {
                        x: 40.0,
                        y: 30.0,
                        width: 900.0,
                        height: 600.0,
                    }),
                    maximized: true,
                },
                SavedWindow {
                    path: missing,
                    ..SavedWindow::default()
                },
            ],
            recent: vec![existing.clone(), dir.0.join("gone.md")],
        };
        save(&dir.0, &session).expect("save");
        let loaded = load(&dir.0);
        assert_eq!(loaded.windows.len(), 1);
        assert_eq!(loaded.windows[0].path, existing);
        assert_eq!(loaded.windows[0].caret, 3);
        assert!(loaded.windows[0].reading);
        assert_eq!(
            loaded.windows[0].frame,
            Some(SavedFrame {
                x: 40.0,
                y: 30.0,
                width: 900.0,
                height: 600.0,
            })
        );
        assert!(loaded.windows[0].maximized);
        assert_eq!(loaded.recent, vec![existing]);
    }

    #[test]
    fn clamp_frame_fits_the_displays_connected_now() {
        let main = DisplayRect {
            x: 0.0,
            y: 0.0,
            width: 1440.0,
            height: 900.0,
        };
        let side = DisplayRect {
            x: 1440.0,
            y: -200.0,
            width: 1024.0,
            height: 768.0,
        };
        let frame = SavedFrame {
            x: 40.0,
            y: 30.0,
            width: 900.0,
            height: 600.0,
        };
        // Fully visible: untouched.
        assert_eq!(clamp_frame(frame, &[main, side]), Some(frame));
        // Partly off the right/bottom: nudged fully inside, size kept.
        assert_eq!(
            clamp_frame(
                SavedFrame {
                    x: 1200.0,
                    y: 700.0,
                    ..frame
                },
                &[main]
            ),
            Some(SavedFrame {
                x: 540.0,
                y: 300.0,
                ..frame
            })
        );
        // Mostly on the side display: that display wins.
        let on_side = SavedFrame {
            x: 1500.0,
            y: -100.0,
            width: 500.0,
            height: 400.0,
        };
        assert_eq!(clamp_frame(on_side, &[main, side]), Some(on_side));
        // On a monitor that is gone, or no displays at all: no frame.
        assert_eq!(
            clamp_frame(
                SavedFrame {
                    x: 5000.0,
                    y: 0.0,
                    ..frame
                },
                &[main, side]
            ),
            None
        );
        assert_eq!(clamp_frame(frame, &[]), None);
        // Larger than the display: shrunk to fit.
        assert_eq!(
            clamp_frame(
                SavedFrame {
                    x: -50.0,
                    y: -50.0,
                    width: 2000.0,
                    height: 2000.0,
                },
                &[main]
            ),
            Some(SavedFrame {
                x: 0.0,
                y: 0.0,
                width: 1440.0,
                height: 900.0,
            })
        );
    }

    #[test]
    fn an_insane_frame_is_dropped_on_load() {
        let dir = TempDir::new();
        let existing = dir.0.join("note.md");
        std::fs::write(&existing, "words").expect("fixture");
        let session = Session {
            windows: vec![SavedWindow {
                path: existing,
                frame: Some(SavedFrame {
                    x: 1.0e9,
                    y: 0.0,
                    width: -3.0,
                    height: 600.0,
                }),
                maximized: true,
                ..SavedWindow::default()
            }],
            recent: Vec::new(),
        };
        save(&dir.0, &session).expect("save");
        let loaded = load(&dir.0);
        assert_eq!(loaded.windows[0].frame, None);
        assert!(!loaded.windows[0].maximized);
    }

    #[test]
    fn remember_moves_to_front_dedupes_and_bounds() {
        let mut recent: Vec<PathBuf> = (0..MAX_RECENT)
            .map(|i| PathBuf::from(format!("{i}")))
            .collect();
        remember(&mut recent, Path::new("5"));
        assert_eq!(recent[0], PathBuf::from("5"));
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(
            recent.iter().filter(|path| *path == Path::new("5")).count(),
            1
        );
        remember(&mut recent, Path::new("new"));
        assert_eq!(recent[0], PathBuf::from("new"));
        assert_eq!(recent.len(), MAX_RECENT);
    }
}
