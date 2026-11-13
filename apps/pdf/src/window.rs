//! The main window: the header (the app and the sidebar toggle; the file search
//! and Open, or while reading the open file's page and view controls; settings),
//! the navigation sidebar (the library, combining files, and the open files, the
//! current one with Read and Pages under it) and a stack of pages built the first
//! time they're shown. Settings opens as a card over the window (see `settings_dialog`).

use crate::sections::{self, Section};
use crate::{doc, prefs, recent, settings_dialog, theme, viewer, widgets};
use gtk::prelude::*;
use gtk::{gdk, glib};
use nyx_ui::{chrome, sidebar};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

struct Ui {
    window: gtk::ApplicationWindow,
    stack: gtk::Stack,
    nav: gtk::Box,
    /// The sidebar's rows, rebuilt as files open and close.
    nav_list: gtk::Box,
    pages: HashMap<String, gtk::Widget>,
    sections: Vec<Section>,
    current: String,
    overlay: gtk::Overlay,
    /// The header, hidden in fullscreen and reading mode.
    chrome: Vec<gtk::Widget>,
    search: gtk::SearchEntry,
    /// The file search and Open, in the header everywhere but the open file.
    lib_center: gtk::Widget,
    lib_end: gtk::Widget,
    /// The open file's page controls (middle) and view controls (end), filled in
    /// when the document page is built and shown only while it is.
    doc_center: gtk::Box,
    doc_end: gtk::Box,
}

thread_local! {
    static UI: RefCell<Option<Rc<RefCell<Ui>>>> = const { RefCell::new(None) };
    static NARROW: Cell<bool> = const { Cell::new(false) };
    /// Set once the user has decided what to do about unsaved changes, so closing goes through.
    static FORCE_CLOSE: Cell<bool> = const { Cell::new(false) };
    static NAV_QUEUED: Cell<bool> = const { Cell::new(false) };
}

fn ui() -> Option<Rc<RefCell<Ui>>> {
    UI.with(|u| u.borrow().clone())
}

pub fn present(app: &gtk::Application, section: Option<&str>) {
    if let Some(ui) = ui() {
        let window = ui.borrow().window.clone();
        if let Some(s) = section {
            navigate(s);
        }
        window.present();
        return;
    }
    theme::install();
    install_icons();
    build(app);
    let start = section.map(String::from).unwrap_or_else(|| nyx_theme::start_page("library", &prefs::get().last_section));
    navigate(&start);
    // Developer aid: NPDF_SNAPSHOT=/path.png renders the window to a PNG
    // (invisibly) and quits, so layouts can be checked without a visible window.
    if let Some(out) = std::env::var_os("NPDF_SNAPSHOT") {
        viewer::run_script_from_env();
        snapshot_and_quit(app, std::path::PathBuf::from(out));
        return;
    }
    if let Some(ui) = ui() {
        ui.borrow().window.present();
    }
    viewer::run_script_from_env();
    doc::offer_recovery();
    if prefs::take_broken() {
        toast("Your settings file couldn't be read, so defaults are in use. The old file is kept as settings.toml.bak.");
    }
}

/// Our own symbolic icons, for things the icon theme has no glyph for.
/// They're written to the cache once and added to the icon search path.
fn install_icons() {
    macro_rules! icons {
        ($($name:literal),* $(,)?) => {
            &[$(($name, include_str!(concat!("../data/icons/npdf-", $name, "-symbolic.svg")))),*]
        };
    }
    const ICONS: &[(&str, &str)] = icons![
        "library", "document", "pages", "select", "highlight", "underline", "strike", "ink", "note", "textbox", "edit-text",
        "sign", "fit-width", "fit-page", "thumbs", "outline", "rotate-left", "rotate-right", "layout", "merge",
    ];
    let dir = crate::paths::cache_dir().join("icons");
    for (name, svg) in ICONS {
        let file = format!("npdf-{name}-symbolic.svg");
        let path = dir.join(&file);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(*svg) {
            let _ = crate::cmd::atomic_write(&path, svg);
        }
    }
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_search_path(&dir);
    }
}

/// The icon rail's mark where a group starts: a short bar in its colour.
fn nav_divider(section: &str) -> gtk::Separator {
    let bar = gtk::Separator::new(gtk::Orientation::Horizontal);
    bar.add_css_class("nav-divider");
    bar.add_css_class("compact-show");
    sidebar::set_section(&bar, section);
    bar.set_visible(false);
    bar
}

