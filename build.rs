// Give the main thread a large stack: the windowed browser runs JavaScript on it,
// and the tree-walking interpreter recurses deeply. Headless commands use a
// dedicated large-stack thread instead.
fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("apple") {
        println!("cargo:rustc-link-arg-bins=-Wl,-stack_size,0x10000000");
    } else if target.contains("windows-msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:268435456");
    } else if target.contains("windows-gnu") {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,268435456");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
