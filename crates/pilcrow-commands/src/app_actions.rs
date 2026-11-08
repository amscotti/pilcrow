//! Application-shell actions.

use std::path::PathBuf;

use gpui::actions;
use gpui::private::schemars::JsonSchema;
use serde::Deserialize;

actions!(
    pilcrow_app,
    [
        NewNote,
        Open,
        Save,
        SaveAs,
        About,
        QuitApp,
        CloseWindow,
        CycleAppearance,
        AppearanceAuto,
        AppearanceLight,
        AppearanceDark,
        ToggleHighContrast,
        CopyHtml,
        ExportHtml,
        NextNote,
        PreviousNote,
        TextSizeUp,
        TextSizeDown,
        TextSizeReset,
        MeasureNarrow,
        MeasureStandard,
        MeasureWide,
    ]
);

/// Open a path picked from File → Open Recent.
#[derive(Clone, Debug, PartialEq, Deserialize, JsonSchema, gpui::Action)]
#[action(namespace = pilcrow_app)]
#[serde(deny_unknown_fields)]
pub struct OpenRecent {
    pub path: PathBuf,
}