/// A file's name without `.pdf`, as the library shows it.
fn stem(d: &doc::Doc) -> String {
    d.path().file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| d.file_name())
}

/// Rebuild the sidebar on the next idle (several changes often arrive together).
pub fn refresh_nav() {
    if NAV_QUEUED.with(|q| q.replace(true)) {
        return;
    }
    glib::idle_add_local_once(|| {
        NAV_QUEUED.with(|q| q.set(false));
        build_nav();
    });
}

/// PDF (the library and Combine PDFs), then the open files; the one you're in
/// has Read and Pages under it.
fn build_nav() {
    let Some(ui) = ui() else { return };
    let (list, current) = {
        let u = ui.borrow();
        (u.nav_list.clone(), u.current.clone())
    };
    let section_item = |id: &str| {
        let u = ui.borrow();
        let s = u.sections.iter().find(|s| s.id == id).expect("a section for each nav item");
        let item = sidebar::nav_item(s.icon, s.title, s.description);
        item.set_active(current == id);
        let id = s.id;
        item.button.connect_clicked(move |_| navigate(id));
        item
    };
    while let Some(c) = list.first_child() {
        list.remove(&c);
    }
    list.append(&nav_divider(sidebar::ACCENT_SECTION));
    let g = sidebar::group_label("PDF", true);
    sidebar::set_section(&g, sidebar::ACCENT_SECTION);
    list.append(&g);
    let library = section_item("library");
    library.set_count(Some(recent::list().len() as u64));
    list.append(&library.button);
    let merge = sidebar::nav_item("npdf-merge-symbolic", "Combine PDFs", "Join several PDFs into a new file");
    merge.button.connect_clicked(|_| sections::pages::merge_dialog());
    list.append(&merge.button);

    let open = doc::all();
    if !open.is_empty() {
        list.append(&nav_divider("open-file"));
        let g = sidebar::group_label("Open files", false);
        sidebar::set_section(&g, "open-file");
        list.append(&g);
        let cur = doc::current();
        let in_file = matches!(current.as_str(), "document" | "pages");
        for d in open {
            let is_cur = cur.as_ref().is_some_and(|c| Rc::ptr_eq(c, &d));
            let path = crate::paths::pretty(&d.path());
            let tip = if d.dirty() { format!("{path}\nUnsaved changes") } else { path };
            let item = sidebar::nav_item("npdf-document-symbolic", &stem(&d), &tip);
            item.set_active(is_cur && in_file);
            if d.dirty() {
                let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                dot.add_css_class("unsaved-dot");
                dot.add_css_class("compact-hide");
                dot.set_valign(gtk::Align::Center);
                if let Some(content) = item.button.child().and_downcast::<gtk::Box>() {
                    content.append(&dot);
                }
            }
            let weak = Rc::downgrade(&d);
            item.button.connect_clicked(move |_| {
                if let Some(d) = weak.upgrade() {
                    doc::switch_to(&d);
                    navigate("document");
                }
            });
            // A middle click closes the file; a right-click offers to.
            let middle = gtk::GestureClick::new();
            middle.set_button(gdk::BUTTON_MIDDLE);
            let weak = Rc::downgrade(&d);
            middle.connect_released(move |_, _, _, _| {
                if let Some(d) = weak.upgrade() {
                    viewer::close_doc(&d);
                }
            });
            item.button.add_controller(middle);
            let (weak, anchor) = (Rc::downgrade(&d), list.clone());
            nyx_ui::menu::on_right_click(&item.button, move |w, x, y| {
                let Some(d) = weak.upgrade() else { return };
                let at = w.compute_point(&anchor, &gtk::graphene::Point::new(x as f32, y as f32)).map_or((x, y), |p| (f64::from(p.x()), f64::from(p.y())));
                let path = d.path();
                let weak = Rc::downgrade(&d);
                nyx_ui::menu::popup_at(
                    &anchor,
                    at.0,
                    at.1,
                    vec![
                        nyx_ui::menu::item("Show in folder", move || crate::cmd::show_in_folder(&path)),
                        nyx_ui::menu::separator(),
                        nyx_ui::menu::item("Close", move || {
                            if let Some(d) = weak.upgrade() {
                                viewer::close_doc(&d);
                            }
                        })
                        .key("Ctrl+W"),
                    ],
                );
            });
            list.append(&item.button);
            if is_cur && in_file {
                let sub = gtk::Box::new(gtk::Orientation::Vertical, 1);
                sub.add_css_class("subnav");
                sub.append(&section_item("document").button);
                let pages = section_item("pages");
                pages.set_count(Some(d.n_pages() as u64));
                sub.append(&pages.button);
                list.append(&sub);
            }
        }
    }

    let hint = widgets::hbox(8);
    hint.add_css_class("nav-hint");
    hint.add_css_class("compact-hide");
    let icon = gtk::Image::from_icon_name("dialog-information-symbolic");
    icon.set_valign(gtk::Align::Start);
    hint.append(&icon);
    let l = widgets::label("Drop PDFs anywhere to open them", "");
    l.set_wrap(true);
    l.set_xalign(0.0);
    l.set_hexpand(true);
    hint.append(&l);
    list.append(&hint);
    refresh_compact();
}

