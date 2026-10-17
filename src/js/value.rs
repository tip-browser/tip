//! JavaScript values and objects.

use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use super::ast::{ClassDef, FuncDef};
use super::interp::{Env, Interp};

pub type JsStr = Rc<str>;

pub struct SymbolData {
    pub description: Option<JsStr>,
}

#[derive(Clone)]
pub struct Sym(pub Rc<SymbolData>);

impl PartialEq for Sym {
    fn eq(&self, o: &Sym) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}
impl Eq for Sym {}
impl Hash for Sym {
    fn hash<H: Hasher>(&self, h: &mut H) {
        (Rc::as_ptr(&self.0) as usize).hash(h)
    }
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    BigInt(i128),
    Str(JsStr),
    Sym(Sym),
    Obj(Obj),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::Str(Rc::from(s))
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => *n != 0.0 && !n.is_nan(),
            Value::BigInt(n) => *n != 0,
            Value::Str(s) => !s.is_empty(),
            Value::Sym(_) | Value::Obj(_) => true,
        }
    }
    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::BigInt(_) => "bigint",
            Value::Str(_) => "string",
            Value::Sym(_) => "symbol",
            Value::Obj(o) => {
                if o.is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self, Value::Obj(o) if o.is_callable())
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Num(n) => write!(f, "{}", num_to_string(*n)),
            Value::BigInt(n) => write!(f, "{n}n"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Sym(s) => write!(f, "Symbol({})", s.0.description.as_deref().unwrap_or("")),
            Value::Obj(o) => write!(f, "[object {}]", o.borrow().class),
        }
    }
}

#[derive(Clone)]
pub enum PropKey {
    Str(JsStr),
    Sym(Sym),
}

impl PartialEq for PropKey {
    fn eq(&self, o: &PropKey) -> bool {
        match (self, o) {
            (PropKey::Str(a), PropKey::Str(b)) => a == b,
            (PropKey::Sym(a), PropKey::Sym(b)) => a == b,
            _ => false,
        }
    }
}
impl Eq for PropKey {}
impl Hash for PropKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        match self {
            PropKey::Str(s) => s.hash(h),
            PropKey::Sym(s) => s.hash(h),
        }
    }
}

impl PropKey {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropKey::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn array_index(&self) -> Option<usize> {
        let s = self.as_str()?;
        if s.is_empty() || s.len() > 10 || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<u32>().ok().filter(|&v| v != u32::MAX).map(|v| v as usize)
    }
    pub fn to_value(&self) -> Value {
        match self {
            PropKey::Str(s) => Value::Str(s.clone()),
            PropKey::Sym(s) => Value::Sym(s.clone()),
        }
    }
}

impl From<&str> for PropKey {
    fn from(s: &str) -> PropKey {
        PropKey::Str(Rc::from(s))
    }
}

#[derive(Clone)]
pub struct Prop {
    pub value: Value,
    pub getter: Option<Value>,
    pub setter: Option<Value>,
    pub enumerable: bool,
    pub writable: bool,
    pub configurable: bool,
}

impl Prop {
    pub fn data(value: Value) -> Prop {
        Prop { value, getter: None, setter: None, enumerable: true, writable: true, configurable: true }
    }
    pub fn hidden(value: Value) -> Prop {
        Prop { value, getter: None, setter: None, enumerable: false, writable: true, configurable: true }
    }
    pub fn is_accessor(&self) -> bool {
        self.getter.is_some() || self.setter.is_some()
    }
}

/// Insertion-ordered property map.
#[derive(Default)]
pub struct PropMap {
    entries: Vec<Option<(PropKey, Prop)>>,
    index: super::fxhash::FxHashMap<PropKey, usize>,
    dead: usize,
}

