# Part 2 — Who Owns an In-Flight Promise?

The code from Part 2: *Who Owns an In-Flight Promise? Ownership and Borrowing in Practice*.

This is the first real PromiseKeeper type, written as public teaching code. It models the engine's actual access pattern and lets the compiler decide who's allowed to touch a promise, and when:

- `&Promise` — a watcher reading status (shared borrow; many at once)
- `&mut Promise` — the engine advancing state (exclusive borrow; one writer)
- `Promise` by value — `submit`, the handoff of responsibility into the engine (move)

It also introduces exactly one lifetime annotation in context: a `PromiseView` that borrows the engine's store for a read, so the view can't outlive the engine it came from.

Everything is single-threaded on purpose. The same rules become data-race prevention once threads arrive (Part 7). The cargo workspace and the first real `pk submit` land in Part 3.

## Run it

```bash
cargo run
```

## Expected output

```
PromiseKeeper, Part 2: who owns an in-flight promise?

Orchestrator built a promise it owns:
  [watcher] promise-0007 (researcher) "summarize the Q3 10-K filings": Pending

Submitting (move/handoff):
  [submit ] promise-0007 handed to the engine (ownership moved)

Engine advances the promise (exclusive &mut borrows):
  [engine ] promise-0007 advancing Pending -> Running
  [engine ] promise-0007 advancing Running -> Resolved("summary complete")

Watchers read status (shared &borrows):
  [watcher] promise-0007 (researcher) "summarize the Q3 10-K filings": Resolved("summary complete")
  [watcher] promise-0007 (researcher) "summarize the Q3 10-K filings": Resolved("summary complete")

A borrowed read-only view (one lifetime in context):
  [view   ] promise-0007 is currently Resolved("summary complete")

One writer XOR many readers, and a handoff the compiler proved.
```

There are two commented-out lines in `src/main.rs` worth uncommenting if you want to watch the compiler do its job. Using `promise` after it's been submitted gives you "value borrowed after move." That error is the whole point of the chapter: the handoff is proven at compile time, not left to code review.

## Verification

Run from inside `part-02/`. All four pass clean, no warnings:

```
cargo fmt --check        # (no output; formatting is clean)
cargo clippy -- -D warnings
cargo build
cargo run
```

Recorded results from the authoring run (Rust 1.95.0):

- `cargo fmt --check` — exit 0, no diff
- `cargo clippy -- -D warnings` — exit 0, `Finished dev profile`
- `cargo build` — exit 0, `Finished dev profile`
- `cargo run` — exit 0, output matches the block above verbatim

## Where this goes

In **Part 3**, these exact types (`Promise`, `PromiseId`, `PromiseState`) move into a cargo workspace and get wired into the first real `pk submit`. In **Part 7**, the single-writer rule you see here stops being a style choice and starts preventing data races across threads. In **Part 9**, `PromiseState` grows into a compiler-enforced state machine where a resolved promise can't exist without a result.
