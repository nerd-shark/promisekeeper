# PromiseKeeper

A promise engine for AI agents, built in Rust, one chapter at a time.

This is the companion code for **Rust for Systems Architects**, an 18-part build series. An orchestrator decides *what* agents should do; PromiseKeeper tracks *whether they did it*, holding every agent task as a promise with a full lifecycle (pending → running → resolved / rejected / timed-out), plus retries, timeouts, resource accounting, and sandboxed tool execution.

By the end of the series, `pk` does this:

```bash
pk submit --agent researcher --task "summarize the Q3 10-K filings"
pk watch                      # live view of in-flight promises
pk status <promise-id>        # one promise's lifecycle and cost
pk cancel <promise-id>
```

## How this repo is organized

Each chapter has its own self-contained, runnable folder. You can open any `part-NN/` directory and run it on its own, or diff two chapters side by side to see exactly what changed.

```
part-01/   The decision + a runnable "promise lifecycle" taste
part-02/   Ownership: who owns an in-flight promise
part-03/   Cargo workspace + the first real `pk submit`
...
part-18/   Capstone: cost, compute, and carbon per task vs. Python
```

Each chapter's commit is also tagged (`part-01`, `part-02`, …), so if you prefer `git checkout part-07` over opening a folder, that works too.

## Running the code

Each part is an ordinary Cargo project. Install the Rust toolchain with [rustup](https://rustup.rs/), then:

```bash
cd part-01
cargo run
```

## The series

Part 1: *Build a Promise Engine for AI Agents in Rust (Why This, Why Rust)*
(Links to each article are added as they publish.)

## Code standards

Every part follows the same conventions, enforced by config at the repo root:

- **Formatting**: `cargo fmt` (see `rustfmt.toml`, stable-only options, 100-column width).
- **Linting**: `cargo clippy -- -D warnings` must pass (see `clippy.toml`).
- **Editors**: `.editorconfig` keeps whitespace and line endings consistent.

Run both from inside any part directory:

```bash
cargo fmt --check
cargo clippy -- -D warnings
```

## License

Licensed under the [MIT License](LICENSE).

## A note on provenance

PromiseKeeper is original, public teaching code written from scratch for this series. It is not a reproduction of any proprietary or internal system.
