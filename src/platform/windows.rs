//! Windows backend: a plain Win32 window that blits our canvas with SetDIBitsToDevice.
#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use crate::app::App;
use crate::bindings::events::Mods;
use crate::style::Cursor;

type HWND = *mut c_void;
type HANDLE = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: u32,
    style: u32,
    lpfnWndProc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HANDLE,
    hIcon: HANDLE,
    hCursor: HANDLE,
    hbrBackground: HANDLE,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HANDLE,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RECT {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: u32,
    wParam: WPARAM,
    lParam: LPARAM,
    time: u32,
    pt: POINT,
    lPrivate: u32,
}

#[repr(C)]
struct PAINTSTRUCT {
    hdc: HANDLE,
    fErase: i32,
    rcPaint: RECT,
    fRestore: i32,
    fIncUpdate: i32,
    rgbReserved: [u8; 32],
}

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: u32,
    biWidth: i32,
    biHeight: i32,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: u32,
    biSizeImage: u32,
    biXPelsPerMeter: i32,
    biYPelsPerMeter: i32,
    biClrUsed: u32,
    biClrImportant: u32,
}

#[repr(C)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 1],
}

#[link(name = "user32")]
unsafe extern "system" {
    fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HANDLE, inst: HANDLE, param: *mut c_void) -> HWND;
    fn DefWindowProcW(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> i32;
    fn UpdateWindow(hwnd: HWND) -> i32;
    fn PeekMessageW(msg: *mut MSG, hwnd: HWND, min: u32, max: u32, remove: u32) -> i32;
    fn TranslateMessage(msg: *const MSG) -> i32;
    fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    fn MsgWaitForMultipleObjects(n: u32, handles: *const HANDLE, wait_all: i32, ms: u32, mask: u32) -> u32;
    fn PostQuitMessage(code: i32);
    fn BeginPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> HANDLE;
    fn EndPaint(hwnd: HWND, ps: *const PAINTSTRUCT) -> i32;
    fn InvalidateRect(hwnd: HWND, rect: *const RECT, erase: i32) -> i32;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> i32;
    fn LoadCursorW(inst: HANDLE, name: *const u16) -> HANDLE;
    fn SetCursor(c: HANDLE) -> HANDLE;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> i32;
    fn GetKeyState(vk: i32) -> i16;
    fn SetWindowTextW(hwnd: HWND, text: *const u16) -> i32;
    fn ScreenToClient(hwnd: HWND, pt: *mut POINT) -> i32;
    fn GetDpiForWindow(hwnd: HWND) -> u32;
    fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
    fn DestroyWindow(hwnd: HWND) -> i32;
    fn OpenClipboard(hwnd: HWND) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(fmt: u32) -> HANDLE;
    fn SetClipboardData(fmt: u32, mem: HANDLE) -> HANDLE;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    fn SetDIBitsToDevice(hdc: HANDLE, x: i32, y: i32, w: u32, h: u32, sx: i32, sy: i32, start: u32, lines: u32, bits: *const c_void, bmi: *const BITMAPINFO, usage: u32) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
    fn GlobalAlloc(flags: u32, bytes: usize) -> HANDLE;
    fn GlobalLock(mem: HANDLE) -> *mut c_void;
    fn GlobalUnlock(mem: HANDLE) -> i32;
}

const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_PAINT: u32 = 0x000F;
const WM_CLOSE: u32 = 0x0010;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SETCURSOR: u32 = 0x0020;
const WM_KEYDOWN: u32 = 0x0100;
const WM_CHAR: u32 = 0x0102;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_MOUSEMOVE: u32 = 0x0200;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_RBUTTONDOWN: u32 = 0x0204;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_MBUTTONDOWN: u32 = 0x0207;
const WM_MBUTTONUP: u32 = 0x0208;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_MOUSEHWHEEL: u32 = 0x020E;
const WM_DPICHANGED: u32 = 0x02E0;

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct State {
    app: App,
    hwnd: HWND,
    scale: f32,
    high_surrogate: Option<u16>,
    title: String,
    start: Instant,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn mods() -> Mods {
    unsafe {
        Mods {
            shift: GetKeyState(0x10) < 0,
            ctrl: GetKeyState(0x11) < 0,
            alt: GetKeyState(0x12) < 0,
            meta: GetKeyState(0x5B) < 0 || GetKeyState(0x5C) < 0,
        }
    }
}

fn xy(st: &State, l: LPARAM) -> (f32, f32) {
    let x = (l & 0xffff) as u16 as i16 as f32;
    let y = ((l >> 16) & 0xffff) as u16 as i16 as f32;
    (x / st.scale, y / st.scale)
}

fn invalidate(hwnd: HWND) {
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

fn vk_name(vk: usize) -> Option<&'static str> {
    Some(match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x1B => "Escape",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "ArrowLeft",
        0x26 => "ArrowUp",
        0x27 => "ArrowRight",
        0x28 => "ArrowDown",
        0x2E => "Delete",
        0x70 => "F1",
        0x71 => "F2",
        0x72 => "F3",
        0x73 => "F4",
        0x74 => "F5",
        0x75 => "F6",
        0x76 => "F7",
        0x77 => "F8",
        0x78 => "F9",
        0x79 => "F10",
        0x7A => "F11",
        0x7B => "F12",
        _ => return None,
    })
}

