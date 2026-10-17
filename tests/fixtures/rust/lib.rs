#[export]
fn greet(name: String) -> String {
    format!("Hello, {name}!")
}

fn main() {
    web::by_id("ext").unwrap().set_text("external loaded");
}
