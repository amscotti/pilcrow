//! The small preferences file.
//!
//! Preferences are deliberately tiny and readable: appearance is the first
//! setting because the page has to agree with the operating system before a
//! person types their first word. Per-note modes belong with the note, not
//! here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use pilcrow_ui::theme::Tokens;

/// How the page chooses light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the operating system's current window appearance.
    #[default]
    Auto,
    /// Always use the light page.
    Light,
    /// Always use the dark page.
    Dark,
}

impl Appearance {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The appearance named by `PILCROW_THEME`, used by tests and previews.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" | "system" => Some(Self::Auto),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Reader-selected line length, in `x` characters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Measure {
    Narrow,
    #[default]
    Standard,
    Wide,
}

impl Measure {
    pub fn chars(self) -> usize {
        match self {
            Self::Narrow => 56,
            // Single-sourced with the editor's default column so a new window
            // and the Standard preference can never disagree.
            Self::Standard => pilcrow_ui::typography::MEASURE_CHARS,
            Self::Wide => 76,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Narrow => "Narrow",
            Self::Standard => "Standard",
            Self::Wide => "Wide",
        }
    }
}

/// How hard the page pushes its ink against its paper.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contrast {
    /// The default warm palettes (already ≥12:1 body text).
    #[default]
    Standard,
    /// True black/white palettes with every text role at WCAG AAA.
    High,
}

impl Contrast {
    /// The contrast named by `PILCROW_CONTRAST`, used by tests and previews.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "standard" | "normal" => Some(Self::Standard),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    pub appearance: Appearance,
    /// Body scale, clamped to the five supported stops at use time.
    #[serde(default = "default_text_scale")]
    pub text_scale: f32,
    #[serde(default)]
    pub measure: Measure,
    #[serde(default)]
    pub contrast: Contrast,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            appearance: Appearance::Auto,
            text_scale: default_text_scale(),
            measure: Measure::default(),
            contrast: Contrast::default(),
        }
    }
}

pub const TEXT_SCALES: [f32; 5] = [0.875, 1.0, 1.125, 1.25, 1.375];

fn default_text_scale() -> f32 {
    1.0
}

/// The next supported stop above/below `current`.
pub fn text_scale_step(current: f32, up: bool) -> f32 {
    if up {
        TEXT_SCALES
            .into_iter()
            .find(|scale| *scale > current + 0.001)
            .unwrap_or(*TEXT_SCALES.last().unwrap())
    } else {
        TEXT_SCALES
            .into_iter()
            .rev()
            .find(|scale| *scale < current - 0.001)
            .unwrap_or(TEXT_SCALES[0])
    }
}

impl Prefs {
    pub fn with_env_override(mut self) -> Self {
        if let Some(value) = std::env::var_os("PILCROW_THEME")
            .and_then(|value| value.into_string().ok())
            .as_deref()
            .and_then(Appearance::parse)
        {
            self.appearance = value;
        }
        if let Some(value) = std::env::var_os("PILCROW_CONTRAST")
            .and_then(|value| value.into_string().ok())
            .as_deref()
            .and_then(Contrast::parse)
        {
            self.contrast = value;
        }
        self
    }
}

/// The application data root.
pub fn data_root() -> PathBuf {
    data_root_from(std::env::var_os("PILCROW_DATA_DIR"))
}

fn data_root_from(override_dir: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(dir) = override_dir {
        if dir.is_empty() {
            panic!("PILCROW_DATA_DIR is set but empty; unset it or name a directory");
        }
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join("Library/Application Support/PilcrowNext");
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA")
        && !appdata.is_empty()
    {
        return PathBuf::from(appdata).join("PilcrowNext");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(data) = std::env::var_os("XDG_DATA_HOME")
            && !data.is_empty()
        {
            return PathBuf::from(data).join("pilcrow-next");
        }
        if let Some(home) = std::env::var_os("HOME")
            && !home.is_empty()
        {
            return PathBuf::from(home).join(".local/share/pilcrow-next");
        }
    }
    std::env::temp_dir().join("pilcrow-next")
}

pub fn prefs_path() -> PathBuf {
    data_root().join("settings.json")
}

/// Read the preferences. A missing or corrupt file is a first run, not an
/// error: defaults are always a usable writing app.
pub fn load() -> Prefs {
    load_from(&prefs_path())
}

/// Preferences are three small fields; anything past this size is not a
/// settings file this app wrote and is treated like a corrupt one.
const MAX_PREFS_BYTES: u64 = 1024 * 1024;

pub fn load_from(path: &Path) -> Prefs {
    match crate::file::read_bounded(path, MAX_PREFS_BYTES) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Prefs::default(),
    }
}

/// Write the preferences atomically. Called on the background executor.
pub fn save(prefs: Prefs) -> Result<()> {
    save_to(&prefs_path(), prefs)
}

