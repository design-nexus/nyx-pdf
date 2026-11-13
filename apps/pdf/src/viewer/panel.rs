//! The side panel: page thumbnails, the outline, and a list of markup.

use super::{AnnotSel, view};
use crate::doc::annots;
use crate::doc::links::{self, Outline};
use crate::doc::{self, geom::Rect};
use crate::widgets;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The panel's width less its padding: what a thumbnail gets.
fn thumb_w(panel_w: i32) -> i32 {
    (panel_w - 60).max(80)
}

pub struct Panel {
    pub root: gtk::Box,
    thumbs: gtk::Box,
    thumbs_scroll: gtk::ScrolledWindow,
    items: RefCell<Vec<Thumb>>,
    outline: gtk::Box,
    markup: gtk::Box,
    active: Cell<usize>,
    tabs: Vec<(String, gtk::ToggleButton)>,
    width: Cell<i32>,
}

struct Thumb {
    button: gtk::Button,
    picture: gtk::Picture,
    loaded: Rc<Cell<bool>>,
}

fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::External)
        .vexpand(true)
        .child(child)
        .build()
}

impl Panel {
    pub fn new() -> Panel {
        let root = widgets::vbox(0);
        root.add_css_class("side-panel");
        let width = crate::prefs::get().panel_width;
        root.set_size_request(width, -1);
        root.set_hexpand(false);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        stack.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 160 });

        let names = ["thumbs", "outline", "markup"];
        let tab = crate::prefs::get().panel_tab;
        let tab = if names.contains(&tab.as_str()) { tab } else { "thumbs".to_string() };
        let seg = widgets::segmented(&widgets::opts(&[("thumbs", "Pages"), ("outline", "Outline"), ("markup", "Markup")]), &tab, {
            let stack = stack.clone();
            move |id| {
                stack.set_visible_child_name(&id);
                crate::prefs::update(|p| p.panel_tab = id);
            }
        });
        seg.set_halign(gtk::Align::Fill);
        seg.add_css_class("panel-tabs");
        let mut tabs = Vec::new();
        for (b, name) in std::iter::successors(seg.first_child(), |w| w.next_sibling()).zip(names) {
            b.set_hexpand(true);
            if let Ok(t) = b.downcast::<gtk::ToggleButton>() {
                tabs.push((name.to_string(), t));
            }
        }
        root.append(&seg);

        let thumbs = widgets::vbox(10);
        thumbs.add_css_class("thumb-list");
        let thumbs_scroll = scroller(&thumbs);
        stack.add_named(&thumbs_scroll, Some("thumbs"));
        let outline = widgets::vbox(2);
        outline.add_css_class("outline-list");
        stack.add_named(&scroller(&outline), Some("outline"));
        let markup = widgets::vbox(6);
        markup.add_css_class("markup-list");
        stack.add_named(&scroller(&markup), Some("markup"));
        root.append(&stack);
        stack.set_visible_child_name(&tab);

        let p = Panel {
            root,
            thumbs,
            thumbs_scroll,
            items: RefCell::new(Vec::new()),
            outline,
            markup,
            active: Cell::new(usize::MAX),
            tabs,
            width: Cell::new(width),
        };
        p.thumbs_scroll.vadjustment().connect_value_changed(|_| {
            if let Some(v) = view() {
                v.panel.load_visible_thumbs();
            }
        });
        p.thumbs_scroll.vadjustment().connect_page_size_notify(|_| {
            if let Some(v) = view() {
                v.panel.load_visible_thumbs();
            }
        });
        p
    }

    /// Show one of "thumbs", "outline" or "markup".
    pub fn show(&self, name: &str) {
        if let Some((_, b)) = self.tabs.iter().find(|(n, _)| n == name) {
            b.set_active(true);
        }
    }

    /// The panel was resized: thumbnails follow.
    pub fn set_width(&self, w: i32) {
        if self.width.replace(w) != w {
            self.build_thumbs();
        }
    }

    /// Rebuild all three lists for the document that's open.
    pub fn rebuild(&self) {
        self.build_thumbs();
        self.build_outline();
        self.refresh_annots();
    }

    fn build_thumbs(&self) {
        while let Some(c) = self.thumbs.first_child() {
            self.thumbs.remove(&c);
        }
        self.items.borrow_mut().clear();
        let Some(d) = doc::current() else { return };
        let tw = thumb_w(self.width.get());
        for i in 0..d.n_pages() {
            let (w, h) = d.page_size(i);
            let button = gtk::Button::new();
            button.add_css_class("thumb");
            button.set_halign(gtk::Align::Center);
            let card = widgets::vbox(4);
            let picture = gtk::Picture::new();
            picture.add_css_class("thumb-picture");
            picture.add_css_class("loading");
            picture.set_can_shrink(true);
            picture.set_content_fit(gtk::ContentFit::Fill);
            picture.set_size_request(tw, (f64::from(tw) * h / w.max(1.0)).round() as i32);
            let label = widgets::label(&(i + 1).to_string(), "tnum");
            label.add_css_class("thumb-number");
            label.set_halign(gtk::Align::Center);
            card.append(&picture);
            card.append(&label);
            button.set_child(Some(&card));
            button.connect_clicked(move |_| {
                if let Some(v) = view() {
                    v.jump(i);
                    v.panel_used();
                }
            });
            self.thumbs.append(&button);
            self.items.borrow_mut().push(Thumb { button, picture, loaded: Rc::new(Cell::new(false)) });
        }
        self.active.set(usize::MAX);
        self.highlight_page(d.page());
        let this = view();
        super::after_layout(&self.thumbs, move || {
            if let Some(v) = this {
                v.panel.load_visible_thumbs();
            }
        });
    }

    /// Draw the thumbnails that are on screen.
    fn load_visible_thumbs(&self) {
        let adj = self.thumbs_scroll.vadjustment();
        let (top, h) = (adj.value(), adj.page_size().max(1.0));
        let scale = self.root.scale_factor().max(1);
        let tw = thumb_w(self.width.get());
        for (i, t) in self.items.borrow().iter().enumerate() {
            if t.loaded.get() {
                continue;
            }
            let Some(b) = t.button.compute_bounds(&self.thumbs) else { continue };
            let (y, bh) = (f64::from(b.y()), f64::from(b.height()));
            if y + bh >= top - h && y <= top + 2.0 * h {
                t.loaded.set(true);
                let pic = t.picture.clone();
                let loaded = t.loaded.clone();
                // Keep the ticket alive by leaking it into the callback's lifetime: the cache owns the result.
                let ticket = super::thumbs::get(i, (tw * scale) as u32, move |tex| {
                    pic.set_paintable(Some(&tex));
                    pic.remove_css_class("loading");
                });
                if ticket.is_none() {
                    loaded.set(true);
                }
                std::mem::forget(ticket);
            }
        }
    }

    pub fn refresh_thumbs(&self) {
        for t in self.items.borrow().iter() {
            t.loaded.set(false);
        }
        self.load_visible_thumbs();
    }

    pub fn highlight_page(&self, page: usize) {
        let old = self.active.replace(page);
        let items = self.items.borrow();
        if let Some(t) = items.get(old) {
            t.button.remove_css_class("active");
        }
        if let Some(t) = items.get(page) {
            t.button.add_css_class("active");
            // Keep it in view.
            let adj = self.thumbs_scroll.vadjustment();
            if let Some(b) = t.button.compute_bounds(&self.thumbs) {
                let (y, h) = (f64::from(b.y()), f64::from(b.height()));
                if y < adj.value() || y + h > adj.value() + adj.page_size() {
                    adj.set_value((y - 20.0).max(0.0));
                }
            }
        }
    }

    fn build_outline(&self) {
        while let Some(c) = self.outline.first_child() {
            self.outline.remove(&c);
        }
        let Some(d) = doc::current() else { return };
        let items = links::outline(&d.pdf());
        if items.is_empty() {
            let l = widgets::label("This file has no outline.", "dim");
            l.set_margin_top(16);
            l.set_margin_start(14);
            self.outline.append(&l);
            return;
        }
        self.outline.append(&outline_list(&items, 0));
    }

    pub fn refresh_annots(&self) {
        while let Some(c) = self.markup.first_child() {
            self.markup.remove(&c);
        }
        let Some(v) = view() else { return };
        let all = v.lo().map(|lo| annots::read_all(&lo)).unwrap_or_default();
        let pdf = doc::current().map(|d| d.pdf());
        if all.is_empty() {
            let l = widgets::label("Nothing marked up yet. Use the tools above the page.", "dim");
            l.set_wrap(true);
            l.set_margin_top(16);
            l.set_margin_start(14);
            l.set_margin_end(14);
            self.markup.append(&l);
            return;
        }
        for (page, a) in all {
            let button = gtk::Button::new();
            button.add_css_class("markup-item");
            let card = widgets::vbox(2);
            let head = widgets::label(&format!("{} · page {}", a.kind.label(), page + 1), "markup-title");
            card.append(&head);
            let snippet = markup_text(pdf.as_ref(), page, &a);
            if !snippet.is_empty() {
                let s = widgets::label(&snippet, "dim");
                s.set_ellipsize(gtk::pango::EllipsizeMode::End);
                s.set_lines(2);
                s.set_wrap(true);
                s.set_max_width_chars(28);
                card.append(&s);
            }
            button.set_child(Some(&card));
            let id = a.id;
            button.connect_clicked(move |_| {
                if let Some(v) = view() {
                    *v.annot_sel.borrow_mut() = Some(AnnotSel { page, id });
                    v.jump(page);
                    super::pageview::queue_all(&v);
                    v.panel_used();
                }
            });
            self.markup.append(&button);
        }
    }
}

