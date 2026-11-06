# Part 6 — Async Foundations

The code from Part 6: *Async Foundations: tokio and `.await` Without the Hand-Waving*.

Before PromiseKeeper can track fifty agents at once, it has to learn to wait without blocking. That is what `async`/`.await` and a runtime like tokio buy you. This chapter is the mental model, built as runnable code: what a future is (a value representing a computation that is not done yet), why Rust futures are lazy (they do nothing until polled), what the runtime's job is (polling futures to completion), and the first time PromiseKeeper does two things at once with `tokio::spawn`. Everything builds on the typed errors (Part 4) and the retry policy (Part 5); the policy types carry over unchanged, only the *waiting* becomes asynchronous.

## What Part 6 adds

- **The tokio runtime, wired into `pk`.** `main` gains `#[tokio::main]`, which builds a multi-threaded runtime and runs the async body on it. The runtime is created once, in the binary. The library never starts a runtime; it only produces futures for `main` to drive. Rust ships the async *language feature*, not a runtime, so you choose one: tokio.
- **The first `async fn`** (`run_agent_task`): it marks a promise `Running`, `.await`s a `tokio::time::sleep` to simulate an agent's think-time (the yield point, non-blocking), then hands the canned outcome to `track`. The agent is *simulated* (a delay plus a canned result) so there are no API keys and the focus stays on async mechanics. A unit test proves the chapter's one big idea directly: a future does nothing until it is `.await`ed.
- **An async retry driver** (`run_with_retry_async`): the Part 5 retry driver with one change. It `.await`s an async operation and `tokio::time::sleep`s between attempts instead of calling an injected blocking closure. Every policy decision is preserved: refuse a non-retryable error immediately, never sleep after the final attempt, honor a server-directed retry-after. The synchronous `run_with_retry` from Part 5 is kept too, because it is still the right tool for clock-free policy tests.
- **`tokio::spawn` running several tasks at once** (the `concurrent` command): the first time PromiseKeeper does two things concurrently. Each spawned task owns its own `Promise`, so there is no shared state and no lock. That is deliberate: holding a lock across an `.await` is the classic trap, and it is Part 7's problem precisely because this chapter has no shared lock to hold.

The rules from earlier chapters carry through: **errors describe the failure, never the payload** (a timeout names the agent, never the task), and failure is a typed value the compiler will not let you ignore.

## Layout

```
part-06/
  Cargo.toml            # [workspace], adds exactly one dependency: tokio
  promisekeeper/        # library: Part 4/5 types + the first async fn and the
    src/lib.rs          #   async retry driver (run_agent_task, run_with_retry_async)
  pk/                   # binary: #[tokio::main]; submit (async, --fail-times),
    src/main.rs         #   batch (sync backpressure), concurrent (tokio::spawn)
```

## Run it

From inside `part-06/`.

The happy path now awaits a simulated think-time before resolving. `--think-ms` sets it:

```bash
cargo run -p pk -- submit --agent researcher --task "summarize the Q3 10-K filings" --think-ms 200
```

```
submitted: a9ee0f0b-fba7-4635-864d-55bf37725f23
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
  state: resolved: summary ready
  result: summary ready
```

`--fail-times N` drives the async retry driver: a flaky agent times out (a retryable error) N times before recovering, with the backoff awaited between attempts. With the default three attempts, `--fail-times 2` recovers:

```bash
cargo run -p pk -- submit --agent researcher --task "..." --think-ms 50 --fail-times 2
```

```
submitted: 3de04f2f-1409-40a3-9e65-88e13c8a7aec
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
  attempt 1: agent 'researcher' timed out after 30s
  attempt 2: agent 'researcher' timed out after 30s
  attempt 3: resolved
  state: resolved: summary ready
  result: summary ready
```

`--fail-times 3` exhausts the policy and surfaces the last typed error through the `anyhow` context chain, exiting non-zero. The report names the agent, never the task:

