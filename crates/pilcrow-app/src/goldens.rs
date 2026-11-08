//! Golden screenshot runner (feature `goldens`, macOS only).
//!
//! Renders the editor page off-screen with the real platform through
//! GPUI's [`VisualTestAppContext`], captures it with direct texture readback
//! (no Screen Recording permission, no visible window), and compares it
//! against the checked-in baselines in `crates/pilcrow-app/goldens/`.
//!
//! This lives in a binary (`pilcrow-goldens`), not a `#[test]`, because the
//! real macOS platform must be created on the main thread and the standard
//! test harness runs tests on worker threads.
//!
//! ```sh
//! cargo run -p pilcrow-app --features goldens --bin pilcrow-goldens
//! PILCROW_UPDATE_GOLDENS=1 cargo run -p pilcrow-app --features goldens --bin pilcrow-goldens
//! ```
//!
//! Goldens are a manual gate, not a CI gate: pixels depend on the host's
//! fonts, OS version, and display scale. A mismatch writes the actual and
//! diff images to `target/goldens/` for inspection.

#[cfg(not(target_os = "macos"))]
pub fn run() -> i32 {
    eprintln!("pilcrow-goldens is only supported on macOS");
    1
}

#[cfg(target_os = "macos")]
pub fn run() -> i32 {
    match imp::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("pilcrow-goldens: {error:#}");
            1
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use anyhow::{Context as _, Result, bail};
    use gpui::prelude::*;
    use gpui::{Entity, Focusable as _, VisualTestAppContext, div, px, size};
    use image::{ImageBuffer, Rgba, RgbaImage};
    use pilcrow_editor::EditorView;
    use pilcrow_ui::theme::{self, Tokens};

    const FIXTURE: &str = "---\ntitle: Golden fixture\n---\n\n# A short note\n\n\
        Pilcrow is a writing app. The page is the product, and the **words** are the \
        interface. There is *emphasis*, `inline code`, and a [link](https://example.com).\n\n\
        - one list item\n- [ ] an open task\n- [x] a finished task\n\n\
        > A quoted line stays quiet.\n\n\
        ```\nlet page = \"the product\";\n```\n\n---\n\n\
        A final paragraph after the rule, long enough to wrap onto a second visual line \
        when the column is measured for comfortable reading.\n";

    const HERO_FIXTURE: &str = "# The first hour\n\n\
        The trick, if there is one, is to stop negotiating with the blank page and \
        start agreeing with it. Nobody writes a **good sentence** first; they write a \
        sentence and then make it good.\n\n\
        ## Three rules that work\n\n\
        - Write the ugly version first.\n\
        - [ ] Keep the note open until the thought is finished.\n\
        - [x] Leave the house before noon.\n\n\
        > The first hour is the one that counts. It is also the one most easily spent \
        deciding where to begin.\n\n\
        ```rust\nfn main() {\n    println!(\"the page is the product\");\n}\n```\n";

    /// Per-channel tolerance for antialiasing differences.
    const PER_PIXEL_THRESHOLD: u8 = 8;
    /// Fraction of pixels allowed to differ past the threshold.
    const MAX_DIFF_FRACTION: f64 = 0.005;

    /// The editor on its paper background, as the note window presents it.
    /// The editor itself paints no window background, so capturing it bare
    /// would record light text on the renderer's black clear color.
    struct GoldenPage {
        editor: Entity<EditorView>,
        tokens: Tokens,
    }

    impl Render for GoldenPage {
        fn render(
            &mut self,
            _window: &mut gpui::Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            div()
                .size_full()
                .bg(self.tokens.paper)
                .child(self.editor.clone())
        }
    }

    struct Case {
        name: &'static str,
        tokens: Tokens,
        fixture: &'static str,
        /// `(anchor, caret)` selection to restore before capturing.
        selection: Option<(usize, usize)>,
    }

    pub fn run() -> Result<i32> {
        let update = std::env::var_os("PILCROW_UPDATE_GOLDENS").is_some_and(|value| {
            !matches!(
                value.to_string_lossy().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        });
        let goldens_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens");
        let selection_start = FIXTURE.find("words** are").context("fixture drift")? + 2;
        let cases = [
            Case {
                name: "page-light",
                tokens: theme::light(),
                fixture: FIXTURE,
                selection: None,
            },
            Case {
                name: "page-dark",
                tokens: theme::dark(),
                fixture: FIXTURE,
                selection: None,
            },
            Case {
                name: "selection-light",
                tokens: theme::light(),
                fixture: FIXTURE,
                selection: Some((selection_start, selection_start + 5)),
            },
            Case {
                name: "hero-dark",
                tokens: theme::dark(),
                fixture: HERO_FIXTURE,
                selection: None,
            },
        ];

        let mut cx = VisualTestAppContext::new(gpui_platform::current_platform(false));
        cx.update(pilcrow_editor::install_fonts);
        let mut failures = 0;
        for case in &cases {
            match check_case(&mut cx, case, &goldens_dir, update) {
                Ok(true) => println!("{}: ok", case.name),
                Ok(false) => failures += 1,
                Err(error) => {
                    eprintln!("{}: {error:#}", case.name);
                    failures += 1;
                }
            }
        }
        Ok(if failures == 0 { 0 } else { 1 })
    }

    fn check_case(
        cx: &mut VisualTestAppContext,
        case: &Case,
        goldens_dir: &Path,
        update: bool,
    ) -> Result<bool> {
        let tokens = case.tokens;
        let window = cx.open_offscreen_window(size(px(900.0), px(700.0)), |_window, cx| {
            let editor = cx.new(|cx| EditorView::new(cx, case.fixture, tokens));
            cx.new(|_cx| GoldenPage { editor, tokens })
        })?;
        cx.update_window(window.into(), |view, window, cx| {
            let page = view.downcast::<GoldenPage>().expect("golden page root");
            let editor = page.read(cx).editor.clone();
            editor.update(cx, |editor, cx| {
                editor.focus_handle(cx).focus(window, cx);
                if let Some((anchor, caret)) = case.selection {
                    editor.restore_view_state(caret, Some(anchor), 0.0, false, false, false, cx);
                }
            });
        })
        .context("focus editor")?;
        // Let layout, shaping, and the caret settle on the simulated clock.
        cx.run_until_parked();
        cx.advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        let actual = cx
            .capture_screenshot(window.into())
            .context("capture screenshot")?;
        cx.update_window(window.into(), |_editor, window, _cx| {
            window.remove_window();
        })
        .ok();
        cx.run_until_parked();

        let baseline_path = goldens_dir.join(format!("{}.png", case.name));
        if update {
            std::fs::create_dir_all(goldens_dir)?;
            actual
                .save(&baseline_path)
                .with_context(|| format!("save {}", baseline_path.display()))?;
            println!("{}: baseline updated", case.name);
            return Ok(true);
        }
        if !baseline_path.is_file() {
            bail!(
                "missing baseline {}; run with PILCROW_UPDATE_GOLDENS=1",
                baseline_path.display()
            );
        }
        let expected = image::open(&baseline_path)
            .with_context(|| format!("open {}", baseline_path.display()))?
            .to_rgba8();
        let comparison = compare(&actual, &expected);
        if comparison.matches() {
            return Ok(true);
        }
        let out_dir = artifact_dir()?;
        let actual_path = out_dir.join(format!("{}-actual.png", case.name));
        actual.save(&actual_path).ok();
        if let Some(diff) = &comparison.diff_image {
            diff.save(out_dir.join(format!("{}-diff.png", case.name)))
                .ok();
        }
        eprintln!(
            "{}: {:.2}% of pixels match ({} differ); actual saved to {}",
            case.name,
            comparison.match_fraction * 100.0,
            comparison.diff_pixels,
            actual_path.display()
        );
        Ok(false)
    }

    fn artifact_dir() -> Result<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/goldens")
            .canonicalize()
            .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/goldens"));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    struct Comparison {
        match_fraction: f64,
        diff_pixels: u64,
        diff_image: Option<RgbaImage>,
    }

    impl Comparison {
        fn matches(&self) -> bool {
            self.match_fraction >= 1.0 - MAX_DIFF_FRACTION
        }
    }

    fn compare(actual: &RgbaImage, expected: &RgbaImage) -> Comparison {
        let (width, height) = actual.dimensions();
        if actual.dimensions() != expected.dimensions() {
            return Comparison {
                match_fraction: 0.0,
                diff_pixels: u64::from(width) * u64::from(height),
                diff_image: None,
            };
        }
        let mut diff: RgbaImage = ImageBuffer::new(width, height);
        let mut diff_pixels = 0u64;
        for (x, y, pixel) in actual.enumerate_pixels() {
            let expected_pixel = expected.get_pixel(x, y);
            let similar = pixel
                .0
                .iter()
                .zip(expected_pixel.0)
                .all(|(a, b)| a.abs_diff(b) <= PER_PIXEL_THRESHOLD);
            if similar {
                diff.put_pixel(x, y, Rgba([0, 128, 0, 255]));
            } else {
                diff_pixels += 1;
                diff.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let total = u64::from(width) * u64::from(height);
        Comparison {
            match_fraction: (total - diff_pixels) as f64 / total as f64,
            diff_pixels,
            diff_image: Some(diff),
        }
    }
}
