#[derive(Debug)]
struct W(i32);
#[derive(Debug)]
enum E { A(i32) }
fn main() { let v: Vec<W> = vec![1, 2].into_iter().map(W).collect(); let e: Vec<E> = vec![3].into_iter().map(E::A).collect(); let o = Some(4).map(W); println!("{:?} {:?} {:?}", v, e, o); }