```bash
$ cargo run -p pk -- submit --agent slowpoke --task "..." --think-ms 50 --fail-times 3
  ...
  attempt 1: agent 'slowpoke' timed out after 30s
  attempt 2: agent 'slowpoke' timed out after 30s
  attempt 3: agent 'slowpoke' timed out after 30s
pk: tracking promise 35261d44-... with retries: agent 'slowpoke' timed out after 30s
$ echo $?
1
```

The chapter's centerpiece is `concurrent`: spawn several agent tasks with `tokio::spawn` and run them at once. Task `n` thinks for `base_ms * n`, so the total wall time is close to the slowest task, not the sum:

```bash
cargo run -p pk -- concurrent --agent researcher --count 4 --base-ms 100
```

```
spawning 4 agent tasks concurrently (agent: researcher)
  task #1 [29b2f343-...] resolved after ~100ms: result #1 (100ms)
  task #2 [a18f6c29-...] resolved after ~200ms: result #2 (200ms)
  task #3 [f55509fd-...] resolved after ~300ms: result #3 (300ms)
  task #4 [dd89cd77-...] resolved after ~400ms: result #4 (400ms)
all 4 tasks done in ~405ms (sequential would be ~1000ms)
```

Four tasks, ~405ms instead of ~1000ms, on one runtime. That gap is the whole point of the chapter.

`batch --count C --capacity K` is the Part 5 backpressure demo, unchanged and deliberately still synchronous: adding a runtime does not force every function to become `async`.

```bash
cargo run -p pk -- batch --count 6 --capacity 3
```

```
queue capacity: 3
  task #1: queued 04e22a64-...
  task #2: queued 9691f579-...
  task #3: queued e1497aa2-...
  task #4: refused (submission queue is full (capacity 3); try again later)
  task #5: refused (submission queue is full (capacity 3); try again later)
  task #6: refused (submission queue is full (capacity 3); try again later)
accepted: 3, refused: 3, queued now: 3
```

## Verification

Run from inside `part-06/`. All pass clean, no warnings:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

Recorded results from the authoring run (Rust 1.95.0, tokio 1.54.1, clap 4.6.7, uuid 1.29.0, thiserror 2, anyhow 1):

- `cargo fmt --check` — exit 0, no diff (two fmt diffs caught + fixed during authoring)
- `cargo clippy --all-targets -- -D warnings` — exit 0 (a `needless_doctest_main` on the `#[tokio::main]` desugaring comment caught + fixed by fencing it as `ignore`), `Finished dev profile`
- `cargo build` — exit 0, `Finished dev profile`
- `cargo test` — exit 0, 14 passed (11 in `promisekeeper`, 3 in `pk`), 0 failed. The async tests use `#[tokio::test(start_paused = true)]` so the awaited timers complete in virtual time (a 1s think-time asserted in zero wall time).
- Real CLI runs captured: happy path (exit 0), `--fail-times 2` (recovers on attempt 3, exit 0), `--fail-times 3` (exhausts, typed error via anyhow chain, exit 1, no task leaked), `concurrent --count 4 --base-ms 100` (~405ms vs ~1000ms sequential, exit 0), `batch --count 6 --capacity 3` (3 accepted, 3 refused, exit 0). Output pasted into the article and above.

## Where this goes

The async foundations here are the groundwork for the next two chapters. In **Part 7** the `concurrent` demo grows into the real engine: one spawned task per in-flight promise, a *shared* promise store behind `Arc<Mutex<...>>`, and the `Send`/`Sync` story that makes sharing across tasks a compile-time guarantee instead of a prayer. That is also where "holding a lock across an `.await`" stops being a trap we point at and becomes a trap we have to design around. In **Part 8** `tokio::select!` races a task against a timeout, and a reaper cancels and cleans up the ones that hang. The `run_agent_task` future and the async retry driver from this chapter are what those chapters build on.
