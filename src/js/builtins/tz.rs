//! Time zones: TZif files from the OS zoneinfo database, POSIX TZ rules, and a built-in
//! rule table where no database exists (Windows).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// A POSIX TZ rule (`CST6CDT,M3.2.0,M11.1.0`).
#[derive(Debug, Clone)]
pub struct Rule {
    pub std_abbr: String,
    /// Seconds east of UTC.
    pub std_off: i64,
    pub dst: Option<DstRule>,
}

#[derive(Debug, Clone)]
pub struct DstRule {
    pub abbr: String,
    pub off: i64,
    pub start: (RuleDate, i64),
    pub end: (RuleDate, i64),
}

#[derive(Debug, Clone, Copy)]
pub enum RuleDate {
    /// `Jn`: day 1..=365, never counting Feb 29.
    Julian1(i64),
    /// `n`: day 0..=365, counting Feb 29.
    Julian0(i64),
    /// `Mm.w.d`: month, week 1..=5 (5 = last), weekday 0 = Sunday.
    Month(i64, i64, i64),
}

#[derive(Debug, Clone)]
pub struct LocalType {
    pub off: i64,
    pub dst: bool,
    pub abbr: String,
}

#[derive(Debug, Clone)]
pub struct Zone {
    pub name: String,
    pub transitions: Vec<(i64, usize)>,
    pub types: Vec<LocalType>,
    pub rule: Option<Rule>,
}

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl RuleDate {
    /// Days since the epoch of this date in `year`.
    fn day(&self, year: i64) -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        match *self {
            RuleDate::Julian1(n) => jan1 + n - 1 + if is_leap(year) && n >= 60 { 1 } else { 0 },
            RuleDate::Julian0(n) => jan1 + n,
            RuleDate::Month(m, w, d) => {
                let first = days_from_civil(year, m, 1);
                let wd_first = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday
                let mut day = 1 + (d - wd_first).rem_euclid(7) + (w - 1) * 7;
                let dim = days_in_month(year, m);
                while day > dim {
                    day -= 7;
                }
                first + day - 1
            }
        }
    }
}

impl Rule {
    fn at(&self, t: i64) -> LocalType {
        let std = LocalType { off: self.std_off, dst: false, abbr: self.std_abbr.clone() };
        let Some(d) = &self.dst else { return std };
        let year = crate::js::builtins::tz::civil_year((t + self.std_off).div_euclid(86400));
        let start = d.start.0.day(year) * 86400 + d.start.1 - self.std_off;
        let end = d.end.0.day(year) * 86400 + d.end.1 - d.off;
        let in_dst = if start < end { t >= start && t < end } else { !(t >= end && t < start) };
        if in_dst {
            LocalType { off: d.off, dst: true, abbr: d.abbr.clone() }
        } else {
            std
        }
    }
}

pub fn civil_year(days: i64) -> i64 {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + if m <= 2 { 1 } else { 0 }
}

impl Zone {
    pub fn fixed(name: &str, off: i64) -> Zone {
        Zone { name: name.to_string(), transitions: vec![], types: vec![LocalType { off, dst: false, abbr: offset_abbr(off) }], rule: None }
    }

    /// Local time type at UTC seconds `t`.
    pub fn at(&self, t: i64) -> LocalType {
        if let Some(&(last, _)) = self.transitions.last() {
            if t >= last {
                if let Some(r) = &self.rule {
                    return r.at(t);
                }
            }
        } else if let Some(r) = &self.rule {
            return r.at(t);
        }
        if self.transitions.is_empty() || t < self.transitions[0].0 {
            let ty = self.types.iter().find(|x| !x.dst).or(self.types.first());
            return ty.cloned().unwrap_or(LocalType { off: 0, dst: false, abbr: "UTC".into() });
        }
        let i = self.transitions.partition_point(|&(tt, _)| tt <= t) - 1;
        self.types[self.transitions[i].1].clone()
    }

    /// Offset (seconds) for a *local* wall-clock time, preferring the earlier instant in overlaps.
    pub fn offset_for_local(&self, local: i64) -> i64 {
        let o1 = self.at(local).off;
        let o2 = self.at(local - o1).off;
        if o1 == o2 {
            return o1;
        }
        let o3 = self.at(local - o2).off;
        if o3 == o2 { o2 } else { o1.max(o2) }
    }
}