impl PropMap {
    pub fn get(&self, k: &PropKey) -> Option<&Prop> {
        self.index.get(k).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }
    pub fn get_mut(&mut self, k: &PropKey) -> Option<&mut Prop> {
        match self.index.get(k) {
            Some(&i) => self.entries[i].as_mut().map(|e| &mut e.1),
            None => None,
        }
    }
    pub fn insert(&mut self, k: PropKey, p: Prop) {
        if let Some(&i) = self.index.get(&k) {
            if let Some(e) = self.entries[i].as_mut() {
                e.1 = p;
                return;
            }
        }
        self.index.insert(k.clone(), self.entries.len());
        self.entries.push(Some((k, p)));
    }
    pub fn remove(&mut self, k: &PropKey) -> bool {
        if let Some(i) = self.index.remove(k) {
            self.entries[i] = None;
            self.dead += 1;
            if self.dead > 16 && self.dead * 2 > self.entries.len() {
                self.compact();
            }
            true
        } else {
            false
        }
    }
    fn compact(&mut self) {
        let old = std::mem::take(&mut self.entries);
        self.index.clear();
        self.dead = 0;
        for e in old.into_iter().flatten() {
            self.index.insert(e.0.clone(), self.entries.len());
            self.entries.push(Some(e));
        }
    }
    pub fn contains(&self, k: &PropKey) -> bool {
        self.index.contains_key(k)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&PropKey, &Prop)> {
        self.entries.iter().flatten().map(|(k, p)| (k, p))
    }
    pub fn keys(&self) -> Vec<PropKey> {
        let mut ints: Vec<(u32, PropKey)> = Vec::new();
        let mut strs = Vec::new();
        let mut syms = Vec::new();
        for (k, _) in self.entries.iter().flatten() {
            match k {
                PropKey::Str(_) => match k.array_index() {
                    Some(i) => ints.push((i as u32, k.clone())),
                    None => strs.push(k.clone()),
                },
                PropKey::Sym(_) => syms.push(k.clone()),
            }
        }
        ints.sort_by_key(|x| x.0);
        ints.into_iter().map(|x| x.1).chain(strs).chain(syms).collect()
    }
    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

pub type NativeFn = Rc<dyn Fn(&mut Interp, Value, &[Value]) -> Result<Value, Value>>;

pub struct Closure {
    pub def: Rc<FuncDef>,
    pub env: Env,
    /// Object whose prototype `super` refers to.
    pub home: RefCell<Option<Obj>>,
    pub class: Option<Rc<ClassInfo>>,
}

pub struct ClassInfo {
    pub def: Rc<ClassDef>,
    pub env: Env,
    pub derived: bool,
    pub parent: Option<Value>,
    /// Instance fields: (key, initializer); evaluated at construction.
    pub fields: RefCell<Vec<(PropKey, Option<usize>)>>,
    pub proto: RefCell<Option<Obj>>,
}

pub enum Func {
    Closure(Rc<Closure>),
    Native { f: NativeFn, ctor: bool },
    Bound { target: Obj, this: Value, args: Vec<Value> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseStatus {
    Pending,
    Fulfilled,
    Rejected,
}

pub struct PromiseState {
    pub status: PromiseStatus,
    pub value: Value,
    pub reactions: Vec<(Option<Value>, Option<Value>, Obj)>,
    pub handled: bool,
}

pub trait HostObject {
    fn class_name(&self) -> &'static str;
    fn get(&self, it: &mut Interp, key: &PropKey) -> Option<Result<Value, Value>>;
    fn set(&self, it: &mut Interp, key: &PropKey, v: &Value) -> Option<Result<(), Value>>;
    fn keys(&self, _it: &mut Interp) -> Vec<PropKey> {
        vec![]
    }
    fn inspect(&self, _it: &mut Interp) -> Option<String> {
        None
    }
    fn as_any(&self) -> &dyn std::any::Any;
}

pub struct MapData {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: HashMap<MapKey, usize>,
}

/// Hashable SameValueZero key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Num(u64),
    BigInt(i128),
    Str(JsStr),
    Ptr(usize),
}

impl MapKey {
    pub fn of(v: &Value) -> MapKey {
        match v {
            Value::Undefined => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Num(n) => {
                let n = if *n == 0.0 { 0.0 } else { *n };
                if n.is_nan() { MapKey::Num(u64::MAX) } else { MapKey::Num(n.to_bits()) }
            }
            Value::BigInt(n) => MapKey::BigInt(*n),
            Value::Str(s) => MapKey::Str(s.clone()),
            Value::Sym(s) => MapKey::Ptr(Rc::as_ptr(&s.0) as usize),
            Value::Obj(o) => MapKey::Ptr(o.id()),
        }
    }
}

impl MapData {
    pub fn new() -> MapData {
        MapData { entries: vec![], index: HashMap::new() }
    }
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&MapKey::of(k)).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }
    pub fn set(&mut self, k: Value, v: Value) {
        let mk = MapKey::of(&k);
        if let Some(&i) = self.index.get(&mk) {
            if let Some(e) = self.entries[i].as_mut() {
                e.1 = v;
                return;
            }
        }
        self.index.insert(mk, self.entries.len());
        self.entries.push(Some((k, v)));
    }
    pub fn delete(&mut self, k: &Value) -> bool {
        match self.index.remove(&MapKey::of(k)) {
            Some(i) => {
                self.entries[i] = None;
                true
            }
            None => false,
        }
    }
    pub fn has(&self, k: &Value) -> bool {
        self.index.contains_key(&MapKey::of(k))
    }
    pub fn clear(&mut self) {
        // keep tombstones so live iterators stay consistent
        for e in self.entries.iter_mut() {
            *e = None;
        }
        self.index.clear();
    }
    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
}

