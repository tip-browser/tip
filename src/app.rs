//! The browser application: tabs, the HTML-rendered toolbar, DevTools and
//! event routing. Platform backends drive it via `AppHandler`.

use std::rc::Rc;
use std::time::Instant;

use crate::bindings::events::Mods;
use crate::css::Color;
use crate::devtools::{self, DevAction, DevTools};
use crate::net::{url::from_user_input, Url};
use crate::page::{HostMsg, LoadOptions, Page};
use crate::paint::{self, DisplayList, Viewport};
use crate::raster::{Canvas, Rect};
use crate::style::Cursor;
use crate::tab::Tab;

pub const CHROME_H: f32 = 76.0;
const SPLITTER: f32 = 5.0;
/// Narrowest a tab gets before the strip starts scrolling.
const MIN_TAB_W: f32 = 96.0;
const MAX_TAB_W: f32 = 220.0;
const TABLIST_W: f32 = 380.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    Chrome,
    Content,
    DevTools,
    Splitter,
    TabList,
}

/// The "all tabs" panel: a searchable list of every tab.
struct TabList {
    page: Page,
    query: String,
    /// Tab indices shown, in order.
    shown: Vec<usize>,
    key: String,
}

struct RegionCache {
    key: Option<(usize, (i32, i32), (u32, u32), u64)>,
    canvas: Canvas,
}

impl RegionCache {
    fn new() -> RegionCache {
        RegionCache { key: None, canvas: Canvas::new(1, 1) }
    }
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    chrome: Page,
    devtools: Option<DevTools>,
    pub devtools_w: f32,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub zoom: f32,
    pub canvas: Canvas,
    focus: Region,
    capture: Option<Region>,
    hover_region: Option<Region>,
    chrome_key: String,
    status: Option<String>,
    caches: [RegionCache; 4],
    /// First tab shown when the strip overflows.
    tab_first: usize,
    /// Scroll the strip to the active tab on the next layout (set when it changes).
    follow_active: bool,
    tablist: Option<TabList>,
    page_gen: Vec<u64>,
    next_gen: u64,
    last_click: (Instant, f32, f32, i32),
    pub title: String,
    pub quit: bool,
    picking: bool,
}

fn content_opts(w: f32, h: f32, scale: f32) -> LoadOptions {
    LoadOptions { viewport: (w.max(100.0), h.max(100.0)), scale, scripting: true, internal: false }
}

impl App {
    pub fn new(url: &str, width: f32, height: f32, scale: f32) -> App {
        let chrome_opts = LoadOptions { viewport: (width, CHROME_H), scale, scripting: false, internal: true };
        let mut chrome = Page::from_html(crate::pages::CHROME, Url::parse("tip://chrome").unwrap(), &chrome_opts);
        chrome.start();
        let mut app = App {
            tabs: vec![],
            active: 0,
            chrome,
            devtools: None,
            devtools_w: (width * 0.4).clamp(320.0, 640.0),
            width,
            height,
            scale,
            zoom: 1.0,
            canvas: Canvas::new((width * scale) as usize, (height * scale) as usize),
            focus: Region::Content,
            capture: None,
            hover_region: None,
            chrome_key: String::new(),
            status: None,
            caches: [RegionCache::new(), RegionCache::new(), RegionCache::new(), RegionCache::new()],
            tab_first: 0,
            follow_active: true,
            tablist: None,
            page_gen: vec![],
            next_gen: 1,
            last_click: (Instant::now(), 0.0, 0.0, 0),
            title: "Tip".into(),
            quit: false,
            picking: false,
        };
        app.new_tab(url);
        if std::env::var("TIP_DEVTOOLS").is_ok() {
            app.toggle_devtools();
        }
        app
    }

    fn content_rect(&self) -> Rect {
        let dw = if self.devtools.is_some() { self.devtools_w } else { 0.0 };
        Rect::new(0.0, CHROME_H, (self.width - dw).max(50.0), (self.height - CHROME_H).max(50.0))
    }

    fn devtools_rect(&self) -> Rect {
        let c = self.content_rect();
        Rect::new(c.right(), CHROME_H, self.width - c.right(), self.height - CHROME_H)
    }

    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn new_tab(&mut self, url: &str) {
        let c = self.content_rect();
        let opts = content_opts(c.w / self.zoom, c.h / self.zoom, self.scale * self.zoom);
        let mut t = Tab::blank(opts);
        if let Some(u) = from_user_input(url) {
            t.go(u, false);
        }
        self.tabs.push(t);
        self.page_gen.push(0);
        self.active = self.tabs.len() - 1;
        self.bump_gen();
    }

