use std::rc::Rc;
use std::cell::RefCell;
use std::collections::{HashSet, BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Color { Red, Green, Blue }

#[derive(Debug, Default)]
struct Stats { count: usize, total: i64 }

struct Counter { n: u32 }
impl Iterator for Counter {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.n < 5 { self.n += 1; Some(self.n) } else { None }
    }
}

#[derive(Debug)]
struct Node { val: i32, children: Vec<Rc<RefCell<Node>>> }

fn sum_tree(n: &Rc<RefCell<Node>>) -> i32 {
    let node = n.borrow();
    node.val + node.children.iter().map(|c| sum_tree(c)).sum::<i32>()
}

fn apply<F: Fn(i32) -> i32>(f: F, x: i32) -> i32 { f(x) }
fn make_adder(n: i32) -> impl Fn(i32) -> i32 { move |x| x + n }
fn make_counter() -> impl FnMut() -> i32 { let mut c = 0; move || { c += 1; c } }

fn largest<T: PartialOrd + Copy>(xs: &[T]) -> T {
    let mut m = xs[0];
    for &x in xs { if x > m { m = x; } }
    m
}

struct Stack<T> { items: Vec<T> }
impl<T: std::fmt::Debug> Stack<T> {
    fn new() -> Self { Stack { items: Vec::new() } }
    fn push(&mut self, t: T) { self.items.push(t); }
    fn pop(&mut self) -> Option<T> { self.items.pop() }
    fn peek(&self) -> Option<&T> { self.items.last() }
    fn len(&self) -> usize { self.items.len() }
    fn is_empty(&self) -> bool { self.items.is_empty() }
}

fn main() {
    let c: Vec<u32> = Counter { n: 0 }.collect();
    println!("{:?}", c);
    let total: u32 = Counter { n: 0 }.zip(Counter { n: 0 }.skip(1)).map(|(a, b)| a * b).filter(|x| x % 3 == 0).sum();
    println!("total {total}");
    let leaf = Rc::new(RefCell::new(Node { val: 3, children: vec![] }));
    let root = Rc::new(RefCell::new(Node { val: 1, children: vec![leaf.clone(), Rc::new(RefCell::new(Node { val: 5, children: vec![] }))] }));
    leaf.borrow_mut().val = 10;
    println!("tree {}", sum_tree(&root));
    println!("rc count {}", Rc::strong_count(&leaf));
    let shared = Rc::new(RefCell::new(vec![1, 2]));
    let s2 = Rc::clone(&shared);
    s2.borrow_mut().push(3);
    println!("{:?}", shared.borrow());
    println!("{} {}", apply(|x| x * 10, 4), make_adder(5)(6));
    let mut ctr = make_counter();
    ctr(); ctr();
    println!("counter {}", ctr());
    println!("{} {} {}", largest(&[3, 9, 2]), largest(&[1.5, 0.2]), largest(&['a', 'z', 'q']));
    let mut st: Stack<String> = Stack::new();
    st.push("a".into()); st.push("b".into());
    println!("{:?} {}", st.peek(), st.len());
    println!("{:?} {}", st.pop(), st.is_empty());
    let mut set = HashSet::new();
    for c in [Color::Red, Color::Blue, Color::Red] { set.insert(c); }
    println!("set len {}", set.len());
    let mut bt = BTreeMap::new();
    bt.insert("z", 1); bt.insert("a", 2); bt.insert("m", 3);
    println!("{:?}", bt);
    for (k, v) in &bt { print!("{k}={v} "); }
    println!();
    let mut dq: VecDeque<i32> = VecDeque::new();
    dq.push_back(1); dq.push_front(0); dq.push_back(2);
    while let Some(x) = dq.pop_front() { print!("{x},"); }
    println!();
    let mut stats = Stats::default();
    for x in [3, 4, 5] { stats.count += 1; stats.total += x; }
    println!("{:?}", stats);
    let mut v = vec![3, 1, 4, 1, 5, 9, 2, 6];
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.dedup();
    println!("{:?} max={:?} pos={:?}", v, v.iter().max(), v.iter().position(|&x| x == 4));
    let text = "a,b,,c";
    let parts: Vec<&str> = text.split(',').collect();
    println!("{:?} {}", parts, parts.join("|"));
    let nums: Result<Vec<i32>, _> = "1 2 x".split(' ').map(|s| s.parse::<i32>()).collect();
    println!("{:?}", nums.is_err());
    let n: i64 = "  42 ".trim().parse().unwrap();
    println!("{}", n * 2);
    let mut s = String::from("hi");
    s.push_str(" there");
    s += "!";
    println!("{s} {} {}", s.len(), &s[0..2]);
    let words: Vec<String> = vec!["x".to_string(); 3];
    println!("{}", words.concat());
    let chunks: Vec<Vec<i32>> = (1..=7).collect::<Vec<_>>().chunks(3).map(|c| c.to_vec()).collect();
    println!("{:?}", chunks);
    let mut matrix = [[0u8; 2]; 2];
    matrix[1][0] = 7;
    println!("{:?}", matrix);
    let x: u8 = 250;
    println!("{:?} {}", x.checked_add(10), x.wrapping_add(10));
    let label = match Color::Green { Color::Red => "r", Color::Green => "g", _ => "?" };
    println!("{label} {}", Color::Blue as i32);
    let o: Option<&str> = None;
    println!("{}", o.map(|s| s.len()).unwrap_or(0));
    let pairs: Vec<(usize, char)> = "abc".chars().enumerate().collect();
    println!("{:?}", pairs);
    let fold = (1..=5).fold(String::new(), |acc, x| format!("{acc}{x}"));
    println!("{fold}");
    'outer: for i in 0..3 { for j in 0..3 { if i * j == 2 { println!("found {i} {j}"); break 'outer; } } }
    let v2: Vec<i32> = Vec::with_capacity(10);
    assert!(v2.is_empty(), "should be empty");
    assert_eq!(1 + 1, 2);
    let s = String::from("héllo");
    println!("{} {}", s.chars().count(), s.len());
    let x = 7;
    let desc = if x % 2 == 0 { "even" } else { "odd" };
    println!("{desc}");
    let r: i32 = 7 / 2; let m = -7 % 3; let e = 2i32.pow(10);
    println!("{r} {m} {e} {}", (-7i32).rem_euclid(3));
    let big: i32 = i32::MAX;
    println!("{:?}", big.checked_mul(2));
}
