//! Date (times are milliseconds since the epoch; local time zone = system offset).

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

const MS_PER_DAY: f64 = 86_400_000.0;

pub fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// Local time zone offset in minutes (UTC - local) at UTC time `t`, as JS getTimezoneOffset.
pub fn tz_offset_min(t: f64) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    -(super::tz::local().at((t / 1000.0).floor() as i64).off as f64) / 60.0
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn make_time(y: f64, mon: f64, d: f64, h: f64, mi: f64, s: f64, ms: f64) -> f64 {
    if ![y, mon, d, h, mi, s, ms].iter().all(|v| v.is_finite()) {
        return f64::NAN;
    }
    let ym = y + (mon / 12.0).floor();
    let mn = mon.rem_euclid(12.0);
    let days = days_from_civil(ym as i64, mn as i64 + 1, 1) as f64 + d - 1.0;
    let t = days * MS_PER_DAY + h * 3_600_000.0 + mi * 60_000.0 + s * 1000.0 + ms;
    if t.abs() > 8.64e15 { f64::NAN } else { t }
}

pub struct Parts {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hour: i64,
    pub min: i64,
    pub sec: i64,
    pub ms: i64,
    pub weekday: i64,
}

pub fn parts(t: f64) -> Parts {
    let days = (t / MS_PER_DAY).floor();
    let ms_in_day = t - days * MS_PER_DAY;
    let (y, m, d) = civil_from_days(days as i64);
    let msd = ms_in_day as i64;
    Parts {
        year: y,
        month: m - 1,
        day: d,
        hour: msd / 3_600_000,
        min: (msd / 60_000) % 60,
        sec: (msd / 1000) % 60,
        ms: msd % 1000,
        weekday: ((days as i64 + 4).rem_euclid(7)),
    }
}

fn local(t: f64) -> f64 {
    t - tz_offset_min(t) * 60_000.0
}

fn utc_of_local(t: f64) -> f64 {
    if !t.is_finite() {
        return t;
    }
    t - super::tz::local().offset_for_local((t / 1000.0).floor() as i64) as f64 * 1000.0
}

