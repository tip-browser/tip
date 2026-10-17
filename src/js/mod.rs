//! JavaScript engine: lexer, parser, tree-walking interpreter and standard library.

pub mod ast;
pub mod builtins;
pub mod coro;
pub mod interp;
pub mod lexer;
pub mod module;
pub mod parser;
pub mod regex;
pub mod value;

pub use interp::{Interp, R};
pub use value::{Obj, ObjKind, PropKey, Value};

#[cfg(test)]
mod tests;
