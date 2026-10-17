//! Headless test automation: run JavaScript test files that drive a page.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::bindings::events::Mods;
use crate::dom::NodeId;
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;
use crate::net::{Request, Url};
use crate::page::{LoadOptions, Page};
use crate::raster::Canvas;
use crate::tab::{LoadKind, Tab};

/// Render the page viewport (or the whole document) to a canvas.
pub fn screenshot(page: &mut Page, full_page: bool, scale: f32) -> Canvas {
    let (vw, vh) = page.host().viewport;
    let saved = page.host().scroll;
    let (w, h) = if full_page {
        let (_dw, dh) = page.host_mut().doc_size();
        (vw, dh.min(20000.0).max(vh))
    } else {
        (vw, vh)
    };
    let _ = w;
    // painting requests CSS background images; give them a moment to arrive
    let mut dl = page.host_mut().display_list();
    let deadline = Instant::now() + Duration::from_millis(3000);
    if page.host().images_pending() {
        while page.host().images_pending() && Instant::now() < deadline {
            if !page.host_mut().poll_images() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        dl = page.host_mut().display_list();
    }
    let mut canvas = Canvas::new((vw * scale).round() as usize, (h * scale).round() as usize);
    let scroll = if full_page { (0.0, 0.0) } else { saved };
    crate::paint::render(&dl, &mut canvas, &crate::paint::Viewport { ox: 0.0, oy: 0.0, w: vw * scale, h: h * scale, scale, scroll });
    canvas
}

pub fn save_png(canvas: &Canvas, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
    }
    let png = crate::image::png::encode_rgb(canvas.w as u32, canvas.h as u32, &canvas.to_rgb());
    std::fs::write(path, png).map_err(|e| format!("{}: {e}", path.display()))
}

