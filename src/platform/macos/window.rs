//! macOS window: an NSWindow with a custom NSView that blits our canvas.

use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::time::Instant;

use super::objc::*;
use crate::app::App;
use crate::bindings::events::Mods;
use crate::style::Cursor;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NSPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NSSize {
    pub w: f64,
    pub h: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct NSRect {
    pub origin: NSPoint,
    pub size: NSSize,
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
    fn CGColorSpaceRelease(cs: *mut c_void);
    fn CGDataProviderCreateWithData(info: *mut c_void, data: *const c_void, size: usize, release: *const c_void) -> *mut c_void;
    fn CGDataProviderRelease(p: *mut c_void);
    fn CGImageCreate(w: usize, h: usize, bpc: usize, bpp: usize, bpr: usize, cs: *mut c_void, info: u32, provider: *mut c_void, decode: *const f64, interpolate: bool, intent: i32) -> *mut c_void;
    fn CGImageRelease(i: *mut c_void);
    fn CGContextDrawImage(ctx: *mut c_void, rect: NSRect, image: *mut c_void);
    fn CGContextSaveGState(ctx: *mut c_void);
    fn CGContextRestoreGState(ctx: *mut c_void);
    fn CGContextTranslateCTM(ctx: *mut c_void, tx: f64, ty: f64);
    fn CGContextScaleCTM(ctx: *mut c_void, sx: f64, sy: f64);
    fn CGContextSetInterpolationQuality(ctx: *mut c_void, q: i32);
}

#[cfg(target_arch = "x86_64")]
#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn objc_msgSend_stret();
}

/// Messages returning NSRect (struct return needs _stret on x86_64).
unsafe fn msg_rect(obj: Id, s: &str) -> NSRect {
    #[cfg(target_arch = "x86_64")]
    {
        let f: unsafe extern "C" fn(*mut NSRect, Id, Sel) = unsafe { std::mem::transmute(objc_msgSend_stret as *const ()) };
        let mut r = NSRect::default();
        unsafe { f(&mut r, obj, sel(s)) };
        r
    }
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        msg0(obj, s)
    }
}