pub fn offset_abbr(off: i64) -> String {
    if off == 0 {
        return "UTC".into();
    }
    let a = off.abs();
    let (h, m) = (a / 3600, a % 3600 / 60);
    let sign = if off < 0 { '-' } else { '+' };
    if m == 0 { format!("GMT{sign}{h}") } else { format!("GMT{sign}{h}:{m:02}") }
}

// ---------------------------------------------------------------- POSIX TZ strings

fn parse_name(s: &[u8], i: &mut usize) -> Option<String> {
    if s.get(*i) == Some(&b'<') {
        let start = *i + 1;
        let end = start + s[start..].iter().position(|&c| c == b'>')?;
        *i = end + 1;
        return Some(String::from_utf8_lossy(&s[start..end]).into_owned());
    }
    let start = *i;
    while *i < s.len() && s[*i].is_ascii_alphabetic() {
        *i += 1;
    }
    if *i - start < 3 {
        return None;
    }
    Some(String::from_utf8_lossy(&s[start..*i]).into_owned())
}

/// `[+-]hh[:mm[:ss]]` in seconds.
fn parse_hms(s: &[u8], i: &mut usize) -> Option<i64> {
    let mut sign = 1;
    match s.get(*i) {
        Some(b'+') => *i += 1,
        Some(b'-') => {
            sign = -1;
            *i += 1
        }
        _ => {}
    }
    let mut total = 0;
    for (k, mul) in [3600, 60, 1].into_iter().enumerate() {
        if k > 0 {
            if s.get(*i) != Some(&b':') {
                break;
            }
            *i += 1;
        }
        let start = *i;
        while *i < s.len() && s[*i].is_ascii_digit() {
            *i += 1;
        }
        if start == *i {
            return None;
        }
        let n: i64 = std::str::from_utf8(&s[start..*i]).ok()?.parse().ok()?;
        total += n * mul;
    }
    Some(sign * total)
}

fn parse_num(s: &[u8], i: &mut usize) -> Option<i64> {
    let start = *i;
    while *i < s.len() && s[*i].is_ascii_digit() {
        *i += 1;
    }
    std::str::from_utf8(&s[start..*i]).ok()?.parse().ok()
}

fn parse_rule_date(s: &[u8], i: &mut usize) -> Option<(RuleDate, i64)> {
    let d = match s.get(*i)? {
        b'J' => {
            *i += 1;
            RuleDate::Julian1(parse_num(s, i)?)
        }
        b'M' => {
            *i += 1;
            let m = parse_num(s, i)?;
            if s.get(*i) != Some(&b'.') {
                return None;
            }
            *i += 1;
            let w = parse_num(s, i)?;
            if s.get(*i) != Some(&b'.') {
                return None;
            }
            *i += 1;
            RuleDate::Month(m, w, parse_num(s, i)?)
        }
        _ => RuleDate::Julian0(parse_num(s, i)?),
    };
    let time = if s.get(*i) == Some(&b'/') {
        *i += 1;
        parse_hms(s, i)?
    } else {
        7200
    };
    Some((d, time))
}

pub fn parse_posix(tz: &str) -> Option<Rule> {
    let s = tz.as_bytes();
    let mut i = 0;
    let std_abbr = parse_name(s, &mut i)?;
    let std_off = -parse_hms(s, &mut i)?;
    if i >= s.len() {
        return Some(Rule { std_abbr, std_off, dst: None });
    }
    let abbr = parse_name(s, &mut i)?;
    let off = if i < s.len() && s[i] != b',' { -parse_hms(s, &mut i)? } else { std_off + 3600 };
    let (start, end) = if s.get(i) == Some(&b',') {
        i += 1;
        let a = parse_rule_date(s, &mut i)?;
        if s.get(i) != Some(&b',') {
            return None;
        }
        i += 1;
        (a, parse_rule_date(s, &mut i)?)
    } else {
        ((RuleDate::Month(3, 2, 0), 7200), (RuleDate::Month(11, 1, 0), 7200))
    };
    Some(Rule { std_abbr, std_off, dst: Some(DstRule { abbr, off, start, end }) })
}

// ---------------------------------------------------------------- TZif

