//! `pk`: the PromiseKeeper command-line interface.
//!
//! Part 3 shipped `pk submit` with a one-variant `SubmitError` defined right
//! here in the binary. Part 4 deletes that placeholder. The domain library now
//! owns validation and failure as a typed [`promisekeeper::PromiseError`], and
//! this binary's job shifts to what a binary is actually for: turning those
//! typed errors into a friendly message a person reads on stderr, with enough
//! context to tell *where* it happened, and a non-zero exit code.
//!
//! The split is the chapter's whole point:
//!
//! - The **library** uses `thiserror`: structured variants a caller can match
//!   on (the retry policy in Part 5, the `Rejected` state in Part 9).
//! - The **binary** uses `anyhow`: it does not care which variant it got, only
//!   that it should attach context ("while submitting the task") and print a
//!   tidy report. `anyhow::Error` swallows any error type, so `?` just works.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use promisekeeper::{AgentId, AgentOutcome, Promise};

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
    /// Submit a task to an agent and get back a promise id.
    Submit {
        /// The agent that will do the work (for example: researcher).
        #[arg(long)]
        agent: String,

        /// The task the agent is being asked to do.
        #[arg(long)]
        task: String,

        /// Simulate a single agent turn so the typed-error path is visible
        /// end to end. One of: done, timeout, tool-error, rate-limited.
        /// Without it, `submit` stops at a pending promise, same as Part 3.
        #[arg(long)]
        simulate: Option<String>,
    },
}

/// Turn the `--simulate` flag into an [`AgentOutcome`] the library can track.
///
/// Returns a `Result` so an unknown value is a typed-ish failure with an
/// `anyhow` message rather than a panic. The valid set is small and closed on
/// purpose; this is a teaching hook, not a real agent runtime.
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

/// Dispatch a parsed command.
///
/// The return type is `anyhow::Result<()>`, so `?` can propagate a
/// `PromiseError` from the library and an `anyhow` error from this module
/// through the same operator. `anyhow` erases the concrete type at the boundary
/// where a human, not a `match`, is the consumer.
fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Submit {
            agent,
            task,
            simulate,
        } => {
            // `?` here propagates a typed PromiseError. `.context(...)` wraps it
            // with a human-readable layer so the report says what we were doing,
            // not just what broke. Note the context names the agent, never the
            // task text.
            let mut promise = Promise::submit(AgentId::new(&agent), task)
                .with_context(|| format!("submitting a task to agent '{agent}'"))?;

            println!("submitted: {}", promise.id());
            println!("  agent: {}", promise.agent());
            println!("  task:  {}", promise.task());
            println!("  state: {}", promise.state());

            // Without --simulate, we stop at a pending promise, exactly like
            // Part 3. With it, we drive one turn so the failure path is real.
            if let Some(flag) = simulate {
                let outcome = outcome_from_flag(&flag)?;
                promise.start();

                // The second `?` in the submit-and-track path. A timeout, tool
                // failure, or rate-limit propagates out of `run` as an error;
                // `done` resolves the promise and prints the result.
                let output = promisekeeper::track(&mut promise, outcome)
                    .with_context(|| format!("tracking promise {}", promise.id()))?;

                println!("  state: {}", promise.state());
                println!("  result: {output}");
            }

            Ok(())
        }
    }
}

fn main() {
    let cli = Cli::parse();

    // One place turns any error into a tidy report and a non-zero exit. `{:#}`
    // prints the full anyhow context chain on a single logical report
    // ("tracking promise X: agent 'Y' timed out after 30s"), which is exactly
    // the diagnostic an operator wants and still free of task contents.
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
    fn timeout_maps_to_a_timed_out_outcome() {
        let outcome = outcome_from_flag("timeout").unwrap();
        assert!(matches!(outcome, AgentOutcome::TimedOut { .. }));
    }

    #[test]
    fn an_unknown_simulate_value_is_an_error_not_a_panic() {
        let result = outcome_from_flag("explode");
        assert!(result.is_err());
        // The message guides the user to the valid set.
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unknown --simulate value"));
    }
}
