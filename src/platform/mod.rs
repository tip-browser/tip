//! Operating-system integration (windowing, clipboard, HTTPS bridges).

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub fn clipboard_get() -> Option<String> {
    use macos::objc::*;
    unsafe {
        let _p = Pool::new();
        let pb: Id = msg0(class("NSPasteboard"), "generalPasteboard");
        let s: Id = msg1(pb, "stringForType:", nsstring("public.utf8-plain-text"));
        if s.is_null() { None } else { Some(from_nsstring(s)) }
    }
}

#[cfg(target_os = "macos")]
pub fn clipboard_set(text: &str) {
    use macos::objc::*;
    unsafe {
        let _p = Pool::new();
        let pb: Id = msg0(class("NSPasteboard"), "generalPasteboard");
        let _: isize = msg0(pb, "clearContents");
        let _: bool = msg2(pb, "setString:forType:", nsstring(text), nsstring("public.utf8-plain-text"));
    }
}

#[cfg(target_os = "windows")]
pub fn clipboard_get() -> Option<String> {
    windows::clipboard_get()
}

#[cfg(target_os = "windows")]
pub fn clipboard_set(text: &str) {
    windows::clipboard_set(text)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn clipboard_get() -> Option<String> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn clipboard_set(_text: &str) {}
