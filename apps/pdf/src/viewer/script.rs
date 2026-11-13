//! Developer aid: `NPDF_SCRIPT="step; step; …"` drives the open window, so layouts and
//! tools can be checked in a snapshot without a pointer. Each step runs about 350 ms
//! after the one before. Steps (page numbers start at 1, positions are points on the page):
//!
//! `section ID` · `tool NAME` · `zoom fit-width|fit-page|N` · `page N` · `colour #rrggbb`
//! `drag PAGE X0 Y0 X1 Y1` (with the current tool) · `click PAGE X Y`
//! `note PAGE X Y TEXT` · `textbox PAGE X0 Y0 X1 Y1 TEXT` · `edit PAGE X0 Y0 X1 Y1 TEXT`
//! `sign PICTURE PAGE X Y` · `search TEXT` · `undo` · `redo` · `save PATH`
//! `panel` (toggle) · `tab thumbs|outline|markup` · `colours off|invert|tint` · `theme ID` · `wait MS`
//! `open PATH` (in a new tab) · `layout single|pairs|book` · `scroll continuous|single` · `present`
//! `back` · `forward` · `jump N` · `resize PAGE X Y` (drag the selected annotation's corner there)
//! `menu zoom|more|highlight|text` (open one of the reader's menus)
//! `status` and `windows` print what's open, for checking from a terminal · `recover` (offer crash recovery) · `clearform` · `printpdf PATH` (print to a file)

use super::{Tool, tools, view};
use crate::doc::annots;
use crate::doc::geom::Rect;
use crate::{doc, window};
use gtk::glib;
use gtk::prelude::*;
use std::time::Duration;

pub fn start(script: &str) {
    let steps: Vec<String> = script.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    next(steps, 0, 1200);
}

fn next(steps: Vec<String>, i: usize, delay: u64) {
    if i >= steps.len() {
        return;
    }
    glib::timeout_add_local_once(Duration::from_millis(delay), move || {
        let step = steps[i].clone();
        eprintln!("script: {step}");
        let wait = run(&step);
        next(steps, i + 1, wait.unwrap_or(350));
    });
}

fn nums(parts: &[&str]) -> Vec<f64> {
    parts.iter().filter_map(|p| p.parse().ok()).collect()
}

fn tool_named(name: &str) -> Option<Tool> {
    Some(match name {
        "select" => Tool::Select,
        "highlight" => Tool::Highlight,
        "underline" => Tool::Underline,
        "strike" => Tool::Strike,
        "ink" => Tool::Ink,
        "note" => Tool::Note,
        "textbox" => Tool::TextBox,
        "edit" => Tool::EditText,
        "sign" => Tool::Sign,
        _ => return None,
    })
}