    fn bump_gen(&mut self) {
        self.follow_active = true;
        self.page_gen[self.active] = self.next_gen;
        self.next_gen += 1;
    }

    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        self.page_gen.remove(i);
        if self.tabs.is_empty() {
            self.new_tab("tip://home");
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        self.bump_gen();
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
            self.bump_gen();
            self.resize(self.width, self.height, self.scale);
        }
    }

    pub fn navigate(&mut self, input: &str) {
        if let Some(u) = from_user_input(input) {
            self.tab().go(u, false);
            self.focus = Region::Content;
            crate::input::focus(&mut self.chrome.interp, None);
        }
    }

    pub fn toggle_devtools(&mut self) {
        if self.devtools.is_some() {
            self.devtools = None;
            self.tabs[self.active].page.host_mut().highlight = None;
            self.picking = false;
        } else {
            let r = Rect::new(0.0, 0.0, self.devtools_w, self.height - CHROME_H);
            self.devtools = Some(DevTools::new((r.w, r.h), self.scale));
            self.bump_gen();
        }
        self.resize(self.width, self.height, self.scale);
    }

    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        self.width = width.max(200.0);
        self.height = height.max(150.0);
        self.scale = scale;
        if self.devtools.is_some() {
            self.devtools_w = self.devtools_w.clamp(240.0, (self.width - 200.0).max(240.0));
        }
        self.canvas.resize((self.width * scale).round() as usize, (self.height * scale).round() as usize);
        for c in self.caches.iter_mut() {
            c.key = None;
        }
        self.chrome.set_viewport(self.width, CHROME_H, scale);
        let c = self.content_rect();
        let z = self.zoom;
        for t in &mut self.tabs {
            t.opts.viewport = (c.w / z, c.h / z);
            t.opts.scale = scale * z;
            t.page.set_viewport(c.w / z, c.h / z, scale * z);
        }
        let d = self.devtools_rect();
        if let Some(dt) = &mut self.devtools {
            dt.page.set_viewport(d.w - SPLITTER, d.h, scale);
        }
    }

    pub fn set_zoom(&mut self, z: f32) {
        self.zoom = z.clamp(0.3, 4.0);
        self.resize(self.width, self.height, self.scale);
    }

    // ------------------------------------------------------------ chrome UI

    fn sync_chrome(&mut self) {
        let _ = self.tab_window();
        let t = &self.tabs[self.active];
        let url = t.pending.as_ref().map(|p| p.url.to_string()).unwrap_or_else(|| t.url().to_string());
        let shown_url = if url.starts_with("tip://home") { String::new() } else { url.clone() };
        let mut key = format!("{}|{}|{}|{}|{}|{}|{}|{}|{}", self.active, shown_url, t.can_go_back(), t.can_go_forward(), t.is_loading(), self.devtools.is_some(), self.width, self.tab_first, self.tablist.is_some());
        let titles: Vec<String> = self.tabs.iter().map(|t| if t.is_loading() && t.page.host().url.scheme == "about" { "Loading…".to_string() } else { t.page.title() }).collect();
        for tt in &titles {
            key.push('|');
            key.push_str(tt);
        }
        if key == self.chrome_key {
            return;
        }
        self.chrome_key = key;
        let (first, count, tab_w) = self.tab_window();
        self.follow_active = false;
        let overflow = count < self.tabs.len();
        let mut tabs_html = String::new();
        if overflow {
            tabs_html.push_str(&format!("<div class=\"scroll{}\" data-action=tabscroll data-arg=-1 title=\"Scroll tabs left\">‹</div>", if first == 0 { " off" } else { "" }));
            if first > 0 {
                tabs_html.push_str(&format!("<span class=more title=\"{first} more tabs\">{first}</span>"));
            }
        }
        for i in first..first + count {
            let title = &titles[i];
            let loading = self.tabs[i].is_loading();
            tabs_html.push_str(&format!(
                "<div class=\"tab{}\" style=\"width:{}px\" data-action=tab data-arg={i} title=\"{}\">{}<span class=title>{}</span><span class=x data-action=closetab data-arg={i}>×</span></div>",
                if i == self.active { " active" } else { "" },
                tab_w.floor(),
                crate::dom::escape_attr(title),
                if loading { "<span class=dot></span>" } else { "" },
                crate::dom::escape_text(title)
            ));
        }
        if overflow {
            let after = self.tabs.len() - first - count;
            if after > 0 {
                tabs_html.push_str(&format!("<span class=more title=\"{after} more tabs\">{after}</span>"));
            }
            tabs_html.push_str(&format!("<div class=\"scroll{}\" data-action=tabscroll data-arg=1 title=\"Scroll tabs right\">›</div>", if after == 0 { " off" } else { "" }));
        }
        tabs_html.push_str("<div id=newtab data-action=newtab title=\"New tab\">+</div>");
        tabs_html.push_str("<div id=spacer></div>");
        tabs_html.push_str(&format!(
            "<div id=alltabs class=\"{}\" data-action=alltabs title=\"Search tabs ({} open)\"><svg viewBox=\"0 0 24 24\" width=16 height=16 fill=none stroke=currentColor stroke-width=2.2 stroke-linecap=round stroke-linejoin=round style=\"margin-top:6px\"><path d=\"m6 9 6 6 6-6\"/></svg></div>",
            if self.tablist.is_some() { "on" } else { "" },
            self.tabs.len()
        ));
        devtools::set_html(&mut self.chrome, "tabs", &tabs_html);
        let t = &self.tabs[self.active];
        let (back, fwd, loading) = (t.can_go_back(), t.can_go_forward(), t.is_loading());
        let secure = url.starts_with("https://");
        let h = self.chrome.host_mut();
        let set_cls = |h: &mut crate::page::PageHost, id: &str, cls: &str| {
            if let Some(n) = h.doc.get_element_by_id(id) {
                h.doc.set_attr(n, "class", cls);
            }
        };
        set_cls(h, "back", if back { "btn" } else { "btn off" });
        set_cls(h, "fwd", if fwd { "btn" } else { "btn off" });
        set_cls(h, "progress", if loading { "loading" } else { "" });
        set_cls(h, "devtools", if self.devtools.is_some() { "btn on" } else { "btn" });
        if let Some(r) = h.doc.get_element_by_id("reload") {
            h.doc.set_text_content(r, if loading { "✕" } else { "↻" });
            h.doc.set_attr(r, "data-action", if loading { "stop" } else { "reload" });
        }
        if let Some(l) = h.doc.get_element_by_id("lock") {
            h.doc.set_text_content(l, if secure { "●" } else { "" });
        }
        if let Some(a) = h.doc.get_element_by_id("addr") {
            if h.focus != Some(a) {
                h.set_control_value(a, shown_url);
            }
        }
    }

    /// Which tabs fit in the strip: (first index, count, tab width). Keeps the active tab visible.
    fn tab_window(&mut self) -> (usize, usize, f32) {
        let n = self.tabs.len();
        // left inset (window buttons) + new tab + all-tabs buttons + padding
        let avail = (self.width - 78.0 - 8.0 - 30.0 - 36.0).max(MIN_TAB_W);
        let gap = 2.0;
        let fits = |k: usize, w: f32| k as f32 * (w + gap) <= avail;
        if fits(n, MIN_TAB_W) {
            self.tab_first = 0;
            let w = ((avail / n.max(1) as f32) - gap).clamp(MIN_TAB_W, MAX_TAB_W);
            return (0, n, w);
        }
        // overflow: room for the scroll buttons and the hidden-tab counters
        let room = avail - 2.0 * 24.0 - 2.0 * 26.0;
        let count = ((room / (MIN_TAB_W + gap)).floor() as usize).clamp(1, n);
        let mut first = self.tab_first.min(n - count);
        if self.follow_active {
            if self.active < first {
                first = self.active;
            } else if self.active >= first + count {
                first = self.active + 1 - count;
            }
        }
        self.tab_first = first;
        let w = ((room / count as f32) - gap).clamp(MIN_TAB_W, MAX_TAB_W);
        (first, count, w)
    }

    /// Scroll the tab strip by `delta` tabs (without changing the active tab).
    fn scroll_tabs(&mut self, delta: i64) {
        let (_, count, _) = self.tab_window();
        let max_first = self.tabs.len().saturating_sub(count);
        let first = (self.tab_first as i64 + delta).clamp(0, max_first as i64) as usize;
        self.follow_active = false;
        self.tab_first = first;
    }

    fn tablist_rect(&self) -> Rect {
        let rows = self.tabs.len().max(1) as f32;
        let h = (72.0 + rows * 40.0 + 8.0).min((self.height - CHROME_H - 16.0).max(120.0));
        let w = TABLIST_W.min(self.width - 16.0);
        Rect::new(self.width - w - 8.0, CHROME_H - 6.0, w, h)
    }

    pub fn toggle_tablist(&mut self) {
        if self.tablist.is_some() {
            self.close_tablist();
            return;
        }
        let r = self.tablist_rect();
        let opts = LoadOptions { viewport: (r.w, r.h), scale: self.scale, scripting: false, internal: true };
        let mut page = Page::from_html(crate::pages::TABLIST, Url::parse("tip://tabs").unwrap(), &opts);
        page.start();
        let q = page.host().doc.get_element_by_id("q");
        crate::input::focus(&mut page.interp, q);
        self.tablist = Some(TabList { page, query: String::new(), shown: vec![], key: String::new() });
        self.focus = Region::TabList;
        self.caches[3].key = None;
        self.sync_tablist();
    }

    fn handle_tablist_pick(&mut self, i: usize) {
        self.switch_tab(i);
        self.close_tablist();
    }

    fn close_tablist(&mut self) {
        if self.tablist.take().is_some() {
            if self.focus == Region::TabList {
                self.focus = Region::Content;
            }
            self.chrome_key.clear();
        }
    }

    /// Rebuild the tab list's rows when tabs or the search text change.
    fn sync_tablist(&mut self) {
        let r = self.tablist_rect();
        let scale = self.scale;
        let active = self.active;
        let items: Vec<(String, String, bool)> = self
            .tabs
            .iter()
            .map(|t| {
                let url = t.pending.as_ref().map(|p| p.url.to_string()).unwrap_or_else(|| t.url().to_string());
                (t.page.title(), url, t.is_loading())
            })
            .collect();
        let Some(tl) = &mut self.tablist else { return };
        let q = tl.page.host().doc.get_element_by_id("q").map(|n| tl.page.host().control_value(n)).unwrap_or_default();
        tl.query = q.to_lowercase();
        let shown: Vec<usize> = (0..items.len())
            .filter(|&i| {
                let (t, u, _) = &items[i];
                tl.query.is_empty() || t.to_lowercase().contains(&tl.query) || u.to_lowercase().contains(&tl.query)
            })
            .collect();
        let key = format!("{active}|{}|{:?}|{:?}", tl.query, shown, items);
        if key == tl.key {
            return;
        }
        tl.key = key;
        let mut html = String::new();
        for &i in &shown {
            let (t, u, loading) = &items[i];
            let title = if t.is_empty() { u.as_str() } else { t.as_str() };
            let shown_url = u.trim_start_matches("https://").trim_start_matches("http://");
            html.push_str(&format!(
                "<div class=\"row{}\" data-action=tab data-arg={i}>{}<span class=text><span class=t>{}</span><span class=u>{}</span></span><span class=x data-action=closetab data-arg={i} title=\"Close tab\">×</span></div>",
                if i == active { " active" } else { "" },
                if *loading { "<span class=dot></span>" } else { "" },
                crate::dom::escape_text(title),
                crate::dom::escape_text(shown_url)
            ));
        }
        if shown.is_empty() {
            html.push_str("<div class=empty>No tabs match</div>");
        }
        devtools::set_html(&mut tl.page, "list", &html);
        let count = if tl.query.is_empty() { format!("{} open tabs", items.len()) } else { format!("{} of {} tabs", shown.len(), items.len()) };
        if let Some(c) = tl.page.host().doc.get_element_by_id("count") {
            tl.page.host_mut().doc.set_text_content(c, &count);
        }
        tl.shown = shown;
        tl.page.set_viewport(r.w, r.h, scale);
    }

    fn handle_chrome_msg(&mut self, msg: HostMsg) {
        match msg {
            HostMsg::Action { action, arg, .. } => match action.as_str() {
                "back" => self.tab().back(false),
                "forward" => self.tab().forward(false),
                "reload" => self.tab().reload(false),
                "stop" => self.tab().pending = None,
                "home" => self.navigate("tip://home"),
                "devtools" => self.toggle_devtools(),
                "alltabs" => self.toggle_tablist(),
                "tabscroll" => {
                    if let Ok(d) = arg.parse::<i64>() {
                        self.scroll_tabs(d)
                    }
                }
                "newtab" => {
                    self.new_tab("tip://home");
                    self.focus_address();
                }
                "tab" => {
                    if let Ok(i) = arg.parse() {
                        self.switch_tab(i)
                    }
                }
                "closetab" => {
                    if let Ok(i) = arg.parse() {
                        self.close_tab(i)
                    }
                }
                _ => {}
            },
            HostMsg::Submit { action, value, .. } => {
                if action == "go" {
                    self.navigate(&value);
                }
            }
        }
    }

    pub fn focus_address(&mut self) {
        self.focus = Region::Chrome;
        let addr = self.chrome.host().doc.get_element_by_id("addr");
        crate::input::focus(&mut self.chrome.interp, addr);
        if let Some(a) = addr {
            self.chrome.select_all(a);
        }
    }

    // ------------------------------------------------------------ event loop

    /// Advance timers, loads and UI state. Returns true if a redraw is needed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        let mut opened = Vec::new();
        for i in 0..self.tabs.len() {
            if self.tabs[i].poll_load() {
                changed = true;
                if i == self.active {
                    self.bump_gen();
                }
            }
            let t = &mut self.tabs[i];
            if t.page.poll() {
                changed = true;
            }
            if i == self.active && t.page.has_animation_frames() {
                t.page.run_animation_frames();
                changed = true;
            }
            if t.process_navigation(false) {
                changed = true;
            }
            opened.append(&mut t.open_requests);
        }
        for u in opened {
            self.new_tab(&u.to_string());
        }
        // chrome messages
        let msgs: Vec<HostMsg> = std::mem::take(&mut self.chrome.host_mut().outbox);
        for m in msgs {
            self.handle_chrome_msg(m);
            changed = true;
        }
        self.chrome.poll();
        // tab list panel
        if self.tablist.is_some() {
            let msgs: Vec<HostMsg> = std::mem::take(&mut self.tablist.as_mut().unwrap().page.host_mut().outbox);
            for m in msgs {
                match m {
                    HostMsg::Action { action, arg, .. } if action == "tab" => {
                        if let Ok(i) = arg.parse() {
                            self.handle_tablist_pick(i);
                        }
                    }
                    HostMsg::Action { action, arg, .. } if action == "closetab" => {
                        if let Ok(i) = arg.parse() {
                            self.close_tab(i);
                        }
                    }
                    HostMsg::Submit { .. } => {
                        // Enter: switch to the first match
                        let first = self.tablist.as_ref().and_then(|t| t.shown.first().copied());
                        if let Some(i) = first {
                            self.switch_tab(i);
                        }
                        self.close_tablist();
                    }
                    _ => {}
                }
                changed = true;
            }
            if let Some(tl) = &mut self.tablist {
                tl.page.poll();
            }
            self.sync_tablist();
        }
        // devtools
        if self.devtools.is_some() {
            let msgs: Vec<HostMsg> = std::mem::take(&mut self.devtools.as_mut().unwrap().page.host_mut().outbox);
            let mut close = false;
            for m in msgs {
                let a = {
                    let dt = self.devtools.as_mut().unwrap();
                    let page = &mut self.tabs[self.active].page;
                    dt.handle(m, page)
                };
                match a {
                    DevAction::Close => close = true,
                    DevAction::Pick(on) => self.picking = on,
                    DevAction::None => {}
                }
                changed = true;
            }
            if close {
                self.toggle_devtools();
            } else {
                let generation = self.page_gen[self.active];
                let dt = self.devtools.as_mut().unwrap();
                let page = &mut self.tabs[self.active].page;
                dt.sync(page, generation);
                if !self.picking {
                    page.host_mut().highlight = dt.selected;
                }
            }
        }
        self.sync_chrome();
        let title = format!("{} — Tip", self.tabs[self.active].page.title());
        if title != self.title {
            self.title = title;
            changed = true;
        }
        if self.render_needed() {
            changed = true;
        }
        changed
    }

    /// Next time the app needs to wake up for a timer (if any).
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.tabs.iter().filter_map(|t| t.page.next_timer_due()).min()
    }

    pub fn busy(&self) -> bool {
        self.tabs.iter().any(|t| t.is_loading() || t.page.has_pending_work() || t.page.has_animation_frames())
    }

    fn region_at(&self, x: f32, y: f32) -> Region {
        if self.tablist.is_some() && self.tablist_rect().contains(x, y) {
            return Region::TabList;
        }
        if y < CHROME_H {
            return Region::Chrome;
        }
        if self.devtools.is_some() {
            let c = self.content_rect();
            if x >= c.right() && x < c.right() + SPLITTER {
                return Region::Splitter;
            }
            if x >= c.right() {
                return Region::DevTools;
            }
        }
        Region::Content
    }

    fn local(&self, r: Region, x: f32, y: f32) -> (f32, f32) {
        match r {
            Region::Chrome => (x, y),
            Region::Content => ((x) / self.zoom, (y - CHROME_H) / self.zoom),
            Region::DevTools | Region::Splitter => {
                let d = self.devtools_rect();
                (x - d.x - SPLITTER, y - d.y)
            }
            Region::TabList => {
                let r = self.tablist_rect();
                (x - r.x, y - r.y)
            }
        }
    }

    fn page_for(&mut self, r: Region) -> Option<&mut Page> {
        match r {
            Region::Chrome => Some(&mut self.chrome),
            Region::Content => Some(&mut self.tabs[self.active].page),
            Region::DevTools => self.devtools.as_mut().map(|d| &mut d.page),
            Region::TabList => self.tablist.as_mut().map(|t| &mut t.page),
            Region::Splitter => None,
        }
    }

    pub fn mouse_move(&mut self, x: f32, y: f32) {
        let r = self.capture.unwrap_or_else(|| self.region_at(x, y));
        self.hover_region = Some(r);
        if r == Region::Splitter && self.capture == Some(Region::Splitter) {
            self.devtools_w = (self.width - x).clamp(240.0, self.width - 200.0);
            self.resize(self.width, self.height, self.scale);
            return;
        }
        let (lx, ly) = self.local(r, x, y);
        if self.picking && r == Region::Content {
            let page = &mut self.tabs[self.active].page;
            let hit = page.host_mut().hit_test(lx, ly);
            page.host_mut().highlight = hit.map(|n| if page.host().doc.is_element(n) { n } else { page.host().doc.parent(n).unwrap_or(n) });
            return;
        }
        if let Some(p) = self.page_for(r) {
            p.mouse_move(lx, ly);
        }
        // link status bubble
        self.status = None;
        if r == Region::Content {
            let page = &mut self.tabs[self.active].page;
            if let Some(n) = page.host().hover {
                let h = page.host();
                let mut cur = Some(n);
                while let Some(c) = cur {
                    if h.doc.tag_is(c, "a") {
                        if let Some(href) = h.doc.attr(c, "href") {
                            self.status = h.resolve_url(href).map(|u| u.to_string());
                        }
                        break;
                    }
                    cur = h.doc.parent(c);
                }
            }
        }
    }

    /// True if (x, y) is empty tab-strip space that should act as the window title bar.
    pub fn is_titlebar(&mut self, x: f32, y: f32) -> bool {
        if y >= 36.0 || self.region_at(x, y) != Region::Chrome {
            return false;
        }
        let Some(n) = self.chrome.host_mut().hit_test(x, y) else { return true };
        let h = self.chrome.host();
        let mut cur = Some(n);
        while let Some(c) = cur {
            if h.doc.has_attr(c, "data-action") {
                return false;
            }
            cur = h.doc.parent(c);
        }
        true
    }

    pub fn mouse_down(&mut self, x: f32, y: f32, button: i32, mods: Mods) {
        let now = Instant::now();
        let (t, px, py, n) = self.last_click;
        let count = if now.duration_since(t).as_millis() < 450 && (px - x).abs() < 5.0 && (py - y).abs() < 5.0 { n + 1 } else { 1 };
        self.last_click = (now, x, y, count);
        let r = self.region_at(x, y);
        if self.tablist.is_some() && r != Region::TabList {
            let on_toggle = r == Region::Chrome && self.chrome_action_at(x, y).as_deref() == Some("alltabs");
            self.close_tablist();
            if !on_toggle {
                return;
            }
            // the toggle button closes it; don't reopen on the same click
            self.capture = None;
            return;
        }
        self.capture = Some(r);
        if r == Region::Splitter {
            return;
        }
        if r != Region::Chrome {
            // clicking elsewhere blurs the address bar
            crate::input::focus(&mut self.chrome.interp, None);
        }
        self.focus = r;
        let (lx, ly) = self.local(r, x, y);
        if self.picking && r == Region::Content {
            return;
        }
        let addr = self.chrome.host().doc.get_element_by_id("addr");
        let addr_was_focused = addr.is_some() && self.chrome.host().focus == addr;
        if let Some(p) = self.page_for(r) {
            p.mouse_down_n(lx, ly, button, mods, count);
        }
        // first click into the address bar selects the whole URL
        if r == Region::Chrome && !addr_was_focused && count == 1 && addr.is_some() && self.chrome.host().focus == addr {
            self.chrome.select_all(addr.unwrap());
        }
    }

    /// The data-action of the chrome UI element at (x, y), if any.
    fn chrome_action_at(&mut self, x: f32, y: f32) -> Option<String> {
        let n = self.chrome.host_mut().hit_test(x, y)?;
        let h = self.chrome.host();
        let mut cur = Some(n);
        while let Some(c) = cur {
            if let Some(a) = h.doc.attr(c, "data-action") {
                return Some(a.to_string());
            }
            cur = h.doc.parent(c);
        }
        None
    }

    pub fn mouse_up(&mut self, x: f32, y: f32, button: i32, mods: Mods) {
        let Some(r) = self.capture.take() else { return };
        if r == Region::Splitter {
            return;
        }
        let (lx, ly) = self.local(r, x, y);
        let count = self.last_click.3;
        if self.picking && r == Region::Content {
            let page = &mut self.tabs[self.active].page;
            if let Some(n) = page.host_mut().hit_test(lx, ly) {
                if let Some(dt) = &mut self.devtools {
                    dt.select(page, n);
                    dt.picking = false;
                }
            }
            self.picking = false;
            if let Some(dt) = &mut self.devtools {
                let h = dt.page.host_mut();
                if let Some(p) = h.doc.get_element_by_id("pick") {
                    h.doc.set_attr(p, "class", "tool");
                }
            }
            return;
        }
        if let Some(p) = self.page_for(r) {
            p.mouse_up(lx, ly, button, mods, count);
        }
        // right-click on content with DevTools open: inspect element
        if button == 2 && r == Region::Content && self.devtools.is_some() {
            let page = &mut self.tabs[self.active].page;
            if let Some(n) = page.host_mut().hit_test(lx, ly) {
                if let Some(dt) = &mut self.devtools {
                    dt.select(page, n);
                }
            }
        }
    }

    pub fn wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        let r = self.region_at(x, y);
        if r == Region::Chrome && y < 36.0 {
            // wheel over the tab strip scrolls through tabs
            let d = if dx.abs() > dy.abs() { dx } else { dy };
            if d.abs() >= 1.0 {
                self.scroll_tabs(if d > 0.0 { 1 } else { -1 });
            }
            return;
        }
        let (lx, ly) = self.local(r, x, y);
        let z = if r == Region::Content { self.zoom } else { 1.0 };
        if let Some(p) = self.page_for(r) {
            p.wheel(lx, ly, dx / z, dy / z);
        }
    }

    /// Returns true if the key was consumed by a browser shortcut.
    pub fn key_down(&mut self, key: &str, text: Option<&str>, mods: Mods) -> bool {
        let cmd = if cfg!(target_os = "macos") { mods.meta } else { mods.ctrl };
        let lower = key.to_ascii_lowercase();
        if self.tablist.is_some() && key == "Escape" {
            self.close_tablist();
            return true;
        }
        if self.tablist.is_some() && (key == "ArrowDown" || key == "ArrowUp") {
            return true;
        }
        if cmd && mods.shift && lower == "a" {
            self.toggle_tablist();
            return true;
        }
        if key == "F12" || (cmd && mods.alt && lower == "i") {
            self.toggle_devtools();
            return true;
        }
        if cmd {
            match lower.as_str() {
                "l" => {
                    self.focus_address();
                    return true;
                }
                "r" => {
                    self.tab().reload(false);
                    return true;
                }
                "t" => {
                    self.new_tab("tip://home");
                    self.focus_address();
                    return true;
                }
                "w" => {
                    let a = self.active;
                    self.close_tab(a);
                    return true;
                }
                "q" => {
                    self.quit = true;
                    return true;
                }
                "[" => {
                    self.tab().back(false);
                    return true;
                }
                "]" => {
                    self.tab().forward(false);
                    return true;
                }
                "=" | "+" => {
                    self.set_zoom(self.zoom * 1.1);
                    return true;
                }
                "-" => {
                    self.set_zoom(self.zoom / 1.1);
                    return true;
                }
                "0" => {
                    self.set_zoom(1.0);
                    return true;
                }
                d if d.len() == 1 && d.chars().next().unwrap().is_ascii_digit() => {
                    let i = d.parse::<usize>().unwrap_or(1).max(1) - 1;
                    let i = if d == "9" { self.tabs.len() - 1 } else { i };
                    self.switch_tab(i);
                    return true;
                }
                _ => {}
            }
        }
        if mods.ctrl && key == "Tab" {
            let n = self.tabs.len();
            let i = if mods.shift { (self.active + n - 1) % n } else { (self.active + 1) % n };
            self.switch_tab(i);
            return true;
        }
        if mods.alt && (key == "ArrowLeft" || key == "ArrowRight") && self.focus != Region::Chrome {
            if key == "ArrowLeft" {
                self.tab().back(false)
            } else {
                self.tab().forward(false)
            }
            return true;
        }
        if key == "Escape" && self.focus == Region::Chrome {
            crate::input::focus(&mut self.chrome.interp, None);
            self.focus = Region::Content;
            self.chrome_key.clear();
            return true;
        }
        let f = self.focus;
        if let Some(p) = self.page_for(f) {
            p.key_down(key, text, mods);
        }
        false
    }

    pub fn cursor(&self) -> Cursor {
        match self.hover_region {
            Some(Region::Splitter) => Cursor::ResizeEw,
            Some(Region::Chrome) => self.chrome.cursor(),
            Some(Region::DevTools) => self.devtools.as_ref().map(|d| d.page.cursor()).unwrap_or(Cursor::Default),
            Some(Region::Content) if self.picking => Cursor::Crosshair,
            Some(Region::Content) => self.tabs[self.active].page.cursor(),
            Some(Region::TabList) => self.tablist.as_ref().map(|t| t.page.cursor()).unwrap_or(Cursor::Default),
            None => Cursor::Default,
        }
    }

    // ------------------------------------------------------------ rendering

    fn region_key(dl: &Rc<DisplayList>, page: &Page, size: (u32, u32), generation: u64) -> (usize, (i32, i32), (u32, u32), u64) {
        let sc = page.host().scroll;
        (Rc::as_ptr(dl) as usize, (sc.0.round() as i32, sc.1.round() as i32), size, generation)
    }

    fn render_needed(&mut self) -> bool {
        let s = self.scale;
        let cr = self.content_rect();
        let size = |r: Rect| ((r.w * s).round() as u32, (r.h * s).round() as u32);
        let chrome_dl = self.chrome.host_mut().display_list();
        if self.caches[0].key != Some(Self::region_key(&chrome_dl, &self.chrome, size(Rect::new(0.0, 0.0, self.width, CHROME_H)), 0)) {
            return true;
        }
        let generation = self.page_gen[self.active];
        let page = &mut self.tabs[self.active].page;
        let dl = page.host_mut().display_list();
        if self.caches[1].key != Some(Self::region_key(&dl, page, size(cr), generation)) {
            return true;
        }
        let d = self.devtools_rect();
        if let Some(dt) = &mut self.devtools {
            let dl = dt.page.host_mut().display_list();
            if self.caches[2].key != Some(Self::region_key(&dl, &dt.page, size(d), 0)) {
                return true;
            }
        }
        let tr = self.tablist_rect();
        if let Some(tl) = &mut self.tablist {
            let dl = tl.page.host_mut().display_list();
            if self.caches[3].key != Some(Self::region_key(&dl, &tl.page, size(tr), 0)) {
                return true;
            }
        }
        false
    }

    fn render_region(cache: &mut RegionCache, page: &mut Page, w: usize, h: usize, scale: f32, generation: u64) {
        let dl = page.host_mut().display_list();
        let key = Self::region_key(&dl, page, (w as u32, h as u32), generation);
        if cache.key == Some(key) {
            return;
        }
        if cache.canvas.w != w || cache.canvas.h != h {
            cache.canvas.resize(w.max(1), h.max(1));
        }
        let scroll = page.host().scroll;
        paint::render(&dl, &mut cache.canvas, &Viewport { ox: 0.0, oy: 0.0, w: w as f32, h: h as f32, scale, scroll });
        cache.key = Some(key);
    }

    fn blit(dst: &mut Canvas, src: &Canvas, ox: usize, oy: usize) {
        for row in 0..src.h {
            let y = oy + row;
            if y >= dst.h {
                break;
            }
            let w = src.w.min(dst.w.saturating_sub(ox));
            let d = y * dst.w + ox;
            dst.px[d..d + w].copy_from_slice(&src.px[row * src.w..row * src.w + w]);
        }
    }

    /// Paint everything into `self.canvas` (device pixels).
    pub fn render(&mut self) -> &Canvas {
        let s = self.scale;
        let wpx = self.canvas.w;
        let chrome_h = (CHROME_H * s).round() as usize;
        Self::render_region(&mut self.caches[0], &mut self.chrome, wpx, chrome_h, s, 0);
        Self::blit(&mut self.canvas, &self.caches[0].canvas, 0, 0);
        let cr = self.content_rect();
        let (cw, ch) = ((cr.w * s).round() as usize, (cr.h * s).round() as usize);
        let generation = self.page_gen[self.active];
        let zoom = self.zoom;
        Self::render_region(&mut self.caches[1], &mut self.tabs[self.active].page, cw, ch, s * zoom, generation);
        Self::blit(&mut self.canvas, &self.caches[1].canvas, 0, chrome_h);
        if self.devtools.is_some() {
            let d = self.devtools_rect();
            let ox = (d.x * s).round() as usize;
            let sw = (SPLITTER * s).round() as usize;
            // splitter
            self.canvas.push_clip(Rect::new(ox as f32, chrome_h as f32, sw as f32, ch as f32));
            self.canvas.fill_rect(Rect::new(ox as f32, chrome_h as f32, sw as f32, ch as f32), Color::rgb(0xdf, 0xe3, 0xea));
            self.canvas.pop_clip();
            let dw = wpx.saturating_sub(ox + sw);
            let dt = self.devtools.as_mut().unwrap();
            Self::render_region(&mut self.caches[2], &mut dt.page, dw, ch, s, 0);
            Self::blit(&mut self.canvas, &self.caches[2].canvas, ox + sw, chrome_h);
        }
        // the all-tabs panel floats over everything
        if self.tablist.is_some() {
            let r = self.tablist_rect();
            let dev = Rect::new(r.x * s, r.y * s, r.w * s, r.h * s);
            self.canvas.fill_path_blurred(&crate::raster::rounded_rect_path(dev.translate(0.0, 4.0 * s), [8.0 * s; 4]), Color::rgba(0, 0, 0, 70), 10.0 * s);
            let (w, h) = (dev.w.round() as usize, dev.h.round() as usize);
            let tl = self.tablist.as_mut().unwrap();
            Self::render_region(&mut self.caches[3], &mut tl.page, w, h, s, 0);
            Self::blit(&mut self.canvas, &self.caches[3].canvas, dev.x.round() as usize, dev.y.round() as usize);
        }
        // status bubble for hovered links
        if let Some(st) = &self.status {
            let text: String = st.chars().take(120).collect();
            let key = crate::text::FontKey::new(crate::style::FontFamily::Sans, 400, false);
            let tw = crate::text::measure(key, 12.0, &text, 0.0);
            let r = Rect::new(4.0 * s, (self.height - 26.0) * s, (tw + 16.0) * s, 22.0 * s);
            self.canvas.fill_path(&crate::raster::rounded_rect_path(r, [5.0 * s; 4]), Color::rgba(240, 242, 245, 245));
            paint::draw_text(&mut self.canvas, r.x + 8.0 * s, r.y + 15.0 * s, &text, key, 12.0, s, Color::rgb(55, 65, 81), 0.0);
        }
        &self.canvas
    }
}

