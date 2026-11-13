//! The document viewer and editor: a tab for each open file, one tool strip, a
//! side panel and a column of pages. This file holds the shared state, zoom,
//! scrolling, layouts and the strip; `pageview` draws and handles one page,
//! `tools` turns gestures into edits.

pub mod colour;
mod forms;
mod pageview;
mod panel;
mod script;
mod search;
mod sign;
pub mod thumbs;
mod tools;

use crate::doc::{self, Change, Doc, geom::Rect, render};
use crate::widgets::{self, Page};
use crate::{prefs, theme, window};
use gtk::prelude::*;
use gtk::{gdk, glib};
use pageview::PageView;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub use tools::Tool;

/// Space around and between pages, in pixels.
const PAD_X: f64 = 28.0;
const PAD_Y: f64 = 20.0;
const GAP: f64 = 14.0;

/// Narrowest and widest the side panel can be dragged, in pixels.
const PANEL_MIN: i32 = 170;
const PANEL_MAX: i32 = 520;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    None,
    Width,
    Page,
}

/// How pages sit side by side.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spread {
    Single,
    /// Two at a time: 1–2, 3–4, …
    Pairs,
    /// Like a book: page one alone, then 2–3, 4–5, …
    Book,
}

impl Spread {
    fn from_pref(s: &str) -> Spread {
        match s {
            "pairs" => Spread::Pairs,
            "book" => Spread::Book,
            _ => Spread::Single,
        }
    }
}

/// The pages on each row, in order.
pub fn groups(n: usize, spread: Spread) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    if spread == Spread::Book && n > 0 {
        out.push(vec![0]);
        i = 1;
    }
    let step = if spread == Spread::Single { 1 } else { 2 };
    while i < n {
        out.push((i..(i + step).min(n)).collect());
        i += step;
    }
    out
}

/// A text selection on one page.
pub struct Sel {
    pub page: usize,
    pub rects: Vec<Rect>,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AnnotSel {
    pub page: usize,
    pub id: lopdf::ObjectId,
}

/// What presentation mode replaced, to put back afterwards.
struct Presenting {
    fit: Fit,
    zoom: f64,
}

pub struct View {
    pub stack: gtk::Stack,
    pub scroller: gtk::ScrolledWindow,
    pub column: gtk::Box,
    rows: RefCell<Vec<gtk::Box>>,
    pub pages: RefCell<Vec<Rc<PageView>>>,
    pub zoom: Rc<Cell<f64>>,
    fit: Cell<Fit>,
    tool: Cell<Tool>,
    colour: RefCell<String>,
    colour_css: gtk::CssProvider,
    tool_colours: RefCell<HashMap<Tool, String>>,
    tool_groups: Vec<Rc<ToolGroup>>,
    colour_btn: gtk::MenuButton,
    colour_row: RefCell<Option<gtk::Box>>,
    options: gtk::Box,
    panel: panel::Panel,
    panel_toggle: gtk::ToggleButton,
    panel_slot: gtk::Box,
    handle: gtk::Box,
    canvas: gtk::Overlay,
    /// On narrow windows the panel floats over the pages while this is set.
    panel_floating_open: Cell<bool>,
    syncing: Cell<bool>,
    page_entry: gtk::Entry,
    total: gtk::Label,
    /// The zoom menu's face: "Fit width", "Fit page" or the zoom.
    fit_label: gtk::Label,
    undo_btn: gtk::Button,
    redo_btn: gtk::Button,
    pub back_btn: gtk::Button,
    forward_btn: gtk::Button,
    save_btn: gtk::Button,
    chrome: gtk::Box,
    banner: gtk::Box,
    disk_banner: gtk::Box,
    disk_label: gtk::Label,
    strip: gtk::Box,
    left: gtk::Box,
    right: gtk::Box,
    /// Back, forward and the page number, for the window header's middle.
    head_center: gtk::Box,
    /// The zoom and layout menu, find and the more menu, for the end of the window header.
    head_end: gtk::Box,
    fit_btn: gtk::MenuButton,
    more_btn: gtk::MenuButton,
    pub search: search::Search,
    programmatic: Cell<bool>,
    visible_queued: Cell<bool>,
    lo_cache: RefCell<Option<(PathBuf, Rc<lopdf::Document>)>>,
    pub selection: RefCell<Option<Sel>>,
    pub annot_sel: RefCell<Option<AnnotSel>>,
    /// The signature picture waiting to be placed.
    pub signature: RefCell<Option<PathBuf>>,
    narrow: Cell<bool>,
    /// Bumped when the page colours change, so pictures are redrawn.
    pub epoch: Cell<u32>,
    settling: Cell<bool>,
    presenting: RefCell<Option<Presenting>>,
    /// Zoom and fit for each open file, so switching tabs keeps them.
    view_states: RefCell<HashMap<PathBuf, (f64, Fit)>>,
    /// Where the pointer is over the pages (for zooming around it).
    pointer: Cell<Option<(f64, f64)>>,
    /// Wheel movement past the end of a page, in one-page-at-a-time mode.
    overscroll: Cell<f64>,
}

thread_local! {
    static VIEW: RefCell<Option<Rc<View>>> = const { RefCell::new(None) };
    static EDIT_NOTE_SHOWN: Cell<bool> = const { Cell::new(false) };
}

pub fn view() -> Option<Rc<View>> {
    VIEW.with(|v| v.borrow().clone())
}

/// How pages are recoloured, from the preference and the theme.
pub fn page_colors() -> render::Colors {
    match prefs::get().page_colors.as_str() {
        "invert" => render::Colors::Invert,
        "tint" => {
            let p = theme::palette();
            match (colour::parse_u8(&p.bg), colour::parse_u8(&p.text)) {
                (Some(paper), Some(ink)) => render::Colors::Tint(paper, ink),
                _ => render::Colors::Off,
            }
        }
        _ => render::Colors::Off,
    }
}

/// Run `f` after GTK has laid out the next frames, when sizes are real.
pub fn after_layout(widget: &impl IsA<gtk::Widget>, f: impl FnOnce() + 'static) {
    let f = RefCell::new(Some(f));
    let ticks = Cell::new(0);
    widget.add_tick_callback(move |_, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 3 {
            return glib::ControlFlow::Continue;
        }
        if let Some(f) = f.borrow_mut().take() {
            f();
        }
        glib::ControlFlow::Break
    });
}

fn icon_toggle(icon: &str, tooltip: &str) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.set_child(Some(&gtk::Image::from_icon_name(icon)));
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

fn icon_btn(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.set_valign(gtk::Align::Center);
    b
}

/// The line between groups of tools.
fn sep() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Vertical);
    s.add_css_class("tool-sep");
    s.set_valign(gtk::Align::Center);
    s
}

/// A tool's button in the toolbar: its icon and name, and for a tool with kinds
/// (highlight, underline, strike out) a menu of them. It shows the kind in use.
struct ToolGroup {
    tools: &'static [Tool],
    current: Cell<Tool>,
    toggle: gtk::ToggleButton,
    icon: gtk::Image,
    label: gtk::Label,
    kinds: Option<gtk::MenuButton>,
}