fn resize_from_client(st: &mut State) {
    let mut r = RECT::default();
    unsafe {
        GetClientRect(st.hwnd, &mut r);
        let dpi = GetDpiForWindow(st.hwnd);
        st.scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    }
    let (wpx, hpx) = ((r.right - r.left).max(1) as f32, (r.bottom - r.top).max(1) as f32);
    st.app.resize(wpx / st.scale, hpx / st.scale, st.scale);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
            with_state(|st| {
                let c = st.app.render();
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: c.w as i32,
                        biHeight: -(c.h as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: 0,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [0],
                };
                unsafe {
                    SetDIBitsToDevice(hdc, 0, 0, c.w as u32, c.h as u32, 0, 0, 0, c.h as u32, c.px.as_ptr() as *const c_void, &bmi, 0);
                }
            });
            unsafe { EndPaint(hwnd, &ps) };
            0
        }
        WM_ERASEBKGND => 1,
        WM_SIZE | WM_DPICHANGED => {
            with_state(|st| {
                resize_from_client(st);
                st.app.tick();
            });
            invalidate(hwnd);
            if msg == WM_DPICHANGED { 0 } else { unsafe { DefWindowProcW(hwnd, msg, wp, lp) } }
        }
        WM_MOUSEMOVE => {
            with_state(|st| {
                let (x, y) = xy(st, lp);
                st.app.mouse_move(x, y);
                st.app.tick();
            });
            invalidate(hwnd);
            0
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            unsafe { SetCapture(hwnd) };
            let b = match msg {
                WM_LBUTTONDOWN => 0,
                WM_MBUTTONDOWN => 1,
                _ => 2,
            };
            with_state(|st| {
                let (x, y) = xy(st, lp);
                st.app.mouse_down(x, y, b, mods());
                st.app.tick();
            });
            invalidate(hwnd);
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            unsafe { ReleaseCapture() };
            let b = match msg {
                WM_LBUTTONUP => 0,
                WM_MBUTTONUP => 1,
                _ => 2,
            };
            with_state(|st| {
                let (x, y) = xy(st, lp);
                st.app.mouse_up(x, y, b, mods());
                st.app.tick();
            });
            invalidate(hwnd);
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = ((wp >> 16) & 0xffff) as u16 as i16 as f32 / 120.0 * 100.0;
            let mut pt = POINT { x: (lp & 0xffff) as u16 as i16 as i32, y: ((lp >> 16) & 0xffff) as u16 as i16 as i32 };
            unsafe { ScreenToClient(hwnd, &mut pt) };
            with_state(|st| {
                let (x, y) = (pt.x as f32 / st.scale, pt.y as f32 / st.scale);
                if msg == WM_MOUSEWHEEL {
                    st.app.wheel(x, y, 0.0, -delta);
                } else {
                    st.app.wheel(x, y, delta, 0.0);
                }
                st.app.tick();
            });
            invalidate(hwnd);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let m = mods();
            let handled = with_state(|st| {
                if let Some(name) = vk_name(wp) {
                    st.app.key_down(name, None, m);
                    st.app.tick();
                    return true;
                }
                // Ctrl/Alt shortcuts with letters and digits (WM_CHAR delivers control codes for these)
                if (m.ctrl || m.alt) && ((0x30..=0x39).contains(&wp) || (0x41..=0x5A).contains(&wp) || matches!(wp, 0xBB | 0xBD | 0xDB | 0xDD)) {
                    let k = match wp {
                        0xBB => "=".to_string(),
                        0xBD => "-".to_string(),
                        0xDB => "[".to_string(),
                        0xDD => "]".to_string(),
                        v => ((v as u8) as char).to_ascii_lowercase().to_string(),
                    };
                    st.app.key_down(&k, None, m);
                    st.app.tick();
                    return true;
                }
                false
            })
            .unwrap_or(false);
            invalidate(hwnd);
            if handled && msg == WM_KEYDOWN {
                0
            } else {
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
        }
        WM_CHAR => {
            let m = mods();
            with_state(|st| {
                let u = wp as u16;
                let s = if (0xD800..0xDC00).contains(&u) {
                    st.high_surrogate = Some(u);
                    return;
                } else if (0xDC00..0xE000).contains(&u) {
                    match st.high_surrogate.take() {
                        Some(h) => String::from_utf16_lossy(&[h, u]),
                        None => return,
                    }
                } else {
                    String::from_utf16_lossy(&[u])
                };
                let c = s.chars().next().unwrap_or('\0');
                if c.is_control() || (m.ctrl && !m.alt) {
                    return;
                }
                st.app.key_down(&s, Some(&s), Mods { ctrl: false, alt: false, ..m });
                st.app.tick();
            });
            invalidate(hwnd);
            0
        }
        WM_SETCURSOR => {
            if (lp & 0xffff) == 1 {
                let c = with_state(|st| st.app.cursor()).unwrap_or(Cursor::Default);
                let id: usize = match c {
                    Cursor::Pointer => 32649,
                    Cursor::Text => 32513,
                    Cursor::Crosshair => 32515,
                    Cursor::ResizeEw => 32644,
                    Cursor::ResizeNs => 32645,
                    Cursor::NotAllowed => 32648,
                    Cursor::Move | Cursor::Grab => 32646,
                    Cursor::Default => 32512,
                };
                unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), id as *const u16)) };
                return 1;
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

