//! Minimal Objective-C runtime bridge.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr, CString};

pub type Id = *mut c_void;
pub type Sel = *mut c_void;
pub type Class = *mut c_void;
pub const NIL: Id = std::ptr::null_mut();

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Class;
    pub fn sel_registerName(name: *const c_char) -> Sel;
    pub fn objc_msgSend();
    pub fn objc_allocateClassPair(superclass: Class, name: *const c_char, extra: usize) -> Class;
    pub fn objc_registerClassPair(cls: Class);
    pub fn class_addMethod(cls: Class, name: Sel, imp: *const c_void, types: *const c_char) -> bool;
    pub fn class_addIvar(cls: Class, name: *const c_char, size: usize, align: u8, types: *const c_char) -> bool;
    pub fn object_getInstanceVariable(obj: Id, name: *const c_char, out: *mut *mut c_void) -> *mut c_void;
    pub fn object_setInstanceVariable(obj: Id, name: *const c_char, value: *mut c_void) -> *mut c_void;
    pub fn objc_autoreleasePoolPush() -> *mut c_void;
    pub fn objc_autoreleasePoolPop(p: *mut c_void);
}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

pub fn class(name: &str) -> Class {
    let c = CString::new(name).unwrap();
    unsafe { objc_getClass(c.as_ptr()) }
}

pub fn sel(name: &str) -> Sel {
    let c = CString::new(name).unwrap();
    unsafe { sel_registerName(c.as_ptr()) }
}

fn imp() -> *const () {
    objc_msgSend as *const ()
}

pub unsafe fn msg0<R>(obj: Id, s: &str) -> R {
    let f: unsafe extern "C" fn(Id, Sel) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s)) }
}

pub unsafe fn msg1<A, R>(obj: Id, s: &str, a: A) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s), a) }
}

pub unsafe fn msg2<A, B, R>(obj: Id, s: &str, a: A, b: B) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s), a, b) }
}

pub unsafe fn msg3<A, B, C, R>(obj: Id, s: &str, a: A, b: B, c: C) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s), a, b, c) }
}

pub unsafe fn msg4<A, B, C, D, R>(obj: Id, s: &str, a: A, b: B, c: C, d: D) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C, D) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s), a, b, c, d) }
}

pub unsafe fn msg5<A, B, C, D, E, R>(obj: Id, s: &str, a: A, b: B, c: C, d: D, e: E) -> R {
    let f: unsafe extern "C" fn(Id, Sel, A, B, C, D, E) -> R = unsafe { std::mem::transmute(imp()) };
    unsafe { f(obj, sel(s), a, b, c, d, e) }
}

/// An autoreleased NSString.
pub fn nsstring(s: &str) -> Id {
    unsafe {
        let alloc: Id = msg0(class("NSString"), "alloc");
        let o: Id = msg3(alloc, "initWithBytes:length:encoding:", s.as_ptr() as *const c_void, s.len(), 4usize);
        msg0(o, "autorelease")
    }
}

pub fn from_nsstring(o: Id) -> String {
    if o.is_null() {
        return String::new();
    }
    unsafe {
        let p: *const c_char = msg0(o, "UTF8String");
        if p.is_null() {
            return String::new();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

pub struct Pool(*mut c_void);

impl Pool {
    pub fn new() -> Pool {
        Pool(unsafe { objc_autoreleasePoolPush() })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        unsafe { objc_autoreleasePoolPop(self.0) }
    }
}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}
