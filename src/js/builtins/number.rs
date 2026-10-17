//! Number, Boolean, BigInt and Math.

use std::cell::Cell;

use crate::js::interp::{arg, throw, Interp, R};
use crate::js::value::*;

pub fn parse_int(s: &str, radix: u32) -> f64 {
    let t = s.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let (neg, mut t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut radix = radix;
    if radix == 0 || radix == 16 {
        if let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            t = r;
            radix = 16;
        }
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let digits: String = t.chars().take_while(|c| c.is_digit(radix)).collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut v = 0f64;
    for d in digits.chars() {
        v = v * radix as f64 + d.to_digit(radix).unwrap() as f64;
    }
    if neg { -v } else { v }
}

pub fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    for prefix in ["Infinity", "+Infinity"] {
        if t.starts_with(prefix) {
            return f64::INFINITY;
        }
    }
    if t.starts_with("-Infinity") {
        return f64::NEG_INFINITY;
    }
    // longest valid prefix
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i == ds || (i == ds + 1 && b[ds] == b'.') {
        return f64::NAN;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    t[..i].parse().unwrap_or(f64::NAN)
}

fn this_num(it: &mut Interp, v: &Value) -> R<f64> {
    match v {
        Value::Num(n) => Ok(*n),
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Num(n) => Ok(*n),
            _ => Err(it.make_error("TypeError", "Number.prototype method called on incompatible receiver")),
        },
        _ => Err(it.make_error("TypeError", "Number.prototype method called on incompatible receiver")),
    }
}

/// Round half away from zero to `digits` decimals, formatted.
pub fn to_fixed(n: f64, digits: usize) -> String {
    if !n.is_finite() || n.abs() >= 1e21 {
        return num_to_string(n);
    }
    // Rust rounds half-to-even on the binary value, which matches JS for exact binary values.
    let s = format!("{:.*}", digits, n);
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        return s[1..].to_string();
    }
    s
}

pub fn to_precision(n: f64, p: usize) -> String {
    if !n.is_finite() {
        return num_to_string(n);
    }
    if n == 0.0 {
        return if p > 1 { format!("0.{}", "0".repeat(p - 1)) } else { "0".into() };
    }
    let e = n.abs().log10().floor() as i32;
    if e < -6 || e >= p as i32 {
        let s = format!("{:.*e}", p - 1, n);
        let (m, ex) = s.split_once('e').unwrap();
        let exn: i32 = ex.parse().unwrap();
        return format!("{m}e{}{}", if exn >= 0 { "+" } else { "-" }, exn.abs());
    }
    let decimals = (p as i32 - 1 - e).max(0) as usize;
    format!("{:.*}", decimals, n)
}

thread_local! {
    static RNG: Cell<u64> = Cell::new({
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E3779B97F4A7C15);
        t ^ 0x2545F4914F6CDD1D
    });
}

pub fn random() -> f64 {
    RNG.with(|r| {
        // xorshift64*
        let mut x = r.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        let v = x.wrapping_mul(0x2545F4914F6CDD1D);
        (v >> 11) as f64 / (1u64 << 53) as f64
    })
}