pub fn run(url: &str) -> i32 {
    unsafe {
        SetProcessDpiAwarenessContext(-4);
        let inst = GetModuleHandleW(std::ptr::null());
        let class = w("TipBrowserWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0x0001 | 0x0002,
            lpfnWndProc: wndproc,
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: inst,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), 32512 as *const u16),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        if RegisterClassExW(&wc) == 0 {
            return 1;
        }
        let title = w("Tip");
        // WS_OVERLAPPEDWINDOW
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), 0x00CF_0000, 0x8000_0000u32 as i32, 0x8000_0000u32 as i32, 1280, 860, std::ptr::null_mut(), std::ptr::null_mut(), inst, std::ptr::null_mut());
        if hwnd.is_null() {
            return 1;
        }
        let dpi = GetDpiForWindow(hwnd);
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r);
        let app = App::new(url, (r.right - r.left) as f32 / scale, (r.bottom - r.top) as f32 / scale, scale);
        STATE.with(|s| *s.borrow_mut() = Some(State { app, hwnd, scale, high_surrogate: None, title: String::new(), start: Instant::now() }));
        ShowWindow(hwnd, 5);
        UpdateWindow(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        loop {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, 1) != 0 {
                if msg.message == 0x0012 {
                    return msg.wParam as i32;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let quit = with_state(|st| {
                if st.app.tick() {
                    invalidate(st.hwnd);
                }
                if st.app.title != st.title {
                    st.title = st.app.title.clone();
                    let t = w(&st.title);
                    SetWindowTextW(st.hwnd, t.as_ptr());
                }
                if let Ok(path) = std::env::var("TIP_SNAPSHOT") {
                    if st.start.elapsed().as_millis() > 1500 && !st.app.busy() {
                        let c = st.app.render();
                        let _ = crate::automation::save_png(c, std::path::Path::new(&path));
                        st.app.quit = true;
                    }
                }
                st.app.quit
            })
            .unwrap_or(false);
            if quit {
                DestroyWindow(hwnd);
            }
            // sleep until input arrives or the next frame is due (~60 Hz)
            MsgWaitForMultipleObjects(0, std::ptr::null(), 0, 16, 0x04FF);
        }
    }
}

pub fn clipboard_get() -> Option<String> {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(13);
        let mut out = None;
        if !h.is_null() {
            let p = GlobalLock(h) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 {
                    n += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
                GlobalUnlock(h);
            }
        }
        CloseClipboard();
        out
    }
}

pub fn clipboard_set(text: &str) {
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return;
        }
        EmptyClipboard();
        let u: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mem = GlobalAlloc(0x0002, u.len() * 2);
        if !mem.is_null() {
            let p = GlobalLock(mem) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(u.as_ptr(), p, u.len());
                GlobalUnlock(mem);
                SetClipboardData(13, mem);
            }
        }
        CloseClipboard();
    }
}
