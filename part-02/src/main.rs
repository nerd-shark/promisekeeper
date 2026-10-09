//! Part 2: who owns an in-flight promise?
//!
//! This is teaching code, written from scratch for the series, not
//! PromiseKeeper proper. The point of this chapter is to settle one question
//! the whole engine depends on: when an agent task is in flight, who is
//! responsible for it right now, and who is merely looking at it?
//!
//! We model PromiseKeeper's real access pattern. The engine mutates a promise,
//! many watchers read it, and submitting a task hands the promise off into the
//! engine for good. Each of those maps to one Rust mechanism:
//!
//!   - `&Promise`      a watcher reading status      (shared borrow)
//!   - `&mut Promise`  the engine changing state     (exclusive borrow)
//!   - `Promise`       submit: handoff into the engine (move)
//!
//! Everything here is single-threaded on purpose. The same ownership rules
//! become data-race prevention once threads arrive (Part 7). The first real
//! `pk submit` and the cargo workspace show up in Part 3, where these types
//! get assembled into the engine for real. The full compiler-enforced
//! lifecycle state machine is Part 9, so this `PromiseState` stays small.

use std::fmt;

/// A typed identifier for a promise.
///
/// A newtype, not a bare `u64`, so you can't accidentally pass an agent id or
/// a retry count where a promise id belongs. The compiler keeps the two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PromiseId(u64);

impl fmt::Display for PromiseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "promise-{:04}", self.0)
    }
}

/// Where a promise is in its lifecycle.
///
/// Kept deliberately small for this chapter. `Resolved` carries its result in
/// the type, so a resolved promise can't exist without one. Part 9 grows this
/// into a state machine where every illegal state is uncompilable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PromiseState {
    Pending,
    Running,
    Resolved(String),
}

/// The first real `Promise`: an agent task the engine is tracking.
#[derive(Debug)]
struct Promise {
    id: PromiseId,
    agent: String,
    task: String,
    state: PromiseState,
}

impl Promise {
    fn new(id: PromiseId, agent: &str, task: &str) -> Self {
        Promise {
            id,
            agent: agent.to_string(),
            task: task.to_string(),
            state: PromiseState::Pending,
        }
    }
}

/// A watcher reading status. Takes `&Promise`, a shared borrow: it can look at
/// the promise but can't change it, and any number of watchers can hold one of
/// these at the same time. This is `pk watch` printing a line.
fn watch_status(promise: &Promise) {
    println!(
        "  [watcher] {} ({}) \"{}\": {:?}",
        promise.id, promise.agent, promise.task, promise.state
    );
}

/// The engine advancing a promise. Takes `&mut Promise`, an exclusive borrow:
/// while the engine holds this, nobody else can touch the promise, not even to
/// read it. Exactly one writer, enforced by the compiler. This is the engine
/// driving the lifecycle forward.
fn engine_advance(promise: &mut Promise, next: PromiseState) {
    println!(
        "  [engine ] {} advancing {:?} -> {:?}",
        promise.id, promise.state, next
    );
    promise.state = next;
}

/// The engine's store of promises it owns and tracks.
#[derive(Default)]
struct Engine {
    store: Vec<Promise>,
}

impl Engine {
    fn new() -> Self {
        Engine::default()
    }

    /// Submit a task. This takes `promise` *by value*, so ownership moves into
    /// the engine. After the caller submits, it no longer has the promise, and
    /// the compiler proves the caller can't touch it again. That's the handoff
    /// of responsibility: the orchestrator asks, the engine now owns the work.
    fn submit(&mut self, promise: Promise) -> PromiseId {
        let id = promise.id;
        println!("  [submit ] {} handed to the engine (ownership moved)", id);
        self.store.push(promise);
        id
    }

    /// Find a promise by id so the engine can advance it.
    fn get_mut(&mut self, id: PromiseId) -> Option<&mut Promise> {
        self.store.iter_mut().find(|p| p.id == id)
    }

    /// Borrow the store to build a read-only view of one promise's status.
    /// The returned `PromiseView` borrows from `self`, which is why it needs a
    /// lifetime (see below).
    fn view(&self, id: PromiseId) -> Option<PromiseView<'_>> {
        self.store.iter().find(|p| p.id == id).map(|p| PromiseView {
            id: p.id,
            state: &p.state,
        })
    }
}

/// A read-only view into one promise's status, borrowed from the engine's
/// store. This is the one lifetime annotation this chapter teaches.
///
/// `'a` ties the view to the engine it came from. The view holds a reference
/// (`&'a PromiseState`) into the store, so the compiler won't let the view
/// outlive the engine. If the engine were dropped, this reference would
/// dangle, and that's exactly the use-after-read the lifetime makes
/// impossible. You never pay for a copy of the state just to look at it, and
/// you can't look at state that's already gone.
struct PromiseView<'a> {
    id: PromiseId,
    state: &'a PromiseState,
}

impl PromiseView<'_> {
    fn describe(&self) -> String {
        format!("{} is currently {:?}", self.id, self.state)
    }
}

fn main() {
    println!("PromiseKeeper, Part 2: who owns an in-flight promise?\n");

    // The orchestrator builds a promise. Right now it owns it.
    let promise = Promise::new(PromiseId(7), "researcher", "summarize the Q3 10-K filings");
    println!("Orchestrator built a promise it owns:");
    watch_status(&promise); // a shared borrow: just looking

    // Submit: ownership MOVES into the engine. After this line the orchestrator
    // no longer has `promise`. Try using it below the move and the code won't
    // compile. That's the handoff, proven at compile time.
    println!("\nSubmitting (move/handoff):");
    let mut engine = Engine::new();
    let id = engine.submit(promise);
    // println!("{:?}", promise); // <- uncomment: "value borrowed after move"

    // The engine, the single writer, advances the promise through its states.
    // Each call takes `&mut Promise`: exclusive access, one writer at a time.
    println!("\nEngine advances the promise (exclusive &mut borrows):");
    if let Some(p) = engine.get_mut(id) {
        engine_advance(p, PromiseState::Running);
    }
    if let Some(p) = engine.get_mut(id) {
        engine_advance(p, PromiseState::Resolved("summary complete".to_string()));
    }

    // Many watchers can read at once (shared &borrows). Here two "sessions"
    // read the same promise back to back; neither can mutate it.
    println!("\nWatchers read status (shared &borrows):");
    if let Some(p) = engine.store.first() {
        watch_status(p);
        watch_status(p);
    }

    // A PromiseView borrows the engine's store for a read. The one lifetime in
    // this chapter: the view can't outlive the engine it borrowed from.
    println!("\nA borrowed read-only view (one lifetime in context):");
    if let Some(view) = engine.view(id) {
        println!("  [view   ] {}", view.describe());
    }

    println!("\nOne writer XOR many readers, and a handoff the compiler proved.");
}