/// The header's file search, with its key at the right while it's empty.
fn search_field(search: &gtk::SearchEntry) -> gtk::Overlay {
    let field = gtk::Overlay::new();
    field.add_css_class("header-search");
    field.set_child(Some(search));
    let key = widgets::label("Ctrl F", "key-hint");
    key.set_halign(gtk::Align::End);
    key.set_valign(gtk::Align::Center);
    key.set_margin_end(10);
    key.set_can_target(false);
    field.add_overlay(&key);
    search.connect_search_changed(move |e| key.set_visible(e.text().is_empty()));
    field
}

/// Open, with its key.
fn open_button() -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("open-button");
    b.set_valign(gtk::Align::Center);
    let content = widgets::hbox(6);
    content.append(&gtk::Image::from_icon_name("folder-symbolic"));
    content.append(&widgets::label("Open", ""));
    content.append(&widgets::label("Ctrl O", "key-hint"));
    b.set_child(Some(&content));
    b.set_tooltip_text(Some("Open files; each is listed in the sidebar (Ctrl+O)"));
    b.connect_clicked(|_| doc::open_dialog());
    b
}

fn build(app: &gtk::Application) {
    let window = gtk::ApplicationWindow::builder().application(app).title("PDF").default_width(1280).default_height(800).build();
    window.add_css_class("pdf-window");
    // No client-side titlebar: Hyprland manages the window.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    nyx_ui::keys::install(&window, nyx_ui::keys::Common { settings: Some(settings_dialog::open), shortcuts: Some(show_shortcuts), close: false, palette: None });
    window.set_icon_name(Some(crate::APP_ID));

    let sections = sections::all();

    // ----- Sidebar -----
    let nav = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nav.add_css_class("settings-navigation");
    nav.set_hexpand(false);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let nav_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls with wheel, trackpad and keyboard; no visible scrollbar.
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(&list)
        .build();
    nav.append(&nav_scroll);

    // ----- Content -----
    let stack = gtk::Stack::new();
    stack.add_css_class("settings-content");
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.append(&stack);

    // ----- Header -----
    // The app and the sidebar toggle; the file search in the middle and Open at the
    // end, or while reading the open file's page and its view controls; Settings.
    let header = chrome::app_header(&window, "npdf-document-symbolic", "PDF");
    let toggle = widgets::bar_button("sidebar-show-symbolic", "Collapse the sidebar (Ctrl+B)");
    toggle.connect_clicked(|_| toggle_sidebar());
    header.start.append(&toggle);
    let doc_center = widgets::hbox(2);
    doc_center.set_visible(false);
    header.center.append(&doc_center);
    let search = search_entry();
    let lib_center = search_field(&search);
    header.center.append(&lib_center);
    let doc_end = widgets::hbox(2);
    doc_end.set_visible(false);
    header.end.set_spacing(4);
    header.end.append(&doc_end);
    let lib_end = open_button();
    lib_end.set_margin_end(4);
    header.end.append(&lib_end);
    header.end.append(&chrome::settings_button(settings_dialog::open));

    let frame = nyx_ui::frame::AppFrame::new(&header.root);
    frame.set_sidebar(&nav);
    frame.set_page(&content);
    // The page bars and the toolbar run to the sheet's edges.
    frame.set_flush(true);
    let overlay = frame.install(&window);

    install_keys(&window);
    window.connect_fullscreened_notify(|w| apply_fullscreen(w.is_fullscreen()));
    window.connect_close_request(|w| {
        if FORCE_CLOSE.with(|f| f.get()) || !doc::all().iter().any(|d| d.dirty()) {
            return glib::Propagation::Proceed;
        }
        let w = w.clone();
        doc::guard_all(move || {
            FORCE_CLOSE.with(|f| f.set(true));
            w.close();
        });
        glib::Propagation::Stop
    });

    // Drop PDFs anywhere on the window to open them.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(|_, value, _, _| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let mut any = false;
        for f in list.files() {
            if let Some(p) = f.path() {
                let pdf = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
                if pdf {
                    doc::request_open(p, None);
                    any = true;
                }
            }
        }
        if !any {
            toast("Only PDF files can be opened.");
        }
        any
    });
    window.add_controller(drop);

    // Narrow windows (a tiled half-screen) get an icon-only sidebar.
    let apply_width = {
        let nav = nav.clone();
        move |w: &gtk::ApplicationWindow| {
            let width = if w.width() > 0 { w.width() } else { w.default_width() };
            let narrow = width > 0 && width < 980;
            settings_dialog::fit(w);
            if narrow == NARROW.with(|n| n.get()) && nav.has_css_class("sized") {
                return;
            }
            nav.add_css_class("sized");
            set_narrow(narrow);
            refresh_compact();
        }
    };
    let aw = apply_width.clone();
    window.connect_default_width_notify(move |w| aw(w));
    let aw = apply_width.clone();
    window.connect_realize(move |w| aw(w));
    // Tiled windows are resized by the compositor without touching the default
    // size: an invisible layer over the whole window reports each real size
    // change, and the layout follows on the next frame.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    probe.set_can_focus(false);
    overlay.add_overlay(&probe);
    overlay.set_measure_overlay(&probe, false);
    {
        let (aw, w2) = (apply_width.clone(), window.clone());
        probe.connect_resize(move |_, _, _| {
            let (aw, w2) = (aw.clone(), w2.clone());
            // After this layout pass, when the window's width is the new one.
            glib::idle_add_local_once(move || aw(&w2));
        });
    }
    // A slow fallback, in case a resize slips by.
    let w2 = window.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
        apply_width(&w2);
        glib::ControlFlow::Continue
    });

    let ui = Ui {
        window: window.clone(),
        stack,
        nav,
        nav_list: list,
        pages: HashMap::new(),
        sections,
        current: String::new(),
        overlay,
        chrome: vec![header.root.clone().upcast()],
        search,
        lib_center: lib_center.upcast(),
        lib_end: lib_end.upcast(),
        doc_center,
        doc_end,
    };
    UI.with(|u| *u.borrow_mut() = Some(Rc::new(RefCell::new(ui))));
    build_nav();

    // The window title, the header and the open files follow the documents.
    let w = window.clone();
    doc::subscribe(&window, move |c| {
        if matches!(c, doc::Change::Opened | doc::Change::Closed | doc::Change::Dirty) {
            w.set_title(Some(&match doc::current() {
                Some(d) => format!("{}{} — PDF", if d.dirty() { "• " } else { "" }, d.title()),
                None => "PDF".to_string(),
            }));
            refresh_header();
        }
        if matches!(c, doc::Change::Opened | doc::Change::Closed | doc::Change::Dirty | doc::Change::Files | doc::Change::Structure) {
            refresh_nav();
        }
    });
}

