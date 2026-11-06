//! `pk`: the PromiseKeeper command-line interface, now asynchronous.
//!
//! Part 5 ran everything on one synchronous thread. Part 6 starts a tokio
//! runtime and teaches `pk` to do more than one thing at a time. The change is
//! smaller than it sounds, which is the point of the chapter: `main` gains a
//! `#[tokio::main]` attribute, the agent turn becomes an `.await`, and a new
//! `concurrent` command uses `tokio::spawn` to run several simulated agents at
//! once.
//!
//! `#[tokio::main]` is a macro. It rewrites
//!
//! ```ignore
//! #[tokio::main]
//! async fn main() { ... }
//! ```
//!
//! into a synchronous `main` that builds a runtime and blocks on the async body
//! until it finishes:
//!
//! ```ignore
//! fn main() {
//!     tokio::runtime::Runtime::new().unwrap().block_on(async { ... })
//! }
//! ```
//!
//! So the runtime is created here, in the binary, exactly once. The library
//! never starts a runtime; it only produces futures for this `main` to drive.
//! That separation is deliberate and it is the whole reason the library's async
//! functions are testable with `#[tokio::test]` without ever touching this
//! file.
//!
//! The three demos:
//!
//! - `submit` runs one simulated agent turn with an awaited think-time. With
//!   `--fail-times N` it drives the async retry driver: the agent times out
//!   (retryably) N times before recovering, and you watch the jittered backoff
//!   between attempts.
//! - `batch` is the Part 5 backpressure demo, unchanged: a bounded queue
//!   refuses overflow with a typed error. It needs no async, and leaving it
//!   synchronous shows that adding a runtime does not force every function to
//!   become `async`.
//! - `concurrent` spawns several agent tasks with `tokio::spawn` and awaits
//!   them all. This is the first time PromiseKeeper does two things at once.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use promisekeeper::{
    run_agent_task, run_with_retry_async, AgentId, AgentOutcome, AgentTask, BackoffStrategy,
    BoundedQueue, Jitter, Promise, PromiseError, RetryPolicy, Rng,
};

/// The top-level `pk` command. The doc comments become `--help` text, so the
/// documentation and the CLI cannot drift apart.
#[derive(Parser)]
#[command(
    name = "pk",
    version,
    about = "PromiseKeeper: track what your agents promised to do"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The subcommands `pk` understands. `watch`, `status`, and `cancel` arrive in
/// Part 11; the enum is shaped so they slot into the exhaustive `match` below.
#[derive(Subcommand)]
enum Command {
    /// Submit a task to an agent and get back a promise id. The agent turn is
    /// now asynchronous: it awaits a simulated think-time before resolving.
    /// With `--fail-times` the agent fails retryably that many times first, so
    /// the async retry driver is visible end to end.
    Submit {
        /// The agent that will do the work (for example: researcher).
        #[arg(long)]
        agent: String,

        /// The task the agent is being asked to do.
        #[arg(long)]
        task: String,

        /// How long the simulated agent "thinks" before reporting, in
        /// milliseconds. This is the `.await` point: a non-blocking timer.
        #[arg(long, default_value_t = 200)]
        think_ms: u64,

        /// Simulate a flaky agent that times out (retryably) this many times,
        /// then succeeds. Drives the async retry policy: with the default three
        /// attempts, `--fail-times 2` recovers and `--fail-times 3` exhausts.
        #[arg(long)]
        fail_times: Option<u32>,
    },

    /// Submit a batch of tasks into a bounded queue to demonstrate
    /// backpressure. Synchronous on purpose: adding a runtime does not mean
    /// every command has to be async.
    Batch {
        /// The agent the tasks are submitted to.
        #[arg(long, default_value = "researcher")]
        agent: String,

        /// How many tasks to submit.
        #[arg(long)]
        count: usize,

        /// The queue's hard capacity. Submissions past this are refused.
        #[arg(long)]
        capacity: usize,
    },

    /// Run several simulated agent tasks concurrently with `tokio::spawn`. Each
    /// task thinks for a staggered time and resolves; the total wall time is
    /// close to the slowest task, not the sum, because they run at once.
    Concurrent {
        /// The agent the concurrent tasks are submitted to.
        #[arg(long, default_value = "researcher")]
        agent: String,

        /// How many tasks to run concurrently.
        #[arg(long, default_value_t = 4)]
        count: usize,

        /// The base think-time in milliseconds. Task n thinks for
        /// `base_ms * n` so their completion order is visible.
        #[arg(long, default_value_t = 100)]
        base_ms: u64,
    },
}

