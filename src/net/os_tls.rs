//! HTTPS through the operating system's TLS stack.
//!
//! TLS is the one piece we deliberately do not implement ourselves: macOS
//! requests go through NSURLConnection and Windows through WinHTTP, both of
//! which use the platform certificate store.

use super::url::Url;
use super::{Request, Response};

#[cfg(target_os = "macos")]
pub fn fetch(req: &Request, url: &Url) -> Result<Response, String> {
    use crate::platform::macos::objc::*;
    use std::ffi::c_void;
    unsafe {
        let _pool = Pool::new();
        let nsurl: Id = msg1(class("NSURL"), "URLWithString:", nsstring(&url.to_string()));
        if nsurl.is_null() {
            return Err(format!("Invalid URL: {url}"));
        }
        let mreq: Id = msg1(class("NSMutableURLRequest"), "requestWithURL:", nsurl);
        let _: () = msg1(mreq, "setHTTPMethod:", nsstring(&req.method));
        let _: () = msg1(mreq, "setTimeoutInterval:", 30.0f64);
        let _: () = msg2(mreq, "setValue:forHTTPHeaderField:", nsstring(super::USER_AGENT), nsstring("User-Agent"));
        for (k, v) in &req.headers {
            let _: () = msg2(mreq, "setValue:forHTTPHeaderField:", nsstring(v), nsstring(k));
        }
        if let Some(b) = &req.body {
            let data: Id = msg2(class("NSData"), "dataWithBytes:length:", b.as_ptr() as *const c_void, b.len());
            let _: () = msg1(mreq, "setHTTPBody:", data);
        }
        let mut resp: Id = NIL;
        let mut err: Id = NIL;
        let data: Id = msg3(class("NSURLConnection"), "sendSynchronousRequest:returningResponse:error:", mreq, &mut resp as *mut Id, &mut err as *mut Id);
        if data.is_null() {
            let desc = if err.is_null() { "unknown error".to_string() } else { from_nsstring(msg0(err, "localizedDescription")) };
            return Err(desc);
        }
        let len: usize = msg0(data, "length");
        let ptr: *const u8 = msg0(data, "bytes");
        let body = if ptr.is_null() || len == 0 { vec![] } else { std::slice::from_raw_parts(ptr, len).to_vec() };
        let mut status = 200u16;
        let mut headers = Vec::new();
        let mut final_url = url.to_string();
        if !resp.is_null() {
            let rurl: Id = msg0(resp, "URL");
            if !rurl.is_null() {
                final_url = from_nsstring(msg0(rurl, "absoluteString"));
            }
            let responds: bool = msg1(resp, "respondsToSelector:", sel("statusCode"));
            if responds {
                let code: isize = msg0(resp, "statusCode");
                status = code as u16;
                let dict: Id = msg0(resp, "allHeaderFields");
                if !dict.is_null() {
                    let keys: Id = msg0(dict, "allKeys");
                    let n: usize = msg0(keys, "count");
                    for i in 0..n {
                        let k: Id = msg1(keys, "objectAtIndex:", i);
                        let v: Id = msg1(dict, "objectForKey:", k);
                        let ks = from_nsstring(k);
                        // NSURLConnection already decoded the body
                        if ks.eq_ignore_ascii_case("content-encoding") || ks.eq_ignore_ascii_case("content-length") {
                            continue;
                        }
                        headers.push((ks, from_nsstring(v)));
                    }
                }
            } else {
                let mime: Id = msg0(resp, "MIMEType");
                if !mime.is_null() {
                    headers.push(("Content-Type".into(), from_nsstring(mime)));
                }
            }
        }
        Ok(Response { url: final_url, status, status_text: String::new(), headers, body, from_os: true })
    }
}

