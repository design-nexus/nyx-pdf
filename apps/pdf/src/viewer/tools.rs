//! What each tool does with a gesture on a page.

use super::pageview::{self, PageView, px_rect};
use super::{AnnotSel, Sel, View, colour, view};
use crate::doc::annots::{self, AnnotInfo, Kind};
use crate::doc::geom::Rect;
use crate::doc::links::Target;
use crate::doc::{self, text};
use crate::{prefs, widgets, window};
use anyhow::Context;
use gtk::prelude::*;
use lopdf::ObjectId;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tool {
    Select,
    Highlight,
    Underline,
    Strike,
    Ink,
    Note,
    TextBox,
    EditText,
    Sign,
}

impl Tool {
    /// The toolbar's buttons, in order: a tool, or a few kinds of one under a menu.
    pub const GROUPS: [&'static [Tool]; 6] = [
        &[Tool::Select],
        &[Tool::Highlight, Tool::Underline, Tool::Strike],
        &[Tool::Ink],
        &[Tool::Note],
        &[Tool::TextBox, Tool::EditText],
        &[Tool::Sign],
    ];

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Select => "npdf-select-symbolic",
            Tool::Highlight => "npdf-highlight-symbolic",
            Tool::Underline => "npdf-underline-symbolic",
            Tool::Strike => "npdf-strike-symbolic",
            Tool::Ink => "npdf-ink-symbolic",
            Tool::Note => "npdf-note-symbolic",
            Tool::TextBox => "npdf-textbox-symbolic",
            Tool::EditText => "npdf-edit-text-symbolic",
            Tool::Sign => "npdf-sign-symbolic",
        }
    }

    /// The tool's name in its menu.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Highlight => "Highlight",
            Tool::Underline => "Underline",
            Tool::Strike => "Strike out",
            Tool::Ink => "Draw",
            Tool::Note => "Note",
            Tool::TextBox => "Text box",
            Tool::EditText => "Edit text",
            Tool::Sign => "Sign",
        }
    }

    /// The name on its button in the toolbar.
    pub fn label(self) -> &'static str {
        match self {
            Tool::TextBox => "Text",
            t => t.name(),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Tool::Select => "V",
            Tool::Highlight => "H",
            Tool::Underline => "U",
            Tool::Strike => "X",
            Tool::Ink => "D",
            Tool::Note => "N",
            Tool::TextBox => "T",
            Tool::EditText => "E",
            Tool::Sign => "S",
        }
    }

    pub fn tooltip(self) -> String {
        match self {
            Tool::Select => "Select text and annotations (V)",
            Tool::Highlight => "Highlight text (H)",
            Tool::Underline => "Underline text (U)",
            Tool::Strike => "Strike out text (X)",
            Tool::Ink => "Draw (D)",
            Tool::Note => "Add a note (N)",
            Tool::TextBox => "Add a text box (T)",
            Tool::EditText => "Edit text: drag over words to replace them (E)",
            Tool::Sign => "Place a signature (S)",
        }
        .to_string()
    }

    /// The colour a tool starts with, when the user hasn't picked one.
    pub fn default_colour(self) -> String {
        match self {
            Tool::Highlight | Tool::Note => prefs::get().highlight_color,
            Tool::TextBox | Tool::EditText => "#000000".into(),
            _ => "#ff3b30".into(),
        }
    }
}

pub fn cursor(t: Tool) -> &'static str {
    match t {
        Tool::Select => "default",
        Tool::Highlight | Tool::Underline | Tool::Strike | Tool::EditText => "text",
        Tool::Ink | Tool::TextBox | Tool::Sign => "crosshair",
        Tool::Note => "cell",
    }
}

#[derive(Clone, Copy)]
enum Drag {
    /// Selecting text from this point.
    Text((f64, f64)),
    Ink,
    Band((f64, f64)),
    Move(AnnotSel, (f64, f64)),
    /// Dragging the corner handle of an annotation whose box started as this.
    Resize(AnnotSel, Rect),
    Nothing,
}

thread_local! {
    static DRAG: RefCell<Drag> = const { RefCell::new(Drag::Nothing) };
}

fn author() -> String {
    prefs::get().author
}

fn rgb(v: &View) -> [f32; 3] {
    colour::parse(&v.colour()).unwrap_or([1.0, 0.84, 0.04])
}

