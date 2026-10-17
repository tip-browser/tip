//! Coroutines for generators and async functions.
//!
//! A tree-walking interpreter cannot suspend in the middle of a function, so
//! each coroutine body runs on its own OS thread. Control is handed back and
//! forth over channels so that exactly one thread ever touches the
//! interpreter at a time; the channel operations provide the necessary
//! happens-before ordering. Values cross threads inside `SendBox`, which is
//! sound only because of that strict hand-off discipline.

use std::cell::{Cell, RefCell};
use std::sync::mpsc::{channel, Receiver, Sender};

use super::interp::Interp;
use super::value::Value;

pub struct SendBox<T>(pub T);
// SAFETY: see module docs — values are only ever accessed by the single running thread.
unsafe impl<T> Send for SendBox<T> {}

pub enum Resume {
    Next(Value),
    Throw(Value),
    Return(Value),
    Abort,
}

pub enum Yield {
    Yielded(Value),
    Awaiting(Value),
    Done(Result<Value, Value>),
}

/// Coroutine-side channel ends, referenced through `Interp::current_co`.
pub struct CoSide {
    tx: Sender<SendBox<Yield>>,
    rx: Receiver<SendBox<(*mut Interp, Resume)>>,
    /// The `current_co` value of whoever resumed us, restored when we suspend.
    outer: Cell<Option<*const CoSide>>,
    outer_depth: Cell<usize>,
    outer_stack: Cell<(usize, usize)>,
    my_stack: Cell<(usize, usize)>,
    pub is_async: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoState {
    NotStarted,
    Suspended,
    Running,
    Done,
}

type Body = Box<dyn FnOnce(&mut Interp) -> Result<Value, Value>>;

pub struct Coroutine {
    pub state: Cell<CoState>,
    tx: Sender<SendBox<(*mut Interp, Resume)>>,
    rx: Receiver<SendBox<Yield>>,
    body: RefCell<Option<SendBox<(Body, Sender<SendBox<Yield>>, Receiver<SendBox<(*mut Interp, Resume)>>)>>>,
    thread: RefCell<Option<std::thread::JoinHandle<()>>>,
    pub is_async: bool,
}

pub const CO_STACK: usize = 64 * 1024 * 1024;

impl Coroutine {
    pub fn new(body: Body, is_async: bool) -> Coroutine {
        let (to_co, co_rx) = channel::<SendBox<(*mut Interp, Resume)>>();
        let (co_tx, from_co) = channel::<SendBox<Yield>>();
        Coroutine {
            state: Cell::new(CoState::NotStarted),
            tx: to_co,
            rx: from_co,
            body: RefCell::new(Some(SendBox((body, co_tx, co_rx)))),
            thread: RefCell::new(None),
            is_async,
        }
    }