/// Entry point for `tip [URL]`.
pub fn run(url: &str) -> i32 {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::window::run(url)
    }
    #[cfg(target_os = "windows")]
    {
        crate::platform::windows::run(url)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        eprintln!("Tip's windowed mode supports macOS and Windows. Use `tip render {url} -o out.png` for headless rendering.");
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_tabs(n: usize) -> App {
        let mut app = App::new("tip://home", 1280.0, 800.0, 1.0);
        for _ in 1..n {
            app.new_tab("tip://home");
        }
        app.tick();
        app
    }

    #[test]
    fn tab_strip_scrolls_and_lists_all_tabs() {
        let mut app = app_with_tabs(30);
        let (first, count, w) = app.tab_window();
        assert!(count < 30 && count >= 5, "visible {count}");
        assert!(first + count == 30, "the new active tab is in view");
        assert!(w >= MIN_TAB_W);
        // scrolling moves the window without changing the active tab
        app.scroll_tabs(-100);
        app.tick();
        assert_eq!(app.tab_window().0, 0);
        assert_eq!(app.active, 29);
        // the list panel shows every tab and switches on click
        app.toggle_tablist();
        app.tick();
        assert_eq!(app.tablist.as_ref().unwrap().shown.len(), 30);
        if let Ok(path) = std::env::var("TIP_APP_SNAPSHOT") {
            let c = app.render();
            let png = crate::image::png::encode_rgb(c.w as u32, c.h as u32, &c.to_rgb());
            std::fs::write(path, png).unwrap();
        }
        app.handle_tablist_pick(3);
        assert_eq!(app.active, 3);
        assert!(app.tablist.is_none());
        let (first, count, _) = app.tab_window();
        assert!(first <= 3 && 3 < first + count);
    }

    #[test]
    fn few_tabs_do_not_scroll() {
        let mut app = app_with_tabs(4);
        assert_eq!(app.tab_window().0, 0);
        assert_eq!(app.tab_window().1, 4);
    }
}
