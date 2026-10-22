//! PromiseKeeper's domain library.
//!
//! This is the first time PromiseKeeper's types live in a library crate of
//! their own, separate from the binary that drives them. The `pk` CLI depends
//! on this crate; so will the service in Part 12 and the durable store in
//! Part 13. Putting the domain here, now, is why those later chapters add a
//! crate instead of untangling one.
//!
//! The types are the ones from Part 2, assembled for real. The change worth
//! noticing: identifiers are newtypes over `Uuid` instead of a bare `u64`.
//! A `PromiseId` and an `AgentId` are both UUIDs underneath, but the compiler
//! treats them as different types, so you can't hand an agent id to something
//! that wants a promise id. The mix-up stops being a code-review catch and
//! becomes a compile error.

use std::fmt;

use uuid::Uuid;

/// A typed identifier for a promise.
///
/// A newtype wrapping a `Uuid`. The point isn't the UUID; it's that
/// `PromiseId` and `AgentId` are distinct types even though both wrap the same
/// thing. You cannot pass one where the other belongs.
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
/// Also a newtype, and deliberately a *different* type from `PromiseId`. An
/// agent is named by whoever submits work (`--agent researcher`), so we build
/// one from a string rather than minting a random UUID.
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

/// Where a promise is in its lifecycle.
///
/// Still deliberately small for now. `Resolved` carries its result in the
/// variant, so a resolved promise can't exist without one. Part 9 grows this
/// into a compiler-enforced state machine where illegal transitions don't
/// compile.
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
    /// Create a new promise for `agent` to do `task`. It starts `Pending` and
    /// gets a fresh id. The caller owns the promise it gets back, until it
    /// hands it to the engine.
    pub fn new(agent: AgentId, task: impl Into<String>) -> Self {
        Promise {
            id: PromiseId::new(),
            agent,
            task: task.into(),
            state: PromiseState::Pending,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_promise_starts_pending() {
        let promise = Promise::new(AgentId::new("researcher"), "summarize the filings");
        assert_eq!(promise.state(), &PromiseState::Pending);
        assert_eq!(promise.agent().as_str(), "researcher");
        assert_eq!(promise.task(), "summarize the filings");
    }

    #[test]
    fn each_promise_gets_a_distinct_id() {
        let a = Promise::new(AgentId::new("a"), "task one");
        let b = Promise::new(AgentId::new("b"), "task two");
        assert_ne!(a.id(), b.id());
    }
}