/// Pump the page's event loop until `done` returns true or the timeout elapses.
pub fn pump_until(tab: &mut Tab, timeout: Duration, mut done: impl FnMut(&mut Tab) -> bool) -> bool {
    let start = Instant::now();
    loop {
        tab.page.poll();
        if tab.page.has_animation_frames() {
            tab.page.run_animation_frames();
        }
        tab.process_navigation(true);
        if done(tab) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// Let pending timers (up to `ms`), fetches and images settle.
pub fn settle(tab: &mut Tab, ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    pump_until(tab, Duration::from_millis(ms), |t| {
        let busy = t.page.has_pending_work() || t.page.has_animation_frames() || t.page.next_timer_due().is_some_and(|d| d <= deadline);
        !busy
    });
}

// ---------------------------------------------------------------- test host

struct TestCase {
    name: String,
    func: Value,
}

pub struct TestHost {
    tab: Tab,
    base: Url,
    file: PathBuf,
    tests: Vec<TestCase>,
    before_each: Vec<Value>,
    after_each: Vec<Value>,
    prefix: Vec<String>,
    screenshot_dir: Option<PathBuf>,
    viewport: (f32, f32),
}

fn th(it: &mut Interp) -> &mut TestHost {
    it.host_mut::<TestHost>().expect("test host")
}

fn find_node(it: &mut Interp, sel: &Value) -> R<NodeId> {
    let s = it.to_str(sel)?.to_string();
    match th(it).tab.page.find(&s) {
        Ok(Some(n)) => Ok(n),
        Ok(None) => throw!(it, "Error", "No element matches selector '{s}'"),
        Err(e) => throw!(it, "SyntaxError", "{e}"),
    }
}

fn wait_for_selector(it: &mut Interp, s: &str, timeout_ms: f64) -> R<NodeId> {
    let sel = s.to_string();
    let mut found = None;
    let ok = pump_until(&mut th(it).tab, Duration::from_millis(timeout_ms as u64), |t| {
        found = t.page.find(&sel).ok().flatten();
        found.is_some()
    });
    if !ok {
        throw!(it, "Error", "Timed out after {timeout_ms}ms waiting for '{s}'");
    }
    Ok(found.unwrap())
}

/// Evaluate code in the page, returning the result transferred via JSON.
fn page_eval(it: &mut Interp, code: &str) -> R<Value> {
    let wrapped = format!("(function(){{ var __r = (function(){{ return eval({}); }})(); return __r; }})()", crate::js::builtins::json::quote(code));
    let page = &mut th(it).tab.page;
    let r = page.interp.eval_script(&wrapped, "page.evaluate");
    page.after_task();
    match r {
        Ok(v) => {
            // promise results: pump until settled
            let v = if let Some((st, _)) = crate::js::builtins::promise::state_of(&v) {
                let _ = st;
                let tab = &mut th(it).tab;
                let pv = v.clone();
                pump_until(tab, Duration::from_secs(10), |t| {
                    t.page.interp.run_microtasks();
                    !matches!(crate::js::builtins::promise::state_of(&pv), Some((PromiseStatus::Pending, _)))
                });
                match crate::js::builtins::promise::state_of(&v) {
                    Some((PromiseStatus::Fulfilled, x)) => x,
                    Some((PromiseStatus::Rejected, e)) => {
                        let msg = th(it).tab.page.interp.describe_error(&e);
                        throw!(it, "Error", "page.evaluate rejected: {msg}");
                    }
                    _ => throw!(it, "Error", "page.evaluate: promise did not settle"),
                }
            } else {
                v
            };
            let page = &mut th(it).tab.page;
            let json = crate::js::builtins::json::stringify(&mut page.interp, &v, &Value::Undefined, "");
            match json {
                Ok(Some(s)) => crate::js::builtins::json::parse(it, &s),
                Ok(None) => Ok(Value::Undefined),
                Err(_) => {
                    let s = page.interp.inspect(&v, 2);
                    Ok(Value::str(&s))
                }
            }
        }
        Err(e) => {
            let msg = th(it).tab.page.interp.describe_error(&e);
            throw!(it, "Error", "page.evaluate failed: {msg}");
        }
    }
}

fn install_page_api(it: &mut Interp) {
    let page = it.new_object();
    it.global.set_hidden("page", Value::Obj(page.clone()));
    it.define_method(&page, "goto", 1, |it, _, args| {
        let u = it.to_str(&arg(args, 0))?.to_string();
        let url = th(it).base.join(&u).or_else(|| crate::net::url::from_user_input(&u));
        let Some(url) = url else { throw!(it, "Error", "Invalid URL '{u}'") };
        let t = th(it);
        t.tab.navigate_blocking(Request::get(url), LoadKind::Push);
        settle(&mut t.tab, 300);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "setContent", 1, |it, _, args| {
        let html = it.to_str(&arg(args, 0))?.to_string();
        let t = th(it);
        let url = t.base.clone();
        let opts = LoadOptions { viewport: t.viewport, scale: 1.0, scripting: true, internal: false };
        t.tab.page = Page::from_html(&html, url, &opts);
        t.tab.page.start();
        settle(&mut t.tab, 300);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "url", 0, |it, _, _| Ok(Value::str(&th(it).tab.url().to_string())));
    it.define_method(&page, "title", 0, |it, _, _| Ok(Value::str(&th(it).tab.page.host().doc.title())));
    it.define_method(&page, "content", 0, |it, _, _| {
        let h = th(it).tab.page.host();
        Ok(Value::str(&h.doc.outer_html(h.doc.document_element().unwrap_or(0))))
    });
    it.define_method(&page, "click", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let t = th(it);
        t.tab.page.click_node(n);
        t.tab.process_navigation(true);
        settle(&mut t.tab, 50);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "dblclick", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let t = th(it);
        t.tab.page.click_node(n);
        t.tab.page.click_node(n);
        settle(&mut t.tab, 50);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "clickAt", 2, |it, _, args| {
        let x = it.to_number(&arg(args, 0))? as f32;
        let y = it.to_number(&arg(args, 1))? as f32;
        let t = th(it);
        let sc = t.tab.page.host().scroll;
        t.tab.page.click_at(x - sc.0, y - sc.1);
        t.tab.process_navigation(true);
        settle(&mut t.tab, 50);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "selection", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let t = th(it);
        let v: Vec<char> = t.tab.page.host().control_value(n).chars().collect();
        let sel = t.tab.page.host().form.get(&n).and_then(|f| f.selection());
        Ok(Value::str(&sel.map(|(a, b)| v[a..b].iter().collect::<String>()).unwrap_or_default()))
    });
    it.define_method(&page, "dropdownOpen", 0, |it, _, _| Ok(Value::Bool(th(it).tab.page.host().select_popup.is_some())));
    it.define_method(&page, "hover", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let t = th(it);
        if let Some(r) = t.tab.page.host_mut().node_rect(n) {
            let sc = t.tab.page.host().scroll;
            t.tab.page.mouse_move(r.x + r.w / 2.0 - sc.0, r.y + r.h / 2.0 - sc.1);
        }
        Ok(Value::Undefined)
    });
    it.define_method(&page, "focus", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let t = th(it);
        crate::input::focus(&mut t.tab.page.interp, Some(n));
        t.tab.page.after_task();
        Ok(Value::Undefined)
    });
    it.define_method(&page, "type", 2, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let text = it.to_str(&arg(args, 1))?.to_string();
        let t = th(it);
        if t.tab.page.host().focus != Some(n) {
            t.tab.page.click_node(n);
        }
        t.tab.page.type_text(&text);
        t.tab.process_navigation(true);
        settle(&mut t.tab, 50);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "fill", 2, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let text = it.to_str(&arg(args, 1))?.to_string();
        let t = th(it);
        crate::input::focus(&mut t.tab.page.interp, Some(n));
        t.tab.page.host_mut().set_control_value(n, text);
        crate::bindings::events::fire(&mut t.tab.page.interp, crate::page::Target::Node(n), "input", "InputEvent", crate::bindings::events::EventInit { bubbles: true, ..Default::default() });
        t.tab.page.after_task();
        Ok(Value::Undefined)
    });
    it.define_method(&page, "press", 2, |it, _, args| {
        let (n, key) = if args.len() >= 2 { (Some(find_node(it, &arg(args, 0))?), it.to_str(&arg(args, 1))?.to_string()) } else { (None, it.to_str(&arg(args, 0))?.to_string()) };
        let t = th(it);
        if let Some(n) = n {
            if t.tab.page.host().focus != Some(n) {
                crate::input::focus(&mut t.tab.page.interp, Some(n));
            }
        }
        let mut mods = Mods::default();
        let mut k = key.as_str();
        while let Some((m, rest)) = k.split_once('+') {
            match m {
                "Control" | "Ctrl" => mods.ctrl = true,
                "Shift" => mods.shift = true,
                "Alt" => mods.alt = true,
                "Meta" | "Cmd" => mods.meta = true,
                _ => {}
            }
            k = rest;
        }
        let text = if k.chars().count() == 1 { Some(k.to_string()) } else { None };
        t.tab.page.key_down(k, text.as_deref(), mods);
        t.tab.process_navigation(true);
        settle(&mut t.tab, 50);
        Ok(Value::Undefined)
    });
    for (name, want) in [("check", true), ("uncheck", false)] {
        it.define_method(&page, name, 1, move |it, _, args| {
            let n = find_node(it, &arg(args, 0))?;
            let t = th(it);
            if t.tab.page.host().is_checked(n) != want {
                t.tab.page.click_node(n);
            }
            Ok(Value::Undefined)
        });
    }
    it.define_method(&page, "select", 2, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let v = it.to_str(&arg(args, 1))?.to_string();
        let t = th(it);
        t.tab.page.host_mut().set_control_value(n, v);
        crate::bindings::events::fire(&mut t.tab.page.interp, crate::page::Target::Node(n), "change", "Event", crate::bindings::events::EventInit { bubbles: true, ..Default::default() });
        t.tab.page.after_task();
        Ok(Value::Undefined)
    });
    it.define_method(&page, "text", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        Ok(Value::str(&th(it).tab.page.text_of(n)))
    });
    it.define_method(&page, "html", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        Ok(Value::str(&th(it).tab.page.host().doc.inner_html(n)))
    });
    it.define_method(&page, "attr", 2, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let a = it.to_str(&arg(args, 1))?.to_string();
        Ok(th(it).tab.page.host().doc.attr(n, &a).map(Value::str).unwrap_or(Value::Null))
    });
    it.define_method(&page, "value", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        Ok(Value::str(&th(it).tab.page.host().control_value(n)))
    });
    it.define_method(&page, "isChecked", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        Ok(Value::Bool(th(it).tab.page.host().is_checked(n)))
    });
    it.define_method(&page, "exists", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?.to_string();
        Ok(Value::Bool(th(it).tab.page.find(&s).ok().flatten().is_some()))
    });
    it.define_method(&page, "count", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?.to_string();
        Ok(Value::Num(th(it).tab.page.find_all(&s).map(|v| v.len()).unwrap_or(0) as f64))
    });
    it.define_method(&page, "visible", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?.to_string();
        let t = th(it);
        let Some(n) = t.tab.page.find(&s).ok().flatten() else { return Ok(Value::Bool(false)) };
        let r = t.tab.page.host_mut().node_rect(n);
        Ok(Value::Bool(r.is_some_and(|r| r.w > 0.0 && r.h > 0.0)))
    });
    it.define_method(&page, "box", 1, |it, _, args| {
        let n = find_node(it, &arg(args, 0))?;
        let r = th(it).tab.page.host_mut().node_rect(n);
        let o = it.new_object();
        if let Some(r) = r {
            o.set_prop("x", Value::Num(r.x as f64));
            o.set_prop("y", Value::Num(r.y as f64));
            o.set_prop("width", Value::Num(r.w as f64));
            o.set_prop("height", Value::Num(r.h as f64));
        }
        Ok(Value::Obj(o))
    });
    it.define_method(&page, "evaluate", 1, |it, _, args| {
        let a = arg(args, 0);
        let code = if a.is_callable() {
            let src = it.to_str(&a)?.to_string();
            let mut call_args = Vec::new();
            for x in &args[1..] {
                let j = crate::js::builtins::json::stringify(it, x, &Value::Undefined, "")?.unwrap_or_else(|| "undefined".into());
                call_args.push(j);
            }
            format!("({src})({})", call_args.join(","))
        } else {
            it.to_str(&a)?.to_string()
        };
        page_eval(it, &code)
    });
    it.define_method(&page, "waitFor", 2, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?.to_string();
        let timeout = if arg(args, 1).is_undefined() { 5000.0 } else { it.to_number(&arg(args, 1))? };
        wait_for_selector(it, &s, timeout)?;
        Ok(Value::Undefined)
    });
    it.define_method(&page, "waitForText", 3, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?.to_string();
        let want = it.to_str(&arg(args, 1))?.to_string();
        let timeout = if arg(args, 2).is_undefined() { 5000.0 } else { it.to_number(&arg(args, 2))? };
        let ok = pump_until(&mut th(it).tab, Duration::from_millis(timeout as u64), |t| t.page.find(&s).ok().flatten().is_some_and(|n| t.page.text_of(n).contains(&want)));
        if !ok {
            throw!(it, "Error", "Timed out after {timeout}ms waiting for '{s}' to contain {want:?}");
        }
        Ok(Value::Undefined)
    });
    it.define_method(&page, "wait", 1, |it, _, args| {
        let ms = it.to_number(&arg(args, 0))?.max(0.0) as u64;
        pump_until(&mut th(it).tab, Duration::from_millis(ms), |_| false);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "screenshot", 1, |it, _, args| {
        let a = arg(args, 0);
        let (path, full) = match &a {
            Value::Obj(_) => {
                let p = it.get_str(&a, "path")?;
                let f = it.get_str(&a, "fullPage")?.truthy();
                (it.to_str(&p)?.to_string(), f)
            }
            v => (it.to_str(v)?.to_string(), false),
        };
        let t = th(it);
        let p = Path::new(&path);
        let p = if p.is_absolute() {
            p.to_path_buf()
        } else {
            match &t.screenshot_dir {
                Some(d) => d.join(p),
                None => t.file.parent().unwrap_or(Path::new(".")).join(p),
            }
        };
        let c = screenshot(&mut t.tab.page, full, 1.0);
        if let Err(e) = save_png(&c, &p) {
            throw!(it, "Error", "screenshot failed: {e}");
        }
        Ok(Value::str(&p.to_string_lossy()))
    });
    it.define_method(&page, "console", 0, |it, _, _| {
        let entries = th(it).tab.page.host().console.clone();
        let items: Vec<Value> = entries
            .into_iter()
            .map(|e| {
                let o = it.new_object();
                o.set_prop("level", Value::str(e.level));
                o.set_prop("text", Value::str(&e.text));
                Value::Obj(o)
            })
            .collect();
        Ok(it.new_array(items))
    });
    it.define_method(&page, "alerts", 0, |it, _, _| {
        let a: Vec<Value> = th(it).tab.page.host().alerts.iter().map(|s| Value::str(s)).collect();
        Ok(it.new_array(a))
    });
    it.define_method(&page, "setViewport", 2, |it, _, args| {
        let w = it.to_number(&arg(args, 0))? as f32;
        let h = it.to_number(&arg(args, 1))? as f32;
        let t = th(it);
        t.viewport = (w, h);
        t.tab.page.set_viewport(w, h, 1.0);
        t.tab.opts.viewport = (w, h);
        Ok(Value::Undefined)
    });
    it.define_method(&page, "scroll", 2, |it, _, args| {
        let x = it.to_number(&arg(args, 0))? as f32;
        let y = it.to_number(&arg(args, 1))? as f32;
        let t = th(it);
        t.tab.page.host_mut().scroll = (x, y);
        t.tab.page.host_mut().clamp_scroll();
        Ok(Value::Undefined)
    });
    for name in ["back", "forward", "reload"] {
        it.define_method(&page, name, 0, move |it, _, _| {
            let t = th(it);
            match name {
                "back" => t.tab.back(true),
                "forward" => t.tab.forward(true),
                _ => t.tab.reload(true),
            }
            settle(&mut t.tab, 200);
            Ok(Value::Undefined)
        });
    }
}

