# GPUI version and upgrade policy

**Status:** current pin

**Recorded:** 2026-11-06

**Verified:** 2026-11-08

## Current pin

```toml
# Workspace
[workspace.dependencies]
gpui = { git = "https://github.com/zed-industries/zed", rev = "db43f05e398036f08e544c03cbc068216c7a0aaa" }
gpui_platform = { git = "https://github.com/zed-industries/zed", rev = "db43f05e398036f08e544c03cbc068216c7a0aaa" }

# App crate: enable only the features the target needs.
[target.'cfg(target_os = "macos")'.dependencies]
gpui_platform = { workspace = true, features = ["font-kit"] }

[target.'cfg(any(target_os = "linux", target_os = "freebsd"))'.dependencies]
gpui_platform = { workspace = true, features = ["wayland", "x11"] }

[target.'cfg(target_os = "windows")'.dependencies]
gpui_platform = { workspace = true }
```

- `db43f05e398036f08e544c03cbc068216c7a0aaa` is Zed `main` as inspected on
  2026-11-06 (`markdown: Respect preview theme for bold text (#63940)`).
- The workspace builds, tests, and bundles against this commit. If a platform
  target needs a different commit, move both crates together and update this
  file.
- `gpui` and every `gpui_*` platform crate must come from the same commit.
  Never mix a git `gpui` with a crates.io `gpui_platform` or a `gpui-pre` crate.

## Why not crates.io

As of 2026-11-06:

- `gpui` on crates.io is **0.2.2**, published 2025-10-22. It is behind Zed
  `main` and does not include the current `gpui_platform` split.
- `gpui_platform` is **not published** on crates.io.
- `gpui-pre` (0.3.11, 2026-11-02) is a third-party crates.io snapshot of Zed
  `f816e1f`. It is a valid fallback if git dependencies are operationally
  unacceptable, but it is a different dependency family and its API will
  differ. Do not mix families.
- The current official README uses `gpui_platform::application()` and
  `gpui_platform` features (`font-kit`, `wayland`, `x11`); the app should follow
  the official path.

## Platform features

```toml
# macOS
gpui_platform = { ..., features = ["font-kit"] }

# Linux / FreeBSD (at least one windowing backend; both for CI)
gpui_platform = { ..., features = ["wayland", "x11"] }

# Windows: no extra features
gpui_platform = { ... }
```

- macOS uses Metal; `font-kit` is required for glyph rasterization. Without it,
  text lays out but renders no glyphs.
- Linux uses wgpu and needs X11 or Wayland at build/run time; Linux text uses
  cosmic-text.
- Windows uses DirectX 11 and DirectWrite.
- `test-support` belongs in `[dev-dependencies]`, not the app's normal
  features.

## Verified API notes for this pin

These are the things that matter to the editor and app shell. Re-verify after
any bump.

- **Application:** `gpui_platform::application().run(|cx: &mut App| { ... })`.
- **Window:** `cx.open_window(WindowOptions { window_bounds, titlebar,
  window_min_size, window_decorations, window_background, .. }, |window, cx| cx.new(...))`.
- **Titlebar:** `TitlebarOptions { title, appears_transparent, traffic_light_position }`.
  `WindowOptions.app_owns_titlebar_drag` exists for a custom macOS titlebar.
- **Custom element:** `impl Element` with `request_layout`, `prepaint`, `paint`;
  `window.request_layout(style, [], cx)`; `window.paint_quad`, `window.paint_path`.
- **Text:** `window.text_system().shape_line(text.into(), font_size, &runs, None)`
  returns `ShapedLine`; `x_for_index`, `closest_index_for_x`, `paint(origin,
  line_height, TextAlign::Left, None, window, cx)`. `shape_text` wraps;
  `layout_line` shapes one line.
- **Text runs:** `TextRun { len, font, color, background_color, underline,
  strikethrough }`. `UnderlineStyle { thickness, color, wavy }` supports the
  spellcheck squiggle.
- **Input:** `impl EntityInputHandler`; `window.handle_input(&focus_handle,
  ElementInputHandler::new(bounds, entity), cx)`; `notify_text_input_changed`,
  `notify_text_selection_changed`.