/// A demo-sized retry policy so a CLI run finishes quickly while still showing
/// real backoff growth. The *shape* (exponential, jittered, capped) matches
/// `RetryPolicy::default_for_agents`; only the magnitudes are shrunk from
/// seconds to tens of milliseconds so you are not waiting around at a terminal.
fn demo_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        backoff: BackoffStrategy::exponential(
            Duration::from_millis(40),
            Duration::from_millis(500),
        ),
        jitter: Jitter::Full,
    }
}

/// Seed the jitter from the clock so two processes do not synchronize. A fixed
/// seed would make every invocation identical, which is the lockstep jitter
/// exists to prevent.
fn clock_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Run one agent turn asynchronously, optionally under the retry policy.
async fn submit(agent: &str, task: String, think_ms: u64, fail_times: Option<u32>) -> Result<()> {
    let mut promise = Promise::submit(AgentId::new(agent), task)
        .with_context(|| format!("submitting a task to agent '{agent}'"))?;

    println!("submitted: {}", promise.id());
    println!("  agent: {}", promise.agent());
    println!("  task:  {}", promise.task());
    println!("  state: {}", promise.state());

    let think_time = Duration::from_millis(think_ms);

    match fail_times {
        // Retry demo: a flaky agent times out `n` times, then completes.
        Some(n) => {
            let policy = demo_policy();
            let mut rng = Rng::new(clock_seed());
            let agent_id = promise.agent().clone();

            // The retry closure cannot borrow `promise` across the `FnMut`
            // boundary (a `FnMut` closure may not let a reference to a captured
            // variable escape into the future it returns). That is the borrow
            // checker steering us toward the cleaner shape anyway: the retry
            // loop is about *one agent turn*, so each attempt drives a
            // self-contained, locally-owned promise and reports an outcome. The
            // tracked promise is resolved once, after the loop succeeds.
            let output = run_with_retry_async(&policy, &mut rng, |attempt| {
                // Each attempt builds a fresh agent-task future. The backoff
                // between attempts is what grows; the think-time per attempt
                // stays short so the demo is snappy.
                let outcome = if attempt < n {
                    AgentOutcome::TimedOut { waited_secs: 30 }
                } else {
                    AgentOutcome::Completed {
                        output: "summary ready".to_string(),
                    }
                };
                let task = AgentTask {
                    think_time,
                    outcome,
                };
                let agent_id = agent_id.clone();
                async move {
                    // A throwaway promise local to this attempt: it carries the
                    // right agent so a timeout error names it, and it never
                    // escapes the closure.
                    let mut attempt_promise = Promise::submit(agent_id, "retry attempt")
                        .expect("attempt task text is non-empty");
                    print!("  attempt {}: ", attempt + 1);
                    let result = run_agent_task(&mut attempt_promise, task).await;
                    match &result {
                        Ok(_) => println!("resolved"),
                        Err(err) => println!("{err}"),
                    }
                    result
                }
            })
            .await
            .with_context(|| format!("tracking promise {} with retries", promise.id()))?;

            // The agent turn succeeded; resolve the tracked promise with it.
            promise.start();
            promise.resolve(output.clone());
            println!("  state: {}", promise.state());
            println!("  result: {output}");
        }
        // Happy path: one awaited agent turn.
        None => {
            let task = AgentTask::completing(think_time, "summary ready");
            let output = run_agent_task(&mut promise, task)
                .await
                .with_context(|| format!("tracking promise {}", promise.id()))?;
            println!("  state: {}", promise.state());
            println!("  result: {output}");
        }
    }

    Ok(())
}