/// While reading, the open file's page and view controls; elsewhere the file
/// search and Open.
fn refresh_header() {
    let Some(ui) = ui() else { return };
    let u = ui.borrow();
    let reading = u.current == "document" && doc::current().is_some();
    u.doc_center.set_visible(reading && u.doc_center.first_child().is_some());
    u.doc_end.set_visible(reading && u.doc_end.first_child().is_some());
    u.lib_center.set_visible(!reading);
    u.lib_end.set_visible(!reading);
}

/// The file search, in the middle of the header.
fn search_entry() -> gtk::SearchEntry {
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search your files"));
    search.add_css_class("bar-search");
    search.set_width_chars(30);
    search.connect_search_changed(|e| on_search(&e.text()));
    search.connect_stop_search(|e| e.set_text(""));
    search
}

/// Ctrl+F: search the open document while reading it; otherwise your files.
fn find_files() {
    if current() == "document" && doc::current().is_some() {
        viewer::focus_search();
        return;
    }
    navigate("library");
    let Some(ui) = ui() else { return };
    let search = ui.borrow().search.clone();
    search.grab_focus();
}

/// Every keyboard shortcut, for the shortcuts dialog and Settings.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Ctrl", "O"], "Open files (each is listed in the sidebar)"),
    (&["Ctrl", "S"], "Save"),
    (&["Ctrl", "Shift", "S"], "Save a copy as…"),
    (&["Ctrl", "P"], "Print"),
    (&["Ctrl", "W"], "Close the file"),
    (&["Ctrl", "Tab"], "Next file (with Shift, the one before)"),
    (&["Ctrl", "Z"], "Undo"),
    (&["Ctrl", "Shift", "Z"], "Redo"),
    (&["Ctrl", "C"], "Copy the selected text"),
    (&["Ctrl", "F"], "Search the document"),
    (&["Ctrl", "+"], "Zoom in"),
    (&["Ctrl", "−"], "Zoom out"),
    (&["Ctrl", "0"], "Fit width"),
    (&["Ctrl", "9"], "Fit page"),
    (&["PgUp"], "Previous page (also K, Shift+Space)"),
    (&["PgDn"], "Next page (also J, Space)"),
    (&["Home"], "First page"),
    (&["End"], "Last page"),
    (&["Alt", "←"], "Back to where you were (after a link)"),
    (&["Alt", "→"], "Forward again"),
    (&["F5"], "Present"),
    (&["F9"], "Show or hide the side panel"),
    (&["F10"], "The more menu: save a copy, print, present, close"),
    (&["F11"], "Fullscreen"),
    (&["R"], "Reading mode: just the document"),
    (&["Ctrl", "B"], "Collapse or expand the sidebar"),
    (&["V"], "Select tool"),
    (&["H"], "Highlight tool"),
    (&["U"], "Underline tool"),
    (&["X"], "Strike-out tool"),
    (&["D"], "Draw tool"),
    (&["N"], "Note tool"),
    (&["T"], "Text box tool"),
    (&["E"], "Edit text tool"),
    (&["S"], "Signature tool"),
    (&["Delete"], "Delete the selected markup"),
    (&["Esc"], "Back to Select, close the search, or stop presenting"),
    (&["Ctrl", ","], "Settings"),
    (&["F1"], "Show these shortcuts (or ?)"),
    (&["Ctrl", "Q"], "Quit"),
];