impl ToolGroup {
    fn new(tools: &'static [Tool], first: &mut Option<gtk::ToggleButton>) -> (Rc<ToolGroup>, gtk::Box) {
        let t = tools[0];
        let wrap = widgets::hbox(0);
        wrap.add_css_class("tool");
        wrap.set_valign(gtk::Align::Center);
        let toggle = gtk::ToggleButton::new();
        toggle.add_css_class("tool-main");
        let content = widgets::hbox(6);
        let icon = gtk::Image::from_icon_name(t.icon());
        content.append(&icon);
        let label = widgets::label(t.label(), "tool-name");
        content.append(&label);
        toggle.set_child(Some(&content));
        toggle.set_tooltip_text(Some(&t.tooltip()));
        toggle.update_property(&[gtk::accessible::Property::Label(t.name())]);
        match first {
            Some(f) => toggle.set_group(Some(f)),
            None => *first = Some(toggle.clone()),
        }
        wrap.append(&toggle);
        let mut kinds = None;
        if tools.len() > 1 {
            wrap.add_css_class("has-kinds");
            let more = gtk::MenuButton::new();
            more.add_css_class("tool-kinds");
            more.set_icon_name("pan-down-symbolic");
            more.set_tooltip_text(Some(&format!("Kinds of {}", t.name().to_lowercase())));
            let pop = gtk::Popover::new();
            pop.add_css_class("menu-popover");
            pop.set_has_arrow(false);
            let bx = widgets::vbox(2);
            bx.add_css_class("menu-box");
            for &k in tools {
                let b = menu_item(&pop, k.name(), k.key(), move || set_tool(k));
                if let Some(row) = b.child().and_downcast::<gtk::Box>() {
                    row.prepend(&gtk::Image::from_icon_name(k.icon()));
                }
                bx.append(&b);
            }
            pop.set_child(Some(&bx));
            more.set_popover(Some(&pop));
            wrap.append(&more);
            kinds = Some(more);
        }
        let g = Rc::new(ToolGroup { tools, current: Cell::new(t), toggle: toggle.clone(), icon, label, kinds });
        let (g2, w2) = (g.clone(), wrap.clone());
        toggle.connect_toggled(move |b| {
            if b.is_active() {
                w2.add_css_class("on");
                set_tool(g2.current.get());
            } else {
                w2.remove_css_class("on");
            }
        });
        (g, wrap)
    }

    /// Show `t`, one of this button's kinds.
    fn show(&self, t: Tool) {
        self.current.set(t);
        self.icon.set_icon_name(Some(t.icon()));
        self.label.set_text(t.label());
        self.toggle.set_tooltip_text(Some(&t.tooltip()));
        self.toggle.update_property(&[gtk::accessible::Property::Label(t.name())]);
    }
}

/// A row in a menu popover: the action on the left, its key on the right.
fn menu_item(pop: &gtk::Popover, text: &str, keys: &str, act: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("flat");
    b.add_css_class("menu-item");
    let row = widgets::hbox(16);
    let l = widgets::label(text, "");
    l.set_hexpand(true);
    row.append(&l);
    if !keys.is_empty() {
        row.append(&widgets::label(keys, "dim"));
    }
    b.set_child(Some(&row));
    let pop = pop.clone();
    b.connect_clicked(move |_| {
        pop.popdown();
        act();
    });
    b
}

fn menu_sep() -> gtk::Separator {
    gtk::Separator::new(gtk::Orientation::Horizontal)
}

pub fn build(page: &Page) {
    let v = View::new();
    page.body.append(&v.stack);
    VIEW.with(|c| *c.borrow_mut() = Some(v.clone()));
    v.wire();
    let c = v.colour();
    v.show_colour(&c);
    v.fill_options();
    v.mark_page_colours();
    set_narrow(window::narrow());
    // Show whatever is already open (a file given on the command line).
    if doc::current().is_some() {
        v.restore_view_state();
        v.rebuild();
        v.refresh_chrome();
    }
}

impl View {
    fn new() -> Rc<View> {
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_hexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(if prefs::get().reduce_motion { 0 } else { 160 });

        stack.add_named(
            &widgets::empty_state(
                "npdf-document-symbolic",
                "No file open",
                "Open a PDF to read it, mark it up, fill it in and sign it. You can also drop files on this window.",
                Some(("Open a file", Box::new(doc::open_dialog))),
            ),
            Some("empty"),
        );

        let root = widgets::vbox(0);
        root.add_css_class("viewer");
        let chrome = widgets::vbox(0);
        root.append(&chrome);

        // ----- Toolbar -----
        // One bar over the page: the panel, the tools and the colour on the left,
        // history and saving on the right. When the window is very narrow the two
        // halves stack (see `apply_strip`).
        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        strip.add_css_class("tool-strip");
        let (left, right) = (widgets::hbox(2), widgets::hbox(2));
        left.set_hexpand(true);
        right.set_halign(gtk::Align::End);
        strip.append(&left);
        strip.append(&right);

        let panel_toggle = icon_toggle("sidebar-show-symbolic", "Side panel (F9)");
        panel_toggle.set_active(true);
        panel_toggle.add_css_class("tb-btn");
        left.append(&panel_toggle);
        left.append(&sep());

        // Select | highlight, draw, note, text | sign.
        let mut tool_groups = Vec::new();
        let mut first: Option<gtk::ToggleButton> = None;
        for (i, tools) in Tool::GROUPS.into_iter().enumerate() {
            if matches!(i, 1 | 5) {
                left.append(&sep());
            }
            let (g, wrap) = ToolGroup::new(tools, &mut first);
            left.append(&wrap);
            tool_groups.push(g);
        }
        if let Some(f) = &first {
            f.set_active(true);
        }

        // The colour (and the tool's other settings) for new markup.
        let colour_btn = gtk::MenuButton::new();
        colour_btn.set_tooltip_text(Some("Colour and size"));
        colour_btn.add_css_class("colour-button");
        colour_btn.set_valign(gtk::Align::Center);
        colour_btn.set_margin_start(4);
        let face = widgets::hbox(4);
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("dot");
        dot.add_css_class("tool-dot");
        dot.set_halign(gtk::Align::Center);
        dot.set_valign(gtk::Align::Center);
        let colour_css = gtk::CssProvider::new();
        #[allow(deprecated)]
        dot.style_context().add_provider(&colour_css, gtk::STYLE_PROVIDER_PRIORITY_USER);
        face.append(&dot);
        let chev = gtk::Image::from_icon_name("pan-down-symbolic");
        chev.add_css_class("chevron");
        face.append(&chev);
        colour_btn.set_child(Some(&face));
        let pop = gtk::Popover::new();
        pop.add_css_class("tool-popover");
        let pop_box = widgets::vbox(10);
        let options = widgets::vbox(8);
        options.add_css_class("tool-options");
        pop_box.append(&options);
        pop.set_child(Some(&pop_box));
        colour_btn.set_popover(Some(&pop));
        colour_btn.set_visible(false);
        left.append(&colour_btn);

        // ----- In the window header -----
        // The middle: back and forward after a link, around the page number.
        let head_center = widgets::hbox(4);
        head_center.set_valign(gtk::Align::Center);
        let back_btn = widgets::bar_button("go-previous-symbolic", "Back (Alt+←)");
        let forward_btn = widgets::bar_button("go-next-symbolic", "Forward (Alt+→)");
        back_btn.connect_clicked(|_| go_back());
        forward_btn.connect_clicked(|_| go_forward());

        let nav = widgets::hbox(6);
        nav.add_css_class("page-nav");
        let page_entry = gtk::Entry::new();
        page_entry.add_css_class("page-entry");
        page_entry.set_width_chars(3);
        page_entry.set_max_width_chars(5);
        EditableExt::set_alignment(&page_entry, 0.5);
        page_entry.set_tooltip_text(Some("Go to page"));
        let total = widgets::label("of 1", "page-total");
        nav.append(&page_entry);
        nav.append(&total);
        nav.set_valign(gtk::Align::Center);
        head_center.append(&back_btn);
        head_center.append(&nav);
        head_center.append(&forward_btn);

        // The end: one menu for zoom and layout, find, and the actions without a
        // button of their own.
        let head_end = widgets::hbox(2);
        head_end.set_valign(gtk::Align::Center);
        let fit_btn = gtk::MenuButton::new();
        fit_btn.add_css_class("fit-button");
        fit_btn.set_valign(gtk::Align::Center);
        fit_btn.set_tooltip_text(Some("Zoom and page layout"));
        let face = widgets::hbox(6);
        face.append(&gtk::Image::from_icon_name("zoom-in-symbolic"));
        let fit_label = widgets::label("Fit width", "");
        fit_label.set_width_chars(7);
        fit_label.set_xalign(0.0);
        face.append(&fit_label);
        let chev = gtk::Image::from_icon_name("pan-down-symbolic");
        chev.add_css_class("chevron");
        face.append(&chev);
        fit_btn.set_child(Some(&face));
        let fit_pop = gtk::Popover::new();
        fit_pop.add_css_class("menu-popover");
        fit_pop.set_has_arrow(false);
        {
            let fp = fit_pop.clone();
            fit_pop.connect_show(move |_| fp.set_child(Some(&zoom_menu(&fp))));
        }
        fit_btn.set_popover(Some(&fit_pop));
        fit_btn.set_margin_end(4);
        head_end.append(&fit_btn);

        let find = widgets::bar_button("system-search-symbolic", "Find in this file (Ctrl+F)");
        find.connect_clicked(|_| focus_search());
        head_end.append(&find);

        let more_btn = gtk::MenuButton::new();
        more_btn.set_icon_name("view-more-symbolic");
        more_btn.set_tooltip_text(Some("More: save a copy, print, present, page colours, close (F10)"));
        more_btn.add_css_class("bar-button");
        more_btn.set_valign(gtk::Align::Center);
        let more_pop = gtk::Popover::new();
        more_pop.add_css_class("menu-popover");
        {
            let mp = more_pop.clone();
            more_pop.connect_show(move |_| mp.set_child(Some(&more_menu(&mp))));
        }
        more_btn.set_popover(Some(&more_pop));
        head_end.append(&more_btn);

        // ----- The strip's right: history and saving -----
        let undo_btn = icon_btn("edit-undo-symbolic", "Undo (Ctrl+Z)");
        let redo_btn = icon_btn("edit-redo-symbolic", "Redo (Ctrl+Shift+Z)");
        for b in [&undo_btn, &redo_btn] {
            b.add_css_class("tb-btn");
            right.append(b);
        }
        undo_btn.connect_clicked(|_| doc::undo());
        redo_btn.connect_clicked(|_| doc::redo());

        // The view's one primary button, there only while there's something to save.
        let save_btn = gtk::Button::with_label("Save");
        save_btn.set_tooltip_text(Some("Save (Ctrl+S)"));
        save_btn.add_css_class("suggested-action");
        save_btn.add_css_class("save-button");
        save_btn.set_valign(gtk::Align::Center);
        save_btn.set_margin_start(6);
        save_btn.set_visible(false);
        save_btn.connect_clicked(|_| doc::save());
        right.append(&save_btn);
        // The strip never holds the window wide: it stacks when there's no room (see
        // `apply_strip`), and in the narrowest windows it can be scrolled sideways.
        let strip_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&strip)
            .build();
        chrome.append(&strip_scroll);