- **Actions:** `actions!` / `#[derive(Action)]`; `cx.bind_keys([KeyBinding::new("cmd-s",
  Save, None)])`; `.key_context("Editor").on_action(cx.listener(...))`.
- **Menus:** `cx.set_menus([Menu::new("File").items([MenuItem::action("Save", Save)])])`;
  `cx.get_menus() -> Option<Vec<OwnedMenu>>` for Windows/Linux app-drawn menus.
- **Dialogs:** `cx.prompt_for_paths(PathPromptOptions { .. })` and
  `cx.prompt_for_new_path(&dir, Some("Untitled.md"))`, both oneshot-based.
- **File drop:** `FileDropEvent` (`Entered`, `Pending`, `Submit`, `Exited`,
  `Ended`) is dispatched as platform input; see `crates/gpui/src/window.rs`
  around the FileDrop handling.
- **Clipboard:** `cx.read_from_clipboard()`, `cx.write_to_clipboard(ClipboardItem::new_string(...))`.
  Rich HTML is not covered by `ClipboardItem`; use a platform/`arboard` module.
- **Appearance:** `window.appearance()`, `cx.window_appearance()`,
  `cx.set_window_appearance(Some(...))`, `window.observe_window_appearance(...)`.
- **Reduced motion:** `cx.reduce_motion()`.
- **Accessibility:** `Element::a11y_role` and `a11y_synthetic_children`;
  `Role::TextInput` + `Role::TextRun` children; `accesskit::TextSelection`.
- **Testing:** `#[gpui::test]`, `TestAppContext::simulate_input`,
  `simulate_keystrokes`, `simulate_click`, `advance_clock`,
  `run_until_parked`.
- **Headless screenshots:** `HeadlessAppContext` in
  `crates/gpui/src/app/headless_app_context.rs`; use the exact platform text
  system and renderer factory from `crates/gpui/examples/testing.rs` /
  `toggle_headless.rs` at this commit.

## Known constraints

1. **GPUI is pre-1.0.** Breaking changes are normal. Pin, test, upgrade
   deliberately.
2. **Text shaping is UI-thread work.** `WindowTextSystem` uses `Cell`/`RefCell`
   caches and is not `Sync`; `TextSystem` likewise has a `RefCell`-based line
   cache. Do not share it across threads. Use visible-only shaping and caches.
3. **`gpui_platform` is unpublished.** A git dependency requires network access
   on a clean build; CI should cache or vendor the checkout. Record the commit
   in artifacts.
4. **Windows/Linux menus are stored, not drawn.** The app must render its own
   menu from the model. The command palette is the fallback.
5. **Rich clipboard formats are not in `ClipboardItem`.** HTML copy needs a
   small platform module.
6. **Linux is the most variable target.** Test X11 and Wayland, wgpu drivers,
   portals, fonts and IME separately.

## Upgrade procedure

1. Pick a target Zed commit; read its GPUI changelog/commits and the diff for
   `crates/gpui`, `crates/gpui_platform`, and platform crates.
2. Update both `gpui` and `gpui_platform` to the same rev in one PR.
3. Run `cargo update -p gpui -p gpui_platform` and commit `Cargo.lock`.
4. Run the full core + GPUI test suite on macOS, Windows and Linux.
5. Regenerate golden screenshots; review every diff. Pay attention to text
   metrics, line height and input behavior.
6. Run the 100k/1M-word benchmarks once those fixtures exist; record the
   numbers in the upgrade PR.
7. Update this file with the new pin, date, API notes and any migrated code.
8. If an upstream change is wrong for Pilcrow, prefer a local adapter or a
   small patch in a fork; do not fork the whole framework for a cosmetic issue.

## Fallback

If the git dependency is a blocker (offline builds, CI reliability, API churn),
evaluate `gpui-pre 0.3.x` as a single-family replacement:

- It is a crates.io snapshot, so builds are reproducible without git.
- It is not official; audit the snapshot and its license/notice.
- It may lag Zed `main`; the editor/app code will need API adjustments.
- Do not mix it with official GPUI crates in the same dependency graph.

Record the decision in this file if the fallback is taken.
