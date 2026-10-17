//! A browsing context: the current page plus session history.

use std::sync::mpsc::{channel, Receiver};

use crate::net::{self, Request, Response, Url};
use crate::page::{LoadOptions, NavRequest, Page};

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub url: Url,
    pub title: String,
    pub scroll: (f32, f32),
}

/// Result of a background document load.
pub struct Loaded {
    pub req: Request,
    pub result: Result<Response, String>,
    pub log: Vec<net::NetLogEntry>,
}

pub struct PendingLoad {
    pub url: Url,
    pub rx: Receiver<Loaded>,
    pub kind: LoadKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadKind {
    Push,
    Replace,
    Traverse(usize),
}

/// Fetch a document and prefetch its stylesheets and scripts (runs off the main thread).
pub fn fetch_document(req: Request) -> Loaded {
    let mut log = Vec::new();
    let result = net::fetch_logged(&req, "document", &mut log);
    if let Ok(resp) = &result {
        let (mime, _) = resp.mime();
        if mime.is_empty() || mime.contains("html") {
            let base = Url::parse(&resp.url).unwrap_or_else(|| req.url.clone());
            let doc = crate::html::parse_document(&resp.text());
            let mut urls = Vec::new();
            for n in doc.element_descendants(0) {
                let href = match doc.tag(n) {
                    Some("link") if doc.attr(n, "rel").is_some_and(|r| r.to_ascii_lowercase().contains("stylesheet")) => doc.attr(n, "href"),
                    Some("script") => doc.attr(n, "src"),
                    _ => None,
                };
                if let Some(u) = href.and_then(|h| base.join(h)) {
                    if matches!(u.scheme.as_str(), "http" | "https") {
                        urls.push(u);
                    }
                }
            }
            net::prefetch_all(urls);
        }
    }
    Loaded { req, result, log }
}

pub struct Tab {
    pub page: Page,
    pub history: Vec<HistoryEntry>,
    pub index: usize,
    pub opts: LoadOptions,
    pub pending: Option<PendingLoad>,
    /// URLs requested to open in a new tab.
    pub open_requests: Vec<Url>,
}

impl Tab {
    pub fn new(url: &Url, opts: LoadOptions) -> Tab {
        let page = Page::from_html("", Url::parse("about:blank").unwrap(), &opts);
        let mut t = Tab { page, history: vec![], index: 0, opts, pending: None, open_requests: vec![] };
        t.navigate_blocking(Request::get(url.clone()), LoadKind::Push);
        t
    }

    pub fn blank(opts: LoadOptions) -> Tab {
        let page = Page::from_html("", Url::parse("about:blank").unwrap(), &opts);
        Tab { page, history: vec![HistoryEntry { url: Url::parse("about:blank").unwrap(), title: String::new(), scroll: (0.0, 0.0) }], index: 0, opts, pending: None, open_requests: vec![] }
    }