/// What to show for an annotation in the list: its own text, or for text markup,
/// the words it covers.
fn markup_text(pdf: Option<&poppler::Document>, page: usize, a: &annots::AnnotInfo) -> String {
    let own = a.contents.trim();
    if !own.is_empty() {
        return own.replace('\n', " ");
    }
    if !matches!(a.kind, annots::Kind::Highlight | annots::Kind::Underline | annots::Kind::StrikeOut) {
        return String::new();
    }
    let Some(p) = pdf.and_then(|pdf| pdf.page(page as i32)) else { return String::new() };
    // The annotation's box is a point bigger than the text all round.
    let r: Rect = a.rect.inflate(-1.0);
    let mut area = poppler::Rectangle::new();
    area.set_x1(r.x0);
    area.set_y1(r.y0);
    area.set_x2(r.x1);
    area.set_y2(r.y1);
    let text = p.selected_text(poppler::SelectionStyle::Glyph, &mut area).map(|t| t.to_string()).unwrap_or_default();
    let words = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.is_empty() { String::new() } else { format!("“{words}”") }
}

fn outline_list(items: &[Outline], depth: i32) -> gtk::Box {
    let list = widgets::vbox(1);
    for it in items {
        let row = widgets::hbox(2);
        row.set_margin_start(depth * 14);
        let toggle = gtk::Button::from_icon_name(if it.open { "pan-down-symbolic" } else { "pan-end-symbolic" });
        toggle.add_css_class("flat");
        toggle.add_css_class("outline-toggle");
        toggle.set_visible(!it.children.is_empty());
        if it.children.is_empty() {
            // Keep titles aligned with siblings that have an arrow.
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_size_request(26, 1);
            row.append(&spacer);
        } else {
            row.append(&toggle);
        }
        let title = gtk::Button::new();
        title.add_css_class("outline-item");
        title.set_hexpand(true);
        let inner = widgets::hbox(6);
        let label = widgets::label(&it.title, "");
        label.set_hexpand(true);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        inner.append(&label);
        if let Some(p) = it.page {
            inner.append(&widgets::label(&(p + 1).to_string(), "dim mono"));
        }
        title.set_child(Some(&inner));
        if let Some(p) = it.page {
            title.connect_clicked(move |_| {
                if let Some(v) = view() {
                    v.jump(p);
                    v.panel_used();
                }
            });
        }
        row.append(&title);
        list.append(&row);
        if !it.children.is_empty() {
            let rev = gtk::Revealer::new();
            rev.set_reveal_child(it.open);
            rev.set_transition_duration(if crate::prefs::get().reduce_motion { 0 } else { 150 });
            rev.set_child(Some(&outline_list(&it.children, depth + 1)));
            let (r2, t2) = (rev.clone(), toggle.clone());
            toggle.connect_clicked(move |_| {
                let open = !r2.reveals_child();
                r2.set_reveal_child(open);
                t2.set_icon_name(if open { "pan-down-symbolic" } else { "pan-end-symbolic" });
            });
            list.append(&rev);
        }
    }
    list
}
