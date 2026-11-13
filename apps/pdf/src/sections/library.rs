//! Recent files: a grid of covers (the top of page one) with where you left off.

use crate::widgets::{self, Page};
use crate::{doc, fmt, paths, recent};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The width page one is drawn at for the covers, in pixels: a wide card on a
/// 2× screen.
const COVER_PX: i32 = 640;
const COVER_H: i32 = 196;
/// The narrowest a card gets before the grid drops a column, and the gap between cards.
const CARD_MIN: i32 = 196;
const CARD_GAP: i32 = 16;

thread_local! {
    static GRID: RefCell<Option<gtk::FlowBox>> = const { RefCell::new(None) };
    static QUERY: RefCell<String> = const { RefCell::new(String::new()) };
    static PAGE: RefCell<Option<gtk::Stack>> = const { RefCell::new(None) };
    /// "24 files" in the page bar.
    static COUNT: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
    /// Combine PDFs' label, which gives way to its icon on a narrow window.
    static MERGE_LABEL: RefCell<Option<gtk::Label>> = const { RefCell::new(None) };
    /// Each card's "Page N of M" line and progress bar, to update in place.
    static CARDS: RefCell<HashMap<PathBuf, (gtk::Label, Option<gtk::ProgressBar>)>> = RefCell::new(HashMap::new());
    /// The list changed while the library wasn't showing; rebuild when it is.
    static STALE: Cell<bool> = const { Cell::new(false) };
}

pub fn set_query(q: &str) {
    QUERY.with(|s| *s.borrow_mut() = q.to_lowercase());
    refresh();
}

pub fn set_narrow(narrow: bool) {
    MERGE_LABEL.with(|l| {
        if let Some(l) = l.borrow().as_ref() {
            l.set_visible(!narrow);
        }
    });
}

/// Delete the cached covers.
pub fn clear_covers() {
    let dir = paths::covers_dir();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    refresh();
}

/// A button with an icon and a label; the label can be hidden to leave the icon.
fn bar_button(icon: &str, text: &str) -> (gtk::Button, gtk::Label) {
    let b = gtk::Button::new();
    b.add_css_class("page-action");
    b.set_valign(gtk::Align::Center);
    let content = widgets::hbox(6);
    content.set_halign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon));
    let l = widgets::label(text, "");
    content.append(&l);
    b.set_child(Some(&content));
    b.update_property(&[gtk::accessible::Property::Label(text)]);
    (b, l)
}

