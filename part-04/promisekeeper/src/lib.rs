//! PromiseKeeper's domain library.
//!
//! Part 3 gave this crate the promise types and a one-variant `SubmitError`
//! placeholder over in the CLI. Part 4 is where failure becomes a first-class
//! part of the domain. Agents fail in creative ways: the model times out, a
//! tool hands back malformed JSON, the API rate-limits mid-task, the task text
//! is garbage. Each of those is now a *value*, a variant of [`PromiseError`],
//! typed and visible in function signatures, and the compiler will not let a
//! caller ignore it.
//!
//! Two design rules drive this module:
//!
//! 1. **Errors are data the caller can match on.** The retry policy in Part 5
//!    decides which variants are worth retrying. The `Rejected` state in Part 9
//!    stores the variant that killed the promise. So [`PromiseError`] carries
//!    structured fields (which agent, which tool, what the upstream said),
//!    not just a string.
//! 2. **Errors describe the failure, never the payload.** A diagnostic that is
//!    useful to an operator must not dump the task text, the prompt, or a
//!    secret into a log line. Variants carry the agent id and a short upstream
//!    reason; they never carry the task contents.

use std::fmt;

use uuid::Uuid;

/// A typed identifier for a promise.
///
/// A newtype wrapping a `Uuid`. `PromiseId` and `AgentId` are distinct types
/// even though both wrap the same thing; you cannot pass one where the other
/// belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PromiseId(Uuid);

impl PromiseId {
    /// Mint a fresh, random promise id.
    pub fn new() -> Self {
        PromiseId(Uuid::new_v4())
    }
}

impl Default for PromiseId {
    fn default() -> Self {
        PromiseId::new()
    }
}

impl fmt::Display for PromiseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A typed identifier for an agent.
///
/// Also a newtype, and deliberately a *different* type from `PromiseId`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentId(String);

impl AgentId {
    /// Build an agent id from a name.
    pub fn new(name: impl Into<String>) -> Self {
        AgentId(name.into())
    }

    /// The agent's name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Everything that can go wrong while PromiseKeeper tries to keep a promise.
///
/// This is the chapter's centerpiece. Each variant is a failure mode an agent
/// orchestrator actually hits in production, carrying the diagnostic data you
/// want when it fires and nothing you would regret seeing in a log.
///
/// `thiserror` derives [`std::fmt::Display`] from the `#[error(...)]`
/// attributes and the [`std::error::Error`] impl (including the `source` chain
/// for any `#[source]`/`#[from]` field). The enum stays a plain data type you
/// can `match` on; the derive only removes the boilerplate.
///
/// Note what the messages deliberately leave out: the task text, the prompt,
/// and any tool argument. `ToolFailed` carries a short `reason` from the tool,
/// but the convention (enforced by the constructor [`PromiseError::tool_failed`])
/// is that the reason is a category, never echoed input. An error that leaks
/// the task contents into a shared log is a data-exposure bug wearing a
/// diagnostic's clothes.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum PromiseError {
    /// The agent did not respond within its deadline. Carries which agent and
    /// how long we waited, so a retry policy can reason about it later.
    #[error("agent '{agent}' timed out after {waited_secs}s")]
    AgentTimeout { agent: AgentId, waited_secs: u64 },

    /// A tool the agent invoked failed. Carries the tool name and a short,
    /// pre-categorized reason (for example "malformed json", "non-2xx
    /// status"). The reason is never raw tool output or task input.
    #[error("tool '{tool}' failed: {reason}")]
    ToolFailed { tool: String, reason: String },

    /// The submitted task was not something the engine will accept. Carries a
    /// machine-usable category, not the offending text.
    #[error("invalid task: {0}")]
    InvalidTask(InvalidTask),

    /// An upstream API rate-limited us. Carries how long it asked us to wait,
    /// which Part 5's backoff policy reads directly.
    #[error("rate limited by upstream; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

impl PromiseError {
    /// Build a [`PromiseError::ToolFailed`] from a tool name and a *category*
    /// reason. The constructor exists so the "reason is a category, not echoed
    /// input" rule has one place to live.
    pub fn tool_failed(tool: impl Into<String>, reason: impl Into<String>) -> Self {
        PromiseError::ToolFailed {
            tool: tool.into(),
            reason: reason.into(),
        }
    }