pub fn save_to(path: &Path, prefs: Prefs) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(&prefs)?;
    crate::file::write_atomic(path, &body)
        .with_context(|| format!("could not save {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
pub fn current_for_test(cx: &gpui::App) -> Prefs {
    let prefs = crate::current_prefs(cx);
    Prefs {
        appearance: prefs.appearance,
        text_scale: prefs.text_scale,
        measure: prefs.measure,
        contrast: prefs.contrast,
    }
}

/// The tokens for a preference and the window's current system appearance.
pub fn tokens(prefs: Prefs, appearance: gpui::WindowAppearance) -> Tokens {
    let dark = match prefs.appearance {
        Appearance::Light => false,
        Appearance::Dark => true,
        Appearance::Auto => matches!(
            appearance,
            gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
        ),
    };
    match (dark, prefs.contrast) {
        (false, Contrast::Standard) => pilcrow_ui::theme::light(),
        (true, Contrast::Standard) => pilcrow_ui::theme::dark(),
        (false, Contrast::High) => pilcrow_ui::theme::light_high_contrast(),
        (true, Contrast::High) => pilcrow_ui::theme::dark_high_contrast(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            loop {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!(
                    "pilcrow-prefs-{}-{}-{id}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|duration| duration.as_nanos())
                        .unwrap_or_default()
                ));
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
    fn measures_are_three_readable_stops() {
        assert_eq!(Measure::Narrow.chars(), 56);
        assert_eq!(Measure::Standard.chars(), 66);
        assert_eq!(
            Measure::Standard.chars(),
            pilcrow_ui::typography::MEASURE_CHARS
        );
        assert_eq!(Measure::Wide.chars(), 76);
    }

    #[test]
    fn an_oversized_settings_file_falls_back_to_defaults() {
        let dir = TempDir::new();
        let path = dir.0.join("settings.json");
        let file = std::fs::File::create(&path).expect("fixture");
        file.set_len(MAX_PREFS_BYTES + 1).expect("sparse");
        drop(file);
        assert_eq!(load_from(&path), Prefs::default());
    }

    #[test]
    fn text_scale_steps_clamp_to_the_five_stops() {
        assert_eq!(text_scale_step(1.0, true), 1.125);
        assert_eq!(text_scale_step(1.0, false), 0.875);
        assert_eq!(text_scale_step(1.375, true), 1.375);
        assert_eq!(text_scale_step(0.875, false), 0.875);
    }

    #[test]
    fn appearance_cycles_and_labels() {
        assert_eq!(Appearance::Auto.next(), Appearance::Light);
        assert_eq!(Appearance::Light.next(), Appearance::Dark);
        assert_eq!(Appearance::Dark.next(), Appearance::Auto);
        assert_eq!(Appearance::parse("system"), Some(Appearance::Auto));
        assert_eq!(Appearance::parse("DARK"), Some(Appearance::Dark));
        assert_eq!(Appearance::parse("sepia"), None);
    }

    #[test]
    fn prefs_round_trip_and_corrupt_files_fall_back_to_defaults() {
        let dir = TempDir::new();
        let path = dir.0.join("settings.json");
        let prefs = Prefs {
            appearance: Appearance::Dark,
            text_scale: 1.25,
            measure: Measure::Wide,
            contrast: Contrast::High,
        };
        save_to(&path, prefs).expect("save");
        assert_eq!(load_from(&path), prefs);

        std::fs::write(&path, b"not json").expect("corrupt");
        assert_eq!(load_from(&path), Prefs::default());
        assert_eq!(load_from(&dir.0.join("missing.json")), Prefs::default());
    }

    #[test]
    fn auto_follows_the_window_appearance_and_explicit_wins() {
        let auto = Prefs {
            appearance: Appearance::Auto,
            text_scale: 1.0,
            measure: Measure::Standard,
            contrast: Contrast::Standard,
        };
        assert_eq!(
            tokens(auto, gpui::WindowAppearance::Dark).paper,
            pilcrow_ui::theme::dark().paper
        );
        assert_eq!(
            tokens(auto, gpui::WindowAppearance::Light).paper,
            pilcrow_ui::theme::light().paper
        );
        let light = Prefs {
            appearance: Appearance::Light,
            text_scale: 1.0,
            measure: Measure::Standard,
            contrast: Contrast::Standard,
        };
        assert_eq!(
            tokens(light, gpui::WindowAppearance::Dark).paper,
            pilcrow_ui::theme::light().paper
        );

        let high = Prefs {
            contrast: Contrast::High,
            ..auto
        };
        assert_eq!(
            tokens(high, gpui::WindowAppearance::Light),
            pilcrow_ui::theme::light_high_contrast()
        );
        assert_eq!(
            tokens(high, gpui::WindowAppearance::Dark),
            pilcrow_ui::theme::dark_high_contrast()
        );
        assert_eq!(Contrast::parse("HIGH"), Some(Contrast::High));
        assert_eq!(Contrast::parse("standard"), Some(Contrast::Standard));
        assert_eq!(Contrast::parse("more"), None);
    }
    #[test]
    #[should_panic(expected = "PILCROW_DATA_DIR is set but empty")]
    fn an_empty_data_override_is_an_error() {
        let _ = data_root_from(Some(std::ffi::OsString::new()));
    }
}
