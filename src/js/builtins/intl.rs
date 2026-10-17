//! Intl: DateTimeFormat, NumberFormat, PluralRules, Collator, RelativeTimeFormat, ListFormat,
//! DisplayNames, Locale. Formatting data covers English fully and common patterns for other
//! languages (separators, field order, plural rules).

use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;

use super::date;
use super::tz::{self, Zone};
use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

pub const DEFAULT_LOCALE: &str = "en-US";

// ---------------------------------------------------------------- locales

/// Canonicalize a BCP 47 tag (`EN-us` → `en-US`); `None` if malformed.
pub fn canonical_locale(tag: &str) -> Option<String> {
    let tag = tag.replace('_', "-");
    let subs: Vec<&str> = tag.split('-').collect();
    let lang = subs.first()?;
    if !(lang.len() >= 2 && lang.len() <= 8 && lang.len() != 4 && lang.chars().all(|c| c.is_ascii_alphabetic())) {
        return None;
    }
    let mut out = vec![lang.to_ascii_lowercase()];
    let mut in_ext = false;
    for (i, s) in subs.iter().enumerate().skip(1) {
        if s.is_empty() || s.len() > 8 || !s.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        if s.len() == 1 {
            in_ext = true;
        }
        let v = if in_ext {
            s.to_ascii_lowercase()
        } else if s.len() == 4 && s.chars().all(|c| c.is_ascii_alphabetic()) && i == 1 {
            let l = s.to_ascii_lowercase();
            l[..1].to_ascii_uppercase() + &l[1..]
        } else if s.len() == 2 && s.chars().all(|c| c.is_ascii_alphabetic()) || s.len() == 3 && s.chars().all(|c| c.is_ascii_digit()) {
            s.to_ascii_uppercase()
        } else {
            s.to_ascii_lowercase()
        };
        out.push(v);
    }
    Some(out.join("-"))
}

