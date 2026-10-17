use std::collections::HashMap;
use std::fmt;
use std::ops::{Mul, Neg, Index, AddAssign};
use std::cmp::Ordering;

trait Shape { const SIDES: u32; fn area(&self) -> f64; fn describe(&self) -> String { format!("{} sides, area {:.2}", Self::SIDES, self.area()) } }
struct Sq(f64);
struct Tri { b: f64, h: f64 }
impl Shape for Sq { const SIDES: u32 = 4; fn area(&self) -> f64 { self.0 * self.0 } }
impl Shape for Tri { const SIDES: u32 = 3; fn area(&self) -> f64 { 0.5 * self.b * self.h } }

fn total_area<S: Shape>(shapes: &[S]) -> f64 { shapes.iter().map(|s| s.area()).sum() }

#[derive(Debug, Clone, Copy, PartialEq)]
struct C { re: f64, im: f64 }
impl Mul for C { type Output = C; fn mul(self, o: C) -> C { C { re: self.re * o.re - self.im * o.im, im: self.re * o.im + self.im * o.re } } }
impl Neg for C { type Output = C; fn neg(self) -> C { C { re: -self.re, im: -self.im } } }
impl AddAssign for C { fn add_assign(&mut self, o: C) { self.re += o.re; self.im += o.im; } }
impl fmt::Display for C { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { if self.im >= 0.0 { write!(f, "{}+{}i", self.re, self.im) } else { write!(f, "{}{}i", self.re, self.im) } } }

struct Poly(Vec<i32>);
impl Index<usize> for Poly { type Output = i32; fn index(&self, i: usize) -> &i32 { &self.0[i] } }

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Emp { dept: String, age: u32, name: String }

#[derive(Debug)]
enum Cmd { Push(i64), Pop, Add, Mul, Dup }

fn run(cmds: &[Cmd]) -> Result<Vec<i64>, String> {
    let mut st: Vec<i64> = Vec::new();
    for (i, c) in cmds.iter().enumerate() {
        match c {
            Cmd::Push(n) => st.push(*n),
            Cmd::Pop => { st.pop().ok_or(format!("pop on empty at {i}"))?; }
            Cmd::Add | Cmd::Mul => {
                let b = st.pop().ok_or("underflow")?;
                let a = st.pop().ok_or_else(|| "underflow".to_string())?;
                st.push(if matches!(c, Cmd::Add) { a + b } else { a * b });
            }
            Cmd::Dup => { let t = *st.last().ok_or("empty")?; st.push(t); }
        }
    }
    Ok(st)
}

fn classify(xs: &[i32]) -> &'static str {
    match xs {
        [] => "empty",
        [x] if *x < 0 => "one negative",
        [_] => "one",
        [first, .., last] if first == last => "same ends",
        [_, rest @ ..] => if rest.len() > 2 { "long" } else { "short" },
    }
}

fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }

fn caesar(s: &str, k: u8) -> String {
    s.chars().map(|c| if c.is_ascii_lowercase() { (((c as u8 - b'a' + k) % 26) + b'a') as char } else { c }).collect()
}