pub fn build(page: &Page) {
    // The page bar: its name and how many files, the order, Combine PDFs and Open a file.
    let top = widgets::hbox(8);
    top.add_css_class("page-bar");
    top.append(&widgets::label("Library", "page-title"));
    let count = widgets::label("", "page-sub");
    count.set_hexpand(true);
    count.set_xalign(0.0);
    top.append(&count);
    let sort = widgets::segmented(&widgets::opts(&[("recent", "Recent"), ("name", "Name")]), &crate::prefs::get().library_sort, |id| {
        crate::prefs::update(|p| p.library_sort = id);
        refresh();
    });
    sort.set_tooltip_text(Some("Order the files by when you opened them, or by name"));
    sort.set_valign(gtk::Align::Center);
    top.append(&sort);
    let (merge, merge_label) = bar_button("npdf-merge-symbolic", "Combine PDFs");
    merge.set_tooltip_text(Some("Join several PDFs into a new file"));
    merge.connect_clicked(|_| crate::sections::pages::merge_dialog());
    top.append(&merge);
    let (open, _) = bar_button("folder-symbolic", "Open a file");
    open.add_css_class("suggested-action");
    open.set_tooltip_text(Some("Open files; each is listed in the sidebar (Ctrl+O)"));
    open.connect_clicked(|_| doc::open_dialog());
    top.append(&open);
    page.body.append(&top);
    COUNT.with(|c| *c.borrow_mut() = Some(count));
    MERGE_LABEL.with(|l| *l.borrow_mut() = Some(merge_label));
    set_narrow(crate::window::narrow());

    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let flow = gtk::FlowBox::new();
    flow.add_css_class("library-grid");
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_valign(gtk::Align::Start);
    flow.set_homogeneous(true);
    flow.set_column_spacing(CARD_GAP as u32);
    flow.set_row_spacing(20);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&flow)
        .build();
    // As many columns as fit, and the cards share the width between them.
    let f = flow.clone();
    scroll.hadjustment().connect_page_size_notify(move |a| {
        // The grid's padding, 20 px at each side.
        let width = a.page_size() as i32 - 40;
        if width > 0 {
            let cols = ((width + CARD_GAP) / (CARD_MIN + CARD_GAP)).clamp(1, 8) as u32;
            f.set_min_children_per_line(cols);
            f.set_max_children_per_line(cols);
        }
    });
    flow.set_min_children_per_line(4);
    flow.set_max_children_per_line(4);
    stack.add_named(&scroll, Some("grid"));
    stack.add_named(
        &widgets::empty_state(
            "npdf-library-symbolic",
            "No recent files",
            "PDFs you open show up here, with the page you were on.",
            Some(("Open a file", Box::new(doc::open_dialog))),
        ),
        Some("empty"),
    );
    stack.add_named(&widgets::empty_state("system-search-symbolic", "No matching files", "Nothing in your recent files matches that search.", None), Some("none"));
    page.body.append(&stack);
    flow.connect_map(|_| {
        if STALE.with(|s| s.replace(false)) {
            refresh();
        }
    });
    GRID.with(|g| *g.borrow_mut() = Some(flow));
    PAGE.with(|p| *p.borrow_mut() = Some(stack));
    refresh();
    // Opening a file moves it to the front; saving under a new name adds one. Turning
    // pages only changes that file's card.
    doc::subscribe(&page.root, |c| match c {
        doc::Change::Opened => queue_refresh(),
        doc::Change::Dirty => {
            let known = doc::current().is_some_and(|d| CARDS.with(|m| m.borrow().contains_key(&d.path())));
            if !known {
                queue_refresh();
            }
        }
        doc::Change::Page => update_progress(),
        _ => {}
    });
}

/// Rebuild the grid now if it's showing, otherwise the next time it is.
fn queue_refresh() {
    let showing = GRID.with(|g| g.borrow().as_ref().is_some_and(|f| f.is_mapped()));
    if showing {
        gtk::glib::idle_add_local_once(refresh);
    } else {
        STALE.with(|s| s.set(true));
    }
}

/// The current file's card shows the page it's on now.
fn update_progress() {
    let Some(d) = doc::current() else { return };
    CARDS.with(|m| {
        if let Some((meta, bar)) = m.borrow().get(&d.path()) {
            show_progress(meta, bar.as_ref(), d.page(), d.n_pages());
        }
    });
}

/// "Page 12 of 48" and how far along that is; the last page reads as the end.
fn show_progress(meta: &gtk::Label, bar: Option<&gtk::ProgressBar>, page: usize, pages: usize) {
    let done = pages > 0 && page + 1 >= pages;
    meta.set_text(&if done { "Read to the end".to_string() } else { format!("Page {} of {pages}", page + 1) });
    if let Some(bar) = bar {
        bar.set_fraction(((page + 1) as f64 / pages.max(1) as f64).min(1.0));
        if done {
            bar.add_css_class("done");
        } else {
            bar.remove_css_class("done");
        }
    }
}

fn cover_file(path: &Path, mtime: u64) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(path, mtime, COVER_PX), &mut h);
    paths::covers_dir().join(format!("{:016x}.png", std::hash::Hasher::finish(&h)))
}

fn mtime(path: &Path) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