fn locale_list(it: &mut Interp, v: &Value) -> R<Vec<String>> {
    let items = match v {
        Value::Undefined => return Ok(vec![]),
        Value::Str(s) => vec![Value::Str(s.clone())],
        Value::Obj(o) if o.borrow().class == "Intl.Locale" => {
            let f = it.get_str(v, "toString")?;
            vec![it.call(&f, v.clone(), &[])?]
        }
        _ => it.iterate_to_vec(v)?,
    };
    let mut out = Vec::new();
    for item in items {
        let s = if let Value::Obj(o) = &item {
            if o.borrow().class == "Intl.Locale" {
                let f = it.get_str(&item, "toString")?;
                it.call(&f, item.clone(), &[])?
            } else {
                item.clone()
            }
        } else {
            item.clone()
        };
        if !matches!(s, Value::Str(_)) {
            throw!(it, "TypeError", "Language ID should be string or object.");
        }
        let s = it.to_str(&s)?;
        let Some(c) = canonical_locale(&s) else { throw!(it, "RangeError", "Incorrect locale information provided") };
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn resolve_locale(it: &mut Interp, v: &Value) -> R<String> {
    Ok(locale_list(it, v)?.into_iter().next().unwrap_or_else(|| DEFAULT_LOCALE.to_string()))
}

/// Base locale without extensions (`de-DE-u-co-phonebk` → `de-DE`).
fn base_name(loc: &str) -> String {
    let mut out = Vec::new();
    for s in loc.split('-') {
        if s.len() == 1 {
            break;
        }
        out.push(s);
    }
    out.join("-")
}

fn lang_of(loc: &str) -> &str {
    loc.split('-').next().unwrap_or("en")
}

fn region_of(loc: &str) -> Option<&str> {
    loc.split('-').skip(1).take_while(|s| s.len() > 1).find(|s| (s.len() == 2 && s.chars().all(|c| c.is_ascii_uppercase())) || (s.len() == 3 && s.chars().all(|c| c.is_ascii_digit())))
}

fn opt_str(it: &mut Interp, opts: &Value, key: &str) -> R<Option<String>> {
    if !matches!(opts, Value::Obj(_)) {
        return Ok(None);
    }
    let v = it.get_str(opts, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    Ok(Some(it.to_str(&v)?.to_string()))
}

fn opt_enum(it: &mut Interp, opts: &Value, key: &str, allowed: &[&str]) -> R<Option<String>> {
    let v = opt_str(it, opts, key)?;
    if let Some(s) = &v {
        if !allowed.contains(&s.as_str()) {
            throw!(it, "RangeError", "Value {s} out of range for Intl options property {key}");
        }
    }
    Ok(v)
}

fn opt_bool(it: &mut Interp, opts: &Value, key: &str) -> R<Option<bool>> {
    if !matches!(opts, Value::Obj(_)) {
        return Ok(None);
    }
    let v = it.get_str(opts, key)?;
    Ok(if v.is_undefined() { None } else { Some(v.truthy()) })
}

fn opt_num(it: &mut Interp, opts: &Value, key: &str, min: f64, max: f64) -> R<Option<u32>> {
    if !matches!(opts, Value::Obj(_)) {
        return Ok(None);
    }
    let v = it.get_str(opts, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let n = it.to_number(&v)?;
    if !n.is_finite() || n < min || n > max {
        throw!(it, "RangeError", "{key} value is out of range.");
    }
    Ok(Some(n.floor() as u32))
}

fn options_object(it: &mut Interp, v: &Value) -> R<Value> {
    match v {
        Value::Undefined => Ok(Value::Undefined),
        Value::Null => throw!(it, "TypeError", "Cannot convert undefined or null to object"),
        Value::Obj(_) => Ok(v.clone()),
        _ => Ok(Value::Obj(it.to_object(v)?)),
    }
}

fn part(it: &mut Interp, ty: &str, value: &str) -> Value {
    let o = it.new_object();
    o.set_prop("type", Value::str(ty));
    o.set_prop("value", Value::str(value));
    Value::Obj(o)
}

fn parts_array(it: &mut Interp, parts: &[(&'static str, String)]) -> Value {
    let items: Vec<Value> = parts.iter().map(|(t, v)| part(it, t, v)).collect();
    it.new_array(items)
}

fn join_parts(parts: &[(&'static str, String)]) -> String {
    parts.iter().map(|(_, v)| v.as_str()).collect()
}

/// Make an Intl constructor on `intl`. `init` fills in the new instance.
fn intl_ctor(it: &mut Interp, intl: &Obj, name: &'static str, requires_new: bool, init: impl Fn(&mut Interp, &Obj, &[Value]) -> R<()> + 'static) -> Obj {
    let proto = it.new_object();
    let tag = PropKey::Sym(it.sym_to_string_tag.clone());
    proto.borrow_mut().props.insert(tag, Prop { value: Value::str(&format!("Intl.{name}")), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
    let p2 = proto.clone();
    let ctor = it.new_native(name, 0, Rc::new(move |it: &mut Interp, _this: Value, args: &[Value]| {
        let constructing = std::mem::replace(&mut it.constructing, false);
        if requires_new && !constructing {
            throw!(it, "TypeError", "Constructor Intl.{name} requires 'new'");
        }
        let o = Obj::new(ObjKind::Ordinary, Some(p2.clone()), "Intl");
        o.borrow_mut().class = match name {
            "Locale" => "Intl.Locale",
            _ => "Intl",
        };
        init(it, &o, args)?;
        Ok(Value::Obj(o))
    }), true);
    ctor.borrow_mut().props.insert(PropKey::from("prototype"), Prop { value: Value::Obj(proto.clone()), getter: None, setter: None, enumerable: false, writable: false, configurable: false });
    proto.borrow_mut().props.insert(PropKey::from("constructor"), Prop::hidden(Value::Obj(ctor.clone())));
    it.define_method(&ctor, "supportedLocalesOf", 1, |it, _, args| {
        let l = locale_list(it, &arg(args, 0))?;
        Ok(it.new_array(l.iter().map(|s| Value::str(s)).collect()))
    });
    intl.set_hidden(name, Value::Obj(ctor.clone()));
    ctor
}

fn method(it: &mut Interp, o: &Obj, name: &str, len: u32, f: impl Fn(&mut Interp, Value, &[Value]) -> R<Value> + 'static) {
    it.define_method(o, name, len, f);
}

fn resolved(it: &mut Interp, fields: &[(&str, Value)]) -> Value {
    let o = it.new_object();
    for (k, v) in fields {
        if !v.is_undefined() {
            o.set_prop(k, v.clone());
        }
    }
    Value::Obj(o)
}

fn sv(s: &Option<String>) -> Value {
    s.as_ref().map(|s| Value::str(s)).unwrap_or(Value::Undefined)
}

// ---------------------------------------------------------------- DateTimeFormat

const MONTHS_LONG: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS_LONG: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn month_names(lang: &str) -> Option<[&'static str; 12]> {
    Some(match lang {
        "de" => ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"],
        "fr" => ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"],
        "es" => ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"],
        "it" => ["gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno", "luglio", "agosto", "settembre", "ottobre", "novembre", "dicembre"],
        "pt" => ["janeiro", "fevereiro", "março", "abril", "maio", "junho", "julho", "agosto", "setembro", "outubro", "novembro", "dezembro"],
        "nl" => ["januari", "februari", "maart", "april", "mei", "juni", "juli", "augustus", "september", "oktober", "november", "december"],
        _ => return None,
    })
}

fn day_names(lang: &str) -> Option<[&'static str; 7]> {
    Some(match lang {
        "de" => ["Sonntag", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag"],
        "fr" => ["dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi"],
        "es" => ["domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado"],
        "it" => ["domenica", "lunedì", "martedì", "mercoledì", "giovedì", "venerdì", "sabato"],
        "pt" => ["domingo", "segunda-feira", "terça-feira", "quarta-feira", "quinta-feira", "sexta-feira", "sábado"],
        "nl" => ["zondag", "maandag", "dinsdag", "woensdag", "donderdag", "vrijdag", "zaterdag"],
        _ => return None,
    })
}

fn month_name(lang: &str, m: usize, style: &str) -> String {
    let long = month_names(lang).map(|n| n[m]).unwrap_or(MONTHS_LONG[m]);
    match style {
        "long" => long.to_string(),
        "narrow" => long.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(),
        _ => {
            if lang == "en" || month_names(lang).is_none() {
                MONTHS_LONG[m][..3].to_string()
            } else {
                let s: String = long.chars().take(if long.chars().count() <= 4 { 4 } else { 3 }).collect();
                if long.chars().count() <= 4 { s } else { format!("{s}.") }
            }
        }
    }
}

fn day_name(lang: &str, d: usize, style: &str) -> String {
    let long = day_names(lang).map(|n| n[d]).unwrap_or(DAYS_LONG[d]);
    match style {
        "long" => long.to_string(),
        "narrow" => long.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default(),
        _ => {
            if day_names(lang).is_none() {
                DAYS_LONG[d][..3].to_string()
            } else {
                let s: String = long.chars().take(2).collect();
                format!("{s}.")
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct DtfOptions {
    pub weekday: Option<String>,
    pub era: Option<String>,
    pub year: Option<String>,
    pub month: Option<String>,
    pub day: Option<String>,
    pub hour: Option<String>,
    pub minute: Option<String>,
    pub second: Option<String>,
    pub fsd: Option<u32>,
    pub tz_name: Option<String>,
    pub hour_cycle: Option<String>,
    pub date_style: Option<String>,
    pub time_style: Option<String>,
}

pub struct Dtf {
    pub locale: String,
    pub zone: Arc<Zone>,
    pub o: DtfOptions,
}

/// Which default fields to add when none are requested (ECMA-402 ToDateTimeOptions).
#[derive(Clone, Copy, PartialEq)]
pub enum DtfDefaults {
    Date,
    Time,
    All,
}

pub fn create_dtf(it: &mut Interp, locales: &Value, options: &Value, required_any: bool, defaults: DtfDefaults) -> R<Dtf> {
    let locale = resolve_locale(it, locales)?;
    let opts = options_object(it, options)?;
    let num2 = ["numeric", "2-digit"];
    let text = ["narrow", "short", "long"];
    let month_vals = ["numeric", "2-digit", "narrow", "short", "long"];
    let mut o = DtfOptions {
        weekday: opt_enum(it, &opts, "weekday", &text)?,
        era: opt_enum(it, &opts, "era", &text)?,
        year: opt_enum(it, &opts, "year", &num2)?,
        month: opt_enum(it, &opts, "month", &month_vals)?,
        day: opt_enum(it, &opts, "day", &num2)?,
        hour: opt_enum(it, &opts, "hour", &num2)?,
        minute: opt_enum(it, &opts, "minute", &num2)?,
        second: opt_enum(it, &opts, "second", &num2)?,
        fsd: opt_num(it, &opts, "fractionalSecondDigits", 1.0, 3.0)?,
        tz_name: opt_enum(it, &opts, "timeZoneName", &["short", "long", "shortOffset", "longOffset", "shortGeneric", "longGeneric"])?,
        hour_cycle: opt_enum(it, &opts, "hourCycle", &["h11", "h12", "h23", "h24"])?,
        date_style: opt_enum(it, &opts, "dateStyle", &["full", "long", "medium", "short"])?,
        time_style: opt_enum(it, &opts, "timeStyle", &["full", "long", "medium", "short"])?,
    };
    if let Some(h12) = opt_bool(it, &opts, "hour12")? {
        o.hour_cycle = Some(if h12 { "h12" } else { "h23" }.into());
    }
    let zone = match opt_str(it, &opts, "timeZone")? {
        Some(name) => match tz::zone(&name) {
            Some(z) => z,
            None => throw!(it, "RangeError", "Invalid time zone specified: {name}"),
        },
        None => tz::local(),
    };
    let has_date = o.weekday.is_some() || o.year.is_some() || o.month.is_some() || o.day.is_some();
    let has_time = o.hour.is_some() || o.minute.is_some() || o.second.is_some() || o.fsd.is_some();
    if (o.date_style.is_some() || o.time_style.is_some()) && (has_date || has_time || o.era.is_some() || o.tz_name.is_some()) {
        throw!(it, "TypeError", "Can't set option {} when dateStyle is used", if has_date { "year" } else { "hour" });
    }
    let _ = required_any;
    if !has_date && !has_time && o.date_style.is_none() && o.time_style.is_none() {
        if matches!(defaults, DtfDefaults::Date | DtfDefaults::All) {
            o.year = Some("numeric".into());
            o.month = Some("numeric".into());
            o.day = Some("numeric".into());
        }
        if matches!(defaults, DtfDefaults::Time | DtfDefaults::All) {
            o.hour = Some("numeric".into());
            o.minute = Some("numeric".into());
            o.second = Some("numeric".into());
        }
    }
    Ok(Dtf { locale, zone, o })
}

impl Dtf {
    fn hour_cycle(&self) -> &str {
        if let Some(h) = &self.o.hour_cycle {
            return h;
        }
        if let Some(hc) = self.locale.split("-hc-").nth(1) {
            if let Some(h) = ["h11", "h12", "h23", "h24"].iter().find(|h| hc.starts_with(**h)) {
                return h;
            }
        }
        match (lang_of(&self.locale), region_of(&self.locale)) {
            ("en", None | Some("US" | "CA" | "AU" | "NZ" | "IN" | "PH")) => "h12",
            ("ko" | "hi" | "ar" | "bn" | "ur", _) => "h12",
            ("es", Some("US" | "MX" | "CO")) => "h12",
            _ => "h23",
        }
    }

    /// Expand dateStyle/timeStyle into component options.
    fn components(&self) -> DtfOptions {
        let mut o = self.o.clone();
        let s = |v: &str| Some(v.to_string());
        if let Some(ds) = &self.o.date_style {
            match ds.as_str() {
                "full" => (o.weekday, o.month, o.day, o.year) = (s("long"), s("long"), s("numeric"), s("numeric")),
                "long" => (o.month, o.day, o.year) = (s("long"), s("numeric"), s("numeric")),
                "medium" => (o.month, o.day, o.year) = (s("short"), s("numeric"), s("numeric")),
                _ => (o.month, o.day, o.year) = (s("numeric"), s("numeric"), s("2-digit")),
            }
        }
        if let Some(ts) = &self.o.time_style {
            o.hour = s("numeric");
            o.minute = s("2-digit");
            if ts != "short" {
                o.second = s("2-digit");
            }
            if ts == "full" || ts == "long" {
                o.tz_name = s("short");
            }
        }
        o
    }

    pub fn to_parts(&self, t: f64) -> Vec<(&'static str, String)> {
        let lt = self.zone.at((t / 1000.0).floor() as i64);
        let p = date::parts(t + lt.off as f64 * 1000.0);
        let o = self.components();
        let lang = lang_of(&self.locale).to_string();
        let region = region_of(&self.locale).map(|s| s.to_string());
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let lit = |out: &mut Vec<(&'static str, String)>, s: &str| {
            if !s.is_empty() {
                out.push(("literal", s.to_string()));
            }
        };
        let num = |v: i64, style: &Option<String>| -> String {
            if style.as_deref() == Some("2-digit") { format!("{:02}", v.rem_euclid(100)) } else { v.to_string() }
        };
        let iso_order = matches!((lang.as_str(), region.as_deref()), ("en", Some("CA")) | ("sv" | "lt" | "fr", Some("CA")) | ("sv" | "lt" | "hu" | "ja" | "zh" | "ko", _));
        let textual = o.month.as_deref().is_some_and(|m| !matches!(m, "numeric" | "2-digit"));
        // ---- date
        let mut date_parts: Vec<(&'static str, String)> = Vec::new();
        let year_s = o.year.as_ref().map(|_| if o.year.as_deref() == Some("2-digit") { format!("{:02}", p.year.rem_euclid(100)) } else { p.year.to_string() });
        if textual {
            let mstyle = o.month.clone().unwrap();
            let m = month_name(&lang, p.month as usize, &mstyle);
            let d = o.day.as_ref().map(|_| num(p.day, &o.day));
            let wd = o.weekday.as_ref().map(|w| day_name(&lang, p.weekday as usize, w));
            if lang == "en" && region.as_deref() != Some("GB") && region.as_deref() != Some("AU") && region.as_deref() != Some("IN") && region.as_deref() != Some("NZ") && region.as_deref() != Some("IE") {
                // Saturday, October 17, 2026
                if let Some(w) = wd {
                    date_parts.push(("weekday", w));
                    lit(&mut date_parts, ", ");
                }
                date_parts.push(("month", m));
                if let Some(d) = d {
                    lit(&mut date_parts, " ");
                    date_parts.push(("day", d));
                    if let Some(y) = &year_s {
                        lit(&mut date_parts, ", ");
                        date_parts.push(("year", y.clone()));
                    }
                } else if let Some(y) = &year_s {
                    lit(&mut date_parts, " ");
                    date_parts.push(("year", y.clone()));
                }
            } else {
                // Saturday 17 October 2026 / samedi 17 octobre 2026 / Samstag, 17. Oktober 2026
                let de = lang == "de";
                let es_pt = matches!(lang.as_str(), "es" | "pt");
                if let Some(w) = wd {
                    date_parts.push(("weekday", w));
                    lit(&mut date_parts, if de || es_pt { ", " } else { " " });
                }
                if let Some(d) = d {
                    date_parts.push(("day", d));
                    lit(&mut date_parts, if de { ". " } else if es_pt { " de " } else { " " });
                }
                date_parts.push(("month", m));
                if let Some(y) = &year_s {
                    lit(&mut date_parts, if es_pt { " de " } else { " " });
                    date_parts.push(("year", y.clone()));
                }
            }
        } else if o.year.is_some() || o.month.is_some() || o.day.is_some() {
            let pad = |v: i64, style: &Option<String>, force: bool| -> String { if force { format!("{v:02}") } else { num(v, style) } };
            let (sep, order, force_pad): (&str, [char; 3], bool) = if iso_order {
                let sep = if matches!(lang.as_str(), "ja" | "zh") { "/" } else if lang == "ko" { ". " } else if lang == "hu" { ". " } else { "-" };
                (sep, ['y', 'm', 'd'], !matches!(lang.as_str(), "ja" | "zh" | "ko"))
            } else if lang == "en" && region.as_deref().is_none_or(|r| matches!(r, "US" | "PH")) {
                ("/", ['m', 'd', 'y'], false)
            } else if matches!(lang.as_str(), "de" | "ru" | "pl" | "fi" | "nb" | "no" | "da" | "cs" | "sk" | "tr" | "uk" | "ro") {
                (".", ['d', 'm', 'y'], matches!(lang.as_str(), "ru" | "pl" | "nb" | "no" | "da" | "tr" | "uk" | "ro"))
            } else if lang == "nl" {
                ("-", ['d', 'm', 'y'], false)
            } else {
                ("/", ['d', 'm', 'y'], matches!(lang.as_str(), "en" | "fr" | "it" | "pt"))
            };
            let mut first = true;
            for f in order {
                let piece = match f {
                    'y' => year_s.clone().map(|y| ("year", y)),
                    'm' => o.month.as_ref().map(|_| ("month", pad(p.month + 1, &o.month, force_pad))),
                    _ => o.day.as_ref().map(|_| ("day", pad(p.day, &o.day, force_pad))),
                };
                if let Some(pc) = piece {
                    if !first {
                        lit(&mut date_parts, sep);
                    }
                    first = false;
                    date_parts.push(pc);
                }
            }
            if lang == "ko" && !date_parts.is_empty() {
                lit(&mut date_parts, ".");
            }
            if let Some(w) = &o.weekday {
                let mut v = vec![("weekday", day_name(&lang, p.weekday as usize, w))];
                lit(&mut v, ", ");
                v.extend(date_parts);
                date_parts = v;
            }
        } else if let Some(w) = &o.weekday {
            date_parts.push(("weekday", day_name(&lang, p.weekday as usize, w)));
        }
        if let Some(e) = &o.era {
            if !date_parts.is_empty() {
                lit(&mut date_parts, " ");
            }
            date_parts.push(("era", if e == "long" { if p.year > 0 { "Anno Domini" } else { "Before Christ" } } else if p.year > 0 { "AD" } else { "BC" }.to_string()));
        }
        // ---- time
        let mut time_parts: Vec<(&'static str, String)> = Vec::new();
        let hc = self.hour_cycle().to_string();
        let mut period = None;
        if o.hour.is_some() {
            let h = p.hour;
            let hv = match hc.as_str() {
                "h11" => h % 12,
                "h12" => if h % 12 == 0 { 12 } else { h % 12 },
                "h24" => if h == 0 { 24 } else { h },
                _ => h,
            };
            if hc == "h11" || hc == "h12" {
                period = Some(if h < 12 { 0 } else { 1 });
            }
            let two = o.hour.as_deref() == Some("2-digit") || (matches!(hc.as_str(), "h23" | "h24") && o.minute.is_some());
            time_parts.push(("hour", if two { format!("{hv:02}") } else { hv.to_string() }));
        }
        if o.minute.is_some() {
            if !time_parts.is_empty() {
                lit(&mut time_parts, ":");
            }
            let two = o.hour.is_some() || o.minute.as_deref() == Some("2-digit");
            time_parts.push(("minute", if two { format!("{:02}", p.min) } else { p.min.to_string() }));
        }
        if o.second.is_some() {
            if !time_parts.is_empty() {
                lit(&mut time_parts, ":");
            }
            let two = o.minute.is_some() || o.second.as_deref() == Some("2-digit");
            time_parts.push(("second", if two { format!("{:02}", p.sec) } else { p.sec.to_string() }));
        }
        if let Some(n) = o.fsd {
            if !time_parts.is_empty() {
                lit(&mut time_parts, ".");
            }
            let ms = format!("{:03}", p.ms);
            time_parts.push(("fractionalSecond", ms[..n as usize].to_string()));
        }
        if let Some(pd) = period {
            let (am, pm) = match (lang.as_str(), region.as_deref()) {
                ("en", Some("CA")) => ("a.m.", "p.m."),
                ("en", Some("AU" | "IN" | "NZ" | "GB" | "IE")) => ("am", "pm"),
                ("ko", _) => ("오전", "오후"),
                _ => ("AM", "PM"),
            };
            lit(&mut time_parts, " ");
            time_parts.push(("dayPeriod", if pd == 0 { am } else { pm }.to_string()));
        }
        if let Some(tzn) = &o.tz_name {
            let off = lt.off;
            let gmt = |long: bool| {
                if off == 0 {
                    return "GMT".to_string();
                }
                let a = off.abs();
                let sign = if off < 0 { '-' } else { '+' };
                if long {
                    format!("GMT{sign}{:02}:{:02}", a / 3600, a % 3600 / 60)
                } else if a % 3600 == 0 {
                    format!("GMT{sign}{}", a / 3600)
                } else {
                    format!("GMT{sign}{}:{:02}", a / 3600, a % 3600 / 60)
                }
            };
            let name = match tzn.as_str() {
                "longOffset" => gmt(true),
                "shortOffset" => gmt(false),
                _ => {
                    if self.zone.name == "UTC" {
                        if tzn == "long" { "Coordinated Universal Time".into() } else { "UTC".into() }
                    } else if lang == "en" && lt.abbr.chars().all(|c| c.is_ascii_alphabetic()) && lt.abbr.len() >= 3 && region.as_deref().is_none_or(|r| r == "US" || r == "CA") && matches!(lt.abbr.as_str(), "EST" | "EDT" | "CST" | "CDT" | "MST" | "MDT" | "PST" | "PDT" | "AKST" | "AKDT" | "HST" | "AST" | "ADT" | "NST" | "NDT") {
                        lt.abbr.clone()
                    } else {
                        gmt(false)
                    }
                }
            };
            if !time_parts.is_empty() {
                lit(&mut time_parts, " ");
            }
            time_parts.push(("timeZoneName", name));
        }
        // ---- combine
        out.extend(date_parts.iter().cloned());
        if !date_parts.is_empty() && !time_parts.is_empty() {
            let sep = if textual && lang == "en" && o.date_style.as_deref().is_some_and(|d| d == "full" || d == "long") { " at " } else if matches!(lang.as_str(), "en" | "de" | "es" | "it" | "pt" | "nl" | "ru" | "pl") { ", " } else { " " };
            lit(&mut out, sep);
        }
        out.extend(time_parts);
        out
    }

    pub fn format(&self, t: f64) -> String {
        join_parts(&self.to_parts(t))
    }
}

fn date_arg(it: &mut Interp, v: &Value) -> R<f64> {
    let t = match v {
        Value::Undefined => date::now_ms(),
        Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Date(_)) => {
            let ObjKind::Date(t) = o.borrow().kind else { unreachable!() };
            t
        }
        _ => it.to_number(v)?,
    };
    if !t.is_finite() || t.abs() > 8.64e15 {
        throw!(it, "RangeError", "Invalid time value");
    }
    Ok(t)
}

fn setup_dtf(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "DateTimeFormat", false, |it, o, args| {
        let dtf = Rc::new(create_dtf(it, &arg(args, 0), &arg(args, 1), true, DtfDefaults::Date)?);
        let d = dtf.clone();
        let fmt = it.native("format", 1, move |it, _, args| {
            let t = date_arg(it, &arg(args, 0))?;
            Ok(Value::str(&d.format(t)))
        });
        o.set_hidden("format", fmt);
        let d = dtf.clone();
        method(it, o, "formatToParts", 1, move |it, _, args| {
            let t = date_arg(it, &arg(args, 0))?;
            let parts = d.to_parts(t);
            Ok(parts_array(it, &parts))
        });
        let d = dtf.clone();
        method(it, o, "formatRange", 2, move |it, _, args| {
            let a = date_arg(it, &arg(args, 0))?;
            let b = date_arg(it, &arg(args, 1))?;
            let (x, y) = (d.format(a), d.format(b));
            Ok(Value::str(&if x == y { x } else { format!("{x} – {y}") }))
        });
        let d = dtf.clone();
        method(it, o, "resolvedOptions", 0, move |it, _, _| {
            let c = if d.o.date_style.is_some() || d.o.time_style.is_some() { d.o.clone() } else { d.components() };
            let has_hour = c.hour.is_some() || d.o.time_style.is_some();
            let hc = d.hour_cycle().to_string();
            Ok(resolved(it, &[
                ("locale", Value::str(&base_name(&d.locale))),
                ("calendar", Value::str("gregory")),
                ("numberingSystem", Value::str("latn")),
                ("timeZone", Value::str(&d.zone.name)),
                ("hourCycle", if has_hour { Value::str(&hc) } else { Value::Undefined }),
                ("hour12", if has_hour { Value::Bool(hc == "h11" || hc == "h12") } else { Value::Undefined }),
                ("weekday", sv(&c.weekday)),
                ("era", sv(&c.era)),
                ("year", sv(&c.year)),
                ("month", sv(&c.month)),
                ("day", sv(&c.day)),
                ("hour", sv(&c.hour)),
                ("minute", sv(&c.minute)),
                ("second", sv(&c.second)),
                ("fractionalSecondDigits", c.fsd.map(|n| Value::Num(n as f64)).unwrap_or(Value::Undefined)),
                ("timeZoneName", sv(&c.tz_name)),
                ("dateStyle", sv(&d.o.date_style)),
                ("timeStyle", sv(&d.o.time_style)),
            ]))
        });
        Ok(())
    });
}

/// Time zone label for `Date.prototype.toString` ("Central Daylight Time" style where known).
pub fn create_dtf_name(lt: &tz::LocalType) -> String {
    match lt.abbr.as_str() {
        "EST" => "Eastern Standard Time".into(),
        "EDT" => "Eastern Daylight Time".into(),
        "CST" => "Central Standard Time".into(),
        "CDT" => "Central Daylight Time".into(),
        "MST" => "Mountain Standard Time".into(),
        "MDT" => "Mountain Daylight Time".into(),
        "PST" => "Pacific Standard Time".into(),
        "PDT" => "Pacific Daylight Time".into(),
        "GMT" => "Greenwich Mean Time".into(),
        "BST" => "British Summer Time".into(),
        "CET" => "Central European Standard Time".into(),
        "CEST" => "Central European Summer Time".into(),
        "EET" => "Eastern European Standard Time".into(),
        "EEST" => "Eastern European Summer Time".into(),
        "JST" => "Japan Standard Time".into(),
        "UTC" => "Coordinated Universal Time".into(),
        a if a.chars().all(|c| c.is_ascii_alphabetic()) && !a.is_empty() => a.to_string(),
        _ => tz::offset_abbr(lt.off),
    }
}

/// `Date.prototype.toLocale{,Date,Time}String`.
pub fn date_to_locale_string(it: &mut Interp, t: f64, locales: &Value, options: &Value, defaults: DtfDefaults) -> R<String> {
    if !t.is_finite() {
        return Ok("Invalid Date".into());
    }
    let mut dtf = create_dtf(it, locales, options, false, defaults)?;
    // toLocaleDateString with only time options still shows the date (and vice versa)
    let o = &mut dtf.o;
    let has_date = o.weekday.is_some() || o.year.is_some() || o.month.is_some() || o.day.is_some() || o.date_style.is_some();
    let has_time = o.hour.is_some() || o.minute.is_some() || o.second.is_some() || o.time_style.is_some();
    if defaults == DtfDefaults::Date && !has_date && o.time_style.is_none() {
        o.year = Some("numeric".into());
        o.month = Some("numeric".into());
        o.day = Some("numeric".into());
    }
    if defaults == DtfDefaults::Time && !has_time && o.date_style.is_none() {
        o.hour = Some("numeric".into());
        o.minute = Some("numeric".into());
        o.second = Some("numeric".into());
    }
    Ok(dtf.format(t))
}

// ---------------------------------------------------------------- NumberFormat

#[derive(Clone)]
pub struct Nf {
    locale: String,
    style: String,
    currency: Option<String>,
    currency_display: String,
    unit: Option<String>,
    unit_display: String,
    min_int: u32,
    min_frac: u32,
    max_frac: u32,
    min_sig: Option<u32>,
    max_sig: Option<u32>,
    grouping: bool,
    notation: String,
    compact_display: String,
    sign_display: String,
}

fn currency_digits(c: &str) -> u32 {
    match c {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "HUF" | "TWD" | "UGX" | "XAF" | "XOF" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" => 3,
        _ => 2,
    }
}

fn currency_symbol(c: &str, loc: &str, display: &str) -> String {
    if display == "code" {
        return c.to_string();
    }
    let region = region_of(loc);
    let narrow = display == "narrowSymbol";
    match c {
        "USD" => if region.is_none_or(|r| r == "US") || narrow || lang_of(loc) != "en" && lang_of(loc) != "fr" { "$" } else { "US$" },
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => if lang_of(loc) == "ja" { "￥" } else { "¥" },
        "CNY" => if lang_of(loc) == "zh" || narrow { "¥" } else { "CN¥" },
        "INR" => "₹",
        "KRW" => "₩",
        "CAD" => if region == Some("CA") || narrow { "$" } else { "CA$" },
        "AUD" => if region == Some("AU") || narrow { "$" } else { "A$" },
        "MXN" => if region == Some("MX") || narrow { "$" } else { "MX$" },
        "BRL" => "R$",
        "ILS" => "₪",
        "VND" => "₫",
        "PHP" => "₱",
        "NGN" if narrow => "₦",
        "RUB" if narrow || lang_of(loc) == "ru" => "₽",
        "PLN" if lang_of(loc) == "pl" || narrow => "zł",
        "SEK" | "NOK" | "DKK" if narrow => "kr",
        _ => return c.to_string(),
    }
    .to_string()
}

/// (group, decimal) separators.
fn separators(loc: &str) -> (&'static str, &'static str) {
    match (lang_of(loc), region_of(loc)) {
        ("de", Some("CH" | "LI")) => ("’", "."),
        ("de" | "es" | "it" | "nl" | "pt" | "id" | "tr" | "da" | "el" | "ro" | "hr" | "sl" | "sr" | "vi", _) => (".", ","),
        ("fr", _) => ("\u{202f}", ","),
        ("ru" | "uk" | "pl" | "cs" | "sk" | "fi" | "sv" | "nb" | "no" | "hu" | "bg" | "lt" | "lv" | "et", _) => ("\u{a0}", ","),
        _ => (",", "."),
    }
}

fn create_nf(it: &mut Interp, locales: &Value, options: &Value) -> R<Nf> {
    let locale = resolve_locale(it, locales)?;
    let opts = options_object(it, options)?;
    let style = opt_enum(it, &opts, "style", &["decimal", "percent", "currency", "unit"])?.unwrap_or_else(|| "decimal".into());
    let currency = opt_str(it, &opts, "currency")?.map(|c| c.to_ascii_uppercase());
    if style == "currency" && currency.is_none() {
        throw!(it, "TypeError", "Currency code is required with currency style.");
    }
    if let Some(c) = &currency {
        if c.len() != 3 || !c.chars().all(|x| x.is_ascii_alphabetic()) {
            throw!(it, "RangeError", "Invalid currency code : {c}");
        }
    }
    let currency_display = opt_enum(it, &opts, "currencyDisplay", &["code", "symbol", "narrowSymbol", "name"])?.unwrap_or_else(|| "symbol".into());
    let unit = opt_str(it, &opts, "unit")?;
    if style == "unit" && unit.is_none() {
        throw!(it, "TypeError", "Unit is required with unit style.");
    }
    let unit_display = opt_enum(it, &opts, "unitDisplay", &["short", "narrow", "long"])?.unwrap_or_else(|| "short".into());
    let notation = opt_enum(it, &opts, "notation", &["standard", "scientific", "engineering", "compact"])?.unwrap_or_else(|| "standard".into());
    let min_int = opt_num(it, &opts, "minimumIntegerDigits", 1.0, 21.0)?.unwrap_or(1);
    let mnfd = opt_num(it, &opts, "minimumFractionDigits", 0.0, 100.0)?;
    let mxfd = opt_num(it, &opts, "maximumFractionDigits", 0.0, 100.0)?;
    let min_sig = opt_num(it, &opts, "minimumSignificantDigits", 1.0, 21.0)?;
    let max_sig = opt_num(it, &opts, "maximumSignificantDigits", 1.0, 21.0)?;
    let (dmin, dmax) = match style.as_str() {
        "currency" => {
            let d = currency_digits(currency.as_deref().unwrap_or("USD"));
            (d, d)
        }
        "percent" => (0, 0),
        _ if notation == "compact" => (0, 0),
        _ => (0, 3),
    };
    let min_frac = mnfd.unwrap_or(dmin.min(mxfd.unwrap_or(dmin)));
    let max_frac = mxfd.unwrap_or(dmax.max(min_frac));
    if min_frac > max_frac {
        throw!(it, "RangeError", "maximumFractionDigits value is out of range.");
    }
    let grouping = if matches!(&opts, Value::Obj(_)) {
        let g = it.get_str(&opts, "useGrouping")?;
        match &g {
            Value::Undefined => true,
            Value::Str(s) => !s.is_empty() && &**s != "false",
            v => v.truthy(),
        }
    } else {
        true
    };
    let compact_display = opt_enum(it, &opts, "compactDisplay", &["short", "long"])?.unwrap_or_else(|| "short".into());
    let sign_display = opt_enum(it, &opts, "signDisplay", &["auto", "never", "always", "exceptZero", "negative"])?.unwrap_or_else(|| "auto".into());
    let (min_sig, max_sig) = match (min_sig, max_sig) {
        (None, None) if notation == "compact" && mnfd.is_none() && mxfd.is_none() => (Some(1), Some(2)),
        (None, None) => (None, None),
        (a, b) => (Some(a.unwrap_or(1)), Some(b.unwrap_or(21))),
    };
    Ok(Nf { locale, style, currency, currency_display, unit, unit_display, min_int, min_frac, max_frac, min_sig, max_sig, grouping, notation, compact_display, sign_display })
}

/// Round `x` (non-negative) to digits: returns (integer digits, fraction digits).
fn round_digits(x: f64, nf: &Nf) -> (String, String) {
    if let (Some(min_s), Some(max_s)) = (nf.min_sig, nf.max_sig) {
        if x == 0.0 {
            return ("0".into(), "0".repeat(min_s.saturating_sub(1) as usize));
        }
        let s = format!("{:.*e}", (max_s - 1) as usize, x);
        let (mant, exp) = s.split_once('e').unwrap();
        let exp: i32 = exp.parse().unwrap_or(0);
        let mut digits: String = mant.replace('.', "");
        // trim trailing zeros beyond the minimum
        while digits.len() > min_s as usize && digits.ends_with('0') {
            digits.pop();
        }
        let point = exp + 1;
        let (int, frac) = if point <= 0 {
            ("0".to_string(), "0".repeat((-point) as usize) + &digits)
        } else if point as usize >= digits.len() {
            (digits.clone() + &"0".repeat(point as usize - digits.len()), String::new())
        } else {
            (digits[..point as usize].to_string(), digits[point as usize..].to_string())
        };
        return (int, frac);
    }
    let s = format!("{:.*}", nf.max_frac as usize, x);
    let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((s.clone(), String::new()));
    let mut frac = frac;
    while frac.len() > nf.min_frac as usize && frac.ends_with('0') {
        frac.pop();
    }
    (int, frac)
}

fn group_int(int: &str, sep: &str, loc: &str, grouping: bool) -> Vec<(&'static str, String)> {
    let n = int.len();
    // Spanish/Polish don't group four-digit numbers
    let min_group = matches!(lang_of(loc), "es" | "pl" | "pt" if region_of(loc) == Some("PT") || lang_of(loc) != "pt");
    if !grouping || n <= 3 || (min_group && n == 4) {
        return vec![("integer", int.to_string())];
    }
    let indian = region_of(loc) == Some("IN");
    let mut groups = Vec::new();
    let mut end = n;
    let mut first = true;
    while end > 0 {
        let size = if first || !indian { 3 } else { 2 };
        let start = end.saturating_sub(size);
        groups.push(int[start..end].to_string());
        end = start;
        first = false;
    }
    groups.reverse();
    let mut out = Vec::new();
    for (i, g) in groups.into_iter().enumerate() {
        if i > 0 {
            out.push(("group", sep.to_string()));
        }
        out.push(("integer", g));
    }
    out
}

const UNITS: &[(&str, &str, &str, &str)] = &[
    // unit, short, long singular, long plural
    ("percent", "%", "percent", "percent"),
    ("second", "sec", "second", "seconds"),
    ("minute", "min", "minute", "minutes"),
    ("hour", "hr", "hour", "hours"),
    ("day", "day", "day", "days"),
    ("week", "wk", "week", "weeks"),
    ("month", "mth", "month", "months"),
    ("year", "yr", "year", "years"),
    ("millisecond", "ms", "millisecond", "milliseconds"),
    ("meter", "m", "meter", "meters"),
    ("kilometer", "km", "kilometer", "kilometers"),
    ("centimeter", "cm", "centimeter", "centimeters"),
    ("millimeter", "mm", "millimeter", "millimeters"),
    ("mile", "mi", "mile", "miles"),
    ("foot", "ft", "foot", "feet"),
    ("inch", "in", "inch", "inches"),
    ("kilogram", "kg", "kilogram", "kilograms"),
    ("gram", "g", "gram", "grams"),
    ("pound", "lb", "pound", "pounds"),
    ("liter", "L", "liter", "liters"),
    ("milliliter", "mL", "milliliter", "milliliters"),
    ("celsius", "°C", "degree Celsius", "degrees Celsius"),
    ("fahrenheit", "°F", "degree Fahrenheit", "degrees Fahrenheit"),
    ("byte", "byte", "byte", "bytes"),
    ("kilobyte", "kB", "kilobyte", "kilobytes"),
    ("megabyte", "MB", "megabyte", "megabytes"),
    ("gigabyte", "GB", "gigabyte", "gigabytes"),
    ("terabyte", "TB", "terabyte", "terabytes"),
    ("kilometer-per-hour", "km/h", "kilometer per hour", "kilometers per hour"),
    ("mile-per-hour", "mph", "mile per hour", "miles per hour"),
];

impl Nf {
    pub fn to_parts(&self, n: f64) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let (gsep, dsep) = separators(&self.locale);
        let lang = lang_of(&self.locale);
        let mut x = n;
        if self.style == "percent" {
            x *= 100.0;
        }
        let neg = x < 0.0 || (x == 0.0 && x.is_sign_negative());
        let mut ax = x.abs();
        // compact notation
        let mut compact = None;
        if self.notation == "compact" && ax.is_finite() {
            let long = self.compact_display == "long";
            let table: [(f64, &str, &str); 4] = [(1e12, "T", " trillion"), (1e9, "B", " billion"), (1e6, "M", " million"), (1e3, "K", " thousand")];
            for (scale, s, l) in table {
                // round first so 999_999 becomes 1M rather than 1000K
                let scaled = ax / scale;
                if scaled >= 1.0 || (ax / scale * 1000.0).round() >= 999.5 && scale < 1e12 && ax >= scale * 0.9995 {
                    ax = scaled;
                    compact = Some(if long { l } else { s });
                    break;
                }
            }
            if compact.is_some() && ax >= 100.0 {
                ax = ax.round();
            }
        }
        let mut sci_exp = None;
        if matches!(self.notation.as_str(), "scientific" | "engineering") && ax != 0.0 && ax.is_finite() {
            let mut e = ax.log10().floor() as i32;
            if self.notation == "engineering" {
                e = e.div_euclid(3) * 3;
            }
            ax /= 10f64.powi(e);
            sci_exp = Some(e);
        }
        let (int, frac) = if ax.is_finite() { round_digits(ax, self) } else { (String::new(), String::new()) };
        let is_zero = ax.is_finite() && int.chars().all(|c| c == '0') && frac.chars().all(|c| c == '0');
        let sign = match self.sign_display.as_str() {
            "never" => None,
            "always" => Some(if neg { "-" } else { "+" }),
            "exceptZero" if is_zero => None,
            "exceptZero" => Some(if neg { "-" } else { "+" }),
            _ if neg && !is_zero || neg && self.sign_display == "auto" && n.is_sign_negative() && !is_zero => Some("-"),
            _ => None,
        };
        let sign_part = sign.map(|s| (if s == "-" { "minusSign" } else { "plusSign" }, s.to_string()));
        let mut body: Vec<(&'static str, String)> = Vec::new();
        if ax.is_nan() {
            body.push(("nan", "NaN".into()));
        } else if ax.is_infinite() {
            body.push(("infinity", "∞".into()));
        } else {
            let int = if (int.len() as u32) < self.min_int { "0".repeat((self.min_int - int.len() as u32) as usize) + &int } else { int };
            body.extend(group_int(&int, gsep, &self.locale, self.grouping));
            if !frac.is_empty() {
                body.push(("decimal", dsep.to_string()));
                body.push(("fraction", frac));
            }
            if let Some(c) = compact {
                if c.starts_with(' ') {
                    body.push(("literal", " ".into()));
                    body.push(("compact", c.trim().to_string()));
                } else {
                    body.push(("compact", c.to_string()));
                }
            }
            if let Some(e) = sci_exp {
                body.push(("exponentSeparator", "E".into()));
                if e < 0 {
                    body.push(("exponentMinusSign", "-".into()));
                }
                body.push(("exponentInteger", e.abs().to_string()));
            }
        }
        let suffix_locale = !matches!(lang, "en" | "ja" | "zh" | "ko" | "he" | "th" | "hi") || (lang == "en" && false);
        match self.style.as_str() {
            "currency" => {
                let code = self.currency.clone().unwrap_or_default();
                if self.currency_display == "name" {
                    out.extend(sign_part);
                    out.extend(body);
                    out.push(("literal", " ".into()));
                    let name = match code.as_str() {
                        "USD" => "US dollars",
                        "EUR" => "euros",
                        "GBP" => "British pounds",
                        "JPY" => "Japanese yen",
                        "CAD" => "Canadian dollars",
                        "AUD" => "Australian dollars",
                        "INR" => "Indian rupees",
                        "CNY" => "Chinese yuan",
                        _ => code.as_str(),
                    };
                    out.push(("currency", name.to_string()));
                    return out;
                }
                let sym = currency_symbol(&code, &self.locale, &self.currency_display);
                if suffix_locale {
                    out.extend(sign_part);
                    out.extend(body);
                    out.push(("literal", "\u{a0}".into()));
                    out.push(("currency", sym));
                } else {
                    out.extend(sign_part);
                    out.push(("currency", sym.clone()));
                    if sym.chars().all(|c| c.is_ascii_alphabetic()) {
                        out.push(("literal", "\u{a0}".into()));
                    }
                    out.extend(body);
                }
            }
            "percent" => {
                out.extend(sign_part);
                out.extend(body);
                if matches!(lang, "de" | "fr" | "sv" | "nb" | "no" | "fi" | "da" | "cs" | "sk" | "ru" | "es") {
                    out.push(("literal", if lang == "fr" { "\u{202f}" } else { "\u{a0}" }.into()));
                }
                out.push(("percentSign", "%".into()));
            }
            "unit" => {
                out.extend(sign_part);
                out.extend(body);
                let unit = self.unit.clone().unwrap_or_default();
                let row = UNITS.iter().find(|u| u.0 == unit);
                let one = (ax - 1.0).abs() < f64::EPSILON && frac_is_empty(&out);
                let label = match (row, self.unit_display.as_str()) {
                    (Some(r), "long") => if one { r.2 } else { r.3 }.to_string(),
                    (Some(r), "narrow") => r.1.to_string(),
                    (Some(r), _) => {
                        if matches!(r.0, "day" | "week" | "month" | "year" | "hour") && !one {
                            match r.0 {
                                "day" => "days".into(),
                                "week" => "wks".into(),
                                "month" => "mths".into(),
                                "year" => "yrs".into(),
                                _ => "hr".into(),
                            }
                        } else {
                            r.1.to_string()
                        }
                    }
                    (None, _) => unit.clone(),
                };
                let tight = self.unit_display == "narrow" && row.is_some_and(|r| !matches!(r.0, "day" | "week" | "month" | "year" | "byte")) || matches!(unit.as_str(), "percent" | "celsius" | "fahrenheit") && self.unit_display != "long";
                if !tight {
                    out.push(("literal", " ".into()));
                }
                out.push(("unit", label));
            }
            _ => {
                out.extend(sign_part);
                out.extend(body);
            }
        }
        out
    }

    pub fn format(&self, n: f64) -> String {
        join_parts(&self.to_parts(n))
    }
}

fn frac_is_empty(parts: &[(&'static str, String)]) -> bool {
    !parts.iter().any(|(t, _)| *t == "fraction")
}

fn nf_value(it: &mut Interp, v: &Value) -> R<f64> {
    match v {
        Value::BigInt(_) => {
            let s = it.to_str(v)?;
            Ok(s.parse::<f64>().unwrap_or(f64::NAN))
        }
        Value::Str(s) => Ok(s.trim().parse::<f64>().unwrap_or_else(|_| if s.trim().is_empty() { 0.0 } else { f64::NAN })),
        _ => it.to_number(v),
    }
}

fn setup_nf(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "NumberFormat", false, |it, o, args| {
        let nf = Rc::new(create_nf(it, &arg(args, 0), &arg(args, 1))?);
        let f = nf.clone();
        let fmt = it.native("format", 1, move |it, _, args| {
            let n = nf_value(it, &arg(args, 0))?;
            Ok(Value::str(&f.format(n)))
        });
        o.set_hidden("format", fmt);
        let f = nf.clone();
        method(it, o, "formatToParts", 1, move |it, _, args| {
            let n = nf_value(it, &arg(args, 0))?;
            let parts = f.to_parts(n);
            Ok(parts_array(it, &parts))
        });
        let f = nf.clone();
        method(it, o, "formatRange", 2, move |it, _, args| {
            let a = nf_value(it, &arg(args, 0))?;
            let b = nf_value(it, &arg(args, 1))?;
            Ok(Value::str(&format!("{}–{}", f.format(a), f.format(b))))
        });
        let f = nf.clone();
        method(it, o, "resolvedOptions", 0, move |it, _, _| {
            Ok(resolved(it, &[
                ("locale", Value::str(&base_name(&f.locale))),
                ("numberingSystem", Value::str("latn")),
                ("style", Value::str(&f.style)),
                ("currency", sv(&f.currency)),
                ("currencyDisplay", if f.currency.is_some() { Value::str(&f.currency_display) } else { Value::Undefined }),
                ("unit", sv(&f.unit)),
                ("minimumIntegerDigits", Value::Num(f.min_int as f64)),
                ("minimumFractionDigits", Value::Num(f.min_frac as f64)),
                ("maximumFractionDigits", Value::Num(f.max_frac as f64)),
                ("minimumSignificantDigits", f.min_sig.map(|n| Value::Num(n as f64)).unwrap_or(Value::Undefined)),
                ("maximumSignificantDigits", f.max_sig.map(|n| Value::Num(n as f64)).unwrap_or(Value::Undefined)),
                ("useGrouping", if f.grouping { Value::str("auto") } else { Value::Bool(false) }),
                ("notation", Value::str(&f.notation)),
                ("signDisplay", Value::str(&f.sign_display)),
            ]))
        });
        Ok(())
    });
}

/// `Number.prototype.toLocaleString`.
pub fn number_to_locale_string(it: &mut Interp, n: f64, locales: &Value, options: &Value) -> R<String> {
    Ok(create_nf(it, locales, options)?.format(n))
}

// ---------------------------------------------------------------- PluralRules

/// CLDR plural category for `n` in `lang`. `v` = number of visible fraction digits.
pub fn plural_category(lang: &str, n: f64, v: usize, ordinal: bool) -> &'static str {
    let i = n.abs().trunc() as i64;
    let int = v == 0;
    if ordinal {
        return match lang {
            "en" => match (i % 10, i % 100) {
                (1, x) if x != 11 => "one",
                (2, x) if x != 12 => "two",
                (3, x) if x != 13 => "few",
                _ => "other",
            },
            "fr" => if i == 1 { "one" } else { "other" },
            "it" => if matches!(i, 11 | 8 | 80 | 800) { "many" } else { "other" },
            "sv" => if matches!(i % 10, 1 | 2) && !matches!(i % 100, 11 | 12) { "one" } else { "other" },
            _ => "other",
        };
    }
    match lang {
        "ja" | "zh" | "ko" | "th" | "vi" | "id" | "ms" | "lo" | "my" | "yue" => "other",
        "fr" | "hy" | "kab" => if i == 0 || i == 1 { "one" } else if int && i != 0 && i % 1_000_000 == 0 { "many" } else { "other" },
        "pt" => if (i == 0 || i == 1) && region_neutral_pt() { "one" } else if int && i != 0 && i % 1_000_000 == 0 { "many" } else { "other" },
        "es" | "it" | "ca" => if n == 1.0 && int { "one" } else if int && i != 0 && i % 1_000_000 == 0 { "many" } else { "other" },
        "ru" | "uk" | "be" => {
            if !int {
                "other"
            } else if i % 10 == 1 && i % 100 != 11 {
                "one"
            } else if (2..=4).contains(&(i % 10)) && !(12..=14).contains(&(i % 100)) {
                "few"
            } else {
                "many"
            }
        }
        "pl" => {
            if !int {
                "other"
            } else if i == 1 {
                "one"
            } else if (2..=4).contains(&(i % 10)) && !(12..=14).contains(&(i % 100)) {
                "few"
            } else {
                "many"
            }
        }
        "cs" | "sk" => if !int { "many" } else if i == 1 { "one" } else if (2..=4).contains(&i) { "few" } else { "other" },
        "ar" => {
            if !int {
                "other"
            } else {
                match (i, i % 100) {
                    (0, _) => "zero",
                    (1, _) => "one",
                    (2, _) => "two",
                    (_, 3..=10) => "few",
                    (_, 11..=99) => "many",
                    _ => "other",
                }
            }
        }
        "he" => if i == 1 && int { "one" } else if i == 2 && int { "two" } else { "other" },
        "lv" => if i % 10 == 0 || (11..=19).contains(&(i % 100)) { "zero" } else if i % 10 == 1 && i % 100 != 11 { "one" } else { "other" },
        "lt" => if !int { "many" } else if i % 10 == 1 && !(11..=19).contains(&(i % 100)) { "one" } else if (2..=9).contains(&(i % 10)) && !(11..=19).contains(&(i % 100)) { "few" } else { "other" },
        "ro" => if i == 1 && int { "one" } else if !int || n == 0.0 || (2..=19).contains(&(i % 100)) { "few" } else { "other" },
        "ga" => match i { 1 => "one", 2 => "two", 3..=6 => "few", 7..=10 => "many", _ => "other" },
        "cy" => match i { 0 => "zero", 1 => "one", 2 => "two", 3 => "few", 6 => "many", _ => "other" },
        "hi" | "bn" | "fa" | "am" | "zu" => if i == 0 || n == 1.0 { "one" } else { "other" },
        _ => if i == 1 && int { "one" } else { "other" },
    }
}

fn region_neutral_pt() -> bool {
    true
}

fn plural_categories(lang: &str, ordinal: bool) -> Vec<&'static str> {
    let mut cats: Vec<&'static str> = Vec::new();
    for n in [0.0, 1.0, 2.0, 3.0, 5.0, 6.0, 7.0, 11.0, 12.0, 13.0, 21.0, 22.0, 23.0, 100.0, 101.0, 1e6] {
        for v in [0usize, 1] {
            let c = plural_category(lang, n + if v == 1 { 0.5 } else { 0.0 }, v, ordinal);
            if !cats.contains(&c) {
                cats.push(c);
            }
        }
    }
    let order = ["zero", "one", "two", "few", "many", "other"];
    cats.sort_by_key(|c| order.iter().position(|o| o == c));
    cats
}

fn setup_plural(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "PluralRules", true, |it, o, args| {
        let locale = resolve_locale(it, &arg(args, 0))?;
        let opts = options_object(it, &arg(args, 1))?;
        let ty = opt_enum(it, &opts, "type", &["cardinal", "ordinal"])?.unwrap_or_else(|| "cardinal".into());
        let nf = Rc::new(create_nf(it, &Value::str(&locale), &opts)?);
        let ordinal = ty == "ordinal";
        let lang = lang_of(&locale).to_string();
        let (l, f) = (lang.clone(), nf.clone());
        method(it, o, "select", 1, move |it, _, args| {
            let n = it.to_number(&arg(args, 0))?;
            if !n.is_finite() {
                return Ok(Value::str("other"));
            }
            let (_, frac) = round_digits(n.abs(), &f);
            let v = frac.len();
            let rounded: f64 = format!("{}.{}", n.abs().trunc() as i64, if frac.is_empty() { "0" } else { &frac }).parse().unwrap_or(n.abs());
            Ok(Value::str(plural_category(&l, rounded, v, ordinal)))
        });
        let l = lang.clone();
        method(it, o, "selectRange", 2, move |it, _, args| {
            let n = it.to_number(&arg(args, 1))?;
            Ok(Value::str(plural_category(&l, n.abs(), if n.fract() == 0.0 { 0 } else { 1 }, ordinal)))
        });
        let (loc, t) = (locale.clone(), ty.clone());
        method(it, o, "resolvedOptions", 0, move |it, _, _| {
            let cats: Vec<Value> = plural_categories(lang_of(&loc), ordinal).into_iter().map(Value::str).collect();
            let cats = it.new_array(cats);
            Ok(resolved(it, &[("locale", Value::str(&base_name(&loc))), ("type", Value::str(&t)), ("minimumIntegerDigits", Value::Num(1.0)), ("minimumFractionDigits", Value::Num(0.0)), ("maximumFractionDigits", Value::Num(3.0)), ("pluralCategories", cats)]))
        });
        Ok(())
    });
}

// ---------------------------------------------------------------- Collator

#[derive(Clone)]
pub struct Collator {
    pub numeric: bool,
    /// "base" | "accent" | "case" | "variant"
    pub sensitivity: String,
    pub ignore_punctuation: bool,
    pub upper_first: bool,
}

impl Default for Collator {
    fn default() -> Self {
        Collator { numeric: false, sensitivity: "variant".into(), ignore_punctuation: false, upper_first: false }
    }
}

/// Strip a Latin diacritic: returns (base letter, accent mark id).
fn decompose(c: char) -> (char, u32) {
    const TABLE: &[(&str, char)] = &[
        ("àáâãäåāăą", 'a'),
        ("çćĉċč", 'c'),
        ("ďđ", 'd'),
        ("èéêëēĕėęě", 'e'),
        ("ĝğġģ", 'g'),
        ("ĥħ", 'h'),
        ("ìíîïĩīĭįı", 'i'),
        ("ĵ", 'j'),
        ("ķ", 'k'),
        ("ĺļľŀł", 'l'),
        ("ñńņňŉ", 'n'),
        ("òóôõöøōŏő", 'o'),
        ("ŕŗř", 'r'),
        ("śŝşšș", 's'),
        ("ţťŧț", 't'),
        ("ùúûüũūŭůűų", 'u'),
        ("ŵ", 'w'),
        ("ýÿŷ", 'y'),
        ("źżž", 'z'),
    ];
    let lower: char = c.to_lowercase().next().unwrap_or(c);
    for (set, base) in TABLE {
        if let Some(pos) = set.chars().position(|x| x == lower) {
            return (*base, pos as u32 + 1);
        }
    }
    (lower, 0)
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum Primary {
    Space(u32),
    Punct(u32),
    Number(String),
    Letter(u32),
}

fn primary_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() {
        2
    } else {
        1
    }
}

impl Collator {
    fn keys(&self, s: &str) -> Vec<(Primary, u32, bool)> {
        let chars: Vec<char> = s.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if self.numeric && c.is_ascii_digit() {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let digits: String = chars[start..i].iter().collect();
                let trimmed = digits.trim_start_matches('0');
                let t = if trimmed.is_empty() { "0" } else { trimmed };
                // length-prefixed so longer numbers sort after shorter ones
                out.push((Primary::Number(format!("{:04}{t}", t.len())), 0, false));
                continue;
            }
            i += 1;
            match primary_class(c) {
                0 => {
                    if !self.ignore_punctuation {
                        out.push((Primary::Space(c as u32), 0, false));
                    }
                }
                1 => {
                    if !self.ignore_punctuation {
                        out.push((Primary::Punct(c as u32), 0, false));
                    }
                }
                _ => {
                    if c.is_ascii_digit() {
                        out.push((Primary::Number(format!("0001{c}")), 0, false));
                        continue;
                    }
                    let (base, accent) = decompose(c);
                    let upper = c.is_uppercase();
                    // ß sorts as "ss"
                    if base == 'ß' {
                        out.push((Primary::Letter('s' as u32), 1, upper));
                        out.push((Primary::Letter('s' as u32), 1, upper));
                    } else {
                        out.push((Primary::Letter(base as u32), accent, upper));
                    }
                }
            }
        }
        out
    }

    pub fn compare(&self, a: &str, b: &str) -> Ordering {
        let ka = self.keys(a);
        let kb = self.keys(b);
        // primary
        let pa: Vec<&Primary> = ka.iter().map(|k| &k.0).collect();
        let pb: Vec<&Primary> = kb.iter().map(|k| &k.0).collect();
        let o = pa.cmp(&pb);
        if o != Ordering::Equal || self.sensitivity == "base" {
            return o;
        }
        if self.sensitivity != "case" {
            let o = ka.iter().map(|k| k.1).cmp(kb.iter().map(|k| k.1));
            if o != Ordering::Equal || self.sensitivity == "accent" {
                return o;
            }
        }
        let o = ka.iter().map(|k| k.2 != self.upper_first).cmp(kb.iter().map(|k| k.2 != self.upper_first));
        if o != Ordering::Equal || self.sensitivity == "case" {
            return o;
        }
        if self.ignore_punctuation {
            return Ordering::Equal;
        }
        Ordering::Equal
    }
}

pub fn create_collator(it: &mut Interp, locales: &Value, options: &Value) -> R<(String, Collator)> {
    let locale = resolve_locale(it, locales)?;
    let opts = options_object(it, options)?;
    let mut c = Collator::default();
    if locale.contains("-kn") {
        c.numeric = !locale.contains("-kn-false");
    }
    if let Some(n) = opt_bool(it, &opts, "numeric")? {
        c.numeric = n;
    }
    if let Some(s) = opt_enum(it, &opts, "sensitivity", &["base", "accent", "case", "variant"])? {
        c.sensitivity = s;
    }
    let _ = opt_enum(it, &opts, "usage", &["sort", "search"])?;
    if let Some(p) = opt_bool(it, &opts, "ignorePunctuation")? {
        c.ignore_punctuation = p;
    }
    if let Some(cf) = opt_enum(it, &opts, "caseFirst", &["upper", "lower", "false"])? {
        c.upper_first = cf == "upper";
    }
    Ok((locale, c))
}

fn ord_value(o: Ordering) -> Value {
    Value::Num(match o {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    })
}

fn setup_collator(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "Collator", false, |it, o, args| {
        let (locale, c) = create_collator(it, &arg(args, 0), &arg(args, 1))?;
        let c = Rc::new(c);
        let cc = c.clone();
        let cmp = it.native("compare", 2, move |it, _, args| {
            let a = it.to_str(&arg(args, 0))?;
            let b = it.to_str(&arg(args, 1))?;
            Ok(ord_value(cc.compare(&a, &b)))
        });
        o.set_hidden("compare", cmp);
        method(it, o, "resolvedOptions", 0, move |it, _, _| {
            Ok(resolved(it, &[
                ("locale", Value::str(&base_name(&locale))),
                ("usage", Value::str("sort")),
                ("sensitivity", Value::str(&c.sensitivity)),
                ("ignorePunctuation", Value::Bool(c.ignore_punctuation)),
                ("collation", Value::str("default")),
                ("numeric", Value::Bool(c.numeric)),
                ("caseFirst", Value::str(if c.upper_first { "upper" } else { "false" })),
            ]))
        });
        Ok(())
    });
}

/// `String.prototype.localeCompare`.
pub fn locale_compare(it: &mut Interp, a: &str, b: &str, locales: &Value, options: &Value) -> R<Value> {
    let c = if locales.is_undefined() && options.is_undefined() { Collator::default() } else { create_collator(it, locales, options)?.1 };
    Ok(ord_value(c.compare(a, b)))
}

// ---------------------------------------------------------------- RelativeTimeFormat / ListFormat

fn setup_relative(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "RelativeTimeFormat", true, |it, o, args| {
        let locale = resolve_locale(it, &arg(args, 0))?;
        let opts = options_object(it, &arg(args, 1))?;
        let numeric = opt_enum(it, &opts, "numeric", &["always", "auto"])?.unwrap_or_else(|| "always".into());
        let style = opt_enum(it, &opts, "style", &["long", "short", "narrow"])?.unwrap_or_else(|| "long".into());
        let nf = Rc::new(create_nf(it, &Value::str(&locale), &Value::Undefined)?);
        let (num2, sty2, nf2) = (numeric.clone(), style.clone(), nf.clone());
        let fmt = move |it: &mut Interp, args: &[Value]| -> R<Vec<(&'static str, String)>> {
            let v = it.to_number(&arg(args, 0))?;
            if !v.is_finite() {
                throw!(it, "RangeError", "Invalid value");
            }
            let unit = it.to_str(&arg(args, 1))?;
            let unit = unit.trim_end_matches('s').to_string();
            if !matches!(unit.as_str(), "second" | "minute" | "hour" | "day" | "week" | "month" | "quarter" | "year") {
                throw!(it, "RangeError", "Invalid unit argument for format() '{unit}'");
            }
            if num2 == "auto" {
                let special = match (unit.as_str(), v) {
                    ("day", x) if x == 0.0 => Some("today"),
                    ("day", x) if x == 1.0 => Some("tomorrow"),
                    ("day", x) if x == -1.0 => Some("yesterday"),
                    ("second", x) if x == 0.0 => Some("now"),
                    (u, x) if x == 0.0 && u != "minute" && u != "hour" => Some(match u { "week" => "this week", "month" => "this month", "quarter" => "this quarter", _ => "this year" }),
                    (u, x) if x == 1.0 && u != "minute" && u != "hour" && u != "second" => Some(match u { "week" => "next week", "month" => "next month", "quarter" => "next quarter", _ => "next year" }),
                    (u, x) if x == -1.0 && u != "minute" && u != "hour" && u != "second" => Some(match u { "week" => "last week", "month" => "last month", "quarter" => "last quarter", _ => "last year" }),
                    ("minute", x) if x == 0.0 => Some("this minute"),
                    ("hour", x) if x == 0.0 => Some("this hour"),
                    _ => None,
                };
                if let Some(s) = special {
                    return Ok(vec![("literal", s.to_string())]);
                }
            }
            let past = v < 0.0 || (v == 0.0 && v.is_sign_negative());
            let n = nf2.format(v.abs());
            let one = v.abs() == 1.0;
            let label = if sty2 == "long" {
                if one { unit.clone() } else { format!("{unit}s") }
            } else {
                match unit.as_str() {
                    "second" => "sec.".into(),
                    "minute" => "min.".into(),
                    "hour" => "hr.".into(),
                    "day" => if one { "day".into() } else { "days".into() },
                    "week" => "wk.".into(),
                    "month" => "mo.".into(),
                    "quarter" => if one { "qtr.".into() } else { "qtrs.".into() },
                    _ => "yr.".into(),
                }
            };
            Ok(if past {
                vec![("integer", n), ("literal", format!(" {label} ago"))]
            } else {
                vec![("literal", "in ".into()), ("integer", n), ("literal", format!(" {label}"))]
            })
        };
        let fmt = Rc::new(fmt);
        let f = fmt.clone();
        method(it, o, "format", 2, move |it, _, args| Ok(Value::str(&join_parts(&f(it, args)?))));
        let f = fmt.clone();
        method(it, o, "formatToParts", 2, move |it, _, args| {
            let p = f(it, args)?;
            Ok(parts_array(it, &p))
        });
        method(it, o, "resolvedOptions", 0, move |it, _, _| Ok(resolved(it, &[("locale", Value::str(&base_name(&locale))), ("style", Value::str(&style)), ("numeric", Value::str(&numeric)), ("numberingSystem", Value::str("latn"))])));
        Ok(())
    });
    intl_ctor(it, intl, "ListFormat", true, |it, o, args| {
        let locale = resolve_locale(it, &arg(args, 0))?;
        let opts = options_object(it, &arg(args, 1))?;
        let ty = opt_enum(it, &opts, "type", &["conjunction", "disjunction", "unit"])?.unwrap_or_else(|| "conjunction".into());
        let style = opt_enum(it, &opts, "style", &["long", "short", "narrow"])?.unwrap_or_else(|| "long".into());
        let (lang, t2, s2) = (lang_of(&locale).to_string(), ty.clone(), style.clone());
        let fmt = Rc::new(move |it: &mut Interp, list: &Value| -> R<Vec<(&'static str, String)>> {
            let items = it.iterate_to_vec(list)?;
            let mut strs = Vec::new();
            for v in items {
                if !matches!(v, Value::Str(_)) {
                    throw!(it, "TypeError", "Iterable yielded {} which is not a string", it.to_string_lossy(&v));
                }
                strs.push(it.to_str(&v)?.to_string());
            }
            let word = match (t2.as_str(), lang.as_str()) {
                ("unit", _) => "",
                ("disjunction", "de") => "oder",
                ("disjunction", "fr") => "ou",
                ("disjunction", "es") => "o",
                ("disjunction", "it") => "o",
                ("disjunction", "nl") => "of",
                ("disjunction", _) => "or",
                (_, "de") => "und",
                (_, "fr") => "et",
                (_, "es") => "y",
                (_, "it") => "e",
                (_, "pt") => "e",
                (_, "nl") => "en",
                _ => if s2 == "long" { "and" } else if s2 == "short" { "&" } else { "" },
            };
            let oxford = lang == "en";
            let mut out = Vec::new();
            let n = strs.len();
            for (i, s) in strs.into_iter().enumerate() {
                if i > 0 {
                    let last = i == n - 1;
                    let sep = if !last || word.is_empty() {
                        if t2 == "unit" && s2 == "narrow" { " ".to_string() } else { ", ".to_string() }
                    } else if n > 2 && oxford {
                        format!(", {word} ")
                    } else {
                        format!(" {word} ")
                    };
                    out.push(("literal", sep));
                }
                out.push(("element", s));
            }
            Ok(out)
        });
        let f = fmt.clone();
        method(it, o, "format", 1, move |it, _, args| Ok(Value::str(&join_parts(&f(it, &arg(args, 0))?))));
        let f = fmt.clone();
        method(it, o, "formatToParts", 1, move |it, _, args| {
            let p = f(it, &arg(args, 0))?;
            Ok(parts_array(it, &p))
        });
        method(it, o, "resolvedOptions", 0, move |it, _, _| Ok(resolved(it, &[("locale", Value::str(&base_name(&locale))), ("type", Value::str(&ty)), ("style", Value::str(&style))])));
        Ok(())
    });
}

// ---------------------------------------------------------------- DisplayNames / Locale

const LANGUAGES: &[(&str, &str, &str)] = &[
    // code, English name, autonym
    ("ar", "Arabic", "العربية"),
    ("bg", "Bulgarian", "български"),
    ("bn", "Bangla", "বাংলা"),
    ("ca", "Catalan", "català"),
    ("cs", "Czech", "čeština"),
    ("da", "Danish", "dansk"),
    ("de", "German", "Deutsch"),
    ("el", "Greek", "Ελληνικά"),
    ("en", "English", "English"),
    ("es", "Spanish", "español"),
    ("et", "Estonian", "eesti"),
    ("fa", "Persian", "فارسی"),
    ("fi", "Finnish", "suomi"),
    ("fil", "Filipino", "Filipino"),
    ("fr", "French", "français"),
    ("he", "Hebrew", "עברית"),
    ("hi", "Hindi", "हिन्दी"),
    ("hr", "Croatian", "hrvatski"),
    ("hu", "Hungarian", "magyar"),
    ("id", "Indonesian", "Indonesia"),
    ("it", "Italian", "italiano"),
    ("ja", "Japanese", "日本語"),
    ("ko", "Korean", "한국어"),
    ("lt", "Lithuanian", "lietuvių"),
    ("lv", "Latvian", "latviešu"),
    ("ms", "Malay", "Melayu"),
    ("nb", "Norwegian Bokmål", "norsk bokmål"),
    ("nl", "Dutch", "Nederlands"),
    ("no", "Norwegian", "norsk"),
    ("pl", "Polish", "polski"),
    ("pt", "Portuguese", "português"),
    ("ro", "Romanian", "română"),
    ("ru", "Russian", "русский"),
    ("sk", "Slovak", "slovenčina"),
    ("sl", "Slovenian", "slovenščina"),
    ("sr", "Serbian", "српски"),
    ("sv", "Swedish", "svenska"),
    ("sw", "Swahili", "Kiswahili"),
    ("ta", "Tamil", "தமிழ்"),
    ("th", "Thai", "ไทย"),
    ("tr", "Turkish", "Türkçe"),
    ("uk", "Ukrainian", "українська"),
    ("ur", "Urdu", "اردو"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("zh", "Chinese", "中文"),
];

const REGIONS: &[(&str, &str)] = &[
    ("AR", "Argentina"), ("AT", "Austria"), ("AU", "Australia"), ("BE", "Belgium"), ("BR", "Brazil"), ("CA", "Canada"), ("CH", "Switzerland"),
    ("CL", "Chile"), ("CN", "China"), ("CO", "Colombia"), ("CZ", "Czechia"), ("DE", "Germany"), ("DK", "Denmark"), ("EG", "Egypt"),
    ("ES", "Spain"), ("FI", "Finland"), ("FR", "France"), ("GB", "United Kingdom"), ("GR", "Greece"), ("HK", "Hong Kong"),
    ("HU", "Hungary"), ("ID", "Indonesia"), ("IE", "Ireland"), ("IL", "Israel"), ("IN", "India"), ("IT", "Italy"), ("JP", "Japan"),
    ("KR", "South Korea"), ("MX", "Mexico"), ("MY", "Malaysia"), ("NG", "Nigeria"), ("NL", "Netherlands"), ("NO", "Norway"),
    ("NZ", "New Zealand"), ("PH", "Philippines"), ("PK", "Pakistan"), ("PL", "Poland"), ("PT", "Portugal"), ("RO", "Romania"),
    ("RU", "Russia"), ("SA", "Saudi Arabia"), ("SE", "Sweden"), ("SG", "Singapore"), ("TH", "Thailand"), ("TR", "Türkiye"),
    ("TW", "Taiwan"), ("UA", "Ukraine"), ("US", "United States"), ("VN", "Vietnam"), ("ZA", "South Africa"),
];

fn language_name(display_lang: &str, code: &str) -> Option<String> {
    let canon = canonical_locale(code)?;
    let lang = lang_of(&canon);
    let row = LANGUAGES.iter().find(|r| r.0 == lang)?;
    let base = if display_lang == lang { row.2 } else if display_lang == "en" { row.1 } else { row.1 };
    match region_of(&canon) {
        Some(r) => {
            let rn = REGIONS.iter().find(|x| x.0 == r).map(|x| x.1).unwrap_or(r);
            Some(format!("{base} ({rn})"))
        }
        None => Some(base.to_string()),
    }
}

fn setup_display_names(it: &mut Interp, intl: &Obj) {
    intl_ctor(it, intl, "DisplayNames", true, |it, o, args| {
        let locale = resolve_locale(it, &arg(args, 0))?;
        let opts = options_object(it, &arg(args, 1))?;
        let Some(ty) = opt_enum(it, &opts, "type", &["language", "region", "script", "currency", "calendar", "dateTimeField"])? else {
            throw!(it, "TypeError", "Required option 'type' is missing");
        };
        let fallback = opt_enum(it, &opts, "fallback", &["code", "none"])?.unwrap_or_else(|| "code".into());
        let (loc, t, fb) = (locale.clone(), ty.clone(), fallback.clone());
        method(it, o, "of", 1, move |it, _, args| {
            let code = it.to_str(&arg(args, 0))?.to_string();
            let lang = lang_of(&loc);
            let name = match t.as_str() {
                "language" => language_name(lang, &code),
                "region" => REGIONS.iter().find(|r| r.0.eq_ignore_ascii_case(&code)).map(|r| r.1.to_string()),
                "currency" => match code.to_ascii_uppercase().as_str() {
                    "USD" => Some("US Dollar".into()),
                    "EUR" => Some("Euro".into()),
                    "GBP" => Some("British Pound".into()),
                    "JPY" => Some("Japanese Yen".into()),
                    "CNY" => Some("Chinese Yuan".into()),
                    "INR" => Some("Indian Rupee".into()),
                    "CAD" => Some("Canadian Dollar".into()),
                    "AUD" => Some("Australian Dollar".into()),
                    "CHF" => Some("Swiss Franc".into()),
                    "MXN" => Some("Mexican Peso".into()),
                    _ => None,
                },
                "script" => match code.as_str() {
                    "Latn" => Some("Latin".into()),
                    "Cyrl" => Some("Cyrillic".into()),
                    "Arab" => Some("Arabic".into()),
                    "Hans" => Some("Simplified Han".into()),
                    "Hant" => Some("Traditional Han".into()),
                    "Grek" => Some("Greek".into()),
                    _ => None,
                },
                "calendar" => (code == "gregory").then(|| "Gregorian Calendar".to_string()),
                _ => Some(code.chars().next().map(|c| c.to_uppercase().collect::<String>() + &code[c.len_utf8()..]).unwrap_or_default()),
            };
            Ok(match name {
                Some(n) => Value::str(&n),
                None if fb == "code" => Value::str(&code),
                None => Value::Undefined,
            })
        });
        method(it, o, "resolvedOptions", 0, move |it, _, _| Ok(resolved(it, &[("locale", Value::str(&base_name(&locale))), ("type", Value::str(&ty)), ("style", Value::str("long")), ("fallback", Value::str(&fallback))])));
        Ok(())
    });
    intl_ctor(it, intl, "Locale", true, |it, o, args| {
        let tag = arg(args, 0);
        if !matches!(tag, Value::Str(_) | Value::Obj(_)) {
            throw!(it, "TypeError", "First argument to Intl.Locale constructor can't be empty or missing");
        }
        let s = it.to_str(&tag)?;
        let Some(mut loc) = canonical_locale(&s) else { throw!(it, "RangeError", "Incorrect locale information provided") };
        let opts = options_object(it, &arg(args, 1))?;
        if let Some(r) = opt_str(it, &opts, "region")? {
            let base: Vec<&str> = loc.split('-').filter(|x| region_of(&format!("x-{x}")).is_none()).collect();
            loc = format!("{}-{}", base.join("-"), r.to_ascii_uppercase());
        }
        let lang = lang_of(&loc).to_string();
        let region = region_of(&loc).map(|s| s.to_string());
        let script = loc.split('-').nth(1).filter(|s| s.len() == 4 && s.chars().all(|c| c.is_ascii_alphabetic())).map(|s| s.to_string());
        o.set_prop("language", Value::str(&lang));
        o.set_prop("region", sv(&region));
        o.set_prop("script", sv(&script));
        o.set_prop("baseName", Value::str(&base_name(&loc)));
        let l2 = loc.clone();
        method(it, o, "toString", 0, move |_, _, _| Ok(Value::str(&l2)));
        let l3 = loc.clone();
        method(it, o, "maximize", 0, move |it, _, _| {
            let region = region_of(&l3).map(|s| s.to_string()).unwrap_or_else(|| {
                match lang_of(&l3) {
                    "en" => "US",
                    "ja" => "JP",
                    "zh" => "CN",
                    "ko" => "KR",
                    "sv" => "SE",
                    "da" => "DK",
                    "uk" => "UA",
                    "cs" => "CZ",
                    "el" => "GR",
                    "he" => "IL",
                    "ar" => "EG",
                    "hi" => "IN",
                    "vi" => "VN",
                    "fa" => "IR",
                    l => return l.to_ascii_uppercase(),
                }
                .to_string()
            });
            let ctor = it.get_str(&Value::Obj(it.global.clone()), "Intl")?;
            let ctor = it.get_str(&ctor, "Locale")?;
            it.construct(&ctor, &[Value::str(&format!("{}-{}", lang_of(&l3), region))], None)
        });
        let l4 = loc.clone();
        method(it, o, "minimize", 0, move |it, _, _| {
            let ctor = it.get_str(&Value::Obj(it.global.clone()), "Intl")?;
            let ctor = it.get_str(&ctor, "Locale")?;
            it.construct(&ctor, &[Value::str(lang_of(&l4))], None)
        });
        Ok(())
    });
}

// ---------------------------------------------------------------- setup

pub fn setup(it: &mut Interp) {
    let intl = it.new_object();
    let tag = PropKey::Sym(it.sym_to_string_tag.clone());
    intl.borrow_mut().props.insert(tag, Prop { value: Value::str("Intl"), getter: None, setter: None, enumerable: false, writable: false, configurable: true });
    setup_dtf(it, &intl);
    setup_nf(it, &intl);
    setup_plural(it, &intl);
    setup_collator(it, &intl);
    setup_relative(it, &intl);
    setup_display_names(it, &intl);
    it.define_method(&intl, "getCanonicalLocales", 1, |it, _, args| {
        let l = locale_list(it, &arg(args, 0))?;
        Ok(it.new_array(l.iter().map(|s| Value::str(s)).collect()))
    });
    it.define_method(&intl, "supportedValuesOf", 1, |it, _, args| {
        let key = it.to_str(&arg(args, 0))?;
        let vals: Vec<String> = match &*key {
            "timeZone" => tz::all_names(),
            "calendar" => vec!["gregory".into(), "iso8601".into()],
            "collation" => vec!["default".into()],
            "currency" => ["AUD", "BRL", "CAD", "CHF", "CNY", "EUR", "GBP", "INR", "JPY", "KRW", "MXN", "USD"].iter().map(|s| s.to_string()).collect(),
            "numberingSystem" => vec!["latn".into()],
            "unit" => UNITS.iter().map(|u| u.0.to_string()).collect(),
            _ => throw!(it, "RangeError", "Invalid key : {key}"),
        };
        Ok(it.new_array(vals.iter().map(|s| Value::str(s)).collect()))
    });
    it.global.set_hidden("Intl", Value::Obj(intl));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(canonical_locale("EN-us").as_deref(), Some("en-US"));
        assert_eq!(canonical_locale("zh-hant-tw").as_deref(), Some("zh-Hant-TW"));
        assert!(canonical_locale("x").is_none());
        assert_eq!(plural_category("en", 1.0, 0, false), "one");
        assert_eq!(plural_category("en", 1.5, 1, false), "other");
        assert_eq!(plural_category("ru", 22.0, 0, false), "few");
        assert_eq!(plural_category("en", 23.0, 0, true), "few");
    }

    #[test]
    fn collation() {
        let c = Collator { numeric: true, sensitivity: "base".into(), ..Default::default() };
        assert_eq!(c.compare("item 2", "item 10"), Ordering::Less);
        assert_eq!(c.compare("Résumé", "resume"), Ordering::Equal);
        let d = Collator::default();
        assert_eq!(d.compare("a", "B"), Ordering::Less);
        assert_eq!(d.compare("a", "A"), Ordering::Less);
        assert_eq!(d.compare("item 10", "item 2"), Ordering::Less);
    }
}