pub fn to_iso(t: f64) -> Option<String> {
    if !t.is_finite() {
        return None;
    }
    let p = parts(t);
    let year = if (0..=9999).contains(&p.year) { format!("{:04}", p.year) } else if p.year < 0 { format!("-{:06}", -p.year) } else { format!("+{:06}", p.year) };
    Some(format!("{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", p.month + 1, p.day, p.hour, p.min, p.sec, p.ms))
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn tz_string(t: f64) -> String {
    let off = -tz_offset_min(t);
    let sign = if off >= 0.0 { '+' } else { '-' };
    let a = off.abs() as i64;
    let lt = super::tz::local().at((t / 1000.0).floor() as i64);
    let name = super::intl::create_dtf_name(&lt);
    format!("GMT{sign}{:02}{:02} ({name})", a / 60, a % 60)
}

fn to_string(t: f64) -> String {
    if !t.is_finite() {
        return "Invalid Date".into();
    }
    let p = parts(local(t));
    format!("{} {} {:02} {:04} {:02}:{:02}:{:02} {}", DAYS[p.weekday as usize], MONTHS[p.month as usize], p.day, p.year, p.hour, p.min, p.sec, tz_string(t))
}

/// Parse ISO 8601 and the common RFC 2822-ish formats.
pub fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    let b = s.as_bytes();
    // ISO: YYYY-MM-DD[THH:MM[:SS[.sss]]][Z|±HH:MM]
    if b.len() >= 4 && b[..4].iter().all(|c| c.is_ascii_digit()) {
        let num = |a: usize, n: usize| -> Option<f64> { s.get(a..a + n).and_then(|x| x.parse::<f64>().ok()) };
        let y = num(0, 4).unwrap_or(f64::NAN);
        let mut mo = 1.0;
        let mut d = 1.0;
        let mut i = 4;
        if s.get(i..i + 1) == Some("-") {
            mo = num(i + 1, 2).unwrap_or(f64::NAN);
            i += 3;
            if s.get(i..i + 1) == Some("-") {
                d = num(i + 1, 2).unwrap_or(f64::NAN);
                i += 3;
            }
        }
        let (mut h, mut mi, mut sec, mut ms) = (0.0, 0.0, 0.0, 0.0);
        let mut date_only = true;
        if matches!(s.get(i..i + 1), Some("T") | Some(" ")) {
            date_only = false;
            h = num(i + 1, 2).unwrap_or(f64::NAN);
            mi = num(i + 4, 2).unwrap_or(f64::NAN);
            i += 6;
            if s.get(i..i + 1) == Some(":") {
                sec = num(i + 1, 2).unwrap_or(f64::NAN);
                i += 3;
                if s.get(i..i + 1) == Some(".") {
                    let st = i + 1;
                    let mut e = st;
                    while e < b.len() && b[e].is_ascii_digit() {
                        e += 1;
                    }
                    let frac = &s[st..e];
                    ms = format!("0.{frac}").parse::<f64>().unwrap_or(0.0) * 1000.0;
                    i = e;
                }
            }
        }
        let t = make_time(y, mo - 1.0, d, h, mi, sec, ms.floor());
        let rest = &s[i.min(s.len())..];
        if rest.is_empty() {
            return if date_only { t } else { utc_of_local(t) };
        }
        if rest == "Z" || rest == "z" {
            return t;
        }
        if let Some(sign) = rest.chars().next().filter(|c| *c == '+' || *c == '-') {
            let r = rest[1..].replace(':', "");
            let oh: f64 = r.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let om: f64 = r.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let off = (oh * 60.0 + om) * 60_000.0;
            return if sign == '+' { t - off } else { t + off };
        }
        return f64::NAN;
    }
    // "Mon, 01 Jan 2024 10:00:00 GMT" / "January 1, 2024" / "1 Jan 2024"
    let words: Vec<String> = s.replace(',', " ").split_whitespace().map(|w| w.to_string()).collect();
    let mut y = None;
    let mut mo = None;
    let mut d = None;
    let (mut h, mut mi, mut sec) = (0.0, 0.0, 0.0);
    let mut utc = false;
    let mut off = 0.0;
    for w in &words {
        let lw = w.to_ascii_lowercase();
        if let Some(m) = MONTHS.iter().position(|m| lw.starts_with(&m.to_ascii_lowercase())) {
            mo = Some(m as f64);
        } else if w.contains(':') {
            let p: Vec<f64> = w.split(':').filter_map(|x| x.parse().ok()).collect();
            h = *p.first().unwrap_or(&0.0);
            mi = *p.get(1).unwrap_or(&0.0);
            sec = *p.get(2).unwrap_or(&0.0);
        } else if lw == "gmt" || lw == "utc" || lw == "z" {
            utc = true;
        } else if (lw.starts_with("gmt+") || lw.starts_with("gmt-") || lw.starts_with('+') || lw.starts_with('-')) && lw.len() >= 5 {
            utc = true;
            let r = lw.trim_start_matches("gmt");
            let sign = if r.starts_with('-') { -1.0 } else { 1.0 };
            let digits: String = r.chars().filter(|c| c.is_ascii_digit()).collect();
            let oh: f64 = digits.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let om: f64 = digits.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0.0);
            off = sign * (oh * 60.0 + om) * 60_000.0;
        } else if let Ok(n) = w.parse::<f64>() {
            if n > 31.0 || (d.is_some() && y.is_none()) {
                y = Some(n);
            } else if d.is_none() {
                d = Some(n);
            } else {
                y = Some(n);
            }
        } else if w.contains('/') {
            let p: Vec<f64> = w.split('/').filter_map(|x| x.parse().ok()).collect();
            if p.len() == 3 {
                mo = Some(p[0] - 1.0);
                d = Some(p[1]);
                y = Some(p[2]);
            }
        }
    }
    match (y, mo, d) {
        (Some(y), Some(mo), Some(d)) => {
            let t = make_time(y, mo, d, h, mi, sec, 0.0);
            if utc { t - off } else { utc_of_local(t) }
        }
        _ => f64::NAN,
    }
}

fn this_time(it: &mut Interp, v: &Value) -> R<f64> {
    if let Value::Obj(o) = v {
        if let ObjKind::Date(t) = o.borrow().kind {
            return Ok(t);
        }
    }
    throw!(it, "TypeError", "this is not a Date object.")
}

