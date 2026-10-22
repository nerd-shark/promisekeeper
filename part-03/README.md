# Part 3 — Project Skeleton: Cargo Workspace, First Type, First Command

The code from Part 3: *Project Skeleton: Cargo Workspace, First Type, First Command*.

This is where PromiseKeeper stops being a single demo file and becomes a real project. It's a cargo **workspace** with two crates:

- `promisekeeper/` — the domain library: `PromiseId(Uuid)`, `AgentId`, `PromiseState`, and the `Promise` struct from Part 2, assembled for real.
- `pk/` — the binary: a `pk submit` command built with [clap](https://docs.rs/clap/)'s derive API that parses `--agent` and `--task`, builds a `Promise`, and prints back a promise id.

The workspace is deliberately overkill for two crates today. PromiseKeeper is multi-crate by Part 12, so setting it up now means later chapters add a member instead of restructuring.

## Layout

```
part-03/
  Cargo.toml            # [workspace] with two members + shared dependencies
  promisekeeper/        # library crate: the domain types
    src/lib.rs
  pk/                   # binary crate: the CLI
    src/main.rs
```

## Run it

From inside `part-03/`:

```bash
cargo run -p pk -- submit --agent researcher --task "summarize the Q3 10-K filings"
```

## Expected output

The promise id is a fresh UUID, so yours will differ:

```
submitted: 2847e993-931c-4abf-92b9-e9032ee02fd7
  agent: researcher
  task:  summarize the Q3 10-K filings
  state: pending
```

clap gives you `--help` for free, generated from the doc comments on the command types:

```
$ cargo run -p pk -- --help
PromiseKeeper: track what your agents promised to do

Usage: pk <COMMAND>

Commands:
  submit  Submit a task to an agent and get back a promise id
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Submit a blank task and `pk` fails like a CLI: a message on stderr and a non-zero exit code, not a panic.

```
$ cargo run -p pk -- submit --agent researcher --task "   "
pk: task text must not be empty
$ echo $?
1
```

## Verification

Run from inside `part-03/`. All pass clean, no warnings:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo build
cargo test
```

Recorded results from the authoring run (Rust 1.95.0, clap 4, uuid 1):

- `cargo fmt --check` — exit 0, no diff
- `cargo clippy --all-targets -- -D warnings` — exit 0, `Finished dev profile`
- `cargo build` — exit 0, `Finished dev profile`
- `cargo test` — exit 0, 4 passed (2 in `promisekeeper`, 2 in `pk`), 0 failed

## Where this goes

In **Part 4**, the `Result` and the one-variant `SubmitError` you see in `pk/src/main.rs` grow into PromiseKeeper's real typed-failure story: a proper error enum, the `?` operator doing real work, and the difference between errors the caller can recover from and ones it can't.

In **Part 11**, this `pk submit` grows into the full orchestrator CLI (`watch`, `status`, `cancel`), which is why the `Command` enum and its exhaustive `match` are already set up to take more variants without a rewrite.