pub fn show_shortcuts() {
    let (dialog, card) = widgets::dialog("Keyboard shortcuts", 520);
    let list = widgets::vbox(0);
    list.add_css_class("group-list");
    for (keys, what) in SHORTCUTS {
        list.append(&widgets::row(what, "", Some(widgets::key_caps(keys).upcast_ref())));
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        // Scrolls without a scrollbar, which would cover the key caps.
        .vscrollbar_policy(gtk::PolicyType::External)
        .propagate_natural_height(true)
        .max_content_height(600)
        .child(&list)
        .build();
    card.append(&scroll);
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::End);
    let d = dialog.clone();
    close.connect_clicked(move |_| d.close());
    card.append(&close);
    dialog.present();
}

/// The layer over the window, for toasts and the settings dialog.
pub fn overlay() -> Option<gtk::Overlay> {
    ui().map(|u| u.borrow().overlay.clone())
}

/// While reading, the sidebar has its own collapsed state, so the page can have
/// the room; elsewhere it follows `sidebar_rail`.
fn collapsed_here() -> bool {
    let p = prefs::get();
    if current() == "document" { p.sidebar_reading_rail } else { p.sidebar_rail }
}

/// The sidebar shows only icons when the window is narrow or the user collapsed it.
fn refresh_compact() {
    let Some(ui) = ui() else { return };
    let nav = ui.borrow().nav.clone();
    let compact = narrow() || collapsed_here();
    sidebar::set_compact(&nav, compact);
    show_compact_marks(nav.upcast_ref(), compact);
}

pub fn toggle_sidebar() {
    let reading = current() == "document";
    prefs::update(|p| {
        if reading {
            p.sidebar_reading_rail = !p.sidebar_reading_rail;
        } else {
            p.sidebar_rail = !p.sidebar_rail;
        }
    });
    refresh_compact();
}

