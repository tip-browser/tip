// <script type="text/rust">: the in-page Rust runtime.

const rust = (src, body = '') => page.setContent(`${body}<script type="text/rust">${src}</script>`);
const errors = () => page.console().filter((m) => m.level === 'error').map((m) => m.text);
const logs = () => page.console().filter((m) => m.level === 'log').map((m) => m.text);

describe('rust scripts', () => {
  test('main runs at load and println! goes to the console', () => {
    rust(`fn main() { let v: Vec<i32> = (1..=4).map(|x| x * x).collect(); println!("squares {:?}", v); }`);
    expect(logs()).toContain('squares [1, 4, 9, 16]');
  });

  test('DOM access through the web module', () => {
    rust(`
      use web::*;
      fn main() {
        let list = by_id("list").unwrap();
        for i in 0..3 {
          let li = create("li");
          li.set_text(&format!("item {i}"));
          li.add_class("row");
          list.append(li);
        }
        let first = query("#list li").unwrap();
        first.set_attr("data-first", "yes");
        body().set_style("background", "rgb(1, 2, 3)");
        web::document().set("title", "rust set this");
      }`, '<ul id=list></ul>');
    expect(page.count('#list li.row')).toBe(3);
    expect(page.text('#list li:last-child')).toBe('item 2');
    expect(page.attr('#list li', 'data-first')).toBe('yes');
    expect(page.title()).toBe('rust set this');
  });

  test('event handlers are Rust closures with shared state', () => {
    rust(`
      use std::{cell::RefCell, rc::Rc};
      fn main() {
        let n = Rc::new(RefCell::new(0));
        let n2 = n.clone();
        web::by_id("b").unwrap().on("click", move |ev| {
          *n2.borrow_mut() += 1;
          let target = ev.target().unwrap();
          web::by_id("out").unwrap().set_text(&format!("{} clicks on {}", n2.borrow(), target.tag()));
        });
      }`, '<button id=b>go</button><p id=out></p>');
    page.click('#b');
    page.click('#b');
    expect(page.text('#out')).toBe('2 clicks on button');
  });

  test('#[export] functions are callable from JavaScript', () => {
    rust(`
      #[derive(Debug)]
      struct Stats { min: i64, max: i64, mean: f64 }
      #[export]
      fn add(a: i32, b: i32) -> i32 { a + b }
      #[export]
      fn stats(xs: Vec<i64>) -> Stats {
        let sum: i64 = xs.iter().sum();
        Stats { min: *xs.iter().min().unwrap(), max: *xs.iter().max().unwrap(), mean: sum as f64 / xs.len() as f64 }
      }
      #[export]
      fn shout(s: String) -> Option<String> { if s.is_empty() { None } else { Some(s.to_uppercase()) } }
    `);
    expect(page.evaluate('rust.add(2, 40)')).toBe(42);
    expect(page.evaluate('JSON.stringify(rust.stats([4, 1, 7]))')).toBe('{"min":1,"max":7,"mean":4}');
    expect(page.evaluate('rust.shout("hey")')).toBe('HEY');
    expect(page.evaluate('rust.shout("")')).toBe(null);
  });

  test('panics surface as JS errors and console messages', () => {
    rust(`#[export] fn boom(i: usize) -> i32 { let v = vec![1, 2]; v[i] }`);
    expect(page.evaluate('(() => { try { rust.boom(5); return "no"; } catch (e) { return e.message; } })()')).toBe('Rust panic: index out of bounds: the len is 2 but the index is 5');
    expect(errors().some((e) => e.startsWith("thread 'main' panicked at inline.rs:1:"))).toBe(true);
  });

  test('compile errors are reported rustc-style', () => {
    rust(`fn main() {\n  let x = 1;\n  let y = x + ;\n}`);
    const e = errors()[0];
    expect(e).toContain('error: expected expression');
    expect(e).toContain('--> inline.rs:3:');
  });

  test('timers call back into Rust', () => {
    rust(`fn main() { web::set_timeout(|| web::by_id("t").unwrap().set_text("fired"), 10); }`, '<p id=t>wait</p>');
    page.wait(100);
    expect(page.text('#t')).toBe('fired');
  });

  test('external Rust files load with src', () => {
    page.goto('fixtures/rust/external.html');
    expect(page.text('#ext')).toBe('external loaded');
    expect(page.evaluate('rust.greet("Tip")')).toBe('Hello, Tip!');
  });

  test('the demo page works end to end', () => {
    page.goto('fixtures/rust/demo.html');
    page.click('#inc');
    page.click('#inc');
    page.click('#dec');
    expect(page.text('#count')).toBe('1');
    expect(page.count('#todos li')).toBe(2);
    page.fill('#todo-text', 'Ship it');
    page.click('#add');
    expect(page.count('#todos li')).toBe(3);
    expect(page.text('#todo-status')).toBe('2 of 3 left');
    page.click('#todos li:nth-child(2)');
    expect(page.text('#todo-status')).toBe('1 of 3 left');
    page.click('#sieve');
    expect(page.text('#primes')).toContain('25 primes: 2 3 5 7');
  });

  test('the session is discarded when the page is left', () => {
    rust(`#[export] fn ping() -> i32 { 1 }`);
    expect(page.evaluate('typeof rust')).toBe('object');
    page.setContent('<p>next</p>');
    expect(page.evaluate('typeof rust')).toBe('undefined');
  });
});
