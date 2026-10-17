//! Runtime values of the Rust runtime.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::Pos;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntKind {
    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    /// An integer literal whose type hasn't been fixed yet (behaves as i32, adopts the other operand's type).
    Lit,
}

impl IntKind {
    pub fn from_name(s: &str) -> Option<IntKind> {
        Some(match s {
            "i8" => IntKind::I8,
            "i16" => IntKind::I16,
            "i32" => IntKind::I32,
            "i64" => IntKind::I64,
            "i128" => IntKind::I128,
            "isize" => IntKind::Isize,
            "u8" => IntKind::U8,
            "u16" => IntKind::U16,
            "u32" => IntKind::U32,
            "u64" => IntKind::U64,
            "u128" => IntKind::U128,
            "usize" => IntKind::Usize,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            IntKind::I8 => "i8",
            IntKind::I16 => "i16",
            IntKind::I32 | IntKind::Lit => "i32",
            IntKind::I64 => "i64",
            IntKind::I128 => "i128",
            IntKind::Isize => "isize",
            IntKind::U8 => "u8",
            IntKind::U16 => "u16",
            IntKind::U32 => "u32",
            IntKind::U64 => "u64",
            IntKind::U128 => "u128",
            IntKind::Usize => "usize",
        }
    }
    pub fn range(self) -> (i128, i128) {
        match self {
            IntKind::I8 => (i8::MIN as i128, i8::MAX as i128),
            IntKind::I16 => (i16::MIN as i128, i16::MAX as i128),
            IntKind::I32 | IntKind::Lit => (i32::MIN as i128, i32::MAX as i128),
            IntKind::I64 | IntKind::Isize => (i64::MIN as i128, i64::MAX as i128),
            IntKind::I128 => (i128::MIN, i128::MAX),
            IntKind::U8 => (0, u8::MAX as i128),
            IntKind::U16 => (0, u16::MAX as i128),
            IntKind::U32 => (0, u32::MAX as i128),
            IntKind::U64 | IntKind::Usize => (0, u64::MAX as i128),
            IntKind::U128 => (0, i128::MAX),
        }
    }
    pub fn bits(self) -> u32 {
        match self {
            IntKind::I8 | IntKind::U8 => 8,
            IntKind::I16 | IntKind::U16 => 16,
            IntKind::I32 | IntKind::U32 | IntKind::Lit => 32,
            IntKind::I64 | IntKind::U64 | IntKind::Isize | IntKind::Usize => 64,
            IntKind::I128 | IntKind::U128 => 128,
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IntKind::I8 | IntKind::I16 | IntKind::I32 | IntKind::I64 | IntKind::I128 | IntKind::Isize | IntKind::Lit)
    }
    /// Wrap `v` into this type's range (two's complement), as `as` casts do.
    pub fn wrap(self, v: i128) -> i128 {
        let bits = self.bits();
        if bits == 128 {
            return v;
        }
        let m = (v as u128) & ((1u128 << bits) - 1);
        if self.signed() && m >> (bits - 1) == 1 { (m as i128) - (1i128 << bits) } else { m as i128 }
    }
}

pub struct TypeInfo {
    pub name: String,
    /// Struct field names (empty for tuple structs).
    pub fields: Vec<String>,
    pub is_tuple: bool,
    /// Enum variants: (name, field names, is_tuple, discriminant).
    pub variants: Vec<(String, Vec<String>, bool, i128)>,
    pub is_enum: bool,
    pub copy: bool,
    pub derives: Vec<String>,
}

impl TypeInfo {
    pub fn variant_index(&self, name: &str) -> Option<usize> {
        self.variants.iter().position(|v| v.0 == name)
    }
}

pub struct StructVal {
    pub ty: Rc<TypeInfo>,
    pub fields: RefCell<Vec<Value>>,
}

pub struct EnumVal {
    pub ty: Rc<TypeInfo>,
    pub variant: usize,
    pub fields: RefCell<Vec<Value>>,
}

#[derive(Default)]
pub struct MapVal {
    pub keys: Vec<Value>,
    pub vals: Vec<Value>,
    pub index: HashMap<Key, usize>,
    /// BTreeMap / BTreeSet keep keys sorted.
    pub sorted: bool,
    pub is_set: bool,
    /// The value type's default (from the declared type), for `entry().or_default()`.
    pub val_default: Option<Value>,
}

impl MapVal {
    pub fn new(sorted: bool, is_set: bool) -> MapVal {
        MapVal { sorted, is_set, ..Default::default() }
    }
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&Key::of(k)).map(|&i| &self.vals[i])
    }
    pub fn insert(&mut self, k: Value, v: Value) -> Option<Value> {
        let key = Key::of(&k);
        if let Some(&i) = self.index.get(&key) {
            return Some(std::mem::replace(&mut self.vals[i], v));
        }
        if self.sorted {
            let pos = self.keys.partition_point(|x| cmp_values(x, &k) == Ordering::Less);
            self.keys.insert(pos, k);
            self.vals.insert(pos, v);
            self.reindex();
        } else {
            self.index.insert(key, self.keys.len());
            self.keys.push(k);
            self.vals.push(v);
        }
        None
    }
    pub fn remove(&mut self, k: &Value) -> Option<(Value, Value)> {
        let i = self.index.remove(&Key::of(k))?;
        let key = self.keys.remove(i);
        let val = self.vals.remove(i);
        self.reindex();
        Some((key, val))
    }
    fn reindex(&mut self) {
        self.index.clear();
        for (i, k) in self.keys.iter().enumerate() {
            self.index.insert(Key::of(k), i);
        }
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
    pub fn clear(&mut self) {
        self.keys.clear();
        self.vals.clear();
        self.index.clear();
    }
}