/// Show what's marked `compact-show` (the rail's group bars) only on the icon rail.
fn show_compact_marks(w: &gtk::Widget, compact: bool) {
    if w.has_css_class("compact-show") {
        w.set_visible(compact);
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        show_compact_marks(&c, compact);
        child = c.next_sibling();
    }
}

fn install_keys(window: &gtk::ApplicationWindow) {
    // Capture phase: these work wherever focus is, except while typing.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w2 = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let alt = mods.contains(gdk::ModifierType::ALT_MASK);
        let typing = gtk::prelude::GtkWindowExt::focus(&w2).is_some_and(|f| {
            f.is::<gtk::Text>() || f.ancestor(gtk::Entry::static_type()).is_some() || f.is::<gtk::SearchEntry>() || f.is::<gtk::TextView>()
        });
        let stop = glib::Propagation::Stop;
        if settings_dialog::is_open() {
            return match key {
                gdk::Key::Escape => {
                    settings_dialog::escape();
                    stop
                }
                gdk::Key::f if ctrl => {
                    settings_dialog::focus_search();
                    stop
                }
                gdk::Key::q if ctrl => {
                    w2.close();
                    stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        let search = ui().map(|u| u.borrow().search.clone());
        let k = key.to_lower();
        let in_doc = current() == "document" && doc::current().is_some();
        // Presenting: arrows, space and the page keys move through the pages; Esc leaves.
        if viewer::is_presenting() {
            match key {
                gdk::Key::Right | gdk::Key::Down | gdk::Key::space | gdk::Key::Page_Down | gdk::Key::Return | gdk::Key::n => viewer::step_page(1),
                gdk::Key::Left | gdk::Key::Up | gdk::Key::BackSpace | gdk::Key::Page_Up | gdk::Key::p => viewer::step_page(-1),
                gdk::Key::Home => viewer::goto_page(0),
                gdk::Key::End => viewer::goto_page(usize::MAX),
                gdk::Key::Escape | gdk::Key::F5 | gdk::Key::q => viewer::toggle_presenting(),
                _ => return glib::Propagation::Proceed,
            }
            return stop;
        }
        if ctrl {
            return match k {
                gdk::Key::f => {
                    find_files();
                    stop
                }
                gdk::Key::b => {
                    toggle_sidebar();
                    stop
                }
                gdk::Key::o => {
                    doc::open_dialog();
                    stop
                }
                gdk::Key::p => {
                    doc::print();
                    stop
                }
                gdk::Key::s if shift => {
                    doc::save_as_dialog();
                    stop
                }
                gdk::Key::s => {
                    doc::save();
                    stop
                }
                gdk::Key::z if !typing && shift => {
                    doc::redo();
                    stop
                }
                gdk::Key::z if !typing => {
                    doc::undo();
                    stop
                }
                gdk::Key::y if !typing => {
                    doc::redo();
                    stop
                }
                gdk::Key::c if !typing && current() == "document" && viewer::copy_selection() => stop,
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    doc::cycle(if shift || key == gdk::Key::ISO_Left_Tab { -1 } else { 1 });
                    stop
                }
                gdk::Key::Page_Down => {
                    doc::cycle(1);
                    stop
                }
                gdk::Key::Page_Up => {
                    doc::cycle(-1);
                    stop
                }
                gdk::Key::w if doc::current().is_some() => {
                    viewer::close_file();
                    stop
                }
                gdk::Key::q | gdk::Key::w => {
                    w2.close();
                    stop
                }
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                    viewer::zoom_by(1.2);
                    stop
                }
                gdk::Key::minus | gdk::Key::KP_Subtract => {
                    viewer::zoom_by(1.0 / 1.2);
                    stop
                }
                gdk::Key::_0 | gdk::Key::KP_0 => {
                    viewer::zoom_fit("fit-width");
                    stop
                }
                gdk::Key::_9 | gdk::Key::KP_9 => {
                    viewer::zoom_fit("fit-page");
                    stop
                }
                _ => glib::Propagation::Proceed,
            };
        }
        if alt && in_doc {
            match key {
                gdk::Key::Left => viewer::go_back(),
                gdk::Key::Right => viewer::go_forward(),
                _ => return glib::Propagation::Proceed,
            }
            return stop;
        }
        match key {
            gdk::Key::F1 => show_shortcuts(),
            gdk::Key::F9 => viewer::toggle_panel(),
            gdk::Key::F10 if in_doc => viewer::open_more_menu(),
            gdk::Key::F11 => toggle_fullscreen(),
            gdk::Key::F5 if in_doc => viewer::toggle_presenting(),
            gdk::Key::Escape if w2.is_fullscreen() => set_fullscreen(false),
            gdk::Key::Escape if READING.with(|r| r.get()) => toggle_reading(),
            gdk::Key::Escape if search.as_ref().is_some_and(|s| s.is_mapped() && !s.text().is_empty()) => {
                if let Some(s) = &search {
                    s.set_text("");
                }
            }
            gdk::Key::Escape if !typing && current() == "document" => viewer::escape(),
            gdk::Key::Delete | gdk::Key::BackSpace if !typing && current() == "document" && viewer::delete_selected() => {}
            _ if typing || alt || !in_doc => return glib::Propagation::Proceed,
            gdk::Key::Page_Down | gdk::Key::space if !shift => viewer::step_page(1),
            gdk::Key::Page_Up => viewer::step_page(-1),
            gdk::Key::space => viewer::step_page(-1),
            gdk::Key::Home => viewer::jump(0),
            gdk::Key::End => viewer::jump(usize::MAX),
            _ => match k {
                gdk::Key::v => viewer::set_tool(viewer::Tool::Select),
                gdk::Key::h => viewer::set_tool(viewer::Tool::Highlight),
                gdk::Key::u => viewer::set_tool(viewer::Tool::Underline),
                gdk::Key::x => viewer::set_tool(viewer::Tool::Strike),
                gdk::Key::d => viewer::set_tool(viewer::Tool::Ink),
                gdk::Key::n => viewer::set_tool(viewer::Tool::Note),
                gdk::Key::t => viewer::set_tool(viewer::Tool::TextBox),
                gdk::Key::e => viewer::set_tool(viewer::Tool::EditText),
                gdk::Key::s => viewer::set_tool(viewer::Tool::Sign),
                gdk::Key::j => viewer::step_page(1),
                gdk::Key::k => viewer::step_page(-1),
                gdk::Key::r => toggle_reading(),
                _ => return glib::Propagation::Proceed,
            },
        }
        stop
    });
    window.add_controller(keys);
}