fn refresh() {
    let (Some(flow), Some(stack)) = (GRID.with(|g| g.borrow().clone()), PAGE.with(|p| p.borrow().clone())) else { return };
    while let Some(c) = flow.first_child() {
        flow.remove(&c);
    }
    CARDS.with(|m| m.borrow_mut().clear());
    let all = recent::list();
    let q = QUERY.with(|q| q.borrow().clone());
    let mut shown: Vec<_> = all
        .iter()
        .filter(|r| q.is_empty() || r.path.to_string_lossy().to_lowercase().contains(&q) || r.title.to_lowercase().contains(&q))
        .cloned()
        .collect();
    if crate::prefs::get().library_sort == "name" {
        let name = |r: &recent::Recent| r.path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        shown.sort_by_key(name);
    }
    stack.set_visible_child_name(if all.is_empty() { "empty" } else if shown.is_empty() { "none" } else { "grid" });
    COUNT.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            let files = fmt::count(all.len(), "file", "files");
            c.set_text(&if q.is_empty() || shown.len() == all.len() { files } else { format!("{} of {files}", shown.len()) });
        }
    });
    for r in shown {
        let child = gtk::FlowBoxChild::new();
        child.set_focusable(false);
        child.set_child(Some(&card(r)));
        flow.append(&child);
    }
}

/// The top of page one, filling the card's width (or its height, for a wide page).
fn cover_area() -> (gtk::DrawingArea, Rc<RefCell<Option<gtk::cairo::ImageSurface>>>) {
    let area = gtk::DrawingArea::new();
    area.add_css_class("cover");
    area.set_content_height(COVER_H);
    area.set_hexpand(true);
    area.set_overflow(gtk::Overflow::Hidden);
    let image: Rc<RefCell<Option<gtk::cairo::ImageSurface>>> = Rc::new(RefCell::new(None));
    let img = image.clone();
    area.set_draw_func(move |_, cr, w, h| {
        let Some(s) = img.borrow().clone() else { return };
        let (iw, ih) = (f64::from(s.width().max(1)), f64::from(s.height().max(1)));
        let scale = (f64::from(w) / iw).max(f64::from(h) / ih);
        cr.translate((f64::from(w) - iw * scale) / 2.0, 0.0);
        cr.scale(scale, scale);
        let _ = cr.set_source_surface(&s, 0.0, 0.0);
        cr.source().set_filter(gtk::cairo::Filter::Good);
        let _ = cr.paint();
    });
    (area, image)
}