/// The rectangle of an annotation on the displayed page.
pub fn annot_rect(v: &View, sel: AnnotSel) -> Option<Rect> {
    let lo = v.lo()?;
    let page = *annots::page_ids(&lo).get(sel.page)?;
    annots::read_page(&lo, page).into_iter().find(|a| a.id == sel.id).map(|a| a.rect)
}

/// True when the selected annotation has a resize handle.
pub fn resizable(v: &View, sel: AnnotSel) -> bool {
    info(v, sel).is_some_and(|a| a.kind.resizable())
}

/// True when (`x`, `y`) is on the resize handle of the selected annotation on this page.
fn on_handle(pv: &PageView, v: &View, x: f64, y: f64) -> Option<(AnnotSel, Rect)> {
    let sel = (*v.annot_sel.borrow()).filter(|s| s.page == pv.index)?;
    let a = info(v, sel).filter(|a| a.kind.resizable())?;
    let r = a.rect.inflate(2.0);
    let reach = (pageview::HANDLE + 3.0) / v.zoom.get();
    ((x - r.x1).abs() <= reach && (y - r.y1).abs() <= reach).then_some((sel, a.rect))
}

fn info(v: &View, sel: AnnotSel) -> Option<AnnotInfo> {
    let lo = v.lo()?;
    let page = *annots::page_ids(&lo).get(sel.page)?;
    annots::read_page(&lo, page).into_iter().find(|a| a.id == sel.id)
}

/// The text between two points, as one rectangle per line, plus the text itself.
fn text_region(pv: &PageView, a: (f64, f64), b: (f64, f64), style: poppler::SelectionStyle) -> (Vec<Rect>, String) {
    let Some(d) = doc::current() else { return (Vec::new(), String::new()) };
    let Some(page) = d.pdf().page(pv.index as i32) else { return (Vec::new(), String::new()) };
    let mut sel = poppler::Rectangle::new();
    sel.set_x1(a.0);
    sel.set_y1(a.1);
    sel.set_x2(b.0);
    sel.set_y2(b.1);
    let rects = page
        .selected_region(1.0, style, &mut sel)
        .map(|r| {
            (0..r.num_rectangles())
                .map(|i| {
                    let q = r.rectangle(i);
                    Rect::new(f64::from(q.x()), f64::from(q.y()), f64::from(q.x() + q.width()), f64::from(q.y() + q.height()))
                })
                .collect()
        })
        .unwrap_or_default();
    let text = page.selected_text(style, &mut sel).map(|t| t.to_string()).unwrap_or_default();
    (rects, text)
}

fn style_for(t: Tool) -> poppler::SelectionStyle {
    if t == Tool::EditText { poppler::SelectionStyle::Word } else { poppler::SelectionStyle::Glyph }
}

pub fn drag_begin(pv: &Rc<PageView>, x: f64, y: f64) {
    let Some(v) = view() else { return };
    // Working on the page puts the floating panel (narrow windows) away.
    v.panel_used();
    if doc::current().is_none() {
        return;
    }
    if v.is_presenting() {
        DRAG.with(|d| *d.borrow_mut() = Drag::Nothing);
        return;
    }
    let handle = if v.tool() == Tool::Select { on_handle(pv, &v, x, y) } else { None };
    let drag = match v.tool() {
        Tool::Select if handle.is_some() => handle.map_or(Drag::Nothing, |(sel, r)| Drag::Resize(sel, r)),
        Tool::Select => {
            let sel = *v.annot_sel.borrow();
            let hit = sel.filter(|s| s.page == pv.index).and_then(|s| info(&v, s)).filter(|a| a.kind.movable() && a.rect.inflate(3.0).contains(x, y));
            match (hit, sel) {
                (Some(_), Some(s)) => Drag::Move(s, (x, y)),
                _ => {
                    *v.selection.borrow_mut() = None;
                    pageview::queue_all(&v);
                    Drag::Text((x, y))
                }
            }
        }
        Tool::Highlight | Tool::Underline | Tool::Strike | Tool::EditText => Drag::Text((x, y)),
        Tool::Ink => {
            pv.st.borrow_mut().ink = vec![(x, y)];
            Drag::Ink
        }
        Tool::TextBox => Drag::Band((x, y)),
        Tool::Note | Tool::Sign => Drag::Nothing,
    };
    DRAG.with(|d| *d.borrow_mut() = drag);
}

