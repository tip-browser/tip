use std::collections::HashMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Point { x: f64, y: f64 }

impl Point {
    fn new(x: f64, y: f64) -> Self { Point { x, y } }
    fn dist(&self, o: &Point) -> f64 { ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt() }
    fn shift(&mut self, dx: f64) { self.x += dx; }
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({:.1}, {:.1})", self.x, self.y)
    }
}

#[derive(Debug)]
enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Empty }

impl Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => std::f64::consts::PI * r * r,
            Shape::Rect { w, h } => w * h,
            Shape::Empty => 0.0,
        }
    }
}

trait Animal {
    fn name(&self) -> String;
    fn speak(&self) -> String { format!("{} makes a sound", self.name()) }
}
struct Dog;
struct Cat { lives: u8 }
impl Animal for Dog { fn name(&self) -> String { "Dog".to_string() } fn speak(&self) -> String { "Woof".into() } }
impl Animal for Cat { fn name(&self) -> String { format!("Cat{}", self.lives) } }

fn fib(n: u64) -> u64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }

fn divide(a: i32, b: i32) -> Result<i32, String> {
    if b == 0 { return Err("div by zero".to_string()); }
    Ok(a / b)
}

fn calc() -> Result<i32, String> {
    let x = divide(10, 2)?;
    let y = divide(x, 0)?;
    Ok(y)
}

fn main() {
    let mut p = Point::new(1.0, 2.0);
    let q = p;
    p.shift(2.0);
    println!("{} {} {:?}", p, q, q);
    println!("dist {:.3}", p.dist(&q));
    let shapes = vec![Shape::Circle(1.0), Shape::Rect { w: 2.0, h: 3.0 }, Shape::Empty];
    let total: f64 = shapes.iter().map(|s| s.area()).sum();
    println!("total {:.2} {:?}", total, shapes[1]);
    let animals: Vec<Box<dyn Animal>> = vec![Box::new(Dog), Box::new(Cat { lives: 9 })];
    for a in &animals { println!("{}", a.speak()); }
    println!("fib {}", fib(20));
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in "the cat the dog the end".split_whitespace() {
        *counts.entry(w.to_string()).or_insert(0) += 1;
    }
    let mut keys: Vec<_> = counts.iter().collect();
    keys.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("{:?}", keys);
    println!("{:?}", calc());
    let v: Vec<i32> = (1..=10).filter(|x| x % 2 == 0).map(|x| x * x).collect();
    println!("{:?} sum={}", v, v.iter().sum::<i32>());
    let s: String = "hello".chars().rev().collect();
    println!("{s} {}", s.to_uppercase());
    let mut grid = vec![vec![0; 3]; 3];
    for i in 0..3 { for j in 0..3 { grid[i][j] = i * 3 + j; } }
    println!("{:?}", grid);
    let words = vec!["apple", "bob", "kayak"];
    let pals: Vec<&str> = words.iter().filter(|w| w.chars().eq(w.chars().rev())).cloned().collect();
    println!("{:?}", pals);
    let mut n = 0u32;
    let r = loop { n += 1; if n == 5 { break n * 2; } };
    println!("loop {r}");
    let opt: Option<i32> = Some(3);
    if let Some(x) = opt { println!("some {x}"); }
    match 42 { 0..=9 => println!("small"), 10 | 20 => println!("ten"), n if n > 40 => println!("big {n}"), _ => println!("other") }
    let t = (1, "two", 3.0);
    let (a, b, c) = t;
    println!("{a} {b} {c} {}", t.1);
    let mut arr = [5, 3, 8, 1];
    arr.sort();
    println!("{:?} {}", arr, arr.len());
    println!("{:>8}|{:<6}|{:^7}|{:08.3}|{:#x}|{:#b}", "r", "l", "c", 3.14159, 255, 5);
    println!("{:#?}", Point::new(0.5, 1.5));
}