impl Default for MapData {
    fn default() -> Self {
        Self::new()
    }
}

pub enum IterKind {
    /// Iterate a list snapshot or live array.
    Array { target: Value, index: usize, kind: u8 },
    Map { map: Obj, index: usize, kind: u8 },
    Str { chars: Vec<String>, index: usize },
    List { items: Vec<Value>, index: usize },
}

pub enum ObjKind {
    Ordinary,
    Array(Vec<Value>),
    Function(Func),
    Error,
    Bool(bool),
    Num(f64),
    Str(JsStr),
    Symbol(Sym),
    Date(f64),
    RegExp(Rc<super::regex::Regex>),
    Map(MapData),
    Set(MapData),
    Promise(PromiseState),
    Host(Rc<dyn HostObject>),
    Iterator(IterKind),
    Generator(Rc<super::coro::Coroutine>),
    Arguments,
    TypedArray(TypedKind),
    ArrayBuffer(Rc<RefCell<Vec<u8>>>),
    Proxy(Rc<ProxyData>),
}

pub struct ProxyData {
    pub target: Obj,
    /// `None` once revoked.
    pub handler: RefCell<Option<Obj>>,
}

#[derive(Clone)]
pub struct TypedKind {
    pub buf: Rc<RefCell<Vec<u8>>>,
    pub offset: usize,
    pub len: usize,
    pub ty: TypedTy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypedTy {
    U8,
    U8C,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    F64,
}

impl TypedTy {
    pub fn size(self) -> usize {
        match self {
            TypedTy::U8 | TypedTy::U8C | TypedTy::I8 => 1,
            TypedTy::U16 | TypedTy::I16 => 2,
            TypedTy::U32 | TypedTy::I32 | TypedTy::F32 => 4,
            TypedTy::F64 => 8,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            TypedTy::U8 => "Uint8Array",
            TypedTy::U8C => "Uint8ClampedArray",
            TypedTy::I8 => "Int8Array",
            TypedTy::U16 => "Uint16Array",
            TypedTy::I16 => "Int16Array",
            TypedTy::U32 => "Uint32Array",
            TypedTy::I32 => "Int32Array",
            TypedTy::F32 => "Float32Array",
            TypedTy::F64 => "Float64Array",
        }
    }
}

impl TypedKind {
    pub fn get(&self, i: usize) -> Option<f64> {
        if i >= self.len {
            return None;
        }
        let b = self.buf.borrow();
        let o = self.offset + i * self.ty.size();
        let s = &b[o..o + self.ty.size()];
        Some(match self.ty {
            TypedTy::U8 | TypedTy::U8C => s[0] as f64,
            TypedTy::I8 => s[0] as i8 as f64,
            TypedTy::U16 => u16::from_le_bytes([s[0], s[1]]) as f64,
            TypedTy::I16 => i16::from_le_bytes([s[0], s[1]]) as f64,
            TypedTy::U32 => u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64,
            TypedTy::I32 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64,
            TypedTy::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64,
            TypedTy::F64 => f64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
        })
    }
    pub fn set(&self, i: usize, v: f64) {
        if i >= self.len {
            return;
        }
        let mut b = self.buf.borrow_mut();
        let o = self.offset + i * self.ty.size();
        let int = |v: f64| -> i64 { if v.is_finite() { v.trunc() as i64 } else { 0 } };
        match self.ty {
            TypedTy::U8 => b[o] = int(v) as u8,
            TypedTy::U8C => b[o] = if v.is_nan() { 0 } else { v.round().clamp(0.0, 255.0) as u8 },
            TypedTy::I8 => b[o] = int(v) as i8 as u8,
            TypedTy::U16 => b[o..o + 2].copy_from_slice(&(int(v) as u16).to_le_bytes()),
            TypedTy::I16 => b[o..o + 2].copy_from_slice(&(int(v) as i16).to_le_bytes()),
            TypedTy::U32 => b[o..o + 4].copy_from_slice(&(int(v) as u32).to_le_bytes()),
            TypedTy::I32 => b[o..o + 4].copy_from_slice(&(int(v) as i32).to_le_bytes()),
            TypedTy::F32 => b[o..o + 4].copy_from_slice(&(v as f32).to_le_bytes()),
            TypedTy::F64 => b[o..o + 8].copy_from_slice(&v.to_le_bytes()),
        }
    }
}

pub struct ObjectData {
    pub props: PropMap,
    pub proto: Option<Obj>,
    pub kind: ObjKind,
    pub extensible: bool,
    pub frozen: bool,
    pub class: &'static str,
}

#[derive(Clone)]
pub struct Obj(pub Rc<RefCell<ObjectData>>);

impl Obj {
    pub fn new(kind: ObjKind, proto: Option<Obj>, class: &'static str) -> Obj {
        Obj(Rc::new(RefCell::new(ObjectData { props: PropMap::default(), proto, kind, extensible: true, frozen: false, class })))
    }
    pub fn borrow(&self) -> Ref<'_, ObjectData> {
        self.0.borrow()
    }
    pub fn borrow_mut(&self) -> RefMut<'_, ObjectData> {
        self.0.borrow_mut()
    }
    pub fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as *const u8 as usize
    }
    pub fn ptr_eq(&self, o: &Obj) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
    pub fn is_callable(&self) -> bool {
        match &self.borrow().kind {
            ObjKind::Function(_) => true,
            ObjKind::Proxy(p) => p.target.is_callable(),
            _ => false,
        }
    }
    pub fn is_array(&self) -> bool {
        match &self.borrow().kind {
            ObjKind::Array(_) => true,
            ObjKind::Proxy(p) => p.target.is_array(),
            _ => false,
        }
    }
    pub fn proxy(&self) -> Option<Rc<ProxyData>> {
        match &self.borrow().kind {
            ObjKind::Proxy(p) => Some(p.clone()),
            _ => None,
        }
    }
    pub fn proto(&self) -> Option<Obj> {
        self.borrow().proto.clone()
    }
    pub fn set_hidden(&self, k: &str, v: Value) {
        self.borrow_mut().props.insert(PropKey::from(k), Prop::hidden(v));
    }
    pub fn set_prop(&self, k: &str, v: Value) {
        self.borrow_mut().props.insert(PropKey::from(k), Prop::data(v));
    }
    pub fn get_own(&self, k: &str) -> Option<Value> {
        self.borrow().props.get(&PropKey::from(k)).map(|p| p.value.clone())
    }
    pub fn host(&self) -> Option<Rc<dyn HostObject>> {
        match &self.borrow().kind {
            ObjKind::Host(h) => Some(h.clone()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------- number formatting

/// Number::toString as specified by ECMAScript.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    if n < 0.0 {
        return format!("-{}", num_to_string(-n));
    }
    if n.fract() == 0.0 && n < 1e21 {
        return format!("{}", n as u128);
    }
    // shortest round-trip digits via Rust's {:e}
    let e = format!("{:e}", n);
    let (mant, exp) = e.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let nexp = exp + 1;
    if k <= nexp && nexp <= 21 {
        let mut s = digits.clone();
        for _ in 0..(nexp - k) {
            s.push('0');
        }
        s
    } else if 0 < nexp && nexp <= 21 {
        format!("{}.{}", &digits[..nexp as usize], &digits[nexp as usize..])
    } else if -6 < nexp && nexp <= 0 {
        format!("0.{}{}", "0".repeat((-nexp) as usize), digits)
    } else {
        let sign = if nexp - 1 >= 0 { "+" } else { "-" };
        if k == 1 {
            format!("{}e{}{}", digits, sign, (nexp - 1).abs())
        } else {
            format!("{}.{}e{}{}", &digits[..1], &digits[1..], sign, (nexp - 1).abs())
        }
    }
}

pub fn num_to_radix(mut n: f64, radix: u32) -> String {
    if radix == 10 {
        return num_to_string(n);
    }
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = n < 0.0;
    if neg {
        n = -n;
    }
    let mut int = n.trunc();
    let mut frac = n - int;
    let mut s = Vec::new();
    if int == 0.0 {
        s.push('0');
    }
    while int >= 1.0 {
        let d = (int % radix as f64) as u32;
        s.push(std::char::from_digit(d, radix).unwrap());
        int = (int / radix as f64).trunc();
    }
    s.reverse();
    let mut out: String = s.into_iter().collect();
    if frac > 0.0 {
        out.push('.');
        let mut count = 0;
        while frac > 0.0 && count < 20 {
            frac *= radix as f64;
            let d = frac.trunc() as u32;
            out.push(std::char::from_digit(d, radix).unwrap());
            frac -= d as f64;
            count += 1;
        }
    }
    if neg { format!("-{out}") } else { out }
}

/// StringToNumber.
pub fn str_to_num(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    let (neg, body) = if let Some(r) = t.strip_prefix('-') { (true, r) } else { (false, t.strip_prefix('+').unwrap_or(t)) };
    let v = if body == "Infinity" {
        f64::INFINITY
    } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u128::from_str_radix(h, 16).map(|v| v as f64).unwrap_or(f64::NAN);
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return u128::from_str_radix(o, 8).map(|v| v as f64).unwrap_or(f64::NAN);
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return u128::from_str_radix(b, 2).map(|v| v as f64).unwrap_or(f64::NAN);
    } else {
        if !body.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) || body.starts_with(['e', 'E']) {
            return f64::NAN;
        }
        body.parse::<f64>().unwrap_or(f64::NAN)
    };
    if neg { -v } else { v }
}

pub fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let m = n.trunc() % 4294967296.0;
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    m as u32 as i32
}

pub fn to_uint32(n: f64) -> u32 {
    to_int32(n) as u32
}

/// UTF-16 length of a string.
pub fn utf16_len(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.encode_utf16().count() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting() {
        assert_eq!(num_to_string(1.0), "1");
        assert_eq!(num_to_string(0.1), "0.1");
        assert_eq!(num_to_string(1.5e-7), "1.5e-7");
        assert_eq!(num_to_string(1e21), "1e+21");
        assert_eq!(num_to_string(123456.789), "123456.789");
        assert_eq!(num_to_string(-0.000001), "-0.000001");
        assert_eq!(num_to_string(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(num_to_radix(255.0, 16), "ff");
    }
}