/// Resolve a path relative to the running test file.
fn test_path(it: &mut Interp, path: &str) -> std::path::PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        th(it).file.parent().unwrap_or(Path::new(".")).join(p)
    }
}

fn install_test_api(it: &mut Interp) {
    let g = it.global.clone();
    it.define_method(&g, "writeFile", 2, |it, _, args| {
        let path = it.to_str(&arg(args, 0))?.to_string();
        let text = it.to_str(&arg(args, 1))?.to_string();
        let p = test_path(it, &path);
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&p, text) {
            throw!(it, "Error", "writeFile {}: {e}", p.display());
        }
        Ok(Value::str(&p.to_string_lossy()))
    });
    it.define_method(&g, "readFile", 1, |it, _, args| {
        let path = it.to_str(&arg(args, 0))?.to_string();
        let p = test_path(it, &path);
        match std::fs::read_to_string(&p) {
            Ok(s) => Ok(Value::str(&s)),
            Err(e) => throw!(it, "Error", "readFile {}: {e}", p.display()),
        }
    });
    it.define_method(&g, "test", 2, |it, _, args| {
        let name = it.to_str(&arg(args, 0))?.to_string();
        let t = th(it);
        let full = if t.prefix.is_empty() { name } else { format!("{} › {name}", t.prefix.join(" › ")) };
        t.tests.push(TestCase { name: full, func: arg(args, 1) });
        Ok(Value::Undefined)
    });
    let tf = g.get_own("test").unwrap();
    g.set_hidden("it", tf);
    it.define_method(&g, "describe", 2, |it, _, args| {
        let name = it.to_str(&arg(args, 0))?.to_string();
        th(it).prefix.push(name);
        let r = it.call(&arg(args, 1), Value::Undefined, &[]);
        th(it).prefix.pop();
        r.map(|_| Value::Undefined)
    });
    it.define_method(&g, "beforeEach", 1, |it, _, args| {
        th(it).before_each.push(arg(args, 0));
        Ok(Value::Undefined)
    });
    it.define_method(&g, "afterEach", 1, |it, _, args| {
        th(it).after_each.push(arg(args, 0));
        Ok(Value::Undefined)
    });
    // expect() is implemented in JS on top of a deep-equality helper.
    it.define_method(&g, "__deepEqual", 2, |it, _, args| Ok(Value::Bool(deep_equal(it, &arg(args, 0), &arg(args, 1), 0))));
    let src = r#"
    function __fmt(v) {
      try { if (typeof v === 'string') return JSON.stringify(v); const s = JSON.stringify(v); return s === undefined ? String(v) : s; } catch (e) { return String(v); }
    }
    function expect(actual) {
      const make = (negate) => {
        const check = (pass, msg) => {
          if (negate ? pass : !pass) throw new Error((negate ? 'Expected not: ' : '') + msg);
        };
        const m = {
          toBe(exp) { check(Object.is(actual, exp), `expected ${__fmt(exp)}, received ${__fmt(actual)}`); },
          toEqual(exp) { check(__deepEqual(actual, exp), `expected ${__fmt(exp)}, received ${__fmt(actual)}`); },
          toStrictEqual(exp) { check(__deepEqual(actual, exp), `expected ${__fmt(exp)}, received ${__fmt(actual)}`); },
          toBeTruthy() { check(!!actual, `expected truthy, received ${__fmt(actual)}`); },
          toBeFalsy() { check(!actual, `expected falsy, received ${__fmt(actual)}`); },
          toBeNull() { check(actual === null, `expected null, received ${__fmt(actual)}`); },
          toBeUndefined() { check(actual === undefined, `expected undefined, received ${__fmt(actual)}`); },
          toBeDefined() { check(actual !== undefined, `expected defined value`); },
          toBeNaN() { check(Number.isNaN(actual), `expected NaN, received ${__fmt(actual)}`); },
          toBeGreaterThan(n) { check(actual > n, `expected > ${n}, received ${__fmt(actual)}`); },
          toBeGreaterThanOrEqual(n) { check(actual >= n, `expected >= ${n}, received ${__fmt(actual)}`); },
          toBeLessThan(n) { check(actual < n, `expected < ${n}, received ${__fmt(actual)}`); },
          toBeLessThanOrEqual(n) { check(actual <= n, `expected <= ${n}, received ${__fmt(actual)}`); },
          toBeCloseTo(n, d = 2) { check(Math.abs(actual - n) < Math.pow(10, -d) / 2, `expected ≈ ${n}, received ${__fmt(actual)}`); },
          toContain(x) { check(actual != null && actual.includes(x), `expected ${__fmt(actual)} to contain ${__fmt(x)}`); },
          toMatch(re) { const r = typeof re === 'string' ? new RegExp(re) : re; check(r.test(String(actual)), `expected ${__fmt(actual)} to match ${r}`); },
          toHaveLength(n) { check(actual != null && actual.length === n, `expected length ${n}, received ${actual == null ? actual : actual.length}`); },
          toHaveProperty(k, v) { const has = actual != null && k in Object(actual); check(has && (arguments.length < 2 || __deepEqual(actual[k], v)), `expected property ${__fmt(k)}${arguments.length > 1 ? ' = ' + __fmt(v) : ''}`); },
          toBeInstanceOf(c) { check(actual instanceof c, `expected instance of ${c && c.name}`); },
          toThrow(msg) {
            let threw = false, err;
            try { actual(); } catch (e) { threw = true; err = e; }
            const ok = threw && (msg === undefined || String(err && err.message || err).includes(msg));
            check(ok, threw ? `expected error containing ${__fmt(msg)}, got ${__fmt(String(err && err.message))}` : 'expected function to throw');
          },
        };
        return m;
      };
      const r = make(false);
      r.not = make(true);
      return r;
    }
    "#;
    if let Err(e) = it.eval_script(src, "expect") {
        let msg = it.describe_error(&e);
        eprintln!("internal: failed to install expect(): {msg}");
    }
}