thread_local! {
    static READING: Cell<bool> = const { Cell::new(false) };
}

/// Reading mode (R): just the document, without going fullscreen.
pub fn toggle_reading() {
    let on = !READING.with(|r| r.get());
    READING.with(|r| r.set(on));
    if let Some(ui) = ui() {
        let u = ui.borrow();
        u.nav.set_visible(!on);
        for w in &u.chrome {
            w.set_visible(!on);
        }
    }
    if !on {
        refresh_compact();
    }
}

pub fn toggle_fullscreen() {
    let on = window().is_some_and(|w| w.is_fullscreen());
    set_fullscreen(!on);
}

pub fn set_fullscreen(on: bool) {
    let Some(w) = window() else { return };
    if on {
        w.fullscreen();
    } else {
        w.unfullscreen();
    }
    apply_fullscreen(on);
}

/// Fullscreen hides the sidebar.
fn apply_fullscreen(on: bool) {
    let Some(ui) = ui() else { return };
    {
        let u = ui.borrow();
        u.nav.set_visible(!on);
        for w in &u.chrome {
            w.set_visible(!on);
        }
        if on {
            u.window.add_css_class("fullscreen");
        } else {
            u.window.remove_css_class("fullscreen");
        }
    }
    if !on {
        viewer::on_unfullscreen();
    }
}

fn on_search(text: &str) {
    let Some(ui) = ui() else { return };
    let q = text.trim().to_string();
    if !q.is_empty() && ui.borrow().current != "library" {
        navigate("library");
    }
    sections::library::set_query(&q);
}

fn set_narrow(narrow: bool) {
    NARROW.with(|n| n.set(narrow));
    let Some(ui) = ui() else { return };
    for page in ui.borrow().pages.values() {
        mark_page(page, narrow);
    }
    viewer::set_narrow(narrow);
    sections::library::set_narrow(narrow);
}

pub fn narrow() -> bool {
    NARROW.with(|n| n.get())
}