        // ----- Notices -----
        let banner = widgets::banner("This file is open for reading only: it's encrypted or damaged, so it can't be edited.", true);
        banner.set_visible(false);
        banner.add_css_class("viewer-banner");
        chrome.append(&banner);

        let disk_banner = widgets::banner("", true);
        disk_banner.add_css_class("viewer-banner");
        disk_banner.set_visible(false);
        let disk_label = disk_banner.first_child().and_then(|i| i.next_sibling()).and_downcast::<gtk::Label>().expect("banner has a label");
        let reload = gtk::Button::with_label("Reload it");
        reload.set_tooltip_text(Some("Read the file again; your unsaved changes are lost"));
        reload.set_valign(gtk::Align::Center);
        reload.connect_clicked(|_| {
            if let Some(d) = doc::current() {
                doc::reload_discarding(&d);
            }
        });
        let keep = gtk::Button::with_label("Keep mine");
        keep.set_tooltip_text(Some("Keep your changes; saving will replace the other version"));
        keep.set_valign(gtk::Align::Center);
        keep.connect_clicked(|_| {
            if let Some(d) = doc::current() {
                d.ignore_disk_change();
            }
        });
        disk_banner.append(&reload);
        disk_banner.append(&keep);
        chrome.append(&disk_banner);

        let search = search::Search::new();
        chrome.append(&search.bar);

        // ----- Panel and canvas -----
        let split = widgets::hbox(0);
        split.set_vexpand(true);
        let panel_slot = widgets::hbox(0);
        let panel = panel::Panel::new();
        panel_slot.append(&panel.root);
        let handle = widgets::hbox(0);
        handle.add_css_class("panel-handle");
        handle.set_cursor_from_name(Some("col-resize"));
        handle.set_tooltip_text(Some("Drag to resize the panel"));
        panel_slot.append(&handle);
        split.append(&panel_slot);