/// A hashable form of a value used as a map key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Unit,
    Bool(bool),
    Int(i128),
    Float(u64),
    Char(char),
    Str(String),
    List(Vec<Key>),
    Variant(String, usize, Vec<Key>),
    Other(usize),
}

impl Key {
    pub fn of(v: &Value) -> Key {
        match v {
            Value::Unit => Key::Unit,
            Value::Bool(b) => Key::Bool(*b),
            Value::Int(i, _) => Key::Int(*i),
            Value::Float(f, _) => Key::Float(f.to_bits()),
            Value::Char(c) => Key::Char(*c),
            Value::Str(s) => Key::Str(s.borrow().clone()),
            Value::Vec(v) | Value::Array(v) | Value::Tuple(v) => Key::List(v.borrow().iter().map(Key::of).collect()),
            Value::Struct(s) => Key::Variant(s.ty.name.clone(), 0, s.fields.borrow().iter().map(Key::of).collect()),
            Value::Enum(e) => Key::Variant(e.ty.name.clone(), e.variant, e.fields.borrow().iter().map(Key::of).collect()),
            Value::Ref(p) => Key::of(&p.get()),
            Value::RcBox(b) => Key::of(b),
            Value::Boxed(b) => Key::of(&b.borrow()),
            Value::Range(r) => Key::List(vec![Key::of(&r.0), Key::of(&r.1)]),
            other => Key::Other(other.addr()),
        }
    }
}

pub type NativeFn = Rc<dyn Fn(&mut super::runtime::Ctx, Vec<Value>, Pos) -> Result<Value, super::runtime::Ctrl>>;

pub enum FuncVal {
    /// A compiled function or closure with its captured variable cells.
    Code(Rc<super::runtime::FnCode>, Vec<Rc<RefCell<Value>>>),
    Native(String, NativeFn),
}

/// Where a `&mut` points.
pub enum Place {
    /// A local variable slot in a live frame (frames don't move while borrowed).
    Slot(*mut Value),
    Cell(Rc<RefCell<Value>>),
    Field(Rc<StructVal>, usize),
    Elem(Rc<RefCell<Vec<Value>>>, usize),
    EnumField(Rc<EnumVal>, usize),
    MapVal(Rc<RefCell<MapVal>>, Value),
    /// A temporary (rvalue) that was borrowed.
    Temp(RefCell<Value>),
}

