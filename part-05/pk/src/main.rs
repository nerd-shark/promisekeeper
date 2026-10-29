//! `pk`: the PromiseKeeper command-line interface.
//!
//! Part 4 made failure a typed value and reported it through `anyhow` at this
//! boundary. Part 5 puts those values to work. The CLI gains two things that
//! make the chapter's three primitives visible end to end:
//!
//! - `submit --fail-times N` drives an agent that fails with a *retryable*
//!   error N times before it recovers, so you can watch the [`RetryPolicy`]
//!   back off (with jitter) and eventually resolve the promise, or exhaust its
//!   attempts and surface the last typed error.
//! - `batch --count C --capacity K` submits C tasks into a [`BoundedQueue`] of
//!   capacity K, so you can watch backpressure refuse the overflow with a
//!   typed `QueueFull` instead of letting the queue grow without bound.
//!
//! The retry *driver* lives in the library. This binary only supplies the
//! operation (one simulated agent turn), the sleep (real, but with a demo-sized
//! policy so a run finishes in under a second), and the reporting. The library
//! owns the policy and the decision; the CLI owns the words a human reads.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use promisekeeper::{
    run_with_retry, AgentId, AgentOutcome, BackoffStrategy, BoundedQueue, Jitter, Promise,
    PromiseError, RetryPolicy, Rng,
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
    /// Submit a task to an agent and get back a promise id. With `--fail-times`
    /// the agent fails retryably that many times before recovering, so the
    /// retry policy is visible end to end.
    Submit {
        /// The agent that will do the work (for example: researcher).
        #[arg(long)]
        agent: String,

        /// The task the agent is being asked to do.
        #[arg(long)]
        task: String,

        /// Drive a single agent turn with a fixed outcome, as in Part 4.
        /// One of: done, timeout, tool-error, rate-limited. Ignored when
        /// `--fail-times` is given.
        #[arg(long)]
        simulate: Option<String>,

        /// Simulate a flaky agent that fails with a retryable timeout this many
        /// times, then succeeds. Drives the retry policy: with the default
        /// three attempts, `--fail-times 2` recovers and `--fail-times 3`
        /// exhausts.
        #[arg(long)]
        fail_times: Option<u32>,
    },

    /// Submit a batch of tasks into a bounded queue to demonstrate
    /// backpressure. Tasks past the capacity are refused with a typed error
    /// instead of buffered without bound.
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
}

/// Turn the `--simulate` flag into an [`AgentOutcome`], as in Part 4.
fn outcome_from_flag(flag: &str) -> Result<AgentOutcome> {
    let outcome = match flag {
        "done" => AgentOutcome::Completed {
            output: "summary ready".to_string(),
        },
        "timeout" => AgentOutcome::TimedOut { waited_secs: 30 },
        "tool-error" => AgentOutcome::ToolError {
            tool: "web_fetch".to_string(),
            reason: "malformed json".to_string(),
        },
        "rate-limited" => AgentOutcome::RateLimited {
            retry_after_secs: 12,
        },
        other => anyhow::bail!(
            "unknown --simulate value '{other}' (expected: done, timeout, tool-error, rate-limited)"
        ),
    };
    Ok(outcome)
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

/// Run one agent turn under the retry policy, printing each attempt so the
/// backoff is visible. Returns the resolved output or the last typed error.
fn submit_with_retry(agent: &str, task: String, fail_times: u32) -> Result<()> {
    let mut promise = Promise::submit(AgentId::new(agent), task)
        .with_context(|| format!("submitting a task to agent '{agent}'"))?;

    println!("submitted: {}", promise.id());
    println!("  agent: {}", promise.agent());
    println!("  task:  {}", promise.task());
    println!("  state: {}", promise.state());

    let policy = demo_policy();
    // Seed the jitter from the clock in a real run so two processes do not
    // synchronize. A fixed seed here would make every invocation identical,
    // which is exactly the lockstep jitter is meant to prevent.
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut rng = Rng::new(seed);

    // The operation: a flaky agent. It times out (a retryable error) for the
    // first `fail_times` attempts, then completes. `attempt` is 0-based.
    let output = run_with_retry(
        &policy,
        &mut rng,
        |attempt| {
            promise.start();
            let outcome = if attempt < fail_times {
                AgentOutcome::TimedOut { waited_secs: 30 }
            } else {
                AgentOutcome::Completed {
                    output: "summary ready".to_string(),
                }
            };
            print!("  attempt {}: ", attempt + 1);
            match promisekeeper::track(&mut promise, outcome) {
                Ok(out) => {
                    println!("resolved");
                    Ok(out)
                }
                Err(err) => {
                    println!("{err}");
                    Err(err)
                }
            }
        },
        |delay| {
            println!("  backing off {}ms", delay.as_millis());
            std::thread::sleep(delay);
        },
    )
    .with_context(|| format!("tracking promise {} with retries", promise.id()))?;

    println!("  state: {}", promise.state());
    println!("  result: {output}");
    Ok(())
}

/// Submit `count` tasks into a bounded queue of `capacity` and report which are
/// accepted and which are refused by backpressure.
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
                // Backpressure fired. This is the expected, healthy outcome
                // under overload, not a crash. We report it and keep going.
                println!("  task #{n}: refused ({err})");
            }
            Err(other) => {
                // Any other error is a genuine problem; surface it.
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

/// Dispatch a parsed command.
fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Submit {
            agent,
            task,
            simulate,
            fail_times,
        } => {
            // `--fail-times` takes precedence: it is the retry demo. Without it,
            // behave exactly like Part 4 (one turn, or stop at pending).
            if let Some(n) = fail_times {
                return submit_with_retry(&agent, task, n);
            }

            let mut promise = Promise::submit(AgentId::new(&agent), task)
                .with_context(|| format!("submitting a task to agent '{agent}'"))?;

            println!("submitted: {}", promise.id());
            println!("  agent: {}", promise.agent());
            println!("  task:  {}", promise.task());
            println!("  state: {}", promise.state());

            if let Some(flag) = simulate {
                let outcome = outcome_from_flag(&flag)?;
                promise.start();
                let output = promisekeeper::track(&mut promise, outcome)
                    .with_context(|| format!("tracking promise {}", promise.id()))?;
                println!("  state: {}", promise.state());
                println!("  result: {output}");
            }

            Ok(())
        }
        Command::Batch {
            agent,
            count,
            capacity,
        } => submit_batch(&agent, count, capacity),
    }
}

fn main() {
    let cli = Cli::parse();

    // One place turns any error into a tidy report and a non-zero exit. `{:#}`
    // prints the full anyhow context chain on a single logical report, which is
    // exactly the diagnostic an operator wants and still free of task contents.
    if let Err(err) = run(cli) {
        eprintln!("pk: {err:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_maps_to_a_completed_outcome() {
        let outcome = outcome_from_flag("done").unwrap();
        assert!(matches!(outcome, AgentOutcome::Completed { .. }));
    }

    #[test]
    fn an_unknown_simulate_value_is_an_error_not_a_panic() {
        let result = outcome_from_flag("explode");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unknown --simulate value"));
    }

    #[test]
    fn the_demo_policy_has_the_right_shape() {
        let p = demo_policy();
        assert_eq!(p.max_attempts, 3);
        assert_eq!(p.jitter, Jitter::Full);
        assert!(matches!(p.backoff, BackoffStrategy::Exponential { .. }));
    }
}
