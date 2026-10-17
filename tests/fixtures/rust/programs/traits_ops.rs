use std::collections::HashMap;
use std::fmt::Display;
use std::ops::Add;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct V2 { x: i32, y: i32 }
impl Add for V2 { type Output = V2; fn add(self, o: V2) -> V2 { V2 { x: self.x + o.x, y: self.y + o.y } } }

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, PartialOrd, Ord)]
enum Suit { Hearts, Spades }

#[derive(Debug, Clone)]
struct Card { suit: Suit, rank: u8 }

struct Matrix { data: Vec<Vec<f64>>, n: usize }
impl Matrix {
    fn identity(n: usize) -> Matrix {
        let mut data = vec![vec![0.0; n]; n];
        for i in 0..n { data[i][i] = 1.0; }
        Matrix { data, n }
    }
    fn mul(&self, o: &Matrix) -> Matrix {
        let mut r = Matrix { data: vec![vec![0.0; self.n]; self.n], n: self.n };
        for i in 0..self.n { for j in 0..self.n { for k in 0..self.n { r.data[i][j] += self.data[i][k] * o.data[k][j]; } } }
        r
    }
}

fn show<T: Display>(items: &[T]) -> String { items.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", ") }

fn modify(v: &mut Vec<i32>) { v.push(100); v[0] = -1; }
fn bump(x: &mut i32) { *x += 1; }

fn primes(n: usize) -> Vec<usize> {
    let mut sieve = vec![true; n + 1];
    sieve[0] = false; if n >= 1 { sieve[1] = false; }
    let mut i = 2;
    while i * i <= n {
        if sieve[i] { let mut j = i * i; while j <= n { sieve[j] = false; j += i; } }
        i += 1;
    }
    sieve.iter().enumerate().filter(|(_, &p)| p).map(|(i, _)| i).collect()
}

enum Expr { Num(f64), Add(Box<Expr>, Box<Expr>), Mul(Box<Expr>, Box<Expr>), Neg(Box<Expr>) }
fn eval(e: &Expr) -> f64 {
    match e {
        Expr::Num(n) => *n,
        Expr::Add(a, b) => eval(a) + eval(b),
        Expr::Mul(a, b) => eval(a) * eval(b),
        Expr::Neg(a) => -eval(a),
    }
}

struct Tokenizer<'a> { chars: std::iter::Peekable<std::str::Chars<'a>> }
impl<'a> Tokenizer<'a> {
    fn new(s: &'a str) -> Self { Tokenizer { chars: s.chars().peekable() } }
    fn tokens(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() { self.chars.next(); }
            else if c.is_ascii_digit() {
                let mut num = String::new();
                while let Some(&d) = self.chars.peek() { if d.is_ascii_digit() { num.push(d); self.chars.next(); } else { break; } }
                out.push(num);
            } else { out.push(c.to_string()); self.chars.next(); }
        }
        out
    }
}

fn main() {
    let a = V2 { x: 1, y: 2 };
    let b = a + V2 { x: 10, ..Default::default() };
    println!("{:?} {:?} {}", b, V2::default(), a == V2 { x: 1, y: 2 });
    let mut cards = vec![Card { suit: Suit::Spades, rank: 3 }, Card { suit: Suit::Hearts, rank: 12 }, Card { suit: Suit::Hearts, rank: 2 }];
    cards.sort_by_key(|c| (c.suit, c.rank));
    println!("{:?}", cards.iter().map(|c| format!("{:?}{}", c.suit, c.rank)).collect::<Vec<_>>());
    let mut by_suit: HashMap<Suit, Vec<u8>> = HashMap::new();
    for c in &cards { by_suit.entry(c.suit).or_default().push(c.rank); }
    let mut ks: Vec<_> = by_suit.keys().copied().collect();
    ks.sort();
    println!("{:?} {:?}", ks, by_suit[&Suit::Hearts]);
    let m = Matrix::identity(3);
    let mut m2 = Matrix::identity(3);
    m2.data[0][1] = 2.0;
    println!("{:?}", m.mul(&m2).data[0]);
    println!("{}", show(&[1.5, 2.0]));
    let mut v = vec![1, 2, 3];
    modify(&mut v);
    let mut n = 5;
    bump(&mut n);
    let r = &mut n;
    *r *= 2;
    println!("{:?} {}", v, n);
    println!("{:?}", primes(30));
    let e = Expr::Add(Box::new(Expr::Num(2.0)), Box::new(Expr::Mul(Box::new(Expr::Num(3.0)), Box::new(Expr::Neg(Box::new(Expr::Num(4.0)))))));
    println!("{}", eval(&e));
    println!("{:?}", Tokenizer::new("12 + 3*(45)").tokens());
    let mut fizz = String::new();
    for i in 1..=15 { fizz += &match (i % 3, i % 5) { (0, 0) => "FB".to_string(), (0, _) => "F".to_string(), (_, 0) => "B".to_string(), _ => i.to_string() }; fizz.push(' '); }
    println!("{}", fizz.trim_end());
    let s = "Hello, World";
    let caps = s.chars().filter(|c| c.is_uppercase()).count();
    let lower = s.to_lowercase().replace(",", "");
    println!("{caps} {lower} {:?} {:?}", s.find("World"), s.split_once(", "));
    let nested = vec![vec![1, 2], vec![3], vec![]];
    let flat: Vec<i32> = nested.into_iter().flatten().collect();
    println!("{:?}", flat);
    let mut sorted = vec!["pear", "fig", "apple"];
    sorted.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    println!("{:?}", sorted);
    let avg = { let xs = [2.0, 4.0, 9.0]; xs.iter().sum::<f64>() / xs.len() as f64 };
    println!("{avg:.2}");
    let t: Vec<String> = std::iter::repeat("ab").take(3).map(String::from).collect();
    println!("{}", t.join("-"));
    let word_lens: HashMap<&str, usize> = ["a", "bb", "ccc"].iter().map(|w| (*w, w.len())).collect();
    let mut wl: Vec<_> = word_lens.into_iter().collect();
    wl.sort();
    println!("{:?}", wl);
    let maybe: Option<Option<i32>> = Some(None);
    println!("{:?} {:?}", maybe, maybe.flatten());
    let x = 5u64; let y = x as f64 / 2.0; let z = -3.7f64 as i32; let c = 65u8 as char; let d = 'a' as u32;
    println!("{y} {z} {c} {d} {}", u8::MAX);
    let mut it = (1..4).peekable();
    while let Some(v) = it.next() { if let Some(nx) = it.peek() { print!("{v}->{nx} "); } else { println!("{v}"); } }
    let text = String::from("line one\nline two\n\nline four");
    for (i, l) in text.lines().enumerate().filter(|(_, l)| !l.is_empty()) { print!("{}:{} ", i + 1, l.len()); }
    println!();
    let closures: Vec<Box<dyn Fn(i32) -> i32>> = vec![Box::new(|x| x + 1), Box::new(|x| x * x)];
    println!("{:?}", closures.iter().map(|f| f(7)).collect::<Vec<_>>());
    let mut total = 0;
    let mut add = |n: i32| total += n;
    add(3); add(4);
    println!("{total}");
}