    /// Resume the coroutine and block until it yields, awaits or finishes.
    pub fn resume(&self, it: &mut Interp, msg: Resume) -> Yield {
        match self.state.get() {
            CoState::Done => {
                return match msg {
                    Resume::Throw(e) => Yield::Done(Err(e)),
                    Resume::Return(v) => Yield::Done(Ok(v)),
                    _ => Yield::Done(Ok(Value::Undefined)),
                };
            }
            CoState::Running => return Yield::Done(Err(it.make_error("TypeError", "Generator is already running"))),
            CoState::NotStarted => {
                match msg {
                    Resume::Return(v) => {
                        self.state.set(CoState::Done);
                        self.body.borrow_mut().take();
                        return Yield::Done(Ok(v));
                    }
                    Resume::Throw(e) => {
                        self.state.set(CoState::Done);
                        self.body.borrow_mut().take();
                        return Yield::Done(Err(e));
                    }
                    Resume::Abort => {
                        self.state.set(CoState::Done);
                        self.body.borrow_mut().take();
                        return Yield::Done(Ok(Value::Undefined));
                    }
                    Resume::Next(_) => {}
                }
                if it.live_coroutines >= 2000 {
                    self.state.set(CoState::Done);
                    return Yield::Done(Err(it.make_error("RangeError", "Too many live generators/async functions")));
                }
                let Some(SendBox((body, co_tx, co_rx))) = self.body.borrow_mut().take() else {
                    return Yield::Done(Ok(Value::Undefined));
                };
                let is_async = self.is_async;
                let payload = SendBox((body, co_tx, co_rx));
                let handle = std::thread::Builder::new().stack_size(CO_STACK).name("tip-js-coroutine".into()).spawn(move || {
                    let payload = payload;
                    let SendBox((body, tx, rx)) = payload;
                    let side = CoSide {
                        tx,
                        rx,
                        outer: Cell::new(None),
                        outer_depth: Cell::new(0),
                        outer_stack: Cell::new((0, 0)),
                        my_stack: Cell::new((super::interp::stack_pointer(), CO_STACK - 4 * 1024 * 1024)),
                        is_async,
                    };
                    let Ok(SendBox((ip, first))) = side.rx.recv() else { return };
                    // SAFETY: the resuming thread is blocked until we yield or finish.
                    let it = unsafe { &mut *ip };
                    side.outer.set(it.current_co);
                    side.outer_depth.set(it.call_depth);
                    side.outer_stack.set((it.stack_base, it.stack_limit));
                    it.current_co = Some(&side as *const CoSide);
                    it.call_depth = 1;
                    (it.stack_base, it.stack_limit) = side.my_stack.get();
                    let result = match first {
                        Resume::Abort => Ok(Value::Undefined),
                        _ => body(it),
                    };
                    let result = it.normalize_coroutine_result(result);
                    it.current_co = side.outer.get();
                    it.call_depth = side.outer_depth.get();
                    (it.stack_base, it.stack_limit) = side.outer_stack.get();
                    let _ = side.tx.send(SendBox(Yield::Done(result)));
                });
                match handle {
                    Ok(h) => {
                        *self.thread.borrow_mut() = Some(h);
                        it.live_coroutines += 1;
                    }
                    Err(_) => {
                        self.state.set(CoState::Done);
                        return Yield::Done(Err(it.make_error("RangeError", "Unable to start coroutine")));
                    }
                }
            }
            CoState::Suspended => {}
        }
        self.state.set(CoState::Running);
        let ip: *mut Interp = it;
        if self.tx.send(SendBox((ip, msg))).is_err() {
            self.finish(it);
            return Yield::Done(Err(it.make_error("Error", "coroutine terminated")));
        }
        match self.rx.recv() {
            Ok(SendBox(y)) => {
                match &y {
                    Yield::Done(_) => self.finish(it),
                    _ => self.state.set(CoState::Suspended),
                }
                y
            }
            Err(_) => {
                self.finish(it);
                Yield::Done(Err(it.make_error("Error", "internal error in coroutine")))
            }
        }
    }

    fn finish(&self, it: &mut Interp) {
        self.state.set(CoState::Done);
        if let Some(h) = self.thread.borrow_mut().take() {
            let _ = h.join();
            it.live_coroutines = it.live_coroutines.saturating_sub(1);
        }
    }

    /// Abort a suspended coroutine, unwinding its frames (finally blocks run).
    pub fn abort(&self, it: &mut Interp) {
        if self.state.get() == CoState::Suspended {
            let _ = self.resume(it, Resume::Abort);
        } else if self.state.get() == CoState::NotStarted {
            self.state.set(CoState::Done);
            self.body.borrow_mut().take();
        }
    }
}

impl Interp {
    /// Suspend the current coroutine with `y`; returns the value it is resumed with.
    pub fn co_suspend(&mut self, y: Yield) -> Result<Value, Value> {
        let Some(sp) = self.current_co else {
            return Err(self.make_error("SyntaxError", "yield/await used outside of a generator or async function"));
        };
        // SAFETY: CoSide lives on the coroutine thread's stack for its whole run.
        let side = unsafe { &*sp };
        let depth = self.call_depth;
        self.current_co = side.outer.get();
        self.call_depth = side.outer_depth.get();
        (self.stack_base, self.stack_limit) = side.outer_stack.get();
        if side.tx.send(SendBox(y)).is_err() {
            return Err(self.abort_marker());
        }
        let Ok(SendBox((ip, msg))) = side.rx.recv() else {
            // resumer is gone: unwind without touching the interpreter further
            return Err(Value::Undefined);
        };
        // SAFETY: as above, the resumer is blocked.
        let it = unsafe { &mut *ip };
        side.outer.set(it.current_co);
        side.outer_depth.set(it.call_depth);
        side.outer_stack.set((it.stack_base, it.stack_limit));
        it.current_co = Some(sp);
        it.call_depth = depth;
        (it.stack_base, it.stack_limit) = side.my_stack.get();
        match msg {
            Resume::Next(v) => Ok(v),
            Resume::Throw(e) => Err(e),
            Resume::Return(v) => {
                it.pending_return = Some(v);
                Err(it.gen_return_marker())
            }
            Resume::Abort => Err(it.abort_marker()),
        }
    }

    pub fn in_async_coroutine(&self) -> bool {
        self.current_co.is_some_and(|p| unsafe { (*p).is_async })
    }
}