pub fn drag_update(pv: &Rc<PageView>, x: f64, y: f64) {
    let Some(v) = view() else { return };
    let drag = DRAG.with(|d| *d.borrow());
    match drag {
        Drag::Text(start) => {
            let (rects, text) = text_region(pv, start, (x, y), style_for(v.tool()));
            if v.tool() == Tool::Select {
                *v.selection.borrow_mut() = Some(Sel { page: pv.index, rects, text });
            } else {
                pv.st.borrow_mut().preview = rects;
            }
            pv.marks.queue_draw();
        }
        Drag::Ink => {
            let mut st = pv.st.borrow_mut();
            if st.ink.last().is_none_or(|l| (l.0 - x).hypot(l.1 - y) > 0.6) {
                st.ink.push((x, y));
            }
            drop(st);
            pv.marks.queue_draw();
        }
        Drag::Band(start) => {
            pv.st.borrow_mut().band = Some(Rect::new(start.0, start.1, x, y));
            pv.marks.queue_draw();
        }
        Drag::Move(sel, from) => {
            if let Some(r) = annot_rect(&v, sel) {
                let (dx, dy) = (x - from.0, y - from.1);
                pv.st.borrow_mut().band = Some(Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy));
                pv.marks.queue_draw();
            }
        }
        Drag::Resize(sel, r) => {
            pv.st.borrow_mut().band = Some(resized(&v, sel, r, x, y));
            pv.marks.queue_draw();
        }
        Drag::Nothing => {}
    }
}

/// The box an annotation would have with its corner dragged to (`x`, `y`). Pictures
/// keep their shape; everything has a sensible smallest size.
fn resized(v: &View, sel: AnnotSel, r: Rect, x: f64, y: f64) -> Rect {
    let picture = info(v, sel).is_some_and(|a| a.kind == Kind::Stamp);
    let w = (x - r.x0).max(12.0);
    let h = if picture { w * r.height() / r.width().max(1.0) } else { (y - r.y0).max(8.0) };
    Rect::new(r.x0, r.y0, r.x0 + w, r.y0 + h)
}