        let column = widgets::vbox(GAP as i32);
        column.add_css_class("page-column");
        column.set_halign(gtk::Align::Center);
        column.set_margin_top(PAD_Y as i32);
        column.set_margin_bottom(PAD_Y as i32);
        column.set_margin_start(PAD_X as i32);
        column.set_margin_end(PAD_X as i32);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::External)
            .hexpand(true)
            .vexpand(true)
            .child(&column)
            .build();
        scroller.add_css_class("canvas");
        let canvas = gtk::Overlay::new();
        canvas.set_child(Some(&scroller));
        split.append(&canvas);
        root.append(&split);
        stack.add_named(&root, Some("viewer"));
        stack.set_visible_child_name("empty");

        let p = prefs::get();
        Rc::new(View {
            stack,
            scroller,
            column,
            rows: RefCell::new(Vec::new()),
            pages: RefCell::new(Vec::new()),
            zoom: Rc::new(Cell::new(1.0)),
            fit: Cell::new(Fit::Width),
            tool: Cell::new(Tool::Select),
            colour: RefCell::new(p.highlight_color.clone()),
            colour_css,
            tool_colours: RefCell::new(HashMap::new()),
            tool_groups,
            colour_btn,
            colour_row: RefCell::new(None),
            options,
            panel,
            panel_toggle,
            panel_slot,
            handle,
            canvas,
            panel_floating_open: Cell::new(false),
            syncing: Cell::new(false),
            page_entry,
            total,
            fit_label,
            undo_btn,
            redo_btn,
            back_btn,
            forward_btn,
            save_btn,
            chrome,
            banner,
            disk_banner,
            disk_label,
            strip,
            left,
            right,
            head_center,
            head_end,
            fit_btn,
            more_btn,
            search,
            programmatic: Cell::new(false),
            visible_queued: Cell::new(false),
            lo_cache: RefCell::new(None),
            selection: RefCell::new(None),
            annot_sel: RefCell::new(None),
            signature: RefCell::new(None),
            narrow: Cell::new(false),
            epoch: Cell::new(0),
            settling: Cell::new(false),
            presenting: RefCell::new(None),
            view_states: RefCell::new(HashMap::new()),
            pointer: Cell::new(None),
            overscroll: Cell::new(0.0),
        })
    }

    fn wire(self: &Rc<Self>) {
        let v = self.clone();
        doc::subscribe(&self.stack, move |c| v.on_change(c));

        let v = self.clone();
        self.scroller.vadjustment().connect_value_changed(move |_| v.queue_visible());
        let v = self.clone();
        self.scroller.hadjustment().connect_value_changed(move |_| v.queue_visible());
        // Fit modes follow the window's size.
        let v = self.clone();
        self.scroller.hadjustment().connect_page_size_notify(move |_| v.on_resize());
        let v = self.clone();
        self.scroller.vadjustment().connect_page_size_notify(move |_| v.on_resize());

        // Remember where the pointer is, to zoom around it.
        let motion = gtk::EventControllerMotion::new();
        let v = self.clone();
        motion.connect_motion(move |_, x, y| v.pointer.set(Some((x, y))));
        let v = self.clone();
        motion.connect_leave(move |_| v.pointer.set(None));
        self.scroller.add_controller(motion);

        // Ctrl+wheel zooms; a plain wheel scrolls, and in one-page-at-a-time mode
        // turns the page when pushed past either end.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        let v = self.clone();
        wheel.connect_scroll(move |c, _, dy| {
            if c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                let z = v.zoom.get() * if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
                v.set_fit_zoom_at(z, Fit::None, v.pointer.get());
                return glib::Propagation::Stop;
            }
            if !v.continuous() {
                let adj = v.scroller.vadjustment();
                let at_end = adj.value() + adj.page_size() >= adj.upper() - 1.0;
                let at_start = adj.value() <= 0.5;
                if (dy > 0.0 && at_end) || (dy < 0.0 && at_start) {
                    let pushed = v.overscroll.get() + dy;
                    v.overscroll.set(pushed);
                    if pushed.abs() >= 2.5 {
                        v.overscroll.set(0.0);
                        let dir = if pushed > 0.0 { 1 } else { -1 };
                        v.step(dir);
                        // Land at the matching end of the new page.
                        let (v2, adj2) = (v.clone(), adj.clone());
                        after_layout(&v.column, move || {
                            v2.programmatic.set(true);
                            adj2.set_value(if dir > 0 { 0.0 } else { (adj2.upper() - adj2.page_size()).max(0.0) });
                            v2.programmatic.set(false);
                        });
                    }
                    return glib::Propagation::Stop;
                }
                v.overscroll.set(0.0);
            }
            glib::Propagation::Proceed
        });
        self.scroller.add_controller(wheel);

        let pinch = gtk::GestureZoom::new();
        let base = Rc::new(Cell::new(1.0));
        let b = base.clone();
        pinch.connect_begin(move |_, _| {
            if let Some(v) = view() {
                b.set(v.zoom.get());
            }
        });
        let b = base.clone();
        pinch.connect_scale_changed(move |g, s| {
            if let Some(v) = view() {
                v.set_fit_zoom_at(b.get() * s, Fit::None, g.bounding_box_center());
            }
        });
        self.scroller.add_controller(pinch);

        // The mouse's back and forward buttons; a right-click goes back a slide when presenting.
        let buttons = gtk::GestureClick::new();
        buttons.set_button(0);
        buttons.set_propagation_phase(gtk::PropagationPhase::Capture);
        let v = self.clone();
        buttons.connect_pressed(move |g, _, _, _| match g.current_button() {
            8 => {
                go_back();
                g.set_state(gtk::EventSequenceState::Claimed);
            }
            9 => {
                go_forward();
                g.set_state(gtk::EventSequenceState::Claimed);
            }
            3 if v.is_presenting() => {
                v.step(-1);
                g.set_state(gtk::EventSequenceState::Claimed);
            }
            _ => {}
        });
        self.scroller.add_controller(buttons);

        let v = self.clone();
        self.panel_toggle.connect_toggled(move |b| {
            if v.syncing.get() {
                return;
            }
            if v.narrow.get() {
                v.panel_floating_open.set(b.is_active());
            } else {
                let on = b.is_active();
                prefs::update(|p| p.panel_visible = on);
            }
            v.apply_panel();
        });

        // Drag the panel's edge to resize it.
        let drag = gtk::GestureDrag::new();
        let start = Rc::new(Cell::new(0));
        let s = start.clone();
        drag.connect_drag_begin(move |_, _, _| s.set(prefs::get().panel_width));
        let v = self.clone();
        let s = start.clone();
        drag.connect_drag_update(move |_, dx, _| {
            let w = (s.get() + dx.round() as i32).clamp(PANEL_MIN, PANEL_MAX);
            v.panel.root.set_size_request(w, -1);
        });
        let v = self.clone();
        drag.connect_drag_end(move |_, dx, _| {
            let w = (start.get() + dx.round() as i32).clamp(PANEL_MIN, PANEL_MAX);
            prefs::update(|p| p.panel_width = w);
            v.panel.set_width(w);
        });
        self.handle.add_controller(drag);

        let v = self.clone();
        self.page_entry.connect_activate(move |e| {
            if let Ok(n) = e.text().trim().parse::<usize>() {
                v.jump(n.saturating_sub(1));
            }
            if let Some(d) = doc::current() {
                e.set_text(&(d.page() + 1).to_string());
            }
        });
        let v = self.clone();
        theme::subscribe(&self.stack, move || {
            v.epoch.set(v.epoch.get() + 1);
            v.mark_page_colours();
            thumbs::clear();
            v.queue_visible();
            for p in v.pages.borrow().iter() {
                p.marks.queue_draw();
            }
            v.panel.refresh_thumbs();
        });
    }

    fn on_change(self: &Rc<Self>, c: Change) {
        match c {
            Change::Opened => {
                self.restore_view_state();
                *self.selection.borrow_mut() = None;
                *self.annot_sel.borrow_mut() = None;
                self.search.reset();
                thumbs::clear();
                self.rebuild();
                self.refresh_chrome();
            }
            Change::Closed => {
                self.clear_pages();
                self.set_presenting(false);
                self.stack.set_visible_child_name("empty");
                self.refresh_chrome();
            }
            Change::Structure => {
                *self.selection.borrow_mut() = None;
                *self.annot_sel.borrow_mut() = None;
                thumbs::clear();
                self.rebuild();
                self.refresh_chrome();
                self.search.on_change(c);
            }
            Change::Content => {
                *self.selection.borrow_mut() = None;
                // The selected annotation may be gone or changed; keep it only if it still exists.
                self.validate_annot_sel();
                thumbs::clear();
                for p in self.pages.borrow().iter() {
                    p.invalidate();
                }
                self.queue_visible();
                self.panel.refresh_annots();
                self.panel.refresh_thumbs();
            }
            Change::Dirty | Change::Files => self.refresh_chrome(),
            Change::Page => self.refresh_readout(),
        }
    }

    /// Zoom as it was the last time this file was showing, or the default.
    fn restore_view_state(&self) {
        let Some(d) = doc::current() else { return };
        if self.is_presenting() {
            return;
        }
        let saved = self.view_states.borrow().get(&d.path()).copied();
        match saved {
            Some((z, fit)) => {
                self.zoom.set(z);
                self.fit.set(fit);
            }
            None => {
                self.fit.set(match prefs::get().default_zoom.as_str() {
                    "fit-page" => Fit::Page,
                    "100" => Fit::None,
                    _ => Fit::Width,
                });
                if self.fit.get() == Fit::None {
                    self.zoom.set(1.0);
                }
            }
        }
    }

    fn clear_pages(&self) {
        while let Some(c) = self.column.first_child() {
            self.column.remove(&c);
        }
        for p in self.pages.borrow().iter() {
            p.release();
        }
        self.pages.borrow_mut().clear();
        self.rows.borrow_mut().clear();
    }

    /// Pages run one after another (not while presenting).
    pub fn continuous(&self) -> bool {
        !self.is_presenting() && prefs::get().continuous
    }

    pub fn spread(&self) -> Spread {
        if self.is_presenting() { Spread::Single } else { Spread::from_pref(&prefs::get().spread) }
    }

    fn groups(&self) -> Vec<Vec<usize>> {
        groups(doc::current().map_or(0, |d| d.n_pages()), self.spread())
    }

    /// Build a frame for every page, in rows when pages sit side by side.
    pub fn rebuild(self: &Rc<Self>) {
        self.clear_pages();
        let Some(d) = doc::current() else { return };
        // Until the new layout has settled, scrolling doesn't mean the reader moved.
        self.programmatic.set(true);
        self.stack.set_visible_child_name("viewer");
        *self.lo_cache.borrow_mut() = None;
        let mut pages = Vec::new();
        let mut rows = Vec::new();
        for g in groups(d.n_pages(), self.spread()) {
            let row = widgets::hbox(GAP as i32);
            row.set_halign(gtk::Align::Center);
            for i in g {
                let pv = PageView::new(i, self.zoom.clone());
                row.append(&pv.frame);
                pages.push(pv);
            }
            self.column.append(&row);
            rows.push(row);
        }
        *self.pages.borrow_mut() = pages;
        *self.rows.borrow_mut() = rows;
        self.apply_fit();
        self.layout_pages();
        self.panel.rebuild();
        let v = self.clone();
        let page = d.page();
        after_layout(&self.column, move || {
            v.apply_strip();
            v.apply_fit();
            v.scroll_to_page(page);
            v.queue_visible();
        });
    }

    pub fn relayout(self: &Rc<Self>) {
        self.layout_pages();
        if let Some(d) = doc::current() {
            self.scroll_to_page(d.page());
        }
        self.queue_visible();
    }

    /// One page (or spread) at a time shows just the current one.
    fn layout_pages(&self) {
        let Some(d) = doc::current() else { return };
        let continuous = self.continuous();
        let groups = self.groups();
        let rows = self.rows.borrow();
        let pages = self.pages.borrow();
        for (g, row) in groups.iter().zip(rows.iter()) {
            let show = continuous || g.contains(&d.page());
            row.set_visible(show);
            for &i in g {
                if let Some(p) = pages.get(i) {
                    p.frame.set_visible(show);
                }
            }
        }
        drop((rows, pages));
        self.apply_sizes();
    }

    fn apply_sizes(&self) {
        let Some(d) = doc::current() else { return };
        let z = self.zoom.get();
        for p in self.pages.borrow().iter() {
            let (w, h) = d.page_size(p.index);
            p.set_size(w, h, z);
        }
        self.fit_label.set_text(&match self.fit.get() {
            Fit::Width => "Fit width".to_string(),
            Fit::Page => "Fit page".to_string(),
            Fit::None => format!("{:.0}%", z * 100.0),
        });
        if !self.is_presenting() {
            self.view_states.borrow_mut().insert(d.path(), (z, self.fit.get()));
        }
    }

    fn viewport(&self) -> (f64, f64) {
        let w = self.scroller.hadjustment().page_size();
        let h = self.scroller.vadjustment().page_size();
        let w = if w > 1.0 { w } else { f64::from(self.scroller.width()) };
        let h = if h > 1.0 { h } else { f64::from(self.scroller.height()) };
        // Before the first layout, guess from the window.
        let fallback = window::window().map(|w| (f64::from(w.width()) - 520.0, f64::from(w.height()) - 160.0)).unwrap_or((800.0, 600.0));
        (if w > 1.0 { w } else { fallback.0.max(300.0) }, if h > 1.0 { h } else { fallback.1.max(300.0) })
    }

    fn apply_fit(self: &Rc<Self>) {
        let Some(d) = doc::current() else { return };
        let (vw, vh) = self.viewport();
        let pad = if self.is_presenting() { 0.0 } else { 1.0 };
        let avail_w = (vw - 2.0 * PAD_X * pad).max(100.0);
        let avail_h = (vh - 2.0 * PAD_Y * pad).max(100.0);
        // A row's width in points, and the gaps between its pages in pixels.
        let row = |g: &[usize]| (g.iter().map(|&i| d.page_size(i).0).sum::<f64>(), GAP * (g.len().saturating_sub(1)) as f64);
        let groups = self.groups();
        let z = match self.fit.get() {
            Fit::None => return,
            Fit::Width => groups.iter().map(|g| {
                let (w, gaps) = row(g);
                (avail_w - gaps).max(50.0) / w.max(1.0)
            })
            .fold(f64::MAX, f64::min),
            Fit::Page => {
                let g = groups.iter().find(|g| g.contains(&d.page())).cloned().unwrap_or_default();
                let (w, gaps) = row(&g);
                let h = g.iter().map(|&i| d.page_size(i).1).fold(1.0, f64::max);
                ((avail_w - gaps).max(50.0) / w.max(1.0)).min(avail_h / h)
            }
        };
        let z = if z.is_finite() { z.clamp(0.1, 8.0) } else { 1.0 };
        if (z - self.zoom.get()).abs() > 0.0005 {
            self.zoom.set(z);
            self.apply_sizes();
            self.queue_visible();
        }
    }

    /// Side by side when there's room, stacked when there isn't.
    fn apply_strip(&self) {
        let w = self.stack.width();
        let natural = |b: &gtk::Box| b.measure(gtk::Orientation::Horizontal, -1).1;
        // Both halves, the gap between them, and the strip's padding.
        let need = natural(&self.left) + natural(&self.right) + 12 + 40;
        let stacked = w > 0 && w < need;
        let want = if stacked { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
        if self.strip.orientation() != want {
            self.strip.set_orientation(want);
            self.right.set_halign(if stacked { gtk::Align::Start } else { gtk::Align::End });
            self.strip.set_spacing(if stacked { 2 } else { 12 });
        }
    }

    fn on_resize(self: &Rc<Self>) {
        self.apply_strip();
        if self.fit.get() != Fit::None && !self.settling.get() {
            self.apply_fit();
        }
        self.queue_visible();
    }

    /// Where a widget sits in the scrolled content (the coordinates the adjustments use).
    /// Measured against the column, which doesn't move when the view scrolls, then
    /// shifted by where the column sits: its margins, and centred when narrower than the view.
    fn bounds_of(&self, w: &impl IsA<gtk::Widget>) -> Option<gtk::graphene::Rect> {
        let b = w.compute_bounds(&self.column)?;
        let content_w = self.scroller.hadjustment().upper().max(self.scroller.hadjustment().page_size());
        let x = ((content_w - f64::from(self.column.width())) / 2.0).max(PAD_X);
        Some(gtk::graphene::Rect::new(b.x() + x as f32, b.y() + PAD_Y as f32, b.width(), b.height()))
    }

    /// Set the zoom (and the fit mode it came from), keeping the reader's place.
    pub fn set_fit_zoom(self: &Rc<Self>, z: f64, fit: Fit) {
        self.set_fit_zoom_at(z, fit, None);
    }

    /// Set the zoom, keeping the spot under `anchor` (in the scroller, pixels) where
    /// it is; without one, the middle of the view stays put.
    pub fn set_fit_zoom_at(self: &Rc<Self>, z: f64, fit: Fit, anchor: Option<(f64, f64)>) {
        let z = z.clamp(0.1, 8.0);
        let (hadj, vadj) = (self.scroller.hadjustment(), self.scroller.vadjustment());
        let a = anchor.unwrap_or((hadj.page_size() / 2.0, vadj.page_size() / 2.0));
        let (cx, cy) = (hadj.value() + a.0, vadj.value() + a.1);
        let old = self.zoom.get();
        // The page under the anchor (or the nearest one), and the point on it in points.
        let mark = self
            .pages
            .borrow()
            .iter()
            .filter(|p| p.frame.is_visible())
            .filter_map(|p| self.bounds_of(&p.frame).map(|b| (p.index, b)))
            .map(|(i, b)| {
                let (x, y, w, h) = (f64::from(b.x()), f64::from(b.y()), f64::from(b.width()), f64::from(b.height()));
                let dx = (x - cx).max(0.0).max(cx - (x + w));
                let dy = (y - cy).max(0.0).max(cy - (y + h));
                (dx.hypot(dy), i, ((cx - x) / old, (cy - y) / old))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, i, p)| (i, p));
        self.fit.set(fit);
        if fit != Fit::None {
            self.apply_fit();
        } else {
            self.zoom.set(z);
        }
        self.apply_sizes();
        let v = self.clone();
        after_layout(&self.column, move || {
            let page = mark.and_then(|(i, p)| v.pages.borrow().get(i).cloned().map(|pv| (pv, p)));
            if let Some((pv, (px, py))) = page
                && let Some(b) = v.bounds_of(&pv.frame)
            {
                let nz = v.zoom.get();
                v.programmatic.set(true);
                hadj.set_value(f64::from(b.x()) + px * nz - a.0);
                vadj.set_value(f64::from(b.y()) + py * nz - a.1);
                v.programmatic.set(false);
            }
            v.queue_visible();
        });
        self.queue_visible();
    }

    pub fn queue_visible(self: &Rc<Self>) {
        if self.visible_queued.replace(true) {
            return;
        }
        let v = self.clone();
        glib::idle_add_local_once(move || {
            v.visible_queued.set(false);
            v.update_visible();
        });
    }

    /// Draw what's on screen, drop what's far away, and work out which page the reader is on.
    fn update_visible(self: &Rc<Self>) {
        let Some(d) = doc::current() else { return };
        let (hadj, vadj) = (self.scroller.hadjustment(), self.scroller.vadjustment());
        let (left, vw) = (hadj.value(), hadj.page_size().max(1.0));
        let (top, vh) = (vadj.value(), vadj.page_size().max(1.0));
        let continuous = self.continuous();
        let z = self.zoom.get();
        let mut best = (d.page(), f64::MIN);
        let pages = self.pages.borrow().clone();
        for pv in &pages {
            if !pv.frame.is_visible() {
                pv.release();
                continue;
            }
            let Some(b) = self.bounds_of(&pv.frame) else { continue };
            let (x, y, w, h) = (f64::from(b.x()), f64::from(b.y()), f64::from(b.width()), f64::from(b.height()));
            let overlap = (y + h).min(top + vh) - y.max(top);
            if y + h > top - vh && y < top + 2.0 * vh {
                pv.ensure_rendered(self, &d);
                // The part of the page on screen, in points.
                let seen = Rect::new(
                    ((left - x) / z).max(0.0),
                    ((top - y) / z).max(0.0),
                    ((left + vw - x) / z).min(w / z),
                    ((top + vh - y) / z).min(h / z),
                );
                let on_screen = overlap > 0.0 && x < left + vw && x + w > left;
                pv.ensure_detail(self, &d, if on_screen { seen } else { Rect::default() });
            } else if y + h < top - 4.0 * vh || y > top + 5.0 * vh {
                pv.release();
            }
            if overlap > best.1 {
                best = (pv.index, overlap);
            }
        }
        if continuous && !self.programmatic.get() && best.1 > 0.0 {
            d.set_page(best.0);
        }
    }

    pub fn page_top(&self, i: usize) -> f64 {
        self.pages.borrow().get(i).and_then(|p| self.bounds_of(&p.frame)).map(|b| f64::from(b.y())).unwrap_or(0.0)
    }

    pub fn scroll_to_page(self: &Rc<Self>, i: usize) {
        let adj = self.scroller.vadjustment();
        self.programmatic.set(true);
        if self.continuous() {
            adj.set_value((self.page_top(i) - 10.0).max(0.0));
        } else {
            adj.set_value(0.0);
        }
        let v = self.clone();
        glib::idle_add_local_once(move || {
            v.programmatic.set(false);
            v.queue_visible();
        });
    }

    pub fn goto_page(self: &Rc<Self>, i: usize) {
        let Some(d) = doc::current() else { return };
        let i = i.min(d.n_pages().saturating_sub(1));
        d.set_page(i);
        if !self.continuous() {
            self.layout_pages();
            if self.fit.get() == Fit::Page {
                self.apply_fit();
            }
        }
        self.scroll_to_page(i);
    }

    /// Go to a page, remembering this one so Back returns to it.
    pub fn jump(self: &Rc<Self>, i: usize) {
        if let Some(d) = doc::current()
            && d.page() != i
        {
            d.remember();
        }
        self.goto_page(i);
        self.refresh_readout();
    }

    /// The next or previous page, or spread when pages sit side by side.
    pub fn step(self: &Rc<Self>, by: i32) {
        let Some(d) = doc::current() else { return };
        let groups = self.groups();
        let Some(at) = groups.iter().position(|g| g.contains(&d.page())) else { return };
        let next = (at as i64 + i64::from(by)).clamp(0, groups.len() as i64 - 1) as usize;
        if let Some(&first) = groups[next].first() {
            self.goto_page(first);
        }
    }

    pub fn tool(&self) -> Tool {
        self.tool.get()
    }

    pub fn colour(&self) -> String {
        self.colour.borrow().clone()
    }

    /// The user picked a colour for the current tool.
    pub fn set_colour(&self, hex: &str) {
        self.tool_colours.borrow_mut().insert(self.tool.get(), hex.to_string());
        self.show_colour(hex);
    }

    /// Settings changed the highlight colour.
    pub fn set_highlight_colour(&self, hex: &str) {
        self.tool_colours.borrow_mut().insert(Tool::Highlight, hex.to_string());
        if self.tool.get() == Tool::Highlight {
            self.show_colour(hex);
        }
    }

    fn show_colour(&self, hex: &str) {
        *self.colour.borrow_mut() = hex.to_string();
        self.colour_css.load_from_string(&format!("box.tool-dot {{ background: {hex}; }}"));
    }

    /// The tool popover: the colour, plus whatever else the tool has.
    fn fill_options(self: &Rc<Self>) {
        let t = self.tool.get();
        self.colour_btn.set_visible(!matches!(t, Tool::Select | Tool::Sign));
        while let Some(c) = self.options.first_child() {
            self.options.remove(&c);
        }
        // The colour row is rebuilt so its selected swatch matches this tool.
        let row = colour::row(&self.colour(), |hex| {
            if let Some(v) = view() {
                v.set_colour(&hex);
            }
        });
        self.options.append(&widgets::label("Color", "popover-title"));
        self.options.append(&row);
        *self.colour_row.borrow_mut() = Some(row);
        let p = prefs::get();
        let slider = |title: &str, min: f64, max: f64, step: f64, value: f64, unit: &'static str, set: fn(&mut prefs::Prefs, f64)| {
            let bx = widgets::vbox(2);
            let head = widgets::hbox(8);
            let t = widgets::label(title, "popover-title");
            t.set_hexpand(true);
            head.append(&t);
            let readout = widgets::label("", "tnum");
            readout.add_css_class("dim");
            head.append(&readout);
            bx.append(&head);
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, step);
            scale.set_value(value);
            scale.set_draw_value(false);
            let show = move |r: &gtk::Label, v: f64| {
                r.set_text(&if unit == "%" { format!("{:.0}%", v * 100.0) } else { format!("{v:.1} {unit}") });
            };
            show(&readout, value);
            scale.connect_value_changed(move |s| {
                let v = s.value();
                show(&readout, v);
                prefs::update(|p| set(p, v));
            });
            bx.append(&scale);
            bx
        };
        match t {
            Tool::Highlight => {
                self.options.append(&slider("Opacity", 0.15, 1.0, 0.05, p.highlight_opacity, "%", |p, v| p.highlight_opacity = v));
            }
            Tool::Ink => {
                self.options.append(&slider("Pen width", 0.5, 12.0, 0.5, p.ink_width, "pt", |p, v| p.ink_width = v));
            }
            Tool::TextBox => {
                self.options.append(&slider("Text size", 6.0, 48.0, 1.0, p.text_size, "pt", |p, v| p.text_size = v));
            }
            _ => {}
        }
    }

    /// The loaded form of the current generation, for reading annotations.
    pub fn lo(&self) -> Option<Rc<lopdf::Document>> {
        let d = doc::current()?;
        let g = d.gen_path();
        if let Some((cg, lo)) = self.lo_cache.borrow().as_ref()
            && *cg == g
        {
            return Some(lo.clone());
        }
        let lo = Rc::new(lopdf::Document::load(&g).ok()?);
        *self.lo_cache.borrow_mut() = Some((g, lo.clone()));
        Some(lo)
    }

    fn validate_annot_sel(&self) {
        let sel = *self.annot_sel.borrow();
        if let Some(s) = sel {
            let alive = self.lo().is_some_and(|lo| doc::annots::exists(&lo, s.id));
            if !alive {
                *self.annot_sel.borrow_mut() = None;
            }
        }
        for p in self.pages.borrow().iter() {
            p.marks.queue_draw();
        }
    }

    /// Tell the stylesheet whether pages are recoloured (form fields and page edges follow).
    fn mark_page_colours(&self) {
        if prefs::get().page_colors == "off" {
            self.stack.remove_css_class("pages-themed");
        } else {
            self.stack.add_css_class("pages-themed");
        }
    }

    /// Put the side panel where it belongs: beside the pages, or floating over them
    /// on a narrow window. The toggle shows whether it's actually visible.
    fn apply_panel(&self) {
        let root = &self.panel.root;
        let narrow = self.narrow.get();
        let floating = root.parent().is_some_and(|p| p == *self.canvas.upcast_ref::<gtk::Widget>());
        if narrow && !floating {
            root.unparent();
            self.canvas.add_overlay(root);
            root.add_css_class("floating");
            root.set_halign(gtk::Align::Start);
        } else if !narrow && floating {
            self.canvas.remove_overlay(root);
            self.panel_slot.prepend(root);
            root.remove_css_class("floating");
            root.set_halign(gtk::Align::Fill);
            self.panel_floating_open.set(false);
        }
        let shown = !self.is_presenting() && doc::current().is_some() && if narrow { self.panel_floating_open.get() } else { prefs::get().panel_visible };
        let width = prefs::get().panel_width.clamp(PANEL_MIN, PANEL_MAX);
        let width = if narrow { width.min((self.canvas.width() - 48).max(PANEL_MIN)) } else { width };
        root.set_size_request(width, -1);
        root.set_visible(shown);
        self.handle.set_visible(shown && !narrow);
        self.syncing.set(true);
        self.panel_toggle.set_active(shown);
        self.syncing.set(false);
    }

    /// The floating panel was used to go somewhere: get it out of the way.
    pub fn panel_used(&self) {
        if self.narrow.get() && self.panel_floating_open.replace(false) {
            self.apply_panel();
        }
    }

    /// Everything in the toolbar and the header that follows the document.
    fn refresh_chrome(self: &Rc<Self>) {
        let d = doc::current();
        match &d {
            Some(d) => {
                self.total.set_text(&format!("of {}", d.n_pages()));
                self.banner.set_visible(!d.editable());
                self.disk_banner.set_visible(d.changed_on_disk());
                self.disk_label.set_text(&format!("{} was changed by another program. Reload it, or keep your unsaved changes?", d.file_name()));
                self.undo_btn.set_sensitive(d.can_undo());
                self.redo_btn.set_sensitive(d.can_redo());
                self.save_btn.set_visible(d.dirty());
            }
            None => {
                self.undo_btn.set_sensitive(false);
                self.redo_btn.set_sensitive(false);
                self.save_btn.set_visible(false);
                self.banner.set_visible(false);
                self.disk_banner.set_visible(false);
            }
        }
        self.refresh_readout();
        self.apply_panel();
    }

    fn refresh_readout(&self) {
        if let Some(d) = doc::current() {
            let text = (d.page() + 1).to_string();
            if self.page_entry.text() != text && !self.page_entry.has_focus() {
                self.page_entry.set_text(&text);
            }
            self.panel.highlight_page(d.page());
            self.back_btn.set_sensitive(d.can_go_back());
            self.forward_btn.set_sensitive(d.can_go_forward());
        }
    }

    pub fn set_tool(self: &Rc<Self>, t: Tool) {
        if self.tool.replace(t) == t {
            return;
        }
        if let Some(g) = self.tool_groups.iter().find(|g| g.tools.contains(&t)) {
            g.show(t);
            if !g.toggle.is_active() {
                g.toggle.set_active(true);
            }
        }
        *self.annot_sel.borrow_mut() = None;
        *self.selection.borrow_mut() = None;
        let colour = self.tool_colours.borrow().get(&t).cloned().unwrap_or_else(|| {
            if t == Tool::Select { prefs::get().highlight_color } else { t.default_colour() }
        });
        self.show_colour(&colour);
        self.fill_options();
        for p in self.pages.borrow().iter() {
            p.set_cursor_for(t);
            p.marks.queue_draw();
        }
        match t {
            Tool::Sign => tools::choose_signature(),
            Tool::EditText if !EDIT_NOTE_SHOWN.with(|s| s.replace(true)) => {
                window::toast("Edit text covers the old words and writes new ones over them. The original stays in the file underneath.");
            }
            _ => {}
        }
        if t != Tool::Select && doc::current().is_some_and(|d| !d.editable()) {
            window::toast("This file is read-only, so edits will be refused.");
        }
    }

    pub fn is_presenting(&self) -> bool {
        self.presenting.borrow().is_some()
    }

    /// Presentation mode: full screen, nothing but the page, one at a time.
    pub fn set_presenting(self: &Rc<Self>, on: bool) {
        if on == self.is_presenting() || (on && doc::current().is_none()) {
            return;
        }
        if on {
            *self.presenting.borrow_mut() = Some(Presenting { fit: self.fit.get(), zoom: self.zoom.get() });
            self.fit.set(Fit::Page);
            self.search.close();
            self.stack.add_css_class("presenting");
            self.chrome.set_visible(false);
            window::set_fullscreen(true);
        } else {
            let was = self.presenting.borrow_mut().take();
            if let Some(p) = was {
                self.fit.set(p.fit);
                self.zoom.set(p.zoom);
            }
            self.stack.remove_css_class("presenting");
            self.chrome.set_visible(true);
            window::set_fullscreen(false);
        }
        self.apply_panel();
        self.rebuild();
    }
}

