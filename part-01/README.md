# Part 1 — See a Promise Move

The code from Part 1: *Build a Promise Engine for AI Agents in Rust (Why This, Why Rust)*.

This is a deliberately tiny taste, not PromiseKeeper proper. It's a promise moving through its lifecycle (pending → running → resolved), with the result carried right there in the type. The real project (a cargo workspace, the `pk` CLI, real domain types) starts in **Part 3**.

## Run it

```bash
cargo run
```

Expected output:

```
Pending
Running
Resolved("summary complete")
result: summary complete
```

## Where this goes

That little `PromiseState` enum is the seed of the whole engine. By **Part 9** it grows into a lifecycle state machine where a resolved promise *cannot* exist without a result, and a rejected one *cannot* exist without an error. The compiler enforces it, so the illegal states aren't caught in testing; they're impossible to write.