pub fn drag_end(pv: &Rc<PageView>, x: f64, y: f64, click: bool) {
    let Some(v) = view() else { return };
    let drag = DRAG.with(|d| std::mem::replace(&mut *d.borrow_mut(), Drag::Nothing));
    let tool = v.tool();
    // Presenting: a click moves on to the next page (links still work).
    if v.is_presenting() {
        if click {
            let link = doc::current().and_then(|d| pv.links(&d).iter().find(|l| l.area.contains(x, y)).map(|l| l.target.clone()));
            match link {
                Some(Target::Page(p)) => v.jump(p),
                Some(Target::Uri(u)) => open_uri(&u),
                None => v.step(1),
            }
        }
        return;
    }
    match drag {
        Drag::Text(start) => {
            if click {
                pageview::clear_preview(pv);
                if tool == Tool::Select {
                    click_select(pv, &v, x, y);
                }
                return;
            }
            let (rects, text) = text_region(pv, start, (x, y), style_for(tool));
            pageview::clear_preview(pv);
            match tool {
                Tool::Select => {
                    *v.selection.borrow_mut() = Some(Sel { page: pv.index, rects, text });
                    *v.annot_sel.borrow_mut() = None;
                    pageview::queue_all(&v);
                }
                Tool::Highlight | Tool::Underline | Tool::Strike => {
                    let kind = match tool {
                        Tool::Highlight => Kind::Highlight,
                        Tool::Underline => Kind::Underline,
                        _ => Kind::StrikeOut,
                    };
                    if rects.is_empty() {
                        return;
                    }
                    let colour = rgb(&v);
                    let opacity = if kind == Kind::Highlight { prefs::get().highlight_opacity } else { 1.0 };
                    commit(pv, move |lo, page| annots::add_markup(lo, page, kind, &rects, colour, opacity, &author()));
                }
                Tool::EditText => edit_text(pv, &v, &rects, &text),
                _ => {}
            }
        }
        Drag::Ink => {
            let stroke = std::mem::take(&mut pv.st.borrow_mut().ink);
            pageview::clear_preview(pv);
            if stroke.len() > 1 {
                let colour = rgb(&v);
                let width = prefs::get().ink_width;
                commit(pv, move |lo, page| annots::add_ink(lo, page, &[stroke], colour, width, &author()));
            }
        }
        Drag::Band(start) => {
            pageview::clear_preview(pv);
            let (w, h) = pv.size.get();
            let size = prefs::get().text_size;
            let r = Rect::new(start.0, start.1, x, y);
            let drawn = !(click || r.width() < 24.0 || r.height() < 14.0);
            let colour = rgb(&v);
            let pv2 = pv.clone();
            let anchor = if drawn { r } else { Rect::new(start.0, start.1, start.0 + 1.0, start.1 + 1.0) };
            prompt_text(pv, &anchor, "Text box", "", "Add", move |text| {
                if text.trim().is_empty() {
                    return;
                }
                // A click (rather than a drawn box) gets a box that fits the text.
                let mut r = r;
                if !drawn {
                    let face = annots::Face::default();
                    let longest = text.lines().map(|l| annots::text_width(face, size, l)).fold(0.0, f64::max);
                    let bw = (longest + 8.0).clamp(40.0, (w - start.0).max(40.0)).min(320.0);
                    let lines = annots::wrap(face, size, bw - 6.0, &text).len().max(1) as f64;
                    r = Rect::new(start.0, start.1, start.0 + bw, start.1 + lines * size * 1.2 + 6.0);
                }
                let r = Rect::new(r.x0.max(0.0), r.y0.max(0.0), r.x1.min(w), r.y1.min(h));
                let id = commit(&pv2, |lo, page| annots::add_text_box(lo, page, r, &text, colour, size, &author()));
                select_new(&pv2, id);
            });
        }
        Drag::Move(sel, from) => {
            pageview::clear_preview(pv);
            let (dx, dy) = (x - from.0, y - from.1);
            if click || dx.hypot(dy) < 1.0 {
                click_select(pv, &v, x, y);
                return;
            }
            let idx = sel.page;
            let moved = doc::try_edit(false, move |lo| {
                let page = *annots::page_ids(lo).get(idx).context("no such page")?;
                annots::move_by(lo, page, sel.id, dx, dy)
            });
            if moved {
                *v.annot_sel.borrow_mut() = Some(sel);
                pageview::queue_all(&v);
            }
        }
        Drag::Resize(sel, r) => {
            pageview::clear_preview(pv);
            let new = resized(&v, sel, r, x, y);
            if click || (new.width() - r.width()).abs() + (new.height() - r.height()).abs() < 1.0 {
                return;
            }
            let idx = sel.page;
            let done = doc::try_edit(false, move |lo| {
                let page = *annots::page_ids(lo).get(idx).context("no such page")?;
                annots::set_rect(lo, page, sel.id, new)
            });
            if done {
                *v.annot_sel.borrow_mut() = Some(sel);
                pageview::queue_all(&v);
            }
        }
        Drag::Nothing => {
            if !click {
                return;
            }
            match tool {
                Tool::Note => {
                    let at = Rect::new(x, y, x + annots::NOTE_SIZE, y + annots::NOTE_SIZE);
                    let colour = rgb(&v);
                    let pv2 = pv.clone();
                    prompt_text(pv, &at, "Note", "", "Add", move |text| {
                        let id = commit(&pv2, |lo, page| annots::add_note(lo, page, x, y, &text, colour, &author()));
                        select_new(&pv2, id);
                    });
                }
                Tool::Sign => place_signature(pv, &v, x, y),
                _ => {}
            }
        }
    }
}

pub fn hover(pv: &Rc<PageView>, x: f64, y: f64) {
    let Some(v) = view() else { return };
    if v.tool() != super::Tool::Select {
        return;
    }
    let Some(d) = doc::current() else { return };
    if on_handle(pv, &v, x, y).is_some() {
        pv.set_hover_cursor("se-resize");
        return;
    }
    let over_link = pv.links(&d).iter().any(|l| l.area.contains(x, y));
    pv.set_hover_cursor(if over_link { "pointer" } else { "default" });
}

fn open_uri(u: &str) {
    if let Err(e) = gtk::gio::AppInfo::launch_default_for_uri(u, gtk::gio::AppLaunchContext::NONE) {
        window::toast(&format!("Couldn't open that link: {e}"));
    }
}

/// Run an edit that adds one annotation to this page; returns its id.
fn commit(pv: &PageView, f: impl FnOnce(&mut lopdf::Document, ObjectId) -> anyhow::Result<ObjectId>) -> Option<ObjectId> {
    let idx = pv.index;
    let mut created = None;
    doc::try_edit(false, |lo| {
        let page = *annots::page_ids(lo).get(idx).context("that page is missing")?;
        created = Some(f(lo, page)?);
        Ok(())
    });
    created
}