/// The zoom menu: zoom in and out, fit width or page, and the page layout (one
/// page at a time or continuous, single or side by side).
fn zoom_menu(pop: &gtk::Popover) -> gtk::Box {
    let bx = widgets::vbox(8);
    bx.add_css_class("menu-box");
    let p = prefs::get();
    let zoom = widgets::hbox(4);
    let title = widgets::label("Zoom", "popover-title");
    title.set_hexpand(true);
    title.set_xalign(0.0);
    zoom.append(&title);
    let readout = widgets::label("", "zoom-readout");
    readout.set_width_chars(5);
    let show = {
        let r = readout.clone();
        move || r.set_text(&view().map(|v| format!("{:.0}%", v.zoom.get() * 100.0)).unwrap_or_default())
    };
    show();
    let zout = widgets::bar_button("zoom-out-symbolic", "Zoom out (Ctrl+−)");
    let s = show.clone();
    zout.connect_clicked(move |_| {
        zoom_by(1.0 / 1.2);
        s();
    });
    let zin = widgets::bar_button("zoom-in-symbolic", "Zoom in (Ctrl++)");
    let s = show.clone();
    zin.connect_clicked(move |_| {
        zoom_by(1.2);
        s();
    });
    zoom.append(&zout);
    zoom.append(&readout);
    zoom.append(&zin);
    bx.append(&zoom);
    let items = widgets::vbox(2);
    items.append(&menu_item(pop, "Fit width", "Ctrl+0", || zoom_fit("fit-width")));
    items.append(&menu_item(pop, "Fit page", "Ctrl+9", || zoom_fit("fit-page")));
    items.append(&menu_item(pop, "Actual size", "", || {
        if let Some(v) = view() {
            v.set_fit_zoom(1.0, Fit::None);
        }
    }));
    bx.append(&items);
    bx.append(&menu_sep());
    bx.append(&widgets::label("Scrolling", "popover-title"));
    let scroll = widgets::segmented(
        &widgets::opts(&[("continuous", "Continuous"), ("single", "One at a time")]),
        if p.continuous { "continuous" } else { "single" },
        |id| {
            prefs::update(|p| p.continuous = id == "continuous");
            relayout();
        },
    );
    bx.append(&scroll);
    bx.append(&widgets::label("Side by side", "popover-title"));
    let spread = widgets::segmented(&widgets::opts(&[("single", "Single"), ("pairs", "Two pages"), ("book", "Book")]), &p.spread, |id| {
        prefs::update(|p| p.spread = id);
        if let Some(v) = view() {
            v.rebuild();
        }
    });
    spread.set_tooltip_text(Some("Book keeps page one on its own, like a cover"));
    bx.append(&spread);
    bx.append(&menu_sep());
    bx.append(&menu_item(pop, "Present", "F5", toggle_presenting));
    bx
}