    /// Whether retrying this failure could plausibly succeed without a human
    /// changing anything. Part 5 turns this into the retry *condition*: a
    /// timeout or a rate-limit is worth another attempt; an invalid task is
    /// not, because the same input will fail the same way.
    ///
    /// It lives here, next to the variants, so adding a variant forces a
    /// decision about its retryability at the same spot.
    pub fn is_retryable(&self) -> bool {
        match self {
            PromiseError::AgentTimeout { .. } => true,
            PromiseError::RateLimited { .. } => true,
            PromiseError::ToolFailed { .. } => true,
            PromiseError::InvalidTask(_) => false,
        }
    }
}

/// Why a task was rejected before any agent ran. A small enum rather than a
/// string so the CLI and later the API can branch on the category without
/// parsing a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidTask {
    /// The task text was empty or only whitespace.
    Empty,
    /// The task text was longer than the engine accepts.
    TooLong { limit: usize },
}

impl fmt::Display for InvalidTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidTask::Empty => write!(f, "task text must not be empty"),
            InvalidTask::TooLong { limit } => {
                write!(f, "task text exceeds the {limit}-character limit")
            }
        }
    }
}

/// Where a promise is in its lifecycle. Still deliberately small; Part 9 grows
/// it into a compiler-enforced state machine. The seam for that is already
/// here: Part 9's `Rejected` state will carry a [`PromiseError`], which is
/// exactly why the error type is structured data and not a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Running,
    Resolved(String),
}

impl fmt::Display for PromiseState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PromiseState::Pending => write!(f, "pending"),
            PromiseState::Running => write!(f, "running"),
            PromiseState::Resolved(result) => write!(f, "resolved: {result}"),
        }
    }
}

/// An agent task the engine is tracking: PromiseKeeper's first-class citizen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promise {
    id: PromiseId,
    agent: AgentId,
    task: String,
    state: PromiseState,
}

impl Promise {
    /// The engine's task-length ceiling. Tasks longer than this are rejected
    /// up front with [`InvalidTask::TooLong`].
    pub const MAX_TASK_LEN: usize = 8_192;

    /// Validate a submit request and build a pending promise, or return a typed
    /// reason it could not.
    ///
    /// This is the library's own `Result`-returning front door. The `pk`
    /// binary used to own this check with a one-variant placeholder; now the
    /// domain owns it and the failure is a real [`PromiseError`] the caller can
    /// match on. The error names the category, never the rejected text.
    pub fn submit(agent: AgentId, task: impl Into<String>) -> Result<Promise, PromiseError> {
        let task = task.into();
        let trimmed = task.trim();
        if trimmed.is_empty() {
            return Err(PromiseError::InvalidTask(InvalidTask::Empty));
        }
        if task.chars().count() > Self::MAX_TASK_LEN {
            return Err(PromiseError::InvalidTask(InvalidTask::TooLong {
                limit: Self::MAX_TASK_LEN,
            }));
        }
        Ok(Promise {
            id: PromiseId::new(),
            agent,
            task,
            state: PromiseState::Pending,
        })
    }

    /// This promise's id.
    pub fn id(&self) -> PromiseId {
        self.id
    }

    /// The agent responsible for this promise.
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// The task text.
    pub fn task(&self) -> &str {
        &self.task
    }

    /// Where the promise is in its lifecycle.
    pub fn state(&self) -> &PromiseState {
        &self.state
    }

    /// Mark the promise running. Returns `&mut self` so calls can chain while
    /// the engine drives the lifecycle.
    pub fn start(&mut self) -> &mut Self {
        self.state = PromiseState::Running;
        self
    }

    /// Resolve the promise with a result the agent produced.
    pub fn resolve(&mut self, result: impl Into<String>) {
        self.state = PromiseState::Resolved(result.into());
    }
}

/// What a single agent turn reported back. The engine hands this to [`track`],
/// which decides whether it means the promise resolved or failed. Modeling the
/// raw outcome as its own type keeps the failure-mapping logic in one testable
/// place instead of scattered through the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// The agent produced a final answer.
    Completed { output: String },
    /// The agent exceeded its deadline after waiting this many seconds.
    TimedOut { waited_secs: u64 },
    /// A tool the agent called failed, with a category reason.
    ToolError { tool: String, reason: String },
    /// An upstream API rate-limited the call.
    RateLimited { retry_after_secs: u64 },
}