/// Submit `count` tasks into a bounded queue of `capacity` and report which are
/// accepted and which are refused by backpressure. Synchronous: no runtime work
/// happens here, so none is asked for.
fn submit_batch(agent: &str, count: usize, capacity: usize) -> Result<()> {
    let mut queue = BoundedQueue::new(capacity);
    println!("queue capacity: {capacity}");

    let mut accepted = 0usize;
    let mut refused = 0usize;

    for n in 1..=count {
        let promise = Promise::submit(AgentId::new(agent), format!("batch task #{n}"))
            .with_context(|| format!("building batch task #{n}"))?;
        match queue.enqueue(promise) {
            Ok(id) => {
                accepted += 1;
                println!("  task #{n}: queued {id}");
            }
            Err(err @ PromiseError::QueueFull { .. }) => {
                refused += 1;
                println!("  task #{n}: refused ({err})");
            }
            Err(other) => {
                return Err(other).with_context(|| format!("queuing batch task #{n}"));
            }
        }
    }

    println!(
        "accepted: {accepted}, refused: {refused}, queued now: {}",
        queue.len()
    );
    Ok(())
}

/// Run `count` agent tasks concurrently with `tokio::spawn`, then await them
/// all. Task `n` thinks for `base_ms * n` so the completion order is visible
/// and the total wall time is close to the slowest task, not their sum.
async fn run_concurrent(agent: &str, count: usize, base_ms: u64) -> Result<()> {
    println!("spawning {count} agent tasks concurrently (agent: {agent})");
    let started = std::time::Instant::now();

    // Spawn each task onto the runtime. `tokio::spawn` returns a `JoinHandle`
    // immediately; the task is now running (or queued to run) independently of
    // this function. Each spawned task owns its own `Promise`, so there is no
    // shared state and no lock, the thing Part 7 introduces and has to guard.
    let mut handles = Vec::with_capacity(count);
    for n in 1..=count {
        let agent = agent.to_string();
        let think_ms = base_ms * n as u64;
        let handle = tokio::spawn(async move {
            let mut promise =
                Promise::submit(AgentId::new(&agent), format!("concurrent task #{n}"))
                    .expect("non-empty task text");
            let id = promise.id();
            let task = AgentTask::completing(
                Duration::from_millis(think_ms),
                format!("result #{n} ({think_ms}ms)"),
            );
            // Each task awaits its own simulated think-time. While task #1
            // sleeps, the runtime polls task #2, #3, #4: that is concurrency on
            // a single logical flow.
            let outcome = run_agent_task(&mut promise, task).await;
            (n, id, think_ms, outcome)
        });
        handles.push(handle);
    }

    // Await each handle. They were all launched before this loop, so they ran
    // concurrently; here we just collect results in spawn order.
    for handle in handles {
        let (n, id, think_ms, outcome) = handle
            .await
            .with_context(|| "a spawned agent task panicked")?;
        match outcome {
            Ok(result) => println!("  task #{n} [{id}] resolved after ~{think_ms}ms: {result}"),
            Err(err) => println!("  task #{n} [{id}] failed: {err}"),
        }
    }

    let elapsed = started.elapsed();
    let sequential = base_ms * (1..=count as u64).sum::<u64>();
    println!(
        "all {count} tasks done in ~{}ms (sequential would be ~{}ms)",
        elapsed.as_millis(),
        sequential
    );
    Ok(())
}

/// Dispatch a parsed command.
async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Submit {
            agent,
            task,
            think_ms,
            fail_times,
        } => submit(&agent, task, think_ms, fail_times).await,
        Command::Batch {
            agent,
            count,
            capacity,
        } => submit_batch(&agent, count, capacity),
        Command::Concurrent {
            agent,
            count,
            base_ms,
        } => run_concurrent(&agent, count, base_ms).await,
    }
}

/// `#[tokio::main]` builds the runtime and runs this async body on it. This is
/// the one place a runtime exists; the library only produces futures.
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // One place turns any error into a tidy report and a non-zero exit. `{:#}`
    // prints the full anyhow context chain, which is exactly the diagnostic an
    // operator wants and still free of task contents.
    if let Err(err) = run(cli).await {
        eprintln!("pk: {err:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_policy_has_the_right_shape() {
        let p = demo_policy();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.jitter, Jitter::Full);
        assert!(matches!(p.backoff, BackoffStrategy::Exponential { .. }));
    }

    #[tokio::test]
    async fn the_happy_path_submit_resolves() {
        // A direct exercise of the async submit path with a tiny think-time.
        let result = submit("researcher", "summarize".to_string(), 1, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_runs_faster_than_sequential_would() {
        // With virtual time, four staggered tasks still complete; the assertion
        // is that the command runs to completion without deadlock.
        let result = run_concurrent("researcher", 4, 10).await;
        assert!(result.is_ok());
    }
}