fn select_new(pv: &PageView, id: Option<ObjectId>) {
    if let (Some(v), Some(id)) = (view(), id) {
        *v.annot_sel.borrow_mut() = Some(AnnotSel { page: pv.index, id });
        pageview::queue_all(&v);
    }
}

/// A click with the Select tool: an annotation, a link, or nothing.
fn click_select(pv: &Rc<PageView>, v: &Rc<View>, x: f64, y: f64) {
    let Some(d) = doc::current() else { return };
    *v.selection.borrow_mut() = None;
    if let Some(lo) = v.lo()
        && let Some(page) = annots::page_ids(&lo).get(pv.index).copied()
    {
        let hit = annots::read_page(&lo, page).into_iter().rev().find(|a| a.rect.inflate(2.0).contains(x, y));
        if let Some(a) = hit {
            *v.annot_sel.borrow_mut() = Some(AnnotSel { page: pv.index, id: a.id });
            pageview::queue_all(v);
            annot_popover(pv, v, &a);
            return;
        }
    }
    *v.annot_sel.borrow_mut() = None;
    pageview::queue_all(v);
    let links = pv.links(&d);
    if let Some(l) = links.iter().find(|l| l.area.contains(x, y)) {
        match &l.target {
            Target::Page(p) => v.jump(*p),
            Target::Uri(u) => open_uri(u),
        }
    }
}