/// Run one step; the return value is how long to wait before the next one.
fn run(step: &str) -> Option<u64> {
    let (cmd, rest) = step.split_once(' ').unwrap_or((step, ""));
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let v = view();
    match cmd {
        "section" => window::navigate(rest.trim()),
        "tool" => {
            if let (Some(v), Some(t)) = (&v, tool_named(rest.trim())) {
                v.set_tool(t);
            }
        }
        "zoom" => match rest.trim() {
            "fit-width" | "fit-page" => super::zoom_fit(rest.trim()),
            n => {
                if let (Some(v), Ok(z)) = (&v, n.parse::<f64>()) {
                    v.set_fit_zoom(z, super::Fit::None);
                }
            }
        },
        "page" => {
            if let (Some(v), Some(n)) = (&v, nums(&parts).first()) {
                v.goto_page(*n as usize - 1);
            }
        }
        "colour" => {
            if let Some(v) = &v {
                v.set_colour(rest.trim());
            }
        }
        "drag" => {
            let n = nums(&parts);
            if let (Some(v), [p, x0, y0, x1, y1]) = (&v, n.as_slice()) {
                let pv = v.pages.borrow().get(*p as usize - 1).cloned();
                if let Some(pv) = pv {
                    tools::drag_begin(&pv, *x0, *y0);
                    tools::drag_update(&pv, (x0 + x1) / 2.0, (y0 + y1) / 2.0);
                    tools::drag_update(&pv, *x1, *y1);
                    tools::drag_end(&pv, *x1, *y1, false);
                }
            }
        }
        "click" => {
            let n = nums(&parts);
            if let (Some(v), [p, x, y]) = (&v, n.as_slice()) {
                let pv = v.pages.borrow().get(*p as usize - 1).cloned();
                if let Some(pv) = pv {
                    tools::drag_begin(&pv, *x, *y);
                    tools::drag_end(&pv, *x, *y, true);
                }
            }
        }
        "note" | "textbox" | "edit" => {
            let want = if cmd == "note" { 3 } else { 5 };
            let n = nums(&parts);
            if n.len() >= want
                && let Some(v) = &v
            {
                let text = parts[want..].join(" ");
                let pidx = n[0] as usize - 1;
                let colour = super::colour::parse(&v.colour()).unwrap_or([0.0, 0.0, 0.0]);
                let d = doc::current()?;
                let region = if cmd == "note" { Rect::default() } else { Rect::new(n[1], n[2], n[3], n[4]) };
                let look = (cmd == "edit").then(|| doc::text::analyse(&d.pdf(), pidx, &region));
                doc::try_edit(false, |lo| {
                    let page = *annots::page_ids(lo).get(pidx).ok_or_else(|| anyhow::anyhow!("no such page"))?;
                    match cmd {
                        "note" => annots::add_note(lo, page, n[1], n[2], &text, colour, "")?,
                        "textbox" => annots::add_text_box(lo, page, region, &text, colour, 12.0, "")?,
                        _ => {
                            let l = look.as_ref().expect("edit has a look");
                            annots::add_edit(lo, page, l.rect, &text, l.face, l.size, l.fg, l.bg, "")?
                        }
                    };
                    Ok(())
                });
            }
        }
        "sign" => {
            if let (Some(v), Some(path)) = (&v, parts.first()) {
                *v.signature.borrow_mut() = Some(std::path::PathBuf::from(path));
                v.set_tool(Tool::Sign);
                let n = nums(&parts[1..]);
                if let [p, x, y] = n.as_slice() {
                    let pv = v.pages.borrow().get(*p as usize - 1).cloned();
                    if let Some(pv) = pv {
                        tools::drag_begin(&pv, *x, *y);
                        tools::drag_end(&pv, *x, *y, true);
                    }
                }
            }
        }
        "formtext" => {
            // formtext PAGE N TEXT: fill the Nth text entry on the page, as if typed and confirmed.
            let n = nums(&parts);
            if let (Some(v), [p, k, ..]) = (&v, n.as_slice()) {
                let pv = v.pages.borrow().get(*p as usize - 1).cloned();
                let text = parts[2..].join(" ");
                if let Some(pv) = pv {
                    let entries: Vec<gtk::Entry> =
                        pv.form_items.borrow().iter().filter_map(|(w, _)| w.clone().downcast::<gtk::Entry>().ok()).collect();
                    if let Some(e) = entries.get(*k as usize) {
                        e.set_text(&text);
                        e.emit_activate();
                    }
                }
            }
        }
        "search" => {
            if let Some(v) = &v {
                v.search.open();
                v.search.type_query(rest.trim());
            }
        }
        "panel" => super::toggle_panel(),
        "tab" => {
            if let Some(v) = &v {
                v.panel.show(rest.trim());
            }
        }
        "colours" => {
            crate::prefs::update(|p| p.page_colors = rest.trim().to_string());
            crate::theme::apply();
        }
        "theme" => {
            crate::prefs::update(|p| {
                p.theme = rest.trim().to_string();
                p.mode = crate::prefs::ThemeMode::Theme;
            });
            crate::theme::apply();
        }
        "undo" => doc::undo(),
        "redo" => doc::redo(),
        "save" => {
            if let Some(d) = doc::current()
                && let Err(e) = d.save_as(std::path::Path::new(rest.trim()))
            {
                eprintln!("script: save failed: {e:#}");
            }
        }
        "open" => doc::request_open(std::path::PathBuf::from(rest.trim()), None),
        "layout" => {
            crate::prefs::update(|p| p.spread = rest.trim().to_string());
            super::rebuild();
        }
        "scroll" => {
            crate::prefs::update(|p| p.continuous = rest.trim() == "continuous");
            super::relayout();
        }
        "present" => super::toggle_presenting(),
        "menu" => super::open_menu(rest.trim()),
        "back" => super::go_back(),
        "forward" => super::go_forward(),
        "jump" => {
            if let Some(n) = nums(&parts).first() {
                super::jump(*n as usize - 1);
            }
        }
        "resize" => {
            let n = nums(&parts);
            if let (Some(v), [p, x, y]) = (&v, n.as_slice()) {
                let pv = v.pages.borrow().get(*p as usize - 1).cloned();
                let corner = (*v.annot_sel.borrow()).and_then(|s| tools::annot_rect(v, s));
                if let (Some(pv), Some(r)) = (pv, corner) {
                    tools::drag_begin(&pv, r.x1 + 2.0, r.y1 + 2.0);
                    tools::drag_update(&pv, *x, *y);
                    tools::drag_end(&pv, *x, *y, false);
                }
            }
        }
        "status" => {
            if let (Some(v), Some(d)) = (&v, doc::current()) {
                println!(
                    "status: page {} of {}, back {}, forward {}, dirty {}, zoom {:.2}, files {}, back button {}",
                    d.page() + 1,
                    d.n_pages(),
                    d.can_go_back(),
                    d.can_go_forward(),
                    d.dirty(),
                    v.zoom.get(),
                    doc::all().len(),
                    v.back_btn.is_sensitive()
                );
                println!("search: {}", v.search.describe());
            }
        }
        "recover" => doc::offer_recovery(),
        "clearform" => super::clear_form(),
        "printpdf" => doc::print_to(Some(std::path::Path::new(rest.trim()))),
        "windows" => {
            let titles: Vec<String> = gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .filter(|w| w.is_visible())
                .map(|w| w.title().map(|t| t.to_string()).unwrap_or_default())
                .collect();
            println!("windows: {titles:?}");
        }
        "wait" => return rest.trim().parse().ok(),
        other => eprintln!("script: unknown step {other:?}"),
    }
    None
}