struct State {
    app: App,
    view: Id,
    window: Id,
    cursor: Cursor,
    last_frame: Instant,
    start: Instant,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn mods_of(event: Id) -> Mods {
    let flags: usize = unsafe { msg0(event, "modifierFlags") };
    Mods { shift: flags & (1 << 17) != 0, ctrl: flags & (1 << 18) != 0, alt: flags & (1 << 19) != 0, meta: flags & (1 << 20) != 0 }
}

fn location(view: Id, event: Id) -> (f32, f32) {
    unsafe {
        let p: NSPoint = msg0(event, "locationInWindow");
        let p: NSPoint = msg2(view, "convertPoint:fromView:", p, NIL);
        (p.x as f32, p.y as f32)
    }
}

fn request_redraw(view: Id) {
    unsafe {
        let _: () = msg1(view, "setNeedsDisplay:", true);
    }
}

fn apply_cursor(c: Cursor) {
    let name = match c {
        Cursor::Pointer => "pointingHandCursor",
        Cursor::Text => "IBeamCursor",
        Cursor::Move | Cursor::Grab => "openHandCursor",
        Cursor::Crosshair => "crosshairCursor",
        Cursor::ResizeEw => "resizeLeftRightCursor",
        Cursor::ResizeNs => "resizeUpDownCursor",
        Cursor::NotAllowed => "operationNotAllowedCursor",
        Cursor::Default => "arrowCursor",
    };
    unsafe {
        let cur: Id = msg0(class("NSCursor"), name);
        let _: () = msg0(cur, "set");
    }
}

fn after_input(st: &mut State) {
    st.app.tick();
    request_redraw(st.view);
    let c = st.app.cursor();
    if c != st.cursor {
        st.cursor = c;
        apply_cursor(c);
    }
}

// ---------------------------------------------------------------- view methods

extern "C" fn draw_rect(this: Id, _: Sel, _dirty: NSRect) {
    with_state(|st| unsafe {
        let bounds = msg_rect(this, "bounds");
        let scale: f64 = msg0(st.window, "backingScaleFactor");
        let (w, h, s) = (bounds.size.w as f32, bounds.size.h as f32, scale as f32);
        if (w - st.app.width).abs() > 0.5 || (h - st.app.height).abs() > 0.5 || (s - st.app.scale).abs() > 0.01 {
            st.app.resize(w, h, s);
            st.app.tick();
        }
        let canvas = st.app.render();
        let ctx_obj: Id = msg0(class("NSGraphicsContext"), "currentContext");
        if ctx_obj.is_null() {
            return;
        }
        let cg: *mut c_void = msg0(ctx_obj, "CGContext");
        let cs = CGColorSpaceCreateDeviceRGB();
        let provider = CGDataProviderCreateWithData(std::ptr::null_mut(), canvas.px.as_ptr() as *const c_void, canvas.px.len() * 4, std::ptr::null());
        // 0x00RRGGBB words in little-endian memory: kCGImageAlphaNoneSkipFirst | kCGBitmapByteOrder32Little
        let image = CGImageCreate(canvas.w, canvas.h, 8, 32, canvas.w * 4, cs, 6 | (2 << 12), provider, std::ptr::null(), false, 0);
        CGContextSaveGState(cg);
        CGContextTranslateCTM(cg, 0.0, bounds.size.h);
        CGContextScaleCTM(cg, 1.0, -1.0);
        CGContextSetInterpolationQuality(cg, 1);
        CGContextDrawImage(cg, NSRect { origin: NSPoint { x: 0.0, y: 0.0 }, size: NSSize { w: canvas.w as f64 / scale, h: canvas.h as f64 / scale } }, image);
        CGContextRestoreGState(cg);
        CGImageRelease(image);
        CGDataProviderRelease(provider);
        CGColorSpaceRelease(cs);
        st.last_frame = Instant::now();
    });
}

extern "C" fn yes(_: Id, _: Sel) -> bool {
    true
}

extern "C" fn yes_ev(_: Id, _: Sel, _: Id) -> bool {
    true
}

fn button_of(event: Id) -> i32 {
    let n: isize = unsafe { msg0(event, "buttonNumber") };
    match n {
        0 => 0,
        1 => 2,
        _ => 1,
    }
}

/// Title-bar behaviour for our custom tab strip: drag moves the window and a
/// double-click follows the user's system setting (zoom by default).
fn titlebar_action(window: Id, event: Id) {
    unsafe {
        let clicks: isize = msg0(event, "clickCount");
        if clicks >= 2 {
            let defaults: Id = msg0(class("NSUserDefaults"), "standardUserDefaults");
            let pref = from_nsstring(msg1(defaults, "stringForKey:", nsstring("AppleActionOnDoubleClick")));
            match pref.as_str() {
                "Minimize" => {
                    let _: () = msg1(window, "performMiniaturize:", NIL);
                }
                "None" => {}
                _ => {
                    let _: () = msg1(window, "performZoom:", NIL);
                }
            }
        } else {
            let _: () = msg1(window, "performWindowDragWithEvent:", event);
        }
    }
}

extern "C" fn mouse_down(this: Id, _: Sel, event: Id) {
    let (x, y) = location(this, event);
    let mut m = mods_of(event);
    let mut b = button_of(event);
    if b == 0 && !m.ctrl {
        let titlebar = with_state(|st| st.app.is_titlebar(x, y).then_some(st.window)).flatten();
        if let Some(window) = titlebar {
            titlebar_action(window, event);
            return;
        }
    }
    if b == 0 && m.ctrl {
        b = 2;
        m.ctrl = false;
    }
    with_state(|st| {
        st.app.mouse_down(x, y, b, m);
        after_input(st);
    });
}

extern "C" fn mouse_up(this: Id, _: Sel, event: Id) {
    let (x, y) = location(this, event);
    let mut m = mods_of(event);
    let mut b = button_of(event);
    if b == 0 && m.ctrl {
        b = 2;
        m.ctrl = false;
    }
    with_state(|st| {
        st.app.mouse_up(x, y, b, m);
        after_input(st);
        if st.app.quit {
            terminate();
        }
    });
}

extern "C" fn mouse_moved(this: Id, _: Sel, event: Id) {
    let (x, y) = location(this, event);
    with_state(|st| {
        st.app.mouse_move(x, y);
        after_input(st);
    });
}

extern "C" fn scroll_wheel(this: Id, _: Sel, event: Id) {
    let (x, y) = location(this, event);
    let (dx, dy, precise): (f64, f64, bool) = unsafe { (msg0(event, "scrollingDeltaX"), msg0(event, "scrollingDeltaY"), msg0(event, "hasPreciseScrollingDeltas")) };
    let k = if precise { 1.0 } else { 40.0 };
    with_state(|st| {
        st.app.wheel(x, y, (-dx * k) as f32, (-dy * k) as f32);
        after_input(st);
    });
}

fn key_name(code: u16, chars: &str) -> String {
    match code {
        36 | 76 => "Enter".into(),
        48 => "Tab".into(),
        49 => " ".into(),
        51 => "Backspace".into(),
        53 => "Escape".into(),
        117 => "Delete".into(),
        123 => "ArrowLeft".into(),
        124 => "ArrowRight".into(),
        125 => "ArrowDown".into(),
        126 => "ArrowUp".into(),
        115 => "Home".into(),
        119 => "End".into(),
        116 => "PageUp".into(),
        121 => "PageDown".into(),
        122 => "F1".into(),
        120 => "F2".into(),
        99 => "F3".into(),
        118 => "F4".into(),
        96 => "F5".into(),
        97 => "F6".into(),
        98 => "F7".into(),
        100 => "F8".into(),
        101 => "F9".into(),
        109 => "F10".into(),
        103 => "F11".into(),
        111 => "F12".into(),
        _ => chars.chars().next().map(|c| c.to_string()).unwrap_or_default(),
    }
}

fn handle_key(event: Id) -> bool {
    let code: u16 = unsafe { msg0(event, "keyCode") };
    let chars = unsafe { from_nsstring(msg0(event, "characters")) };
    let raw = unsafe { from_nsstring(msg0(event, "charactersIgnoringModifiers")) };
    let m = mods_of(event);
    let name = key_name(code, if m.meta || m.ctrl { &raw } else { &chars });
    let printable = chars.chars().next().is_some_and(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c));
    let text = if printable && !matches!(name.as_str(), "Enter" | "Tab" | "Backspace" | "Delete" | "Escape") { Some(chars.clone()) } else { None };
    with_state(|st| {
        let consumed = st.app.key_down(&name, text.as_deref(), m);
        after_input(st);
        if st.app.quit {
            terminate();
        }
        consumed || !(m.meta)
    })
    .unwrap_or(false)
}