    pub fn url(&self) -> Url {
        self.page.url()
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    fn save_scroll(&mut self) {
        let sc = self.page.host().scroll;
        let title = self.page.title();
        if let Some(e) = self.history.get_mut(self.index) {
            e.scroll = sc;
            e.title = title;
        }
    }

    fn install(&mut self, loaded: Loaded, kind: LoadKind) {
        self.save_scroll();
        let opts = LoadOptions { viewport: self.page.host().viewport, scale: self.page.host().scale, scripting: self.opts.scripting, internal: false };
        let url = loaded.req.url.clone();
        let mut page = match loaded.result {
            Ok(resp) => Page::from_response(&resp, &opts),
            Err(e) => {
                let mut p = Page::from_html(&crate::page::error_page(&url.to_string(), &e), url.clone(), &LoadOptions { scripting: false, ..opts });
                p.host_mut().log("error", format!("Failed to load {url}: {e}"));
                p
            }
        };
        page.host_mut().net_log.splice(0..0, loaded.log);
        let final_url = page.url();
        match kind {
            LoadKind::Push => {
                self.history.truncate(self.index + 1);
                self.history.push(HistoryEntry { url: final_url, title: String::new(), scroll: (0.0, 0.0) });
                self.index = self.history.len() - 1;
            }
            LoadKind::Replace => {
                if self.history.is_empty() {
                    self.history.push(HistoryEntry { url: final_url, title: String::new(), scroll: (0.0, 0.0) });
                    self.index = 0;
                } else {
                    self.history[self.index] = HistoryEntry { url: final_url, title: String::new(), scroll: (0.0, 0.0) };
                }
            }
            LoadKind::Traverse(i) => self.index = i,
        }
        page.host_mut().history_len = self.history.len();
        self.page = page;
        self.page.start();
        if let LoadKind::Traverse(i) = kind {
            let sc = self.history[i].scroll;
            self.page.host_mut().scroll = sc;
            self.page.host_mut().clamp_scroll();
        }
        if let Some(e) = self.history.get_mut(self.index) {
            e.title = self.page.title();
        }
    }

    pub fn navigate_blocking(&mut self, req: Request, kind: LoadKind) {
        self.pending = None;
        let loaded = fetch_document(req);
        self.install(loaded, kind);
    }

    /// Start a background load; call `poll_load` to finish it.
    pub fn navigate_async(&mut self, req: Request, kind: LoadKind) {
        let url = req.url.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_document(req));
        });
        self.pending = Some(PendingLoad { url, rx, kind });
    }

    /// Returns true if a pending load completed.
    pub fn poll_load(&mut self) -> bool {
        let Some(p) = &self.pending else { return false };
        match p.rx.try_recv() {
            Ok(loaded) => {
                let kind = p.kind;
                self.pending = None;
                self.install(loaded, kind);
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(_) => {
                self.pending = None;
                false
            }
        }
    }

    pub fn wait_load(&mut self) {
        if let Some(p) = self.pending.take() {
            if let Ok(loaded) = p.rx.recv() {
                self.install(loaded, p.kind);
            }
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.history.len()
    }

    /// Process a navigation request from the page. `blocking` loads synchronously.
    pub fn handle_nav(&mut self, nav: NavRequest, blocking: bool) {
        let go = |t: &mut Tab, req: Request, kind: LoadKind| {
            if blocking {
                t.navigate_blocking(req, kind)
            } else {
                t.navigate_async(req, kind)
            }
        };
        match nav {
            NavRequest::Load { url, method, body, replace, headers } => {
                let req = Request { method, url, headers, body };
                go(self, req, if replace { LoadKind::Replace } else { LoadKind::Push });
            }
            NavRequest::Reload => {
                let u = self.url();
                go(self, Request::get(u), LoadKind::Replace);
            }
            NavRequest::History(d) => {
                let target = self.index as i64 + d as i64;
                if target >= 0 && (target as usize) < self.history.len() {
                    let t = target as usize;
                    let url = self.history[t].url.clone();
                    if url.without_fragment() == self.url().without_fragment() && url != self.url() {
                        self.save_scroll();
                        self.index = t;
                        self.page.host_mut().url = url.clone();
                        if let Some(f) = url.fragment.clone() {
                            self.page.scroll_to_anchor(&f);
                        }
                        crate::bindings::events::dispatch(&mut self.page, crate::page::Target::Window, "popstate", Default::default());
                        self.page.after_task();
                    } else {
                        go(self, Request::get(url), LoadKind::Traverse(t));
                    }
                }
            }
            NavRequest::SameDocument { url, replace } => {
                self.save_scroll();
                if replace {
                    if let Some(e) = self.history.get_mut(self.index) {
                        e.url = url;
                    }
                } else {
                    self.history.truncate(self.index + 1);
                    self.history.push(HistoryEntry { url, title: self.page.title(), scroll: (0.0, 0.0) });
                    self.index = self.history.len() - 1;
                }
                self.page.host_mut().history_len = self.history.len();
            }
            NavRequest::Open(url) => self.open_requests.push(url),
        }
    }

    /// Drain navigation requested by the page.
    pub fn process_navigation(&mut self, blocking: bool) -> bool {
        let mut any = false;
        for _ in 0..8 {
            let Some(nav) = self.page.take_navigation() else { break };
            any = true;
            self.handle_nav(nav, blocking);
        }
        any
    }

    pub fn go(&mut self, url: Url, blocking: bool) {
        self.handle_nav(NavRequest::Load { url, method: "GET".into(), body: None, replace: false, headers: vec![] }, blocking);
    }

    pub fn back(&mut self, blocking: bool) {
        self.handle_nav(NavRequest::History(-1), blocking);
    }

    pub fn forward(&mut self, blocking: bool) {
        self.handle_nav(NavRequest::History(1), blocking);
    }

    pub fn reload(&mut self, blocking: bool) {
        self.handle_nav(NavRequest::Reload, blocking);
    }
}