fn set_time(v: &Value, t: f64) -> Value {
    let t = if t.is_finite() && t.abs() <= 8.64e15 { t.trunc() } else { f64::NAN };
    if let Value::Obj(o) = v {
        o.borrow_mut().kind = ObjKind::Date(t);
    }
    Value::Num(t)
}

pub fn setup(it: &mut Interp) {
    let dp = it.p.date.clone();
    let ctor = it.define_ctor("Date", 7, &dp, |it, this, args| {
        if !it.constructing {
            return Ok(Value::str(&to_string(now_ms())));
        }
        it.constructing = false;
        let t = match args.len() {
            0 => now_ms(),
            1 => {
                let v = &args[0];
                if let Value::Obj(o) = v {
                    if let ObjKind::Date(t) = o.borrow().kind {
                        t
                    } else {
                        let p = it.to_primitive(v, "default")?;
                        match p {
                            Value::Str(s) => parse_date(&s),
                            p => it.to_number(&p)?,
                        }
                    }
                } else {
                    match v {
                        Value::Str(s) => parse_date(s),
                        v => it.to_number(v)?,
                    }
                }
            }
            _ => {
                let mut n = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                for (i, a) in args.iter().take(7).enumerate() {
                    n[i] = it.to_number(a)?;
                }
                if (0.0..=99.0).contains(&n[0]) && n[0].fract() == 0.0 {
                    n[0] += 1900.0;
                }
                utc_of_local(make_time(n[0], n[1], n[2], n[3], n[4], n[5], n[6]))
            }
        };
        if let Value::Obj(o) = &this {
            o.borrow_mut().kind = ObjKind::Date(if t.is_finite() { t.trunc() } else { f64::NAN });
            o.borrow_mut().class = "Date";
        }
        Ok(this)
    });
    it.define_method(&ctor, "now", 0, |_, _, _| Ok(Value::Num(now_ms())));
    it.define_method(&ctor, "parse", 1, |it, _, args| {
        let s = it.to_str(&arg(args, 0))?;
        Ok(Value::Num(parse_date(&s)))
    });
    it.define_method(&ctor, "UTC", 7, |it, _, args| {
        let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (i, a) in args.iter().take(7).enumerate() {
            n[i] = it.to_number(a)?;
        }
        if (0.0..=99.0).contains(&n[0]) {
            n[0] += 1900.0;
        }
        Ok(Value::Num(make_time(n[0], n[1], n[2], n[3], n[4], n[5], n[6])))
    });

    type Getter = fn(&Parts) -> i64;
    let getters: &[(&str, Getter)] = &[
        ("FullYear", |p| p.year),
        ("Month", |p| p.month),
        ("Date", |p| p.day),
        ("Day", |p| p.weekday),
        ("Hours", |p| p.hour),
        ("Minutes", |p| p.min),
        ("Seconds", |p| p.sec),
        ("Milliseconds", |p| p.ms),
    ];
    for &(name, f) in getters {
        it.define_method(&dp, &format!("get{name}"), 0, move |it, this, _| {
            let t = this_time(it, &this)?;
            Ok(Value::Num(if t.is_finite() { f(&parts(local(t))) as f64 } else { f64::NAN }))
        });
        it.define_method(&dp, &format!("getUTC{name}"), 0, move |it, this, _| {
            let t = this_time(it, &this)?;
            Ok(Value::Num(if t.is_finite() { f(&parts(t)) as f64 } else { f64::NAN }))
        });
    }
    it.define_method(&dp, "getYear", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        Ok(Value::Num(parts(local(t)).year as f64 - 1900.0))
    });
    it.define_method(&dp, "getTime", 0, |it, this, _| Ok(Value::Num(this_time(it, &this)?)));
    it.define_method(&dp, "valueOf", 0, |it, this, _| Ok(Value::Num(this_time(it, &this)?)));
    it.define_method(&dp, "getTimezoneOffset", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        Ok(Value::Num(tz_offset_min(t)))
    });
    it.define_method(&dp, "setTime", 1, |it, this, args| {
        this_time(it, &this)?;
        let t = it.to_number(&arg(args, 0))?;
        Ok(set_time(&this, t))
    });
    // setters: (name, index of first component, max components)
    let setters: &[(&str, usize, usize)] = &[
        ("FullYear", 0, 3),
        ("Month", 1, 2),
        ("Date", 2, 1),
        ("Hours", 3, 4),
        ("Minutes", 4, 3),
        ("Seconds", 5, 2),
        ("Milliseconds", 6, 1),
    ];
    for &(name, first, count) in setters {
        for utc in [false, true] {
            let fname = if utc { format!("setUTC{name}") } else { format!("set{name}") };
            it.define_method(&dp, &fname, count as u32, move |it, this, args| {
                let t = this_time(it, &this)?;
                let base = if t.is_finite() { if utc { t } else { local(t) } } else if first == 0 { 0.0 } else { return Ok(Value::Num(f64::NAN)) };
                let p = parts(base);
                let mut c = [p.year as f64, p.month as f64, p.day as f64, p.hour as f64, p.min as f64, p.sec as f64, p.ms as f64];
                for k in 0..count.min(args.len().max(1)) {
                    c[first + k] = it.to_number(&arg(args, k))?;
                }
                let nt = make_time(c[0], c[1], c[2], c[3], c[4], c[5], c[6]);
                Ok(set_time(&this, if utc { nt } else { utc_of_local(nt) }))
            });
        }
    }
    it.define_method(&dp, "toISOString", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        match to_iso(t) {
            Some(s) => Ok(Value::str(&s)),
            None => throw!(it, "RangeError", "Invalid time value"),
        }
    });
    it.define_method(&dp, "toJSON", 1, |it, this, _| {
        let t = this_time(it, &this)?;
        Ok(to_iso(t).map(|s| Value::str(&s)).unwrap_or(Value::Null))
    });
    it.define_method(&dp, "toString", 0, |it, this, _| Ok(Value::str(&to_string(this_time(it, &this)?))));
    it.define_method(&dp, "toUTCString", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        if !t.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(t);
        Ok(Value::str(&format!("{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT", DAYS[p.weekday as usize], p.day, MONTHS[p.month as usize], p.year, p.hour, p.min, p.sec)))
    });
    let utcs = dp.get_own("toUTCString").unwrap();
    dp.set_hidden("toGMTString", utcs);
    it.define_method(&dp, "toDateString", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        if !t.is_finite() {
            return Ok(Value::str("Invalid Date"));
        }
        let p = parts(local(t));
        Ok(Value::str(&format!("{} {} {:02} {:04}", DAYS[p.weekday as usize], MONTHS[p.month as usize], p.day, p.year)))
    });
    it.define_method(&dp, "toTimeString", 0, |it, this, _| {
        let t = this_time(it, &this)?;
        let p = parts(local(t));
        Ok(Value::str(&format!("{:02}:{:02}:{:02} {}", p.hour, p.min, p.sec, tz_string(t))))
    });
    use super::intl::{date_to_locale_string, DtfDefaults};
    for (name, defaults) in [("toLocaleDateString", DtfDefaults::Date), ("toLocaleTimeString", DtfDefaults::Time), ("toLocaleString", DtfDefaults::All)] {
        it.define_method(&dp, name, 0, move |it, this, args| {
            let t = this_time(it, &this)?;
            Ok(Value::str(&date_to_locale_string(it, t, &arg(args, 0), &arg(args, 1), defaults)?))
        });
    }
    let tp = it.sym_to_primitive.clone();
    it.define_method_sym(&dp, &tp, "[Symbol.toPrimitive]", |it, this, args| {
        let hint = it.to_str(&arg(args, 0))?;
        let t = this_time(it, &this)?;
        Ok(if &*hint == "number" { Value::Num(t) } else { Value::str(&to_string(t)) })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let t = make_time(2024.0, 1.0, 29.0, 13.0, 45.0, 30.0, 123.0);
        assert_eq!(to_iso(t).unwrap(), "2024-02-29T13:45:30.123Z");
        assert_eq!(parse_date("2024-02-29T13:45:30.123Z"), t);
        assert_eq!(parts(0.0).weekday, 4);
    }
}