extern "C" fn key_down(_this: Id, _: Sel, event: Id) {
    handle_key(event);
}

extern "C" fn perform_key_equivalent(_this: Id, _: Sel, event: Id) -> bool {
    let m = mods_of(event);
    let ty: usize = unsafe { msg0(event, "type") };
    if (m.meta || m.ctrl) && ty == 10 {
        let code: u16 = unsafe { msg0(event, "keyCode") };
        let raw = unsafe { from_nsstring(msg0(event, "charactersIgnoringModifiers")) };
        let _ = code;
        // let the menu handle Cmd+Q / Cmd+H / Cmd+M
        if m.meta && matches!(raw.as_str(), "q" | "h" | "m") {
            return false;
        }
        handle_key(event);
        return true;
    }
    false
}

extern "C" fn tick(this: Id, _: Sel, _timer: Id) {
    with_state(|st| {
        let changed = st.app.tick();
        // self-test hook: TIP_SNAPSHOT=path.png saves a frame and quits
        if let Ok(path) = std::env::var("TIP_SNAPSHOT") {
            let wait: u64 = std::env::var("TIP_SNAPSHOT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
            if st.start.elapsed().as_millis() as u64 > wait && !st.app.busy() {
                let c = st.app.render();
                let _ = crate::automation::save_png(c, std::path::Path::new(&path));
                st.app.quit = true;
            }
        }
        if changed {
            request_redraw(this);
        }
        let title = st.app.title.clone();
        unsafe {
            let cur: Id = msg0(st.window, "title");
            if from_nsstring(cur) != title {
                let _: () = msg1(st.window, "setTitle:", nsstring(&title));
            }
        }
        let c = st.app.cursor();
        if c != st.cursor {
            st.cursor = c;
            apply_cursor(c);
        }
        if st.app.quit {
            terminate();
        }
    });
}

extern "C" fn window_will_close(_: Id, _: Sel, _: Id) {
    terminate();
}

extern "C" fn should_terminate_after_last_window_closed(_: Id, _: Sel, _: Id) -> bool {
    true
}

fn terminate() {
    unsafe {
        let app: Id = msg0(class("NSApplication"), "sharedApplication");
        let _: () = msg1(app, "terminate:", NIL);
    }
}

fn add_method(cls: Class, name: &str, imp: *const c_void, types: &str) {
    let t = CString::new(types).unwrap();
    unsafe {
        class_addMethod(cls, sel(name), imp, t.as_ptr());
    }
}

fn register_view_class() -> Class {
    let name = CString::new("TipView").unwrap();
    unsafe {
        let existing = objc_getClass(name.as_ptr());
        if !existing.is_null() {
            return existing;
        }
        let cls = objc_allocateClassPair(class("NSView"), name.as_ptr(), 0);
        let rect_t = "v@:{CGRect={CGPoint=dd}{CGSize=dd}}";
        add_method(cls, "drawRect:", draw_rect as *const c_void, rect_t);
        add_method(cls, "isFlipped", yes as *const c_void, "B@:");
        add_method(cls, "acceptsFirstResponder", yes as *const c_void, "B@:");
        add_method(cls, "acceptsFirstMouse:", yes_ev as *const c_void, "B@:@");
        add_method(cls, "isOpaque", yes as *const c_void, "B@:");
        for m in ["mouseDown:", "rightMouseDown:", "otherMouseDown:"] {
            add_method(cls, m, mouse_down as *const c_void, "v@:@");
        }
        for m in ["mouseUp:", "rightMouseUp:", "otherMouseUp:"] {
            add_method(cls, m, mouse_up as *const c_void, "v@:@");
        }
        for m in ["mouseMoved:", "mouseDragged:", "rightMouseDragged:"] {
            add_method(cls, m, mouse_moved as *const c_void, "v@:@");
        }
        add_method(cls, "scrollWheel:", scroll_wheel as *const c_void, "v@:@");
        add_method(cls, "keyDown:", key_down as *const c_void, "v@:@");
        add_method(cls, "performKeyEquivalent:", perform_key_equivalent as *const c_void, "B@:@");
        add_method(cls, "tick:", tick as *const c_void, "v@:@");
        add_method(cls, "windowWillClose:", window_will_close as *const c_void, "v@:@");
        add_method(cls, "applicationShouldTerminateAfterLastWindowClosed:", should_terminate_after_last_window_closed as *const c_void, "B@:@");
        objc_registerClassPair(cls);
        cls
    }
}

fn build_menu(app: Id) {
    unsafe {
        let bar: Id = msg0(class("NSMenu"), "new");
        let app_item: Id = msg0(class("NSMenuItem"), "new");
        let _: () = msg1(bar, "addItem:", app_item);
        let app_menu: Id = msg0(class("NSMenu"), "new");
        let alloc: Id = msg0(class("NSMenuItem"), "alloc");
        let hide: Id = msg3(alloc, "initWithTitle:action:keyEquivalent:", nsstring("Hide Tip"), sel("hide:"), nsstring("h"));
        let _: () = msg1(app_menu, "addItem:", hide);
        let alloc: Id = msg0(class("NSMenuItem"), "alloc");
        let quit: Id = msg3(alloc, "initWithTitle:action:keyEquivalent:", nsstring("Quit Tip"), sel("terminate:"), nsstring("q"));
        let _: () = msg1(app_menu, "addItem:", quit);
        let _: () = msg1(app_item, "setSubmenu:", app_menu);
        let _: () = msg1(app, "setMainMenu:", bar);
    }
}

pub fn run(url: &str) -> i32 {
    unsafe {
        let _pool = Pool::new();
        let app: Id = msg0(class("NSApplication"), "sharedApplication");
        let _: bool = msg1(app, "setActivationPolicy:", 0isize);
        build_menu(app);

        let (w, h) = (1280.0, 860.0);
        let rect = NSRect { origin: NSPoint { x: 0.0, y: 0.0 }, size: NSSize { w, h } };
        // titled | closable | miniaturizable | resizable | full-size content view
        let style: usize = 1 | 2 | 4 | 8 | (1 << 15);
        let window: Id = msg0(class("NSWindow"), "alloc");
        let window: Id = msg4(window, "initWithContentRect:styleMask:backing:defer:", rect, style, 2usize, false);
        let _: () = msg1(window, "setTitlebarAppearsTransparent:", true);
        let _: () = msg1(window, "setTitleVisibility:", 1isize);
        let _: () = msg1(window, "setTitle:", nsstring("Tip"));
        let _: () = msg1(window, "setMinSize:", NSSize { w: 480.0, h: 320.0 });
        let _: () = msg0(window, "center");
        let _: () = msg1(window, "setReleasedWhenClosed:", false);

        let cls = register_view_class();
        let view: Id = msg0(cls, "alloc");
        let view: Id = msg1(view, "initWithFrame:", rect);
        let _: () = msg1(window, "setContentView:", view);
        let _: () = msg1(window, "setDelegate:", view);
        let _: () = msg1(app, "setDelegate:", view);
        let _: bool = msg1(window, "makeFirstResponder:", view);
        let _: () = msg1(window, "setAcceptsMouseMovedEvents:", true);

        // mouse tracking
        let ta: Id = msg0(class("NSTrackingArea"), "alloc");
        let opts: usize = 0x01 | 0x02 | 0x80 | 0x200;
        let ta: Id = msg4(ta, "initWithRect:options:owner:userInfo:", rect, opts, view, NIL);
        let _: () = msg1(view, "addTrackingArea:", ta);

        let scale: f64 = msg0(window, "backingScaleFactor");
        let state = State { app: App::new(url, w as f32, h as f32, scale as f32), view, window, cursor: Cursor::Default, last_frame: Instant::now(), start: Instant::now() };
        STATE.with(|s| *s.borrow_mut() = Some(state));

        let timer: Id = msg5(class("NSTimer"), "scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:", 1.0f64 / 60.0, view, sel("tick:"), NIL, true);
        let rl: Id = msg0(class("NSRunLoop"), "currentRunLoop");
        let _: () = msg2(rl, "addTimer:forMode:", timer, nsstring("kCFRunLoopCommonModes"));

        let _: () = msg1(window, "makeKeyAndOrderFront:", NIL);
        let _: () = msg1(app, "activateIgnoringOtherApps:", true);
        let _: () = msg0(app, "run");
    }
    0
}