/// The "more" popover: the actions that don't need a button of their own.
fn more_menu(pop: &gtk::Popover) -> gtk::Box {
    let bx = widgets::vbox(2);
    bx.add_css_class("menu-box");
    let d = doc::current();
    bx.append(&menu_item(pop, "Open…", "Ctrl+O", doc::open_dialog));
    let save_as = menu_item(pop, "Save a copy as…", "Ctrl+Shift+S", doc::save_as_dialog);
    let print = menu_item(pop, "Print…", "Ctrl+P", doc::print);
    let share = menu_item(pop, "Share…", "", crate::share::document);
    let present = menu_item(pop, "Present", "F5", toggle_presenting);
    let close = menu_item(pop, "Close file", "Ctrl+W", close_file);
    for b in [&save_as, &print, &share, &present, &close] {
        b.set_sensitive(d.is_some());
    }
    bx.append(&save_as);
    bx.append(&print);
    bx.append(&share);
    bx.append(&menu_sep());
    bx.append(&present);
    if d.as_ref().is_some_and(|d| has_fields(d)) {
        let clear = menu_item(pop, "Clear the form", "", clear_form);
        clear.set_tooltip_text(Some("Empty every field (undo brings the answers back)"));
        bx.append(&clear);
    }
    if let Some(d) = &d {
        let folder = d.path().parent().map(|p| p.to_path_buf());
        let path = d.path();
        let show = menu_item(pop, "Show in folder", "", move || crate::cmd::show_in_folder(&path));
        show.set_sensitive(folder.is_some());
        bx.append(&show);
    }
    bx.append(&menu_sep());
    let colours = widgets::vbox(6);
    colours.add_css_class("menu-section");
    colours.append(&widgets::label("Page colours", "popover-title"));
    let seg = widgets::segmented(&widgets::opts(&[("off", "Off"), ("invert", "Invert"), ("tint", "Theme tint")]), &prefs::get().page_colors, |v| {
        prefs::update(|p| p.page_colors = v);
        theme::apply();
    });
    seg.set_tooltip_text(Some("Recolour pages to the theme for reading at night"));
    colours.append(&seg);
    bx.append(&colours);
    bx.append(&menu_sep());
    bx.append(&close);
    bx
}