pub fn parse_tzif(name: &str, data: &[u8]) -> Option<Zone> {
    if data.len() < 44 || &data[..4] != b"TZif" {
        return None;
    }
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    let counts = |h: &[u8]| (be32(&h[20..]), be32(&h[24..]), be32(&h[28..]), be32(&h[32..]), be32(&h[36..]), be32(&h[40..]));
    let version = data[4];
    let (isut, isstd, leap, timecnt, typecnt, charcnt) = counts(data);
    let v1_len = timecnt * 5 + typecnt * 6 + charcnt + leap * 8 + isstd + isut;
    let (body, tsize, leapsize, counts) = if version >= b'2' && data.len() > 44 + v1_len + 44 {
        let h2 = &data[44 + v1_len..];
        (&h2[44..], 8, 12, counts(h2))
    } else {
        (&data[44..], 4, 8, (isut, isstd, leap, timecnt, typecnt, charcnt))
    };
    let (isut, isstd, leap, timecnt, typecnt, charcnt) = counts;
    let need = timecnt * (tsize + 1) + typecnt * 6 + charcnt + leap * leapsize + isstd + isut;
    if body.len() < need {
        return None;
    }
    let mut p = 0;
    let mut times = Vec::with_capacity(timecnt);
    for _ in 0..timecnt {
        let t = if tsize == 8 { i64::from_be_bytes(body[p..p + 8].try_into().ok()?) } else { i32::from_be_bytes(body[p..p + 4].try_into().ok()?) as i64 };
        times.push(t);
        p += tsize;
    }
    let idx: Vec<usize> = body[p..p + timecnt].iter().map(|&b| b as usize).collect();
    p += timecnt;
    let mut raw_types = Vec::with_capacity(typecnt);
    for _ in 0..typecnt {
        let off = i32::from_be_bytes(body[p..p + 4].try_into().ok()?) as i64;
        raw_types.push((off, body[p + 4] != 0, body[p + 5] as usize));
        p += 6;
    }
    let chars = &body[p..p + charcnt];
    p += charcnt + leap * leapsize + isstd + isut;
    let types: Vec<LocalType> = raw_types
        .into_iter()
        .map(|(off, dst, ai)| {
            let end = chars[ai.min(chars.len())..].iter().position(|&c| c == 0).map(|e| ai + e).unwrap_or(chars.len());
            LocalType { off, dst, abbr: String::from_utf8_lossy(&chars[ai.min(end)..end]).into_owned() }
        })
        .collect();
    if types.is_empty() {
        return None;
    }
    let transitions = times.into_iter().zip(idx).filter(|(_, i)| *i < types.len()).collect();
    let rule = if tsize == 8 {
        let footer = &body[p..];
        let s = String::from_utf8_lossy(footer);
        let s = s.trim_matches('\n');
        if s.is_empty() { None } else { parse_posix(s) }
    } else {
        None
    };
    Some(Zone { name: name.to_string(), transitions, types, rule })
}

// ---------------------------------------------------------------- lookup

const ZONEINFO_DIRS: &[&str] = &["/usr/share/zoneinfo", "/var/db/timezone/zoneinfo", "/usr/lib/zoneinfo", "/usr/share/lib/zoneinfo"];

