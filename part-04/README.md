# Part 4 — Typed Failures: Error Handling in Rust

The code from Part 4: *Typed Failures: When Agents and Tools Let You Down*.

Agents fail in creative ways. The model times out, a tool returns malformed JSON, the API rate-limits mid-task, the task text is garbage. In a language with exceptions, each of those is an invisible second return path a caller can forget. In Rust, PromiseKeeper's failure modes are **values**: typed, visible in signatures, and impossible to ignore without the compiler noticing.

This chapter turns Part 3's one-variant `SubmitError` placeholder into the real thing, and makes one architectural call explicit:

- **`thiserror` in the library** (`promisekeeper/`): a structured `PromiseError` enum (`AgentTimeout`, `ToolFailed`, `InvalidTask`, `RateLimited`) that a caller can `match` on. Part 5's retry policy and Part 9's `Rejected` state both read these variants, so they carry data (which agent, which tool, how long to wait), not just a string.
- **`anyhow` at the CLI boundary** (`pk/`): the binary attaches human context (`.context("...")`) and prints one tidy report on stderr. It does not care which variant it got; `anyhow::Error` absorbs any error type so `?` just works.

A security rule runs through all of it: **errors describe the failure, never the payload.** No variant carries the task text, the prompt, or a tool argument. `ToolFailed` carries a *category* reason ("malformed json"), not raw output.

## Layout

```
part-04/
  Cargo.toml            # [workspace] + thiserror and anyhow added
  promisekeeper/        # library crate: promise types + the PromiseError enum
    src/lib.rs
  pk/                   # binary crate: the CLI, now with anyhow at the boundary
    src/main.rs
```

## Run it

From inside `part-04/`. The happy path is unchanged from Part 3:

```bash
cargo run -p pk -- submit --agent researcher --task "summarize the Q3 10-K filings"
```

```
submitted: 10596475-a7ca-4f07-a24f-80e593f84429
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
```

New in Part 4: `--simulate` drives one agent turn so the typed-error path is visible end to end. `done` resolves the promise:

```bash
cargo run -p pk -- submit --agent researcher --task "..." --simulate done
```

```
submitted: 3d7c7fa6-9a82-4075-8b9b-cf63d695e6b8
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
  state: resolved: summary ready
  result: summary ready
```

Each failure mode surfaces as a typed `PromiseError`, reported through `anyhow`'s context chain, and exits non-zero. The message names the agent and the category; it never prints the task text:

```bash
$ cargo run -p pk -- submit --agent slowpoke --task "..." --simulate timeout
pk: tracking promise 27d428c5-...: agent 'slowpoke' timed out after 30s
$ echo $?
1

$ cargo run -p pk -- submit --agent researcher --task "..." --simulate tool-error
pk: tracking promise 36f3e90a-...: tool 'web_fetch' failed: malformed json

$ cargo run -p pk -- submit --agent researcher --task "..." --simulate rate-limited
pk: tracking promise f7466d1c-...: rate limited by upstream; retry after 12s
```

A blank task is still rejected up front, now with a typed `InvalidTask::Empty` wrapped in CLI context:

```bash
$ cargo run -p pk -- submit --agent researcher --task "   "
pk: submitting a task to agent 'researcher': invalid task: task text must not be empty
$ echo $?
1
```

## Verification

Run from inside `part-04/`. All pass clean, no warnings:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

Recorded results from the authoring run (Rust 1.95.0, clap 4.6.7, uuid 1.28.0, thiserror 2, anyhow 1):

- `cargo fmt --check` — exit 0, no diff (one fmt diff caught + fixed during authoring)
- `cargo clippy --all-targets -- -D warnings` — exit 0, `Finished dev profile`
- `cargo build` — exit 0, `Finished dev profile`
- `cargo test` — exit 0, 11 passed (8 in `promisekeeper`, 3 in `pk`), 0 failed

## Where this goes

In **Part 5**, `PromiseError::is_retryable()` becomes the retry *condition*: the backoff policy retries `AgentTimeout`, `RateLimited`, and `ToolFailed`, and refuses to retry `InvalidTask` because the same input fails the same way. `RateLimited { retry_after_secs }` feeds the backoff delay directly.

In **Part 9**, the lifecycle enum grows a `Rejected` state that carries a `PromiseError`. That is the reason the error type is structured data rather than a string: a rejected promise can report exactly which failure killed it, and the API in Part 12 can serialize it.