/// True when any page of the document has form fields.
fn has_fields(d: &Doc) -> bool {
    let pdf = d.pdf();
    (0..pdf.n_pages()).any(|i| pdf.page(i).is_some_and(|p| !p.form_field_mapping().is_empty()))
}

/// Empty every form field.
pub(super) fn clear_form() {
    let Some(d) = doc::current() else { return };
    match d.edit_forms(|pdf| pdf.reset_form(&[], true)) {
        Ok(()) => {
            if let Some(v) = view() {
                v.rebuild();
            }
            window::toast("The form is empty again. Undo brings the answers back.");
        }
        Err(e) => window::toast(&format!("Couldn't clear the form: {e:#}")),
    }
}

// ----- The functions the window calls -----

/// Run the developer script from `NPDF_SCRIPT`, if there is one.
pub fn run_script_from_env() {
    if let Some(s) = std::env::var_os("NPDF_SCRIPT") {
        script::start(&s.to_string_lossy());
    }
}

/// Close the current file (asking about unsaved changes). With nothing left open,
/// go back to the library.
pub fn close_file() {
    doc::guard_unsaved(|| {
        doc::close();
        if doc::current().is_none() {
            window::navigate("library");
        }
    });
}

/// Close a particular file, from its row in the sidebar.
pub fn close_doc(d: &Rc<Doc>) {
    doc::switch_to(d);
    close_file();
}