fn main() {
    println!("{}", Sq(2.0).describe());
    println!("{}", Tri { b: 3.0, h: 4.0 }.describe());
    println!("{:.1}", total_area(&[Sq(1.0), Sq(3.0)]));
    let mut z = C { re: 1.0, im: 2.0 };
    let w = z * C { re: 0.0, im: 1.0 };
    z += w;
    println!("{} {} {}", w, -z, z);
    let p = Poly(vec![3, 0, 2]);
    println!("{} {}", p[0], p[2]);
    let mut emps = vec![
        Emp { dept: "eng".into(), age: 30, name: "Zed".into() },
        Emp { dept: "art".into(), age: 25, name: "Amy".into() },
        Emp { dept: "eng".into(), age: 22, name: "Bob".into() },
    ];
    emps.sort();
    println!("{:?}", emps.iter().map(|e| e.name.as_str()).collect::<Vec<_>>());
    let oldest = emps.iter().max_by_key(|e| e.age).unwrap();
    println!("{}", oldest.name);
    let mut by_dept: HashMap<&str, Vec<&Emp>> = HashMap::new();
    for e in &emps { by_dept.entry(e.dept.as_str()).or_insert_with(Vec::new).push(e); }
    let mut depts: Vec<_> = by_dept.iter().map(|(d, es)| (d.to_string(), es.len())).collect();
    depts.sort_by(|a, b| b.1.cmp(&a.1));
    println!("{:?}", depts);
    println!("{:?}", run(&[Cmd::Push(2), Cmd::Push(3), Cmd::Add, Cmd::Dup, Cmd::Mul]));
    println!("{:?}", run(&[Cmd::Push(1), Cmd::Add]));
    println!("{:?}", run(&[Cmd::Pop]));
    for xs in [&[][..], &[-1], &[5], &[1, 2, 1], &[1, 2, 3, 4, 5], &[1, 2]] { print!("{} | ", classify(xs)); }
    println!();
    println!("{}", gcd(48, 18));
    println!("{}", caesar("hello, world", 3));
    let width = 10; let prec = 2;
    println!("[{:>width$.prec$}] [{:<w2$}] [{:+}] [{:e}]", 3.14159, "ab", 5, 1234.5, w2 = 4);
    println!("{:?} {:?} {:?}", "q\"uote\n", 'c', '\'');
    println!("{0} {1} {0} {name}", "a", "b", name = "n");
    let v = vec![10, 20, 30, 40];
    println!("{:?} {:?} {:?}", v.binary_search(&30), v.binary_search(&25), v.iter().rev().skip(1).step_by(2).collect::<Vec<_>>());
    println!("{:?}", v.windows(2).map(|w| w[1] - w[0]).collect::<Vec<_>>());
    let (evens, odds): (Vec<i32>, Vec<i32>) = (1..10).partition(|x| x % 2 == 0);
    println!("{:?} {:?}", evens, odds);
    let s = "The quick brown fox";
    let acronym: String = s.split(' ').filter_map(|w| w.chars().next()).collect();
    let longest = s.split_whitespace().max_by(|a, b| a.len().cmp(&b.len())).unwrap();
    println!("{acronym} {longest} {}", s.to_lowercase().matches('o').count());
    let x = 5;
    let x = x * 2;
    { let x = "shadow"; print!("{x} "); }
    println!("{x}");
    let mut fns: Vec<Box<dyn Fn() -> usize>> = Vec::new();
    for i in 0..3 { fns.push(Box::new(move || i * 100)); }
    println!("{:?}", fns.iter().map(|f| f()).collect::<Vec<_>>());
    let n: i32 = -17;
    println!("{} {} {} {}", n / 5, n % 5, n.abs(), (n as f64).abs().sqrt().floor());
    println!("{} {}", u32::MAX.wrapping_add(1), (300i32 as u8));
    let mut counts = [0usize; 26];
    for b in "banana".bytes() { counts[(b - b'a') as usize] += 1; }
    println!("{:?}", counts.iter().enumerate().filter(|(_, &c)| c > 0).map(|(i, c)| format!("{}{}", (b'a' + i as u8) as char, c)).collect::<Vec<_>>());
    let ord = 3.cmp(&5);
    println!("{:?} {:?} {}", ord, 2.0f64.partial_cmp(&1.0), ord == Ordering::Less);
    let data = vec![("a", 3), ("b", 1), ("c", 2)];
    let best = data.iter().min_by(|x, y| x.1.cmp(&y.1)).map(|(k, _)| *k);
    println!("{:?}", best);
    let text = "k1=v1;k2=v2;bad;k3=";
    let kv: Vec<(String, String)> = text.split(';').filter_map(|p| { let (k, v) = p.split_once('=')?; Some((k.to_string(), v.to_string())) }).collect();
    println!("{:?}", kv);
    let mut grid = vec![vec!['.'; 5]; 3];
    grid[1][2] = '#';
    for row in &grid { println!("{}", row.iter().collect::<String>()); }
    let total: u64 = (1..=20u64).product();
    println!("{total}");
}
