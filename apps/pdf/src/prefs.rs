//! This app's own preferences (`~/.config/nyx-pdf/settings.toml`).

pub use nyx_theme::ThemeMode;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub mode: ThemeMode,
    pub theme: String,
    pub reduce_motion: bool,
    pub glow: bool,
    pub last_section: String,
    /// The sidebar shows only icons, whatever the window width. (Named afresh
    /// when the sidebar gained labels and open files, so it starts expanded.)
    pub sidebar_rail: bool,
    /// The same, while reading a document.
    pub sidebar_reading_rail: bool,
    /// fit-width, fit-page or 100.
    pub default_zoom: String,
    /// How pages are coloured: off, invert or tint.
    pub page_colors: String,
    /// Pages scroll as one column (otherwise one page at a time).
    pub continuous: bool,
    /// Pages side by side: single, pairs, or book (page one alone, then pairs).
    pub spread: String,
    /// The side panel is shown (on wide windows).
    pub panel_visible: bool,
    /// The side panel's tab: thumbs, outline or markup.
    pub panel_tab: String,
    /// The side panel's width, in pixels.
    pub panel_width: i32,
    /// Open each file on the page it was left on.
    pub remember_page: bool,
    /// Name stored on annotations.
    pub author: String,
    /// Default markup colour, `#rrggbb`.
    pub highlight_color: String,
    /// How opaque new highlights are, 0.1 to 1.
    pub highlight_opacity: f64,
    /// Pen width for drawings, in points.
    pub ink_width: f64,
    /// Font size for text boxes, in points.
    pub text_size: f64,
    /// Library order: recent or name.
    pub library_sort: String,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Omarchy,
            theme: "tokyo-night".into(),
            reduce_motion: false,
            glow: true,
            last_section: "library".into(),
            sidebar_rail: false,
            sidebar_reading_rail: false,
            default_zoom: "fit-width".into(),
            page_colors: "off".into(),
            continuous: true,
            spread: "single".into(),
            panel_visible: true,
            panel_tab: "thumbs".into(),
            panel_width: 180,
            remember_page: true,
            author: String::new(),
            highlight_color: "#ffd60a".into(),
            highlight_opacity: 1.0,
            ink_width: 2.0,
            text_size: 12.0,
            library_sort: "recent".into(),
        }
    }
}

nyx_core::prefs_store!(Prefs);