impl Place {
    pub fn get(&self) -> Value {
        match self {
            // SAFETY: slot places are only created for the duration of a call on a live frame.
            Place::Slot(p) => unsafe { (**p).clone() }.unboxed(),
            Place::Cell(c) => c.borrow().clone(),
            Place::Field(s, i) => s.fields.borrow().get(*i).cloned().unwrap_or(Value::Unit),
            Place::Elem(v, i) => v.borrow().get(*i).cloned().unwrap_or(Value::Unit),
            Place::EnumField(e, i) => e.fields.borrow().get(*i).cloned().unwrap_or(Value::Unit),
            Place::MapVal(m, k) => m.borrow().get(k).cloned().unwrap_or(Value::Unit),
            Place::Temp(c) => c.borrow().clone(),
        }
    }
    pub fn set(&self, v: Value) {
        match self {
            Place::Slot(p) => unsafe {
                if let Value::Boxed(b) = &**p {
                    *b.borrow_mut() = v;
                } else {
                    **p = v;
                }
            },
            Place::Cell(c) => *c.borrow_mut() = v,
            Place::Field(s, i) => {
                if let Some(slot) = s.fields.borrow_mut().get_mut(*i) {
                    *slot = v;
                }
            }
            Place::Elem(vec, i) => {
                if let Some(slot) = vec.borrow_mut().get_mut(*i) {
                    *slot = v;
                }
            }
            Place::EnumField(e, i) => {
                if let Some(slot) = e.fields.borrow_mut().get_mut(*i) {
                    *slot = v;
                }
            }
            Place::MapVal(m, k) => {
                m.borrow_mut().insert(k.clone(), v);
            }
            Place::Temp(c) => *c.borrow_mut() = v,
        }
    }
}

pub struct IterVal {
    pub next: Box<dyn FnMut(&mut super::runtime::Ctx) -> Result<Option<Value>, super::runtime::Ctrl>>,
    /// A value already pulled by `peek()`.
    pub peeked: Option<Option<Value>>,
}

impl IterVal {
    pub fn new(next: impl FnMut(&mut super::runtime::Ctx) -> Result<Option<Value>, super::runtime::Ctrl> + 'static) -> IterVal {
        IterVal { next: Box::new(next), peeked: None }
    }
    pub fn pull(&mut self, ctx: &mut super::runtime::Ctx) -> Result<Option<Value>, super::runtime::Ctrl> {
        if let Some(p) = self.peeked.take() {
            return Ok(p);
        }
        (self.next)(ctx)
    }
}

#[derive(Clone)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i128, IntKind),
    Float(f64, bool),
    Char(char),
    /// `String` and `&str`
    Str(Rc<RefCell<String>>),
    /// `Vec<T>` and slices
    Vec(Rc<RefCell<Vec<Value>>>),
    /// `[T; N]` — copied when moved
    Array(Rc<RefCell<Vec<Value>>>),
    /// tuples — copied when moved
    Tuple(Rc<RefCell<Vec<Value>>>),
    Struct(Rc<StructVal>),
    Enum(Rc<EnumVal>),
    Map(Rc<RefCell<MapVal>>),
    Func(Rc<FuncVal>),
    Ref(Rc<Place>),
    /// `Rc<T>` / `Arc<T>`: clone shares
    RcBox(Rc<Value>),
    /// `RefCell<T>` / `Cell<T>` / `Mutex<T>`
    Cell(Rc<RefCell<Value>>),
    /// A local captured by a closure (internal; never observed by programs)
    Boxed(Rc<RefCell<Value>>),
    Iter(Rc<RefCell<IterVal>>),
    /// `a..b`, `a..=b`, `..b`, `a..` (Unit for a missing bound)
    Range(Rc<(Value, Value, bool)>),
    /// A JavaScript value (DOM element, event, ...)
    Js(crate::js::Value),
    /// `fmt::Formatter`
    Formatter(Rc<RefCell<String>>, Rc<FmtSpec>),
}