/// Advance a running promise from a single agent outcome.
///
/// The happy path resolves the promise and returns its result. Every failure
/// path returns a typed [`PromiseError`] carrying the agent id and the useful
/// diagnostic. This is the function `?` was built for: a caller writes
/// `let output = track(&mut p, outcome)?;` and the error, if any, propagates
/// out with its type intact rather than being swallowed.
///
/// `track` does not retry and does not sleep. It maps one outcome to one
/// result or one typed failure. Deciding *whether* to retry a retryable
/// variant is Part 5's job, kept out of here on purpose.
pub fn track(promise: &mut Promise, outcome: AgentOutcome) -> Result<String, PromiseError> {
    match outcome {
        AgentOutcome::Completed { output } => {
            promise.resolve(output.clone());
            Ok(output)
        }
        AgentOutcome::TimedOut { waited_secs } => Err(PromiseError::AgentTimeout {
            agent: promise.agent().clone(),
            waited_secs,
        }),
        AgentOutcome::ToolError { tool, reason } => Err(PromiseError::tool_failed(tool, reason)),
        AgentOutcome::RateLimited { retry_after_secs } => {
            Err(PromiseError::RateLimited { retry_after_secs })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_promise_starts_pending() {
        let promise = Promise::submit(AgentId::new("researcher"), "summarize the filings").unwrap();
        assert_eq!(promise.state(), &PromiseState::Pending);
        assert_eq!(promise.agent().as_str(), "researcher");
        assert_eq!(promise.task(), "summarize the filings");
    }

    #[test]
    fn each_promise_gets_a_distinct_id() {
        let a = Promise::submit(AgentId::new("a"), "task one").unwrap();
        let b = Promise::submit(AgentId::new("b"), "task two").unwrap();
        assert_ne!(a.id(), b.id());
    }

    #[test]
    fn submit_rejects_a_blank_task_with_a_typed_error() {
        let err = Promise::submit(AgentId::new("researcher"), "   ").unwrap_err();
        assert_eq!(err, PromiseError::InvalidTask(InvalidTask::Empty));
        assert!(!err.is_retryable());
    }

    #[test]
    fn submit_rejects_an_overlong_task() {
        let huge = "x".repeat(Promise::MAX_TASK_LEN + 1);
        let err = Promise::submit(AgentId::new("researcher"), huge).unwrap_err();
        assert!(matches!(
            err,
            PromiseError::InvalidTask(InvalidTask::TooLong { .. })
        ));
    }

    #[test]
    fn track_resolves_on_a_completed_outcome() {
        let mut promise = Promise::submit(AgentId::new("researcher"), "do the thing").unwrap();
        promise.start();
        let output = track(
            &mut promise,
            AgentOutcome::Completed {
                output: "done".to_string(),
            },
        )
        .unwrap();
        assert_eq!(output, "done");
        assert_eq!(promise.state(), &PromiseState::Resolved("done".to_string()));
    }

    #[test]
    fn track_maps_a_timeout_to_a_typed_error() {
        let mut promise = Promise::submit(AgentId::new("slowpoke"), "do the thing").unwrap();
        promise.start();
        let err = track(&mut promise, AgentOutcome::TimedOut { waited_secs: 30 }).unwrap_err();
        assert_eq!(
            err,
            PromiseError::AgentTimeout {
                agent: AgentId::new("slowpoke"),
                waited_secs: 30,
            }
        );
        assert!(err.is_retryable());
    }

    #[test]
    fn track_maps_a_tool_error_without_leaking_input() {
        let mut promise = Promise::submit(AgentId::new("researcher"), "secret task text").unwrap();
        promise.start();
        let err = track(
            &mut promise,
            AgentOutcome::ToolError {
                tool: "web_fetch".to_string(),
                reason: "malformed json".to_string(),
            },
        )
        .unwrap_err();
        // The rendered message carries the tool and the category, never the task.
        let rendered = err.to_string();
        assert!(rendered.contains("web_fetch"));
        assert!(rendered.contains("malformed json"));
        assert!(!rendered.contains("secret task text"));
    }

    #[test]
    fn rate_limited_is_retryable_and_carries_its_delay() {
        let err = PromiseError::RateLimited {
            retry_after_secs: 12,
        };
        assert!(err.is_retryable());
        assert!(err.to_string().contains("12"));
    }
}