#[cfg(target_os = "windows")]
pub fn fetch(req: &Request, url: &Url) -> Result<Response, String> {
    use std::ffi::c_void;
    type H = *mut c_void;
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> H;
        fn WinHttpConnect(session: H, server: *const u16, port: u16, reserved: u32) -> H;
        fn WinHttpOpenRequest(conn: H, verb: *const u16, object: *const u16, version: *const u16, referrer: *const u16, accept: *const *const u16, flags: u32) -> H;
        fn WinHttpSendRequest(req: H, headers: *const u16, headers_len: u32, optional: *const c_void, optional_len: u32, total: u32, ctx: usize) -> i32;
        fn WinHttpReceiveResponse(req: H, reserved: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(req: H, level: u32, name: *const u16, buf: *mut c_void, len: *mut u32, index: *mut u32) -> i32;
        fn WinHttpQueryDataAvailable(req: H, size: *mut u32) -> i32;
        fn WinHttpReadData(req: H, buf: *mut c_void, to_read: u32, read: *mut u32) -> i32;
        fn WinHttpCloseHandle(h: H) -> i32;
        fn WinHttpSetOption(h: H, option: u32, buf: *const c_void, len: u32) -> i32;
        fn WinHttpQueryOption(h: H, option: u32, buf: *mut c_void, len: *mut u32) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLastError() -> u32;
    }
    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }
    struct Guard(H);
    impl Drop for Guard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }
    unsafe {
        let agent = w(super::USER_AGENT);
        let session = Guard(WinHttpOpen(agent.as_ptr(), 4, std::ptr::null(), std::ptr::null(), 0));
        if session.0.is_null() {
            // older Windows: default proxy
            return Err(format!("WinHttpOpen failed ({})", GetLastError()));
        }
        let host = w(&url.host);
        let conn = Guard(WinHttpConnect(session.0, host.as_ptr(), url.port_or_default(), 0));
        if conn.0.is_null() {
            return Err(format!("Could not connect to {} ({})", url.host, GetLastError()));
        }
        let verb = w(&req.method);
        let obj = w(&url.path_and_query());
        let flags = if url.scheme == "https" { 0x0080_0000 } else { 0 };
        let r = Guard(WinHttpOpenRequest(conn.0, verb.as_ptr(), obj.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), flags));
        if r.0.is_null() {
            return Err(format!("WinHttpOpenRequest failed ({})", GetLastError()));
        }
        let decompress: u32 = 3;
        WinHttpSetOption(r.0, 118, &decompress as *const u32 as *const c_void, 4);
        let mut hdrs = String::new();
        for (k, v) in &req.headers {
            hdrs.push_str(&format!("{k}: {v}\r\n"));
        }
        let hw: Vec<u16> = hdrs.encode_utf16().collect();
        let (bptr, blen) = match &req.body {
            Some(b) => (b.as_ptr() as *const c_void, b.len() as u32),
            None => (std::ptr::null(), 0),
        };
        if WinHttpSendRequest(r.0, if hw.is_empty() { std::ptr::null() } else { hw.as_ptr() }, hw.len() as u32, bptr, blen, blen, 0) == 0 {
            return Err(format!("Request failed ({})", GetLastError()));
        }
        if WinHttpReceiveResponse(r.0, std::ptr::null_mut()) == 0 {
            return Err(format!("No response ({})", GetLastError()));
        }
        let mut status: u32 = 0;
        let mut len: u32 = 4;
        WinHttpQueryHeaders(r.0, 19 | 0x2000_0000, std::ptr::null(), &mut status as *mut u32 as *mut c_void, &mut len, std::ptr::null_mut());
        // raw headers
        let mut size: u32 = 0;
        WinHttpQueryHeaders(r.0, 22, std::ptr::null(), std::ptr::null_mut(), &mut size, std::ptr::null_mut());
        let mut headers = Vec::new();
        if size > 0 {
            let mut buf = vec![0u16; (size / 2 + 1) as usize];
            if WinHttpQueryHeaders(r.0, 22, std::ptr::null(), buf.as_mut_ptr() as *mut c_void, &mut size, std::ptr::null_mut()) != 0 {
                let text = String::from_utf16_lossy(&buf[..(size / 2) as usize]);
                for line in text.split("\r\n").skip(1) {
                    if let Some((k, v)) = line.split_once(':') {
                        let k = k.trim();
                        if k.eq_ignore_ascii_case("content-encoding") || k.eq_ignore_ascii_case("content-length") {
                            continue;
                        }
                        headers.push((k.to_string(), v.trim().to_string()));
                    }
                }
            }
        }
        let mut body = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(r.0, &mut avail) == 0 || avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if WinHttpReadData(r.0, body[start..].as_mut_ptr() as *mut c_void, avail, &mut read) == 0 {
                break;
            }
            body.truncate(start + read as usize);
            if body.len() > super::MAX_BODY {
                return Err("response too large".into());
            }
        }
        let mut final_url = url.to_string();
        let mut ulen: u32 = 0;
        WinHttpQueryOption(r.0, 34, std::ptr::null_mut(), &mut ulen);
        if ulen > 0 {
            let mut ub = vec![0u16; (ulen / 2 + 1) as usize];
            if WinHttpQueryOption(r.0, 34, ub.as_mut_ptr() as *mut c_void, &mut ulen) != 0 {
                final_url = String::from_utf16_lossy(&ub[..(ulen / 2) as usize]).trim_end_matches('\0').to_string();
            }
        }
        Ok(Response { url: final_url, status: status as u16, status_text: String::new(), headers, body, from_os: true })
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn fetch(_req: &Request, url: &Url) -> Result<Response, String> {
    Err(format!("HTTPS is not available on this platform build ({url}). Tip delegates TLS to the operating system on macOS and Windows."))
}