fn deep_equal(it: &mut Interp, a: &Value, b: &Value, depth: u32) -> bool {
    if depth > 50 {
        return false;
    }
    match (a, b) {
        (Value::Obj(x), Value::Obj(y)) => {
            if x.ptr_eq(y) {
                return true;
            }
            if x.is_array() != y.is_array() {
                return false;
            }
            let ka = it.own_keys(x, false);
            let kb = it.own_keys(y, false);
            if ka.len() != kb.len() {
                return false;
            }
            for k in ka {
                if !kb.contains(&k) {
                    return false;
                }
                let va = it.get(a, &k).unwrap_or(Value::Undefined);
                let vb = it.get(b, &k).unwrap_or(Value::Undefined);
                if !deep_equal(it, &va, &vb, depth + 1) {
                    return false;
                }
            }
            true
        }
        (Value::Num(x), Value::Num(y)) => x == y || (x.is_nan() && y.is_nan()),
        _ => crate::js::interp::strict_equals(a, b),
    }
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub file: String,
    pub name: String,
    pub passed: bool,
    pub error: Option<String>,
    pub ms: u128,
}

fn await_result(it: &mut Interp, v: Value, timeout: Duration) -> Result<(), String> {
    let Some(_) = crate::js::builtins::promise::state_of(&v) else {
        it.run_microtasks();
        return Ok(());
    };
    let start = Instant::now();
    loop {
        it.run_microtasks();
        match crate::js::builtins::promise::state_of(&v) {
            Some((PromiseStatus::Fulfilled, _)) => return Ok(()),
            Some((PromiseStatus::Rejected, e)) => {
                crate::js::builtins::promise::mark_handled(&v);
                return Err(it.describe_error(&e));
            }
            _ => {}
        }
        if start.elapsed() > timeout {
            return Err(format!("Test timed out after {}ms", timeout.as_millis()));
        }
        // keep the page alive while the test awaits
        let t = th(it);
        t.tab.page.poll();
        t.tab.process_navigation(true);
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Run one test file; returns results.
pub fn run_file(path: &Path, screenshot_dir: Option<PathBuf>, verbose: bool) -> Vec<TestResult> {
    let file_label = path.to_string_lossy().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return vec![TestResult { file: file_label.clone(), name: "(load)".into(), passed: false, error: Some(e.to_string()), ms: 0 }],
    };
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let base = Url::from_file_path(&abs.to_string_lossy());
    let viewport = (1024.0, 768.0);
    let opts = LoadOptions { viewport, scale: 1.0, scripting: true, internal: false };
    let host = TestHost { tab: Tab::blank(opts), base, file: abs.clone(), tests: vec![], before_each: vec![], after_each: vec![], prefix: vec![], screenshot_dir, viewport };
    let mut it = Interp::new();
    it.host = Some(Box::new(host));
    install_page_api(&mut it);
    install_test_api(&mut it);
    if let Err(e) = it.eval_script(&src, &file_label) {
        let msg = it.describe_error(&e);
        let line = it.cur_line + 1;
        return vec![TestResult { file: file_label, name: "(file)".into(), passed: false, error: Some(format!("{msg} (line {line})")), ms: 0 }];
    }
    it.run_microtasks();
    let tests: Vec<(String, Value)> = th(&mut it).tests.drain(..).map(|t| (t.name, t.func)).collect();
    let mut results = Vec::new();
    for (name, f) in tests {
        let t0 = Instant::now();
        let mut err = None;
        let hooks: Vec<Value> = th(&mut it).before_each.clone();
        for h in hooks {
            if let Err(e) = it.call(&h, Value::Undefined, &[]).map_err(|e| it.describe_error(&e)).and_then(|v| await_result(&mut it, v, Duration::from_secs(30))) {
                err = Some(format!("beforeEach: {e}"));
                break;
            }
        }
        if err.is_none() {
            it.deadline = Some(Instant::now() + Duration::from_secs(60));
            let r = it.call(&f, Value::Undefined, &[]);
            it.deadline = None;
            err = match r {
                Ok(v) => await_result(&mut it, v, Duration::from_secs(30)).err(),
                Err(e) => {
                    let line = it.cur_line + 1;
                    Some(format!("{} (line {line})", it.describe_error(&e)))
                }
            };
        }
        let hooks: Vec<Value> = th(&mut it).after_each.clone();
        for h in hooks {
            let _ = it.call(&h, Value::Undefined, &[]).map(|v| await_result(&mut it, v, Duration::from_secs(30)));
        }
        let ms = t0.elapsed().as_millis();
        if verbose {
            for c in th(&mut it).tab.page.host().console.iter() {
                eprintln!("      [console.{}] {}", c.level, c.text);
            }
        }
        results.push(TestResult { file: file_label.clone(), name, passed: err.is_none(), error: err, ms });
    }
    results
}

pub fn collect_test_files(inputs: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for i in inputs {
        let p = PathBuf::from(i);
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&p).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
            entries.sort();
            for e in entries {
                let name = e.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                if e.is_dir() {
                    out.extend(collect_test_files(&[e.to_string_lossy().to_string()]));
                } else if name.ends_with(".test.js") || name.ends_with(".spec.js") {
                    out.push(e);
                }
            }
        } else {
            out.push(p);
        }
    }
    out
}

