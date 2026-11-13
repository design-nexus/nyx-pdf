//! PDF — a PDF viewer and editor for Omarchy.

mod dialog_dismissal;
mod cmd;
mod control;
mod doc;
mod fmt;
mod paths;
mod prefs;
mod recent;
mod sections;
mod share;
mod settings_dialog;
#[cfg(test)]
mod testpdf;
mod theme;
mod viewer;
mod widgets;
mod window;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

pub const APP_ID: &str = "io.github.design_nexus.nyx.Pdf";

const USAGE: &str = "Usage: nyx-pdf [OPTIONS] [FILE…]\n\
\n\
  FILE            open this PDF (several files are listed in the sidebar)\n\
  --page N        open FILE on page N (1 is the first)\n\
  --section ID    open (or switch the open window) to a page: library, document, pages, settings\n\
  --toggle        close the window if it's open, otherwise open it (for a keybinding)\n";

fn main() -> glib::ExitCode {
    nyx_core::app::init(nyx_core::app::App { slug: "nyx-pdf", name: "PDF" });
    nyx_core::migrate::from(&["nexus-pdf"]);
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return glib::ExitCode::SUCCESS;
    }

    // GTK's Vulkan renderer enumerates every GPU at startup, which wakes a
    // sleeping discrete GPU on hybrid laptops. GL renders only on the one in use.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: still single-threaded; nothing else reads the environment yet.
        unsafe { std::env::set_var("GSK_RENDERER", "ngl") };
    }

    // Developer aid: a different id gives a separate instance, so a test run doesn't hand
    // its arguments to the copy that's open.
    let id = std::env::var("NPDF_APP_ID").unwrap_or_else(|_| APP_ID.to_string());
    // Windows without an application, like dialogs, take their Wayland app ID from this.
    glib::set_prgname(Some(id.as_str()));
    let app = gtk::Application::builder().application_id(id).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    // Nyx Assistant opens, reads and searches PDFs through this (primary instance only).
    app.connect_startup(|app| nyx_core::control::serve(app, control::tools(), control::handle));
    app.connect_command_line(|app, cl| {
        let argv: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let has = |flag: &str| argv.iter().any(|a| a == flag);
        let value = |flag: &str| argv.iter().position(|a| a == flag).and_then(|i| argv.get(i + 1)).cloned();
        let section = value("--section");
        let page = value("--page").and_then(|p| p.parse::<usize>().ok()).map(|p| p.saturating_sub(1));
        let mut files: Vec<PathBuf> = Vec::new();
        let mut skip = true; // argv[0]
        for a in &argv {
            if std::mem::take(&mut skip) {
                continue;
            }
            if a == "--section" || a == "--page" {
                skip = true;
                continue;
            }
            if a.starts_with("--") {
                continue;
            }
            // Relative paths resolve against the caller's directory, and URIs work too.
            if let Some(p) = cl.create_file_for_arg(a).path() {
                files.push(p);
            }
        }

        if has("--toggle")
            && let Some(w) = window::window()
            && w.is_visible()
        {
            w.close();
            return glib::ExitCode::SUCCESS;
        }
        window::present(app, section.as_deref().or(if files.is_empty() { None } else { Some("document") }));
        // Every file gets a tab; --page applies to the last one, which ends up showing.
        let last = files.len().saturating_sub(1);
        for (i, path) in files.into_iter().enumerate() {
            doc::request_open(path, if i == last { page } else { None });
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_shutdown(|_| {
        prefs::flush();
        doc::close_all();
    });
    app.run()
}