fn mark_page(page: &gtk::Widget, narrow: bool) {
    let body = page
        .downcast_ref::<gtk::ScrolledWindow>()
        .and_then(|s| s.child())
        .and_then(|v| v.first_child())
        .unwrap_or_else(|| page.clone());
    if narrow {
        body.add_css_class("narrow");
    } else {
        body.remove_css_class("narrow");
    }
}

fn ensure_built(id: &str) -> bool {
    let Some(ui) = ui() else { return false };
    if ui.borrow().pages.contains_key(id) {
        return true;
    }
    let section = {
        let u = ui.borrow();
        u.sections.iter().find(|s| s.id == id).map(|s| (s.id, s.build, s.fill, s.bare))
    };
    let Some((sid, build, fill, bare)) = section else { return false };
    let page = widgets::page(sid);
    if fill {
        page.fill();
    }
    if bare {
        page.bare();
    }
    build(&page);
    if sid == "document"
        && let Some((center, end)) = viewer::header_parts()
    {
        let u = ui.borrow();
        u.doc_center.append(&center);
        u.doc_end.append(&end);
    }
    let page: gtk::Widget = page.root.upcast();
    mark_page(&page, narrow());
    let stack = ui.borrow().stack.clone();
    stack.add_named(&page, Some(id));
    ui.borrow_mut().pages.insert(id.to_string(), page);
    true
}

pub fn navigate(id: &str) {
    let Some(ui) = ui() else { return };
    // Settings is a dialog over the window, not a page.
    if id == "settings" {
        settings_dialog::open();
        if !ui.borrow().current.is_empty() {
            return;
        }
    }
    let id = if id == "settings" { "library" } else { id };
    let id = if ensure_built(id) { id.to_string() } else { "library".to_string() };
    if !ensure_built(&id) {
        return;
    }
    let mut u = ui.borrow_mut();
    u.stack.set_visible_child_name(&id);
    let changed = u.current != id;
    u.current = id.clone();
    drop(u);
    prefs::update(|p| p.last_section = id);
    refresh_header();
    if changed {
        refresh_nav();
        refresh_compact();
    }
}

/// Show a short message at the bottom of the window.
pub fn toast(message: &str) {
    let Some(ui) = ui() else {
        eprintln!("nyx-pdf: {message}");
        return;
    };
    let overlay = ui.borrow().overlay.clone();
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_max_width_chars(70);
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bx.add_css_class("toast");
    bx.append(&label);
    bx.set_halign(gtk::Align::Center);
    bx.set_valign(gtk::Align::End);
    overlay.add_overlay(&bx);
    glib::timeout_add_local_once(std::time::Duration::from_millis(3500), move || {
        overlay.remove_overlay(&bx);
    });
}

pub fn current() -> String {
    ui().map(|u| u.borrow().current.clone()).unwrap_or_default()
}

pub fn window() -> Option<gtk::ApplicationWindow> {
    ui().map(|u| u.borrow().window.clone())
}

fn snapshot_and_quit(app: &gtk::Application, out: std::path::PathBuf) {
    let Some(ui) = ui() else { return };
    let window = ui.borrow().window.clone();
    window.set_opacity(0.01);
    // A distinct title lets a window rule float it at a set size for screenshots.
    window.set_title(Some("PDF snapshot"));
    window.set_default_size(
        std::env::var("NPDF_SNAPSHOT_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1180),
        std::env::var("NPDF_SNAPSHOT_H").ok().and_then(|v| v.parse().ok()).unwrap_or(820),
    );
    window.present();
    let app = app.clone();
    let delay = std::env::var("NPDF_SNAPSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
        if let Some(child) = window.child() {
            let paintable = gtk::WidgetPaintable::new(Some(&child));
            let (w, h) = (child.width(), child.height());
            let snapshot = gtk::Snapshot::new();
            snapshot.append_color(&gdk::RGBA::BLACK, &gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            paintable.snapshot(&snapshot, w as f64, h as f64);
            if let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) {
                let texture = renderer.render_texture(node, None);
                match texture.save_to_png(&out) {
                    Ok(()) => println!("snapshot {w}x{h} -> {}", out.display()),
                    Err(e) => eprintln!("snapshot failed: {e}"),
                }
            }
        }
        FORCE_CLOSE.with(|f| f.set(true));
        app.quit();
    });
}