/// Current rules for common zones, used when the OS has no zoneinfo database.
const BUILTIN: &[(&str, &str)] = &[
    ("Africa/Cairo", "EET-2EEST,M4.5.5/0,M10.5.4/24"),
    ("Africa/Johannesburg", "SAST-2"),
    ("Africa/Lagos", "WAT-1"),
    ("Africa/Nairobi", "EAT-3"),
    ("Africa/Casablanca", "<+01>-1"),
    ("America/Anchorage", "AKST9AKDT,M3.2.0,M11.1.0"),
    ("America/Argentina/Buenos_Aires", "<-03>3"),
    ("America/Bogota", "<-05>5"),
    ("America/Chicago", "CST6CDT,M3.2.0,M11.1.0"),
    ("America/Denver", "MST7MDT,M3.2.0,M11.1.0"),
    ("America/Halifax", "AST4ADT,M3.2.0,M11.1.0"),
    ("America/Lima", "<-05>5"),
    ("America/Los_Angeles", "PST8PDT,M3.2.0,M11.1.0"),
    ("America/Mexico_City", "CST6"),
    ("America/New_York", "EST5EDT,M3.2.0,M11.1.0"),
    ("America/Phoenix", "MST7"),
    ("America/Santiago", "<-04>4<-03>,M9.1.6/24,M4.1.6/24"),
    ("America/Sao_Paulo", "<-03>3"),
    ("America/St_Johns", "NST3:30NDT,M3.2.0,M11.1.0"),
    ("America/Toronto", "EST5EDT,M3.2.0,M11.1.0"),
    ("America/Vancouver", "PST8PDT,M3.2.0,M11.1.0"),
    ("Asia/Bangkok", "<+07>-7"),
    ("Asia/Dhaka", "<+06>-6"),
    ("Asia/Dubai", "<+04>-4"),
    ("Asia/Hong_Kong", "HKT-8"),
    ("Asia/Jakarta", "WIB-7"),
    ("Asia/Jerusalem", "IST-2IDT,M3.4.4/26,M10.5.0"),
    ("Asia/Karachi", "PKT-5"),
    ("Asia/Kathmandu", "<+0545>-5:45"),
    ("Asia/Kolkata", "IST-5:30"),
    ("Asia/Manila", "PST-8"),
    ("Asia/Riyadh", "<+03>-3"),
    ("Asia/Seoul", "KST-9"),
    ("Asia/Shanghai", "CST-8"),
    ("Asia/Singapore", "<+08>-8"),
    ("Asia/Taipei", "CST-8"),
    ("Asia/Tehran", "<+0330>-3:30"),
    ("Asia/Tokyo", "JST-9"),
    ("Atlantic/Azores", "<-01>1<+00>,M3.5.0/0,M10.5.0/1"),
    ("Atlantic/Reykjavik", "GMT0"),
    ("Australia/Adelaide", "ACST-9:30ACDT,M10.1.0,M4.1.0/3"),
    ("Australia/Brisbane", "AEST-10"),
    ("Australia/Darwin", "ACST-9:30"),
    ("Australia/Melbourne", "AEST-10AEDT,M10.1.0,M4.1.0/3"),
    ("Australia/Perth", "AWST-8"),
    ("Australia/Sydney", "AEST-10AEDT,M10.1.0,M4.1.0/3"),
    ("Europe/Amsterdam", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Athens", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Berlin", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Brussels", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Bucharest", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Budapest", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Copenhagen", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Dublin", "IST-1GMT0,M10.5.0,M3.5.0/1"),
    ("Europe/Helsinki", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Istanbul", "<+03>-3"),
    ("Europe/Kyiv", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Lisbon", "WET0WEST,M3.5.0/1,M10.5.0"),
    ("Europe/London", "GMT0BST,M3.5.0/1,M10.5.0"),
    ("Europe/Madrid", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Moscow", "MSK-3"),
    ("Europe/Oslo", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Paris", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Prague", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Rome", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Stockholm", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Vienna", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Warsaw", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Zurich", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Pacific/Auckland", "NZST-12NZDT,M9.5.0,M4.1.0/3"),
    ("Pacific/Honolulu", "HST10"),
    ("UTC", "UTC0"),
];

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() < 64 && !name.contains("..") && !name.starts_with('/') && name.bytes().all(|c| c.is_ascii_alphanumeric() || b"/_+-".contains(&c))
}

fn load_uncached(name: &str) -> Option<Zone> {
    let upper = name.to_ascii_uppercase();
    if matches!(upper.as_str(), "UTC" | "ETC/UTC" | "GMT" | "ETC/GMT" | "Z" | "ZULU" | "UCT" | "ETC/UCT" | "UNIVERSAL" | "ETC/UNIVERSAL") {
        return Some(Zone::fixed("UTC", 0));
    }
    // `+05:30` style offsets
    if name.starts_with(['+', '-']) {
        let s = name.as_bytes();
        let mut i = 0;
        let off = parse_hms(s, &mut i)?;
        if i != s.len() || off.abs() >= 86400 {
            return None;
        }
        let h = off.abs() / 3600;
        let m = off.abs() % 3600 / 60;
        return Some(Zone::fixed(&format!("{}{h:02}:{m:02}", if off < 0 { '-' } else { '+' }), off));
    }
    // Etc/GMT+5 means UTC-5
    if let Some(rest) = upper.strip_prefix("ETC/GMT") {
        let n: i64 = rest.parse().ok()?;
        if n.abs() > 14 {
            return None;
        }
        return Some(Zone::fixed(&format!("Etc/GMT{rest}"), -n * 3600));
    }
    if !valid_name(name) {
        return None;
    }
    for dir in ZONEINFO_DIRS {
        if let Ok(data) = std::fs::read(format!("{dir}/{name}")) {
            if let Some(mut z) = parse_tzif(name, &data) {
                z.name = canonical_case(name);
                return Some(z);
            }
        }
    }
    let (n, rule) = BUILTIN.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?;
    Some(Zone { name: n.to_string(), transitions: vec![], types: vec![], rule: parse_posix(rule) })
}

/// Restore conventional capitalization (`america/new_york` → `America/New_York`).
fn canonical_case(name: &str) -> String {
    if let Some((n, _)) = BUILTIN.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        return n.to_string();
    }
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        return name.to_string();
    }
    name.split('/')
        .map(|part| part.split('_').map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }).collect::<Vec<_>>().join("_"))
        .collect::<Vec<_>>()
        .join("/")
}

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<Zone>>>> {
    static C: OnceLock<Mutex<HashMap<String, Option<Arc<Zone>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up an IANA time zone (case-insensitive). `None` if unknown.
pub fn zone(name: &str) -> Option<Arc<Zone>> {
    let key = name.to_ascii_lowercase();
    if let Some(z) = cache().lock().unwrap().get(&key) {
        return z.clone();
    }
    let z = load_uncached(name).map(Arc::new);
    cache().lock().unwrap().insert(key, z.clone());
    z
}

/// The system's local time zone (honours `TZ`).
pub fn local() -> Arc<Zone> {
    static L: OnceLock<Arc<Zone>> = OnceLock::new();
    L.get_or_init(|| Arc::new(detect_local())).clone()
}

fn detect_local() -> Zone {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if let Some(z) = zone(tz) {
            return (*z).clone();
        }
        if let Some(rule) = parse_posix(tz) {
            return Zone { name: tz.to_string(), transitions: vec![], types: vec![], rule: Some(rule) };
        }
    }
    #[cfg(unix)]
    {
        if let Ok(target) = std::fs::read_link("/etc/localtime") {
            let t = target.to_string_lossy().to_string();
            if let Some(i) = t.find("zoneinfo/") {
                if let Some(z) = zone(&t[i + 9..]) {
                    return (*z).clone();
                }
            }
        }
        if let Ok(name) = std::fs::read_to_string("/etc/timezone") {
            if let Some(z) = zone(name.trim()) {
                return (*z).clone();
            }
        }
        if let Ok(data) = std::fs::read("/etc/localtime") {
            if let Some(z) = parse_tzif("UTC", &data) {
                return z;
            }
        }
    }
    #[cfg(windows)]
    {
        if let Some(z) = windows_local() {
            return z;
        }
    }
    Zone::fixed("UTC", 0)
}