pub fn junit_xml(results: &[TestResult], secs: f64) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let failures = results.iter().filter(|r| !r.passed).count();
    let mut out = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites tests=\"{}\" failures=\"{}\" time=\"{:.3}\">\n", results.len(), failures, secs);
    let mut files: Vec<&str> = results.iter().map(|r| r.file.as_str()).collect();
    files.dedup();
    for f in files {
        let rs: Vec<&TestResult> = results.iter().filter(|r| r.file == f).collect();
        let fails = rs.iter().filter(|r| !r.passed).count();
        out.push_str(&format!("  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\">\n", esc(f), rs.len(), fails));
        for r in rs {
            out.push_str(&format!("    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"", esc(f), esc(&r.name), r.ms as f64 / 1000.0));
            match &r.error {
                Some(e) => out.push_str(&format!(">\n      <failure message=\"{}\"/>\n    </testcase>\n", esc(e))),
                None => out.push_str("/>\n"),
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

/// `tip test` entry point. Returns the process exit code.
pub fn run_cli(files: &[String], junit: Option<&str>, screenshots: Option<&str>, verbose: bool) -> i32 {
    // SAFETY: set before the runner spawns any threads.
    unsafe { std::env::set_var("TIP_AUTOMATION", "1") };
    let paths = collect_test_files(files);
    if paths.is_empty() {
        eprintln!("No test files found (looking for *.test.js / *.spec.js)");
        return 2;
    }
    let t0 = Instant::now();
    let mut all = Vec::new();
    for p in &paths {
        println!("\n {}", p.display());
        let results = run_file(p, screenshots.map(PathBuf::from), verbose);
        for r in &results {
            if r.passed {
                println!("   \x1b[32m✓\x1b[0m {} \x1b[2m({}ms)\x1b[0m", r.name, r.ms);
            } else {
                println!("   \x1b[31m✗ {}\x1b[0m \x1b[2m({}ms)\x1b[0m", r.name, r.ms);
                if let Some(e) = &r.error {
                    for line in e.lines() {
                        println!("       \x1b[31m{line}\x1b[0m");
                    }
                }
            }
        }
        all.extend(results);
    }
    let secs = t0.elapsed().as_secs_f64();
    let passed = all.iter().filter(|r| r.passed).count();
    let failed = all.len() - passed;
    println!(
        "\n {} tests, \x1b[32m{passed} passed\x1b[0m, {}{failed} failed\x1b[0m ({secs:.2}s)",
        all.len(),
        if failed > 0 { "\x1b[31m" } else { "" }
    );
    if let Some(j) = junit {
        if let Err(e) = std::fs::write(j, junit_xml(&all, secs)) {
            eprintln!("could not write {j}: {e}");
        }
    }
    if failed > 0 { 1 } else { 0 }
}

