//! `pk`: the PromiseKeeper command-line interface.
//!
//! This chapter's goal is a real `pk submit` you can run from your terminal:
//!
//! ```text
//! pk submit --agent researcher --task "summarize the Q3 10-K filings"
//! ```
//!
//! It parses the arguments, builds a `Promise` from the domain library, and
//! prints back the promise id. The lifecycle beyond `submit` (watch, status,
//! cancel) arrives in later chapters; the subcommand enum is already shaped so
//! those slot in without reworking the dispatch.

use clap::{Parser, Subcommand};
use promisekeeper::{AgentId, Promise};

/// The top-level `pk` command. clap's derive API turns this struct into an
/// argument parser: the doc comments become the `--help` text, so the
/// documentation and the CLI can't drift apart.
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

/// The subcommands `pk` understands. Today only `Submit` does real work; the
/// rest are declared so the shape is set and the `match` in `run` is already
/// exhaustive. Add a variant later and the compiler makes you handle it.
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
    },
}

/// Why a submit couldn't go through. One variant today; typed failures get
/// their proper treatment in Part 4. Even this small enum is enough to return
/// a `Result` instead of panicking on bad input.
#[derive(Debug)]
enum SubmitError {
    EmptyTask,
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::EmptyTask => write!(f, "task text must not be empty"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// Build a promise from a submit request and return its id.
///
/// Returns a `Result`: the happy path yields a `PromiseId`, and a blank task
/// yields a `SubmitError` instead of a half-built promise. This is `Result<T, E>`
/// showing up where it first earns its place, not as a tutorial aside.
fn submit(agent: &str, task: &str) -> Result<Promise, SubmitError> {
    if task.trim().is_empty() {
        return Err(SubmitError::EmptyTask);
    }
    Ok(Promise::new(AgentId::new(agent), task))
}

/// Dispatch a parsed command. The `match` is exhaustive over `Command`, which
/// is the enum feature experienced engineers tend to like most: every case is
/// handled, and the compiler proves it.
fn run(cli: Cli) -> Result<(), SubmitError> {
    match cli.command {
        Command::Submit { agent, task } => {
            let promise = submit(&agent, &task)?;
            println!("submitted: {}", promise.id());
            println!("  agent: {}", promise.agent());
            println!("  task:  {}", promise.task());
            println!("  state: {}", promise.state());
            Ok(())
        }
    }
}

fn main() {
    let cli = Cli::parse();

    // Turn an error into a non-zero exit and a message on stderr, rather than a
    // panic. A real CLI fails like a CLI.
    if let Err(err) = run(cli) {
        eprintln!("pk: {err}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_builds_a_pending_promise() {
        let promise = submit("researcher", "summarize the filings").unwrap();
        assert_eq!(promise.agent().as_str(), "researcher");
        assert_eq!(promise.task(), "summarize the filings");
    }

    #[test]
    fn submit_rejects_a_blank_task() {
        let result = submit("researcher", "   ");
        assert!(matches!(result, Err(SubmitError::EmptyTask)));
    }
}