/// IANA zone names available to `Intl.supportedValuesOf("timeZone")`.
pub fn all_names() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in ZONEINFO_DIRS {
        let Ok(areas) = std::fs::read_dir(dir) else { continue };
        for a in areas.flatten() {
            let area = a.file_name().to_string_lossy().to_string();
            if !matches!(area.as_str(), "Africa" | "America" | "Antarctica" | "Arctic" | "Asia" | "Atlantic" | "Australia" | "Europe" | "Indian" | "Pacific") {
                continue;
            }
            walk(&a.path(), &area, &mut out);
        }
        if !out.is_empty() {
            break;
        }
    }
    if out.is_empty() {
        out = BUILTIN.iter().map(|(n, _)| n.to_string()).filter(|n| n != "UTC").collect();
    }
    out.push("UTC".into());
    out.sort();
    out.dedup();
    out
}

fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let full = format!("{prefix}/{n}");
        if e.path().is_dir() {
            walk(&e.path(), &full, out);
        } else if n.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            out.push(full);
        }
    }
}

#[cfg(windows)]
fn windows_local() -> Option<Zone> {
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct SystemTime {
        year: u16,
        month: u16,
        dow: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        ms: u16,
    }
    #[repr(C)]
    struct Dtzi {
        bias: i32,
        std_name: [u16; 32],
        std_date: SystemTime,
        std_bias: i32,
        dst_name: [u16; 32],
        dst_date: SystemTime,
        dst_bias: i32,
        key: [u16; 128],
        disabled: u8,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDynamicTimeZoneInformation(p: *mut Dtzi) -> u32;
    }
    let mut z: Dtzi = unsafe { std::mem::zeroed() };
    if unsafe { GetDynamicTimeZoneInformation(&mut z) } == u32::MAX {
        return None;
    }
    let key = String::from_utf16_lossy(&z.key[..z.key.iter().position(|&c| c == 0).unwrap_or(0)]);
    const MAP: &[(&str, &str)] = &[
        ("Pacific Standard Time", "America/Los_Angeles"),
        ("Mountain Standard Time", "America/Denver"),
        ("US Mountain Standard Time", "America/Phoenix"),
        ("Central Standard Time", "America/Chicago"),
        ("Eastern Standard Time", "America/New_York"),
        ("Atlantic Standard Time", "America/Halifax"),
        ("Alaskan Standard Time", "America/Anchorage"),
        ("Hawaiian Standard Time", "Pacific/Honolulu"),
        ("GMT Standard Time", "Europe/London"),
        ("W. Europe Standard Time", "Europe/Berlin"),
        ("Romance Standard Time", "Europe/Paris"),
        ("Central Europe Standard Time", "Europe/Budapest"),
        ("Central European Standard Time", "Europe/Warsaw"),
        ("FLE Standard Time", "Europe/Kyiv"),
        ("GTB Standard Time", "Europe/Bucharest"),
        ("Russian Standard Time", "Europe/Moscow"),
        ("India Standard Time", "Asia/Kolkata"),
        ("China Standard Time", "Asia/Shanghai"),
        ("Tokyo Standard Time", "Asia/Tokyo"),
        ("Korea Standard Time", "Asia/Seoul"),
        ("Singapore Standard Time", "Asia/Singapore"),
        ("AUS Eastern Standard Time", "Australia/Sydney"),
        ("New Zealand Standard Time", "Pacific/Auckland"),
        ("E. South America Standard Time", "America/Sao_Paulo"),
        ("UTC", "UTC"),
    ];
    let name = MAP.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string());
    if let Some(n) = &name {
        if let Some(zz) = zone(n) {
            return Some((*zz).clone());
        }
    }
    let std_off = -((z.bias + z.std_bias) as i64) * 60;
    let dst = if z.std_date.month != 0 && z.disabled == 0 {
        let rd = |d: &SystemTime| (RuleDate::Month(d.month as i64, d.day as i64, d.dow as i64), d.hour as i64 * 3600 + d.minute as i64 * 60 + d.second as i64);
        Some(DstRule { abbr: offset_abbr(-((z.bias + z.dst_bias) as i64) * 60), off: -((z.bias + z.dst_bias) as i64) * 60, start: rd(&z.dst_date), end: rd(&z.std_date) })
    } else {
        None
    };
    Some(Zone { name: name.unwrap_or_else(|| offset_abbr(std_off)), transitions: vec![], types: vec![], rule: Some(Rule { std_abbr: offset_abbr(std_off), std_off, dst }) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_rules() {
        let r = parse_posix("CST6CDT,M3.2.0,M11.1.0").unwrap();
        // 2026-07-01T12:00Z is daylight time in Chicago, 2026-01-15 is standard
        let jul = days_from_civil(2026, 7, 1) * 86400 + 12 * 3600;
        let jan = days_from_civil(2026, 1, 15) * 86400;
        assert_eq!(r.at(jul).off, -5 * 3600);
        assert_eq!(r.at(jan).off, -6 * 3600);
        // DST starts 2026-03-08 08:00Z (2:00 CST)
        let start = days_from_civil(2026, 3, 8) * 86400 + 8 * 3600;
        assert_eq!(r.at(start - 1).off, -6 * 3600);
        assert_eq!(r.at(start).off, -5 * 3600);
        let syd = parse_posix("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(syd.at(jan).off, 11 * 3600);
        assert_eq!(syd.at(jul).off, 10 * 3600);
        assert_eq!(parse_posix("<+0545>-5:45").unwrap().std_off, 5 * 3600 + 45 * 60);
    }

    #[test]
    fn zone_lookup() {
        let z = zone("europe/berlin").unwrap();
        assert_eq!(z.name, "Europe/Berlin");
        let jul = days_from_civil(2026, 7, 1) * 86400;
        assert_eq!(z.at(jul).off, 7200);
        assert!(zone("Not/AZone").is_none());
        assert_eq!(zone("Etc/GMT+5").unwrap().at(0).off, -5 * 3600);
    }
}
