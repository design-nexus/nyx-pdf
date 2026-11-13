//! Every page in the sidebar, in order.

use crate::widgets::Page;

pub mod document;
pub mod library;
pub mod pages;
pub mod settings;

pub struct Section {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub description: &'static str,
    pub build: fn(&Page),
    /// The page manages its own scrolling (the viewer, the page grid).
    pub fill: bool,
    /// No page padding (the viewer).
    pub bare: bool,
}

pub fn all() -> Vec<Section> {
    let s = |id, title, icon, description, build, fill, bare| Section {
        id,
        title,
        icon,
        description,
        build,
        fill,
        bare,
    };
    vec![
        s("library", "Library", "npdf-library-symbolic", "Your recent PDFs, with the page you were on.", library::build, true, false),
        s("document", "Read", "view-reveal-symbolic", "Read, mark up, fill in and sign the open PDF.", document::build, true, true),
        s("pages", "Pages", "npdf-pages-symbolic", "Reorder, rotate, delete, add and extract pages.", pages::build, true, false),
        // Opened from the top bar as a dialog, not listed in the sidebar.
        s(
            "settings",
            "Settings",
            "emblem-system-symbolic",
            "Viewing, editing and this window.",
            settings::build,
            false,
            false,
        ),
    ]
}