fn card(r: recent::Recent) -> gtk::Button {
    let exists = r.path.is_file();
    let button = gtk::Button::new();
    button.add_css_class("pdf-card");
    let body = widgets::vbox(8);
    let (cover, image) = cover_area();
    body.append(&cover);
    let info = widgets::vbox(0);
    info.add_css_class("card-body");
    let name = r.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = widgets::label(&name, "card-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    info.append(&title);
    let dir = r.path.parent().map(paths::pretty).unwrap_or_default();
    let place = widgets::label(&dir, "card-place");
    place.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    place.set_xalign(0.0);
    info.append(&place);
    let meta_label = widgets::label("", "card-meta");
    meta_label.set_xalign(0.0);
    meta_label.set_margin_top(5);
    info.append(&meta_label);
    let mut progress = None;
    if !exists {
        meta_label.set_text("File not found");
    } else if r.pages == 0 {
        meta_label.set_text(&fmt::count(r.pages, "page", "pages"));
    } else {
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("card-progress");
        bar.set_margin_top(4);
        info.append(&bar);
        show_progress(&meta_label, Some(&bar), r.page.min(r.pages - 1), r.pages);
        progress = Some(bar);
    }
    if exists {
        CARDS.with(|m| m.borrow_mut().insert(r.path.clone(), (meta_label, progress)));
    }
    body.append(&info);
    button.set_child(Some(&body));
    let tip = paths::pretty(&r.path);
    button.set_tooltip_text(Some(&if r.title.is_empty() || Some(r.title.as_str()) == r.path.file_stem().and_then(|s| s.to_str()) {
        tip
    } else {
        format!("{}\n{tip}", r.title)
    }));
    if exists {
        cover.add_css_class("loading");
        load_cover(&cover, image, &r.path);
    } else {
        cover.add_css_class("missing");
    }
    let path = r.path.clone();
    button.connect_clicked(move |_| {
        if path.is_file() {
            doc::request_open(path.clone(), None);
        } else {
            crate::window::toast("That file isn't there any more.");
        }
    });

    // Right-click or long press: take it off the list.
    let menu = gtk::Popover::new();
    menu.set_parent(&button);
    menu.add_css_class("menu-popover");
    let forget = widgets::two_click("Remove from list", "Click again to remove", {
        let (path, menu) = (r.path.clone(), menu.clone());
        move || {
            recent::forget(&path);
            menu.popdown();
            refresh();
            crate::window::refresh_nav();
        }
    });
    let holder = widgets::vbox(4);
    if exists {
        let show = gtk::Button::with_label("Show in folder");
        show.add_css_class("flat");
        show.add_css_class("menu-item");
        let (path, menu) = (r.path.clone(), menu.clone());
        show.connect_clicked(move |_| {
            menu.popdown();
            crate::cmd::show_in_folder(&path);
        });
        holder.append(&show);
    }
    holder.append(&forget);
    holder.append(&widgets::label("The file itself isn't touched.", "dim"));
    menu.set_child(Some(&holder));
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_SECONDARY);
    let m2 = menu.clone();
    click.connect_pressed(move |_, _, _, _| m2.popup());
    button.add_controller(click);
    let m3 = menu.clone();
    button.connect_destroy(move |_| m3.unparent());
    button
}

fn show_cover(area: &gtk::DrawingArea, image: &RefCell<Option<gtk::cairo::ImageSurface>>, file: &Path) {
    let surface = std::fs::File::open(file).ok().and_then(|mut f| gtk::cairo::ImageSurface::create_from_png(&mut f).ok());
    *image.borrow_mut() = surface;
    area.remove_css_class("loading");
    area.queue_draw();
}

/// Show page one: from the cache when it's there, otherwise draw it and keep it.
fn load_cover(area: &gtk::DrawingArea, image: Rc<RefCell<Option<gtk::cairo::ImageSurface>>>, path: &Path) {
    let file = cover_file(path, mtime(path));
    if file.is_file() {
        show_cover(area, &image, &file);
        return;
    }
    let (path, out) = (path.to_path_buf(), file);
    let weak = area.downgrade();
    doc::background(
        move || {
            use gtk::gio::prelude::FileExt;
            let uri = gtk::gio::File::for_path(&path).uri();
            let pdf = poppler::Document::from_file(&uri, None).ok()?;
            let page = pdf.page(0)?;
            let (w, h) = page.size();
            let scale = f64::from(COVER_PX) / w.max(1.0);
            let (pw, ph) = ((w * scale).ceil() as i32, (h * scale).ceil() as i32);
            let surface = gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, pw, ph).ok()?;
            {
                let cr = gtk::cairo::Context::new(&surface).ok()?;
                cr.set_source_rgb(1.0, 1.0, 1.0);
                cr.paint().ok()?;
                cr.scale(scale, scale);
                page.render(&cr);
            }
            std::fs::create_dir_all(out.parent()?).ok()?;
            let tmp = out.with_extension("tmp");
            let mut f = std::fs::File::create(&tmp).ok()?;
            surface.write_to_png(&mut f).ok()?;
            std::fs::rename(&tmp, &out).ok()?;
            Some(out)
        },
        move |res| {
            if let Some(area) = weak.upgrade() {
                area.remove_css_class("loading");
                if let Some(file) = res {
                    show_cover(&area, &image, &file);
                }
            }
        },
    );
}