#[derive(Default, Clone, Debug)]
pub struct FmtSpec {
    pub alternate: bool,
    pub width: Option<usize>,
    pub precision: Option<usize>,
    pub fill: Option<char>,
    pub align: Option<char>,
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(RefCell::new(s.into())))
    }
    pub fn vec(v: Vec<Value>) -> Value {
        Value::Vec(Rc::new(RefCell::new(v)))
    }
    pub fn tuple(v: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(RefCell::new(v)))
    }
    pub fn int(v: i128, k: IntKind) -> Value {
        Value::Int(v, k)
    }
    pub fn usize(v: usize) -> Value {
        Value::Int(v as i128, IntKind::Usize)
    }
    pub fn addr(&self) -> usize {
        match self {
            Value::Str(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Vec(r) | Value::Array(r) | Value::Tuple(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Struct(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Enum(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Map(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Func(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Cell(r) | Value::Boxed(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::RcBox(r) => Rc::as_ptr(r) as *const u8 as usize,
            Value::Iter(r) => Rc::as_ptr(r) as *const u8 as usize,
            _ => 0,
        }
    }
    /// Strip the internal capture box.
    pub fn unboxed(self) -> Value {
        match self {
            Value::Boxed(b) => b.borrow().clone(),
            v => v,
        }
    }
    /// Follow references (`&`, `&mut`, `Rc`, `Box`) to the underlying value.
    pub fn deref_all(&self) -> Value {
        let mut v = self.clone();
        for _ in 0..64 {
            v = match v {
                Value::Ref(p) => p.get(),
                Value::RcBox(b) => (*b).clone(),
                Value::Boxed(b) => b.borrow().clone(),
                other => return other,
            };
        }
        v
    }
    /// Value semantics for Copy aggregates: arrays, tuples and `Copy` structs are copied when moved.
    pub fn moved(self) -> Value {
        match self {
            Value::Array(a) => Value::Array(Rc::new(RefCell::new(a.borrow().iter().map(|v| v.clone().moved()).collect()))),
            Value::Tuple(a) => Value::Tuple(Rc::new(RefCell::new(a.borrow().iter().map(|v| v.clone().moved()).collect()))),
            Value::Struct(s) if s.ty.copy => Value::Struct(Rc::new(StructVal { ty: s.ty.clone(), fields: RefCell::new(s.fields.borrow().iter().map(|v| v.clone().moved()).collect()) })),
            Value::Boxed(b) => b.borrow().clone().moved(),
            v => v,
        }
    }
    /// `.clone()`: a deep copy, except shared pointers (`Rc`) which share.
    pub fn deep_clone(&self) -> Value {
        match self {
            Value::Str(s) => Value::str(s.borrow().clone()),
            Value::Vec(v) => Value::Vec(Rc::new(RefCell::new(v.borrow().iter().map(|x| x.deep_clone()).collect()))),
            Value::Array(v) => Value::Array(Rc::new(RefCell::new(v.borrow().iter().map(|x| x.deep_clone()).collect()))),
            Value::Tuple(v) => Value::Tuple(Rc::new(RefCell::new(v.borrow().iter().map(|x| x.deep_clone()).collect()))),
            Value::Struct(s) => Value::Struct(Rc::new(StructVal { ty: s.ty.clone(), fields: RefCell::new(s.fields.borrow().iter().map(|x| x.deep_clone()).collect()) })),
            Value::Enum(e) => Value::Enum(Rc::new(EnumVal { ty: e.ty.clone(), variant: e.variant, fields: RefCell::new(e.fields.borrow().iter().map(|x| x.deep_clone()).collect()) })),
            Value::Map(m) => {
                let m = m.borrow();
                Value::Map(Rc::new(RefCell::new(MapVal { keys: m.keys.iter().map(|x| x.deep_clone()).collect(), vals: m.vals.iter().map(|x| x.deep_clone()).collect(), index: m.index.clone(), sorted: m.sorted, is_set: m.is_set, val_default: m.val_default.clone() })))
            }
            Value::Cell(c) => Value::Cell(Rc::new(RefCell::new(c.borrow().deep_clone()))),
            Value::Ref(p) => {
                // cloning through a shared reference clones the referent
                p.get().deep_clone()
            }
            Value::Boxed(b) => b.borrow().deep_clone(),
            other => other.clone(),
        }
    }
    pub fn truthy(&self) -> bool {
        matches!(self.deref_all(), Value::Bool(true))
    }
}

/// Structural equality (`==`).
pub fn eq_values(a: &Value, b: &Value) -> bool {
    let (a, b) = (a.deref_all(), b.deref_all());
    match (&a, &b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x, _), Value::Int(y, _)) => x == y,
        (Value::Float(x, _), Value::Float(y, _)) => x == y,
        (Value::Int(x, _), Value::Float(y, _)) | (Value::Float(y, _), Value::Int(x, _)) => *x as f64 == *y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => *x.borrow() == *y.borrow(),
        (Value::Vec(x) | Value::Array(x) | Value::Tuple(x), Value::Vec(y) | Value::Array(y) | Value::Tuple(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| eq_values(a, b))
        }
        (Value::Struct(x), Value::Struct(y)) => Rc::ptr_eq(&x.ty, &y.ty) && {
            let (x, y) = (x.fields.borrow(), y.fields.borrow());
            x.iter().zip(y.iter()).all(|(a, b)| eq_values(a, b))
        },
        (Value::Enum(x), Value::Enum(y)) => x.ty.name == y.ty.name && x.variant == y.variant && {
            let (x, y) = (x.fields.borrow(), y.fields.borrow());
            x.iter().zip(y.iter()).all(|(a, b)| eq_values(a, b))
        },
        (Value::Map(x), Value::Map(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            x.len() == y.len() && x.keys.iter().zip(x.vals.iter()).all(|(k, v)| y.get(k).is_some_and(|w| eq_values(v, w)))
        }
        (Value::Cell(x), Value::Cell(y)) => eq_values(&x.borrow(), &y.borrow()),
        (Value::Range(x), Value::Range(y)) => eq_values(&x.0, &y.0) && eq_values(&x.1, &y.1) && x.2 == y.2,
        (Value::Js(x), Value::Js(y)) => crate::js::interp::strict_equals(x, y),
        _ => false,
    }
}

/// Ordering (`<`, sort, BTreeMap).
pub fn cmp_values(a: &Value, b: &Value) -> Ordering {
    let (a, b) = (a.deref_all(), b.deref_all());
    match (&a, &b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Int(x, _), Value::Int(y, _)) => x.cmp(y),
        (Value::Float(x, _), Value::Float(y, _)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x, _), Value::Float(y, _)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x, _), Value::Int(y, _)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.borrow().as_str().cmp(y.borrow().as_str()),
        (Value::Vec(x) | Value::Array(x) | Value::Tuple(x), Value::Vec(y) | Value::Array(y) | Value::Tuple(y)) => {
            let (x, y) = (x.borrow(), y.borrow());
            for (a, b) in x.iter().zip(y.iter()) {
                let o = cmp_values(a, b);
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Struct(x), Value::Struct(y)) => {
            let (x, y) = (x.fields.borrow(), y.fields.borrow());
            for (a, b) in x.iter().zip(y.iter()) {
                let o = cmp_values(a, b);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        }
        (Value::Enum(x), Value::Enum(y)) => x.variant.cmp(&y.variant).then_with(|| {
            let (x, y) = (x.fields.borrow(), y.fields.borrow());
            for (a, b) in x.iter().zip(y.iter()) {
                let o = cmp_values(a, b);
                if o != Ordering::Equal {
                    return o;
                }
            }
            Ordering::Equal
        }),
        (Value::Unit, Value::Unit) => Ordering::Equal,
        _ => Ordering::Equal,
    }
}