pub fn focus_search() {
    if let Some(v) = view() {
        v.search.open();
    }
}

/// The page controls and the view controls, for the window header to place.
pub fn header_parts() -> Option<(gtk::Box, gtk::Box)> {
    view().map(|v| (v.head_center.clone(), v.head_end.clone()))
}

/// F10: the more menu in the header.
pub fn open_more_menu() {
    if let Some(v) = view() {
        v.more_btn.popup();
    }
}

/// Open one of the reader's menus: "zoom", "more", or a tool's kinds ("highlight", "text").
pub fn open_menu(name: &str) {
    let Some(v) = view() else { return };
    match name {
        "zoom" => v.fit_btn.popup(),
        "more" => v.more_btn.popup(),
        tool => {
            let kinds = v.tool_groups.iter().find(|g| g.tools[0].name().to_lowercase().starts_with(tool)).and_then(|g| g.kinds.clone());
            if let Some(k) = kinds {
                k.popup();
            }
        }
    }
}

pub fn zoom_by(f: f64) {
    if let Some(v) = view() {
        let z = v.zoom.get() * f;
        v.set_fit_zoom(z, Fit::None);
    }
}

pub fn zoom_fit(mode: &str) {
    if let Some(v) = view() {
        let fit = if mode == "fit-page" { Fit::Page } else { Fit::Width };
        v.set_fit_zoom(v.zoom.get(), fit);
    }
}

pub fn toggle_panel() {
    if let Some(v) = view() {
        v.panel_toggle.set_active(!v.panel_toggle.is_active());
    }
}

/// Esc: leave presenting, close the floating panel or the search, clear a
/// selection, or go back to the Select tool, whichever comes first.
pub fn escape() {
    let Some(v) = view() else { return };
    if v.is_presenting() {
        v.set_presenting(false);
    } else if v.narrow.get() && v.panel_floating_open.get() {
        v.panel_used();
    } else if v.search.is_open() {
        v.search.close();
    } else if v.selection.borrow().is_some() || v.annot_sel.borrow().is_some() {
        *v.selection.borrow_mut() = None;
        *v.annot_sel.borrow_mut() = None;
        for p in v.pages.borrow().iter() {
            p.marks.queue_draw();
        }
    } else if v.tool() != Tool::Select {
        v.set_tool(Tool::Select);
    }
}

pub fn step_page(d: i32) {
    if let Some(v) = view() {
        v.step(d);
    }
}

pub fn goto_page(i: usize) {
    if let Some(v) = view() {
        v.goto_page(i);
    }
}

/// Go to a page and remember where we were (Home, End, links, the outline).
pub fn jump(i: usize) {
    if let Some(v) = view() {
        let i = if i == usize::MAX { doc::current().map_or(0, |d| d.n_pages().saturating_sub(1)) } else { i };
        v.jump(i);
    }
}

pub fn go_back() {
    if let (Some(v), Some(d)) = (view(), doc::current())
        && let Some(p) = d.take_back()
    {
        v.goto_page(p);
        v.refresh_readout();
    }
}

pub fn go_forward() {
    if let (Some(v), Some(d)) = (view(), doc::current())
        && let Some(p) = d.take_forward()
    {
        v.goto_page(p);
        v.refresh_readout();
    }
}

pub fn toggle_presenting() {
    if let Some(v) = view() {
        window::navigate("document");
        let on = !v.is_presenting();
        v.set_presenting(on);
    }
}

pub fn is_presenting() -> bool {
    view().is_some_and(|v| v.is_presenting())
}

/// The window left full screen some other way (Esc, F11): stop presenting too.
pub fn on_unfullscreen() {
    if let Some(v) = view()
        && v.is_presenting()
    {
        v.set_presenting(false);
    }
}

pub fn set_tool(t: Tool) {
    if let Some(v) = view() {
        v.set_tool(t);
    }
}

/// A narrow window: the panel floats over the pages and the tools keep only their icons.
pub fn set_narrow(narrow: bool) {
    if let Some(v) = view() {
        v.narrow.set(narrow);
        for g in &v.tool_groups {
            g.label.set_visible(!narrow);
        }
        v.apply_panel();
    }
}

pub fn relayout() {
    if let Some(v) = view() {
        v.relayout();
    }
}

/// Lay the pages out again from scratch (the side-by-side setting changed).
pub fn rebuild() {
    if let Some(v) = view() {
        v.rebuild();
    }
}

/// Copy the selected text, if there is any. True when something was copied.
pub fn copy_selection() -> bool {
    let Some(v) = view() else { return false };
    let text = v.selection.borrow().as_ref().map(|s| s.text.clone()).filter(|t| !t.is_empty());
    match text {
        Some(t) => {
            if let Some(display) = gdk::Display::default() {
                display.clipboard().set_text(&t);
            }
            window::toast("Copied.");
            true
        }
        None => false,
    }
}

pub fn delete_selected() -> bool {
    tools::delete_selected()
}

/// Open the document at a page from elsewhere in the app (library, page organiser).
pub fn show_page(d: &Doc, page: usize) {
    d.set_page(page);
    window::navigate("document");
    if let Some(v) = view() {
        v.goto_page(page);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_pages_into_rows() {
        assert_eq!(groups(3, Spread::Single), vec![vec![0], vec![1], vec![2]]);
        assert_eq!(groups(5, Spread::Pairs), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(groups(4, Spread::Book), vec![vec![0], vec![1, 2], vec![3]]);
        assert_eq!(groups(1, Spread::Book), vec![vec![0]]);
        assert!(groups(0, Spread::Pairs).is_empty());
    }
}
