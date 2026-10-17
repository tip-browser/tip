//! Unit tests for the Rust runtime (headless). The programs under
//! `tests/fixtures/rust/programs` have expected output produced by real `rustc` builds.

use super::run_headless;

fn run(src: &str) -> String {
    let (out, err) = run_headless(src, "test.rs");
    if let Some(e) = err {
        panic!("{out}\n{e}");
    }
    out
}

fn fails(src: &str) -> String {
    run_headless(src, "test.rs").1.expect("program should fail")
}

#[test]
fn hello() {
    assert_eq!(run("fn main() { println!(\"hi {}\", 1 + 2); print!(\"a\"); print!(\"b\\n\"); }"), "hi 3\nab\n");
}

#[test]
fn programs_match_rustc() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rust/programs");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let src = std::fs::read_to_string(&path).unwrap();
            let want = std::fs::read_to_string(path.with_extension("out")).unwrap();
            let (got, err) = run_headless(&src, &path.display().to_string());
            assert!(err.is_none(), "{}: {}", path.display(), err.unwrap());
            assert_eq!(got, want, "{}", path.display());
            n += 1;
        }
    }
    assert!(n >= 5);
}

#[test]
fn panics_and_errors() {
    assert!(fails("fn main() { let v = vec![1]; v[3]; }").contains("index out of bounds: the len is 1 but the index is 3"));
    assert!(fails("fn main() { let x: u8 = 255; let y = x + 1; }").contains("attempt to add with overflow"));
    assert!(fails("fn main() { let o: Option<i32> = None; o.unwrap(); }").contains("called `Option::unwrap()` on a `None` value"));
    assert!(fails("fn main() { let x = 1 +; }").starts_with("error: expected expression"));
    assert!(fails("fn main() { nope(); }").contains("cannot find function `nope`"));
    assert!(fails("fn f(n: u64) -> u64 { f(n + 1) }\nfn main() { f(0); }").contains("stack overflow"));
    assert!(fails("fn main() -> Result<(), String> { Err(\"bad\".into()) }").contains("Error: \"bad\""));
}

#[test]
fn web_api_needs_a_page() {
    assert!(fails("fn main() { web::by_id(\"x\"); }").contains("only available inside a page"));
}