/// A small popover with a text area, anchored at `at` (points on the page).
pub fn prompt_text(pv: &PageView, at: &Rect, title: &str, initial: &str, confirm: &str, on_ok: impl Fn(String) + 'static) {
    let pop = gtk::Popover::new();
    pop.set_parent(&pv.frame);
    pop.set_pointing_to(Some(&px_rect(pv, at)));
    pop.add_css_class("prompt-popover");
    let card = widgets::vbox(8);
    card.append(&widgets::label(title, "popover-title"));
    let tv = gtk::TextView::new();
    tv.set_wrap_mode(gtk::WrapMode::WordChar);
    tv.add_css_class("prompt-text");
    tv.buffer().set_text(initial);
    let scroll = gtk::ScrolledWindow::builder().min_content_width(280).min_content_height(84).max_content_height(200).child(&tv).build();
    scroll.add_css_class("prompt-scroll");
    card.append(&scroll);
    let buttons = widgets::hbox(8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let ok = gtk::Button::with_label(confirm);
    ok.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&ok);
    card.append(&buttons);
    pop.set_child(Some(&card));
    let done = Rc::new(std::cell::Cell::new(false));
    let on_ok = Rc::new(on_ok);
    let submit = {
        let (pop, tv, on_ok, done) = (pop.clone(), tv.clone(), on_ok.clone(), done.clone());
        move || {
            if done.replace(true) {
                return;
            }
            let b = tv.buffer();
            let text = b.text(&b.start_iter(), &b.end_iter(), false).to_string();
            pop.popdown();
            on_ok(text);
        }
    };
    let s = submit.clone();
    ok.connect_clicked(move |_| s());
    let p = pop.clone();
    cancel.connect_clicked(move |_| p.popdown());
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, mods| {
        if key == gtk::gdk::Key::Return && mods.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
            submit();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    tv.add_controller(keys);
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
    tv.grab_focus();
}

/// The popover for a selected annotation: edit its text or colour, or delete it.
fn annot_popover(pv: &Rc<PageView>, v: &Rc<View>, a: &AnnotInfo) {
    let pop = gtk::Popover::new();
    pop.set_parent(&pv.frame);
    pop.set_pointing_to(Some(&px_rect(pv, &a.rect)));
    pop.add_css_class("prompt-popover");
    let card = widgets::vbox(8);
    card.append(&widgets::label(a.kind.label(), "popover-title"));
    let (page_idx, id) = (pv.index, a.id);

    if a.kind.has_text() {
        let tv = gtk::TextView::new();
        tv.set_wrap_mode(gtk::WrapMode::WordChar);
        tv.add_css_class("prompt-text");
        tv.buffer().set_text(&a.contents);
        let scroll = gtk::ScrolledWindow::builder().min_content_width(260).min_content_height(70).max_content_height(180).child(&tv).build();
        scroll.add_css_class("prompt-scroll");
        card.append(&scroll);
        let apply = gtk::Button::with_label("Apply text");
        apply.set_halign(gtk::Align::End);
        let (pop2, tv2) = (pop.clone(), tv.clone());
        apply.connect_clicked(move |_| {
            let b = tv2.buffer();
            let text = b.text(&b.start_iter(), &b.end_iter(), false).to_string();
            pop2.popdown();
            doc::try_edit(false, move |lo| {
                let page = *annots::page_ids(lo).get(page_idx).context("no such page")?;
                annots::set_contents(lo, page, id, &text)
            });
        });
        card.append(&apply);
    }
    if a.kind.recolourable() {
        let current = a.colour.map(|c| format!("#{:02x}{:02x}{:02x}", (c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8)).unwrap_or_default();
        let last = Rc::new(RefCell::new(current.clone()));
        let row = colour::row(&current, move |hex| {
            if *last.borrow() == hex {
                return;
            }
            *last.borrow_mut() = hex.clone();
            let Some(c) = colour::parse(&hex) else { return };
            doc::try_edit(false, move |lo| {
                let page = *annots::page_ids(lo).get(page_idx).context("no such page")?;
                annots::set_colour(lo, page, id, c)
            });
        });
        card.append(&row);
    }
    let del = gtk::Button::with_label("Delete");
    del.add_css_class("destructive-action");
    del.set_halign(gtk::Align::End);
    let pop2 = pop.clone();
    del.connect_clicked(move |_| {
        pop2.popdown();
        delete_annot(AnnotSel { page: page_idx, id });
    });
    card.append(&del);
    pop.set_child(Some(&card));
    pop.connect_closed(|p| {
        let p = p.clone();
        gtk::glib::idle_add_local_once(move || p.unparent());
    });
    let _ = v;
    pop.popup();
}

pub fn delete_annot(sel: AnnotSel) {
    doc::try_edit(false, move |lo| {
        let page = *annots::page_ids(lo).get(sel.page).context("no such page")?;
        annots::delete(lo, page, sel.id)
    });
}

pub fn delete_selected() -> bool {
    let Some(v) = view() else { return false };
    let sel = v.annot_sel.borrow_mut().take();
    match sel {
        Some(s) => {
            delete_annot(s);
            true
        }
        None => false,
    }
}

/// Replace the words under the drag with new text, written over them.
fn edit_text(pv: &Rc<PageView>, v: &Rc<View>, rects: &[Rect], selected: &str) {
    if rects.is_empty() {
        return;
    }
    let Some(d) = doc::current() else { return };
    let region = rects.iter().skip(1).fold(rects[0], |a, b| a.union(b));
    let look = text::analyse(&d.pdf(), pv.index, &region);
    let at = look.rect;
    let pv2 = pv.clone();
    let initial = selected.trim().replace('\n', " ");
    prompt_text(pv, &at, "Replace text", &initial, "Replace", move |new| {
        let new = new.replace('\n', " ");
        let id = commit(&pv2, |lo, page| {
            annots::add_edit(lo, page, look.rect, &new, look.face, look.size, look.fg, look.bg, &author())
        });
        select_new(&pv2, id);
    });
    let _ = v;
}

// ---------- Signatures ----------

pub fn choose_signature() {
    super::sign::choose(|path| {
        if let Some(v) = view() {
            match path {
                Some(p) => {
                    *v.signature.borrow_mut() = Some(p);
                    window::toast("Click on the page where the signature should go.");
                }
                None => v.set_tool(Tool::Select),
            }
        }
    });
}

fn place_signature(pv: &Rc<PageView>, v: &Rc<View>, x: f64, y: f64) {
    let Some(path) = v.signature.borrow().clone() else {
        choose_signature();
        return;
    };
    let pic = match super::sign::load_picture(&path) {
        Ok(p) => p,
        Err(e) => {
            window::toast(&format!("Couldn't read that signature: {e}"));
            return;
        }
    };
    let w = 150.0;
    let h = w * pic.height as f64 / pic.width.max(1) as f64;
    let (pw, ph) = pv.size.get();
    let x0 = (x - w / 2.0).clamp(0.0, (pw - w).max(0.0));
    let y0 = (y - h / 2.0).clamp(0.0, (ph - h).max(0.0));
    let r = Rect::new(x0, y0, x0 + w, y0 + h);
    let id = commit(pv, |lo, page| annots::add_stamp(lo, page, r, &pic, &author()));
    select_new(pv, id);
    v.set_tool(Tool::Select);
}