pub fn setup(it: &mut Interp) {
    let np = it.p.number.clone();
    let ctor = it.define_ctor("Number", 1, &np, |it, this, args| {
        let n = match args.first() {
            None => 0.0,
            Some(Value::BigInt(b)) => *b as f64,
            Some(v) => it.to_number(v)?,
        };
        if it.constructing {
            it.constructing = false;
            if let Value::Obj(o) = &this {
                o.borrow_mut().kind = ObjKind::Num(n);
                o.borrow_mut().class = "Number";
                return Ok(this);
            }
        }
        Ok(Value::Num(n))
    });
    for (name, v) in [
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        ctor.set_hidden(name, Value::Num(v));
    }
    it.define_method(&ctor, "isInteger", 1, |_, _, args| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite() && n.fract() == 0.0))));
    it.define_method(&ctor, "isSafeInteger", 1, |_, _, args| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= 9007199254740991.0))));
    it.define_method(&ctor, "isFinite", 1, |_, _, args| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_finite()))));
    it.define_method(&ctor, "isNaN", 1, |_, _, args| Ok(Value::Bool(matches!(arg(args, 0), Value::Num(n) if n.is_nan()))));
    let g = it.global.clone();
    for name in ["parseFloat", "parseInt"] {
        if let Some(f) = g.get_own(name) {
            ctor.set_hidden(name, f);
        }
    }
    it.define_method(&np, "toString", 1, |it, this, args| {
        let n = this_num(it, &this)?;
        let r = arg(args, 0);
        let radix = if r.is_undefined() { 10 } else { it.to_integer(&r)? as u32 };
        if !(2..=36).contains(&radix) {
            throw!(it, "RangeError", "toString() radix must be between 2 and 36");
        }
        Ok(Value::str(&num_to_radix(n, radix)))
    });
    it.define_method(&np, "toLocaleString", 0, |it, this, args| {
        let n = this_num(it, &this)?;
        Ok(Value::str(&super::intl::number_to_locale_string(it, n, &arg(args, 0), &arg(args, 1))?))
    });
    it.define_method(&np, "valueOf", 0, |it, this, _| Ok(Value::Num(this_num(it, &this)?)));
    it.define_method(&np, "toFixed", 1, |it, this, args| {
        let n = this_num(it, &this)?;
        let d = it.to_integer(&arg(args, 0))?;
        if !(0.0..=100.0).contains(&d) {
            throw!(it, "RangeError", "toFixed() digits argument must be between 0 and 100");
        }
        Ok(Value::str(&to_fixed(n, d as usize)))
    });
    it.define_method(&np, "toPrecision", 1, |it, this, args| {
        let n = this_num(it, &this)?;
        if arg(args, 0).is_undefined() {
            return Ok(Value::str(&num_to_string(n)));
        }
        let p = it.to_integer(&arg(args, 0))?;
        if !(1.0..=100.0).contains(&p) {
            throw!(it, "RangeError", "toPrecision() argument must be between 1 and 100");
        }
        Ok(Value::str(&to_precision(n, p as usize)))
    });
    it.define_method(&np, "toExponential", 1, |it, this, args| {
        let n = this_num(it, &this)?;
        let d = arg(args, 0);
        let s = if d.is_undefined() { format!("{:e}", n) } else { format!("{:.*e}", it.to_integer(&d)? as usize, n) };
        let (m, e) = s.split_once('e').unwrap();
        let en: i32 = e.parse().unwrap();
        Ok(Value::str(&format!("{m}e{}{}", if en >= 0 { "+" } else { "-" }, en.abs())))
    });

    let bp = it.p.boolean.clone();
    it.define_ctor("Boolean", 1, &bp, |it, this, args| {
        let b = arg(args, 0).truthy();
        if it.constructing {
            it.constructing = false;
            if let Value::Obj(o) = &this {
                o.borrow_mut().kind = ObjKind::Bool(b);
                o.borrow_mut().class = "Boolean";
                return Ok(this);
            }
        }
        Ok(Value::Bool(b))
    });
    let this_bool = |it: &mut Interp, v: &Value| -> R<bool> {
        match v {
            Value::Bool(b) => Ok(*b),
            Value::Obj(o) => match &o.borrow().kind {
                ObjKind::Bool(b) => Ok(*b),
                _ => Err(it.make_error("TypeError", "not a boolean")),
            },
            _ => Err(it.make_error("TypeError", "not a boolean")),
        }
    };
    it.define_method(&bp, "toString", 0, move |it, this, _| Ok(Value::str(if this_bool(it, &this)? { "true" } else { "false" })));
    it.define_method(&bp, "valueOf", 0, move |it, this, _| Ok(Value::Bool(this_bool(it, &this)?)));

    let bip = it.p.bigint.clone();
    let bctor = it.define_ctor("BigInt", 1, &bip, |it, _, args| {
        if it.constructing {
            it.constructing = false;
            throw!(it, "TypeError", "BigInt is not a constructor");
        }
        Ok(match arg(args, 0) {
            Value::Num(n) if n.fract() == 0.0 => Value::BigInt(n as i128),
            Value::Num(_) => throw!(it, "RangeError", "The number cannot be converted to a BigInt because it is not an integer"),
            Value::BigInt(b) => Value::BigInt(b),
            Value::Bool(b) => Value::BigInt(b as i128),
            Value::Str(s) => match s.trim().parse::<i128>() {
                Ok(v) => Value::BigInt(v),
                Err(_) => throw!(it, "SyntaxError", "Cannot convert {s} to a BigInt"),
            },
            _ => throw!(it, "TypeError", "Cannot convert value to a BigInt"),
        })
    });
    let _ = bctor;
    it.define_method(&bip, "toString", 0, |it, this, args| {
        let Value::BigInt(b) = this else { throw!(it, "TypeError", "not a BigInt") };
        let r = arg(args, 0);
        let radix = if r.is_undefined() { 10 } else { it.to_integer(&r)? as u32 };
        if radix == 10 {
            return Ok(Value::str(&b.to_string()));
        }
        let neg = b < 0;
        let mut x = b.unsigned_abs();
        let mut digits = Vec::new();
        if x == 0 {
            digits.push('0');
        }
        while x > 0 {
            digits.push(std::char::from_digit((x % radix as u128) as u32, radix).unwrap());
            x /= radix as u128;
        }
        digits.reverse();
        let s: String = digits.into_iter().collect();
        Ok(Value::str(&if neg { format!("-{s}") } else { s }))
    });
    it.define_method(&bip, "valueOf", 0, |_, this, _| Ok(this));
    it.define_method(&bip, "toLocaleString", 0, |_, this, _| Ok(match this {
        Value::BigInt(b) => Value::str(&b.to_string()),
        v => v,
    }));

    // Math
    let m = it.new_object();
    it.global.set_hidden("Math", Value::Obj(m.clone()));
    let tag = it.sym_to_string_tag.clone();
    m.borrow_mut().props.insert(PropKey::Sym(tag), Prop::hidden(Value::str("Math")));
    for (name, v) in [
        ("PI", std::f64::consts::PI),
        ("E", std::f64::consts::E),
        ("LN2", std::f64::consts::LN_2),
        ("LN10", std::f64::consts::LN_10),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("SQRT2", std::f64::consts::SQRT_2),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ] {
        m.set_hidden(name, Value::Num(v));
    }
    let unary: &[(&str, fn(f64) -> f64)] = &[
        ("abs", f64::abs),
        ("floor", f64::floor),
        ("ceil", f64::ceil),
        ("sqrt", f64::sqrt),
        ("cbrt", f64::cbrt),
        ("sin", f64::sin),
        ("cos", f64::cos),
        ("tan", f64::tan),
        ("asin", f64::asin),
        ("acos", f64::acos),
        ("atan", f64::atan),
        ("sinh", f64::sinh),
        ("cosh", f64::cosh),
        ("tanh", f64::tanh),
        ("asinh", f64::asinh),
        ("acosh", f64::acosh),
        ("atanh", f64::atanh),
        ("exp", f64::exp),
        ("expm1", f64::exp_m1),
        ("log", f64::ln),
        ("log2", f64::log2),
        ("log10", f64::log10),
        ("log1p", f64::ln_1p),
        ("trunc", f64::trunc),
        ("fround", |x| x as f32 as f64),
        ("sign", |x: f64| if x.is_nan() { f64::NAN } else if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { x }),
        ("round", |x: f64| if x.is_nan() || x.is_infinite() { x } else if x.fract() == -0.5 { x.ceil() } else { (x + 0.5).floor() }),
        ("clz32", |x: f64| to_uint32(x).leading_zeros() as f64),
    ];
    for (name, f) in unary {
        let f = *f;
        it.define_method(&m, name, 1, move |it, _, args| Ok(Value::Num(f(it.to_number(&arg(args, 0))?))));
    }
    it.define_method(&m, "atan2", 2, |it, _, args| Ok(Value::Num(it.to_number(&arg(args, 0))?.atan2(it.to_number(&arg(args, 1))?))));
    it.define_method(&m, "pow", 2, |it, _, args| Ok(Value::Num(crate::js::interp::js_pow(it.to_number(&arg(args, 0))?, it.to_number(&arg(args, 1))?))));
    it.define_method(&m, "imul", 2, |it, _, args| Ok(Value::Num(to_int32(it.to_number(&arg(args, 0))?).wrapping_mul(to_int32(it.to_number(&arg(args, 1))?)) as f64)));
    it.define_method(&m, "max", 2, |it, _, args| {
        let mut r = f64::NEG_INFINITY;
        for a in args {
            let n = it.to_number(a)?;
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n > r || (n == 0.0 && r == 0.0 && !n.is_sign_negative()) {
                r = n;
            }
        }
        Ok(Value::Num(r))
    });
    it.define_method(&m, "min", 2, |it, _, args| {
        let mut r = f64::INFINITY;
        for a in args {
            let n = it.to_number(a)?;
            if n.is_nan() {
                return Ok(Value::Num(f64::NAN));
            }
            if n < r || (n == 0.0 && r == 0.0 && n.is_sign_negative()) {
                r = n;
            }
        }
        Ok(Value::Num(r))
    });
    it.define_method(&m, "hypot", 2, |it, _, args| {
        let mut s = 0.0;
        for a in args {
            let n = it.to_number(a)?;
            s += n * n;
        }
        Ok(Value::Num(s.sqrt()))
    });
    it.define_method(&m, "random", 0, |_, _, _| Ok(Value::Num(random())));
}
