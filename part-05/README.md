# Part 5 — Retry, Backoff, and Backpressure

The code from Part 5: *Retry, Backoff, and Backpressure: Keeping a Flaky Swarm Alive*.

An agent fails. Do you retry immediately? Fifty agents fail at once because the model API is rate-limiting. Do you retry all fifty immediately? That is not a retry policy, that is a self-inflicted outage. This chapter adds the three primitives that separate "worked in the demo" from "survived Tuesday," all built on the typed `PromiseError` from Part 4 and all synchronous (the tokio machinery lands in Parts 6 through 8).

## What Part 5 adds

- **Retry as a typed decision** (`RetryPolicy` + `run_with_retry`): the driver reuses `PromiseError::is_retryable()` from Part 4 to decide what to retry. A timeout, a rate-limit, or a transient tool failure is retried; an `InvalidTask` or a `QueueFull` is not, because retrying it is a bug. The driver takes its `sleep` as an injected closure, so the policy is tested in microseconds with no real clock.
- **Backoff as an enum** (`BackoffStrategy::Fixed` / `Exponential`): modeling it as an enum means the compiler forces every delay computation to handle both variants, the same property Part 9 leans on for the lifecycle state machine. Exponential backoff uses saturating arithmetic so a long-lived policy cannot overflow its way to a bad delay.
- **Jitter** (`Jitter::None` / `Full` / `Equal`): the non-obvious, high-value idea. Backoff alone just moves the thundering herd a few seconds later; jitter breaks the synchronization so fifty clients that failed together do not all retry on the same beat. Drawn from an injected, seedable `Rng` (a SplitMix64, not the `rand` crate) so tests pin the sequence and production seeds from the clock.
- **Backpressure** (`BoundedQueue`): a hard-capacity queue that refuses overflow with a typed `PromiseError::QueueFull` instead of buffering until memory runs out. Retry and backoff protect the dependency from us; backpressure protects us from our clients.

A rule from Part 4 carries through: **errors describe the failure, never the payload.** `QueueFull` carries the capacity, not the task; a test enforces it.

## Layout

```
part-05/
  Cargo.toml            # [workspace], no new dependencies vs Part 4
  promisekeeper/        # library: Part 4 types + RetryPolicy, BackoffStrategy,
    src/lib.rs          #   Jitter, Rng, run_with_retry, BoundedQueue
  pk/                   # binary: submit --fail-times (retry demo), batch (backpressure demo)
    src/main.rs
```

## Run it

From inside `part-05/`.

The Part 4 paths are unchanged. The happy path still resolves:

```bash
cargo run -p pk -- submit --agent researcher --task "summarize the Q3 10-K filings" --simulate done
```

```
submitted: b9cb74eb-e025-4a31-a6b7-696318a716ee
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
  state: resolved: summary ready
  result: summary ready
```

New in Part 5: `--fail-times N` drives a flaky agent that times out (a retryable error) N times before recovering. With the default three attempts, `--fail-times 2` recovers. Watch the backoff grow, jittered:

```bash
cargo run -p pk -- submit --agent researcher --task "..." --fail-times 2
```

```
submitted: 6d75ebef-659f-4e56-a6ce-09769efd4e4e
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
  attempt 1: agent 'researcher' timed out after 30s
  backing off 18ms
  attempt 2: agent 'researcher' timed out after 30s
  backing off 35ms
  attempt 3: resolved
  state: resolved: summary ready
  result: summary ready
```

`--fail-times 3` exhausts the policy and surfaces the last typed error through the `anyhow` context chain, exiting non-zero. The report names the agent, never the task:

```bash
$ cargo run -p pk -- submit --agent slowpoke --task "..." --fail-times 3
submitted: 55ba1861-...
  ...
  attempt 1: agent 'slowpoke' timed out after 30s
  backing off 22ms
  attempt 2: agent 'slowpoke' timed out after 30s
  backing off 21ms
  attempt 3: agent 'slowpoke' timed out after 30s
pk: tracking promise 55ba1861-... with retries: agent 'slowpoke' timed out after 30s
$ echo $?
1
```

(The two runs above backed off 18/35ms and 22/21ms. Different, because jitter desynchronizes them. That is the whole point.)

`batch --count C --capacity K` submits C tasks into a bounded queue of capacity K. Backpressure refuses the overflow with a typed error instead of growing without bound:

```bash
cargo run -p pk -- batch --count 6 --capacity 3
```

```
queue capacity: 3
  task #1: queued 890595a8-d08a-4bdd-acf8-c8d6ade99309
  task #2: queued 1d2a9ece-d9ce-4788-b3ff-036fbdb0fb60
  task #3: queued 5381ae80-984b-490d-a53c-6f60ea4a84a3
  task #4: refused (submission queue is full (capacity 3); try again later)
  task #5: refused (submission queue is full (capacity 3); try again later)
  task #6: refused (submission queue is full (capacity 3); try again later)
accepted: 3, refused: 3, queued now: 3
```

## Verification

Run from inside `part-05/`. All pass clean, no warnings:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

Recorded results from the authoring run (Rust 1.95.0, clap 4.6.7, uuid 1.29.0, thiserror 2, anyhow 1):

- `cargo fmt --check` — exit 0, no diff (fmt diffs caught + fixed during authoring)
- `cargo clippy --all-targets -- -D warnings` — exit 0 (one `let_and_return` caught + fixed), `Finished dev profile`
- `cargo build` — exit 0, `Finished dev profile`
- `cargo test` — exit 0, 24 passed (21 in `promisekeeper`, 3 in `pk`), 0 failed
- Real CLI runs captured: happy path (exit 0), `--fail-times 2` (recovers, exit 0), `--fail-times 3` (exhausts, typed error via anyhow chain, exit 1, no task leaked), `batch --count 6 --capacity 3` (3 accepted, 3 refused, exit 0). Output pasted into the article and above.

## Where this goes

Everything here is synchronous. In **Part 6** the engine learns `.await`, and in **Part 7** the retry loop and the bounded queue move into a concurrent engine where many promises run at once. The `sleep` closure becomes `tokio::time::sleep`, and the `BoundedQueue` becomes a bounded `tokio::sync::mpsc` channel, but the *policy* types in this chapter carry over unchanged. In **Part 8** timeouts join retries: retry decides whether to try again, a timeout decides when to give up on a single attempt.
