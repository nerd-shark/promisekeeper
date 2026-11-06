//! PromiseKeeper's domain library.
//!
//! Part 5 built the reliability policy types (retry, backoff, jitter,
//! backpressure) and kept everything synchronous on purpose: the retry driver
//! took its `sleep` as an injected closure, so you could pass
//! `std::thread::sleep` in production and a recording closure in a test. That
//! was the right call for *learning the policy*. It is the wrong call for an
//! engine that has to track fifty agents at once, because `std::thread::sleep`
//! blocks the whole thread, and a thread that is blocked cannot make progress
//! on anything else.
//!
//! Part 6 is where PromiseKeeper learns to wait without blocking. That is what
//! `async`/`.await` and a runtime like tokio buy you. This chapter adds three
//! things and nothing more, because the point is the mental model, not a pile
//! of concurrency features:
//!
//! 1. **An async agent task.** [`run_agent_task`] is an `async fn`: it returns
//!    a *future*, a value representing a computation that is not done yet. It
//!    `.await`s a [`tokio::time::sleep`] to simulate an agent that takes a
//!    moment to think, then produces an [`AgentOutcome`]. No API keys, no
//!    network: the delay and the result are canned, so the focus stays on the
//!    async mechanics.
//! 2. **An async retry driver.** [`run_with_retry_async`] is the Part 5 retry
//!    driver with one change: it `.await`s an async operation and
//!    `tokio::time::sleep`s between attempts instead of calling an injected
//!    blocking closure. Every policy type from Part 5 ([`RetryPolicy`],
//!    [`BackoffStrategy`], [`Jitter`]) carries over unchanged. Only the waiting
//!    became asynchronous.
//! 3. **Nothing that shares state across tasks yet.** Spawning several tasks
//!    concurrently is a thing the *binary* does with [`tokio::spawn`]; the
//!    shared-state engine (`Arc`, `Mutex`, one task per promise) is Part 7.
//!    Holding a lock across an `.await` is the classic trap, and it is Part 7's
//!    problem precisely because this chapter has no shared lock to hold.
//!
//! The one idea to carry out of this chapter: **Rust futures are lazy.** An
//! `async fn` that is never `.await`ed (or spawned) does literally nothing. It
//! is not a thread that starts running the moment you call it, the way a
//! Python coroutine scheduled on an event loop or a JavaScript promise does.
//! It is an inert value that makes progress only when a runtime polls it. Get
//! that one fact right and most "why isn't my future running?" confusion
//! evaporates.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use uuid::Uuid;

// ---------------------------------------------------------------------------
// Part 2-5 carryover: identifiers, the promise, typed errors, `track`, and the
// whole reliability policy (Rng, BackoffStrategy, Jitter, RetryPolicy,
// BoundedQueue). Unchanged from Part 5 except where a doc comment notes it.
// Read Parts 4 and 5 for the full story; this chapter consumes these types and
// makes them run on an async runtime.
// ---------------------------------------------------------------------------

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
/// Introduced in Part 4, extended in Part 5 with [`PromiseError::QueueFull`].
/// Each variant is a failure mode an agent orchestrator actually hits, carrying
/// the diagnostic data you want when it fires and nothing you would regret
/// seeing in a log.
///
/// Note what the messages deliberately leave out: the task text, the prompt,
/// and any tool argument. An error that leaks the task contents into a shared
/// log is a data-exposure bug wearing a diagnostic's clothes.
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

    /// The submission queue is at capacity and refused the work. Backpressure
    /// made visible: the engine protects itself by saying no now rather than
    /// buffering until it falls over. Carries the capacity so a caller can
    /// report what limit it hit.
    #[error("submission queue is full (capacity {capacity}); try again later")]
    QueueFull { capacity: usize },
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
    /// changing anything. Part 5 turned this into the retry *condition*: a
    /// timeout, a rate-limit, or a transient tool failure is worth another
    /// attempt; an invalid task or a full queue is not.
    pub fn is_retryable(&self) -> bool {
        match self {
            PromiseError::AgentTimeout { .. } => true,
            PromiseError::RateLimited { .. } => true,
            PromiseError::ToolFailed { .. } => true,
            PromiseError::InvalidTask(_) => false,
            PromiseError::QueueFull { .. } => false,
        }
    }

    /// If the upstream told us exactly how long to wait, surface it. The
    /// backoff policy honors this over its own computed delay.
    ///
    /// Returns `None` for every variant except [`PromiseError::RateLimited`].
    pub fn server_retry_after(&self) -> Option<Duration> {
        match self {
            PromiseError::RateLimited { retry_after_secs } => {
                Some(Duration::from_secs(*retry_after_secs))
            }
            _ => None,
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
/// it into a compiler-enforced state machine.
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
/// which decides whether it means the promise resolved or failed.
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
/// path returns a typed [`PromiseError`]. `track` does not retry and does not
/// sleep; it maps one outcome to one result or one typed failure. Deciding
/// *whether* to retry is [`RetryPolicy`]'s job; running the attempt on a
/// runtime is [`run_agent_task`]'s.
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

// ---------------------------------------------------------------------------
// Part 5 carryover: the reliability policy. A tiny deterministic PRNG for
// jitter, the backoff strategies, the jitter modes, and the retry policy data.
// All unchanged from Part 5. The SYNCHRONOUS driver (`run_with_retry`) is kept
// too, because it is still the right tool when there is no runtime around (the
// policy unit tests use it, clock-free). Part 6 adds an ASYNC sibling below.
// ---------------------------------------------------------------------------

/// A small, seedable pseudo-random generator used to spread backoff delays.
///
/// Deliberately not cryptographically secure and deliberately not the `rand`
/// crate. Jitter's only requirement is that two clients computing the same
/// nominal delay land on *different* actual delays; a cheap, seedable generator
/// meets that bar and lets tests pin the sequence. This is a SplitMix64.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seed the generator. Seed it from the clock in production; seed it with a
    /// constant in tests so the delays are reproducible.
    pub fn new(seed: u64) -> Self {
        Rng {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Next raw 64-bit value (SplitMix64).
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A float in `[0.0, 1.0)`. The jitter multiplier is built from this.
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// How to space out retry attempts. Modeled as an enum so the compiler forces
/// every delay computation to handle both variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackoffStrategy {
    /// Wait the same `delay` before every retry.
    Fixed { delay: Duration },
    /// Wait `initial` before the first retry, multiply by `multiplier` each
    /// subsequent attempt, and never exceed `max`.
    Exponential {
        initial: Duration,
        multiplier: u32,
        max: Duration,
    },
}

impl BackoffStrategy {
    /// A sensible exponential default: start at `initial`, double each time,
    /// cap at `max`.
    pub fn exponential(initial: Duration, max: Duration) -> Self {
        BackoffStrategy::Exponential {
            initial,
            multiplier: 2,
            max,
        }
    }

    /// The *nominal* delay before a given retry attempt, before jitter.
    /// `attempt` is 1-based. Saturating arithmetic keeps a long-lived policy
    /// from overflowing its way to a bad delay.
    pub fn nominal_delay(&self, attempt: u32) -> Duration {
        match self {
            BackoffStrategy::Fixed { delay } => *delay,
            BackoffStrategy::Exponential {
                initial,
                multiplier,
                max,
            } => {
                let factor = multiplier.saturating_pow(attempt.saturating_sub(1));
                initial.checked_mul(factor).unwrap_or(*max).min(*max)
            }
        }
    }
}

/// How much to randomize a nominal backoff delay. See Part 5 for why backoff
/// without jitter only reschedules the thundering herd.
#[derive(Debug, Clone, PartialEq)]
pub enum Jitter {
    /// No jitter. Every client retries in lockstep. Danger you opt into.
    None,
    /// "Full" jitter: a uniform random value in `[0, nominal]`.
    Full,
    /// "Equal" jitter: half the nominal delay plus a random `[0, nominal/2]`.
    Equal,
}

impl Jitter {
    /// Apply this jitter to a nominal delay, drawing from `rng`.
    pub fn apply(&self, nominal: Duration, rng: &mut Rng) -> Duration {
        match self {
            Jitter::None => nominal,
            Jitter::Full => nominal.mul_f64(rng.next_f64()),
            Jitter::Equal => {
                let half = nominal.mul_f64(0.5);
                half + half.mul_f64(rng.next_f64())
            }
        }
    }
}

/// A retry policy: the data that decides whether, how often, and how slowly to
/// retry a failing operation. Carries no behavior of its own beyond small
/// helpers; the retrying is done by a driver that reads the policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts, including the first.
    pub max_attempts: u32,
    /// How to space the retries out.
    pub backoff: BackoffStrategy,
    /// How much to randomize each delay.
    pub jitter: Jitter,
}

impl RetryPolicy {
    /// A reasonable default for a flaky agent dependency: three attempts,
    /// exponential backoff from 1s to 30s, full jitter.
    pub fn default_for_agents() -> Self {
        RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(30)),
            jitter: Jitter::Full,
        }
    }

    /// The delay to wait before a given retry `attempt` (1-based), honoring a
    /// server-supplied `retry_after` over the computed backoff when present.
    pub fn delay_before(
        &self,
        attempt: u32,
        server_retry_after: Option<Duration>,
        rng: &mut Rng,
    ) -> Duration {
        let nominal = server_retry_after.unwrap_or_else(|| self.backoff.nominal_delay(attempt));
        self.jitter.apply(nominal, rng)
    }
}

/// The outcome of a retried operation: either it eventually succeeded, or it
/// exhausted its attempts and we return the *last* typed error, or it failed
/// with a non-retryable error and we stopped immediately.
pub type RetryResult<T> = Result<T, PromiseError>;

/// Drive an operation under a [`RetryPolicy`], synchronously.
///
/// Kept from Part 5. This is still the right tool when there is no runtime in
/// scope: the policy unit tests use it with a recording `sleep` closure and
/// assert the whole backoff schedule in microseconds, no clock, no runtime.
/// When the operation is async and should not block a thread, reach for
/// [`run_with_retry_async`] instead.
pub fn run_with_retry<T, Op, Sleep>(
    policy: &RetryPolicy,
    rng: &mut Rng,
    mut operation: Op,
    mut sleep: Sleep,
) -> RetryResult<T>
where
    Op: FnMut(u32) -> RetryResult<T>,
    Sleep: FnMut(Duration),
{
    let attempts = policy.max_attempts.max(1);
    let mut last_error: Option<PromiseError> = None;
    for attempt in 0..attempts {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => {
                if !err.is_retryable() {
                    return Err(err);
                }
                let is_last = attempt + 1 >= attempts;
                if is_last {
                    return Err(err);
                }
                let server_hint = err.server_retry_after();
                let delay = policy.delay_before(attempt + 1, server_hint, rng);
                last_error = Some(err);
                sleep(delay);
            }
        }
    }
    Err(last_error.unwrap_or(PromiseError::RateLimited {
        retry_after_secs: 0,
    }))
}

/// A fixed-capacity FIFO queue for pending promises: the backpressure
/// primitive from Part 5. `enqueue` returns a typed [`PromiseError::QueueFull`]
/// once at capacity instead of growing without bound.
#[derive(Debug)]
pub struct BoundedQueue {
    inner: VecDeque<Promise>,
    capacity: usize,
}

impl BoundedQueue {
    /// Build a queue that will hold at most `capacity` promises.
    pub fn new(capacity: usize) -> Self {
        BoundedQueue {
            inner: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Try to add a promise. Returns the promise's id on success, or
    /// [`PromiseError::QueueFull`] if the queue is at capacity.
    pub fn enqueue(&mut self, promise: Promise) -> Result<PromiseId, PromiseError> {
        if self.inner.len() >= self.capacity {
            return Err(PromiseError::QueueFull {
                capacity: self.capacity,
            });
        }
        let id = promise.id();
        self.inner.push_back(promise);
        Ok(id)
    }

    /// Remove and return the oldest promise, or `None` if the queue is empty.
    pub fn dequeue(&mut self) -> Option<Promise> {
        self.inner.pop_front()
    }

    /// How many promises are currently queued.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// The queue's hard capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Whether the queue is at capacity and will refuse the next `enqueue`.
    pub fn is_full(&self) -> bool {
        self.inner.len() >= self.capacity
    }
}

// ---------------------------------------------------------------------------
// Part 6, piece 1: the first `async fn`.
//
// `run_agent_task` is PromiseKeeper's first asynchronous function. The job of
// this chapter is to make what it does completely un-magical, so read the type
// signature carefully:
//
//     async fn run_agent_task(...) -> Result<String, PromiseError>
//
// desugars to:
//
//     fn run_agent_task(...) -> impl Future<Output = Result<String, PromiseError>>
//
// Calling it does NOT run it. It returns a *future*: a value that represents
// the "run this agent task" computation, not the result of having run it. The
// future is inert. It makes progress only when something `.await`s it (which
// hands control back up to whoever `.await`ed us) or when a runtime polls it as
// a spawned task. This laziness is the single biggest difference from Python's
// coroutines-on-an-event-loop or a JavaScript promise, both of which are
// already scheduled the moment you create them.
//
// To keep the focus on async and not on HTTP, the "agent" here is simulated: it
// awaits a timer to stand in for think-time, then returns a canned outcome. No
// API keys, no network, no flakiness except what we ask for.
// ---------------------------------------------------------------------------

/// A description of a simulated agent task: how long it "thinks" and what it
/// ultimately reports back. This is a plain data struct on purpose; the async
/// behavior lives in [`run_agent_task`], not in the data.
///
/// In a real engine this would be a prompt, a model handle, and a toolset. Here
/// it is a sleep duration and a canned [`AgentOutcome`], which is all we need to
/// exercise `.await` without dragging in a network.
#[derive(Debug, Clone)]
pub struct AgentTask {
    /// How long the simulated agent "thinks" before reporting. The task awaits
    /// a [`tokio::time::sleep`] of this length; this is the `.await` point.
    pub think_time: Duration,
    /// What the agent reports when its think-time elapses. A `Completed` makes
    /// the task resolve; the failure variants drive the typed-error paths.
    pub outcome: AgentOutcome,
}

impl AgentTask {
    /// A task that thinks for `think_time` and then completes with `output`.
    pub fn completing(think_time: Duration, output: impl Into<String>) -> Self {
        AgentTask {
            think_time,
            outcome: AgentOutcome::Completed {
                output: output.into(),
            },
        }
    }

    /// A task that thinks for `think_time` and then times out: the canonical
    /// retryable failure, used to drive the async retry demo.
    pub fn timing_out(think_time: Duration, waited_secs: u64) -> Self {
        AgentTask {
            think_time,
            outcome: AgentOutcome::TimedOut { waited_secs },
        }
    }
}

/// Run one simulated agent turn against a promise, asynchronously.
///
/// This is the chapter's centerpiece. It is `async`, so it returns a future;
/// calling it builds the future, and `.await`ing it drives the work:
///
/// 1. It marks the promise `Running`.
/// 2. It `.await`s a [`tokio::time::sleep`] for the task's `think_time`. This
///    is the yield point. While the timer counts down, this future is parked
///    and the runtime is free to poll *other* tasks (the whole reason the
///    binary can run several of these at once on one thread).
/// 3. When the timer fires, it hands the task's `outcome` to [`track`], which
///    resolves the promise or returns a typed [`PromiseError`].
///
/// Note the `await` is on a non-blocking timer. If this function called
/// `std::thread::sleep` instead, it would block the runtime's worker thread for
/// the whole think-time, and nothing else scheduled on that thread could make
/// progress. Blocking the executor is beginner trap number one, and swapping
/// the blocking sleep for `tokio::time::sleep` is exactly how you avoid it.
pub async fn run_agent_task(
    promise: &mut Promise,
    task: AgentTask,
) -> Result<String, PromiseError> {
    promise.start();
    // The `.await` point. Control returns to the runtime until the timer fires;
    // it does not block the thread.
    tokio::time::sleep(task.think_time).await;
    track(promise, task.outcome)
}

// ---------------------------------------------------------------------------
// Part 6, piece 2: the async retry driver.
//
// This is the Part 5 `run_with_retry` with exactly one substantive change: the
// operation it calls is async (so it is `.await`ed), and the wait between
// attempts is `tokio::time::sleep(delay).await` instead of an injected blocking
// closure. Every decision the synchronous driver made is preserved: refuse a
// non-retryable error immediately, never sleep after the final attempt, and
// honor a server-directed retry-after over the computed backoff.
//
// Why not just inject an async sleep closure the way Part 5 injected a blocking
// one? Because an async closure that returns a future is noticeably more
// painful to spell out in Rust today, and the lesson here is the async control
// flow, not closure gymnastics. `tokio::time::sleep` is the real production
// timer; using it directly keeps the driver readable. The async operation is
// still injected (it is a parameter), so the driver stays testable with a fake
// agent, which the tests below do.
// ---------------------------------------------------------------------------

/// Drive an async operation under a [`RetryPolicy`].
///
/// `operation` is called with the attempt number (0-based) and returns a future
/// resolving to `Result<T, PromiseError>`. On success the driver returns
/// immediately. On a *retryable* failure it computes a jittered delay from the
/// policy, `.await`s `tokio::time::sleep` for it (yielding to the runtime, not
/// blocking a thread), and tries again until attempts run out. On a
/// *non-retryable* failure it returns at once, because retrying is a bug.
///
/// The operation is a closure returning a future rather than a bare async
/// closure, which keeps the signature expressible on stable Rust: `Op: FnMut(u32)
/// -> Fut, Fut: Future<Output = RetryResult<T>>`. The binary passes a closure
/// that builds a `run_agent_task` future per attempt.
pub async fn run_with_retry_async<T, Op, Fut>(
    policy: &RetryPolicy,
    rng: &mut Rng,
    mut operation: Op,
) -> RetryResult<T>
where
    Op: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = RetryResult<T>>,
{
    let attempts = policy.max_attempts.max(1);
    let mut last_error: Option<PromiseError> = None;
    for attempt in 0..attempts {
        match operation(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                // Retrying a non-retryable error is the bug this guard prevents.
                if !err.is_retryable() {
                    return Err(err);
                }
                // The last attempt: surface the error, do not sleep.
                let is_last = attempt + 1 >= attempts;
                if is_last {
                    return Err(err);
                }
                // `attempt` is 0-based; the backoff curve is 1-based.
                let server_hint = err.server_retry_after();
                let delay = policy.delay_before(attempt + 1, server_hint, rng);
                last_error = Some(err);
                // The async wait: yields to the runtime instead of blocking.
                tokio::time::sleep(delay).await;
            }
        }
    }
    Err(last_error.unwrap_or(PromiseError::RateLimited {
        retry_after_secs: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Part 4/5 carryover (failure is a value; the policy still holds) -

    #[test]
    fn a_new_promise_starts_pending() {
        let promise = Promise::submit(AgentId::new("researcher"), "summarize the filings").unwrap();
        assert_eq!(promise.state(), &PromiseState::Pending);
        assert_eq!(promise.agent().as_str(), "researcher");
    }

    #[test]
    fn submit_rejects_a_blank_task_with_a_typed_error() {
        let err = Promise::submit(AgentId::new("researcher"), "   ").unwrap_err();
        assert_eq!(err, PromiseError::InvalidTask(InvalidTask::Empty));
        assert!(!err.is_retryable());
    }

    #[test]
    fn exponential_backoff_doubles_and_caps() {
        let b = BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(10));
        assert_eq!(b.nominal_delay(1), Duration::from_secs(1));
        assert_eq!(b.nominal_delay(2), Duration::from_secs(2));
        assert_eq!(b.nominal_delay(5), Duration::from_secs(10)); // capped from 16
        assert_eq!(b.nominal_delay(50), Duration::from_secs(10)); // no overflow
    }

    #[test]
    fn the_sync_driver_still_works_clock_free() {
        // The synchronous Part 5 driver is retained for exactly this: asserting
        // a backoff schedule in microseconds, no runtime, no real clock.
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(30)),
            jitter: Jitter::None,
        };
        let mut rng = Rng::new(0);
        let mut slept: Vec<Duration> = Vec::new();
        let mut calls = 0;
        let result: RetryResult<&str> = run_with_retry(
            &policy,
            &mut rng,
            |_attempt| {
                calls += 1;
                if calls < 3 {
                    Err(PromiseError::AgentTimeout {
                        agent: AgentId::new("slowpoke"),
                        waited_secs: 30,
                    })
                } else {
                    Ok("recovered")
                }
            },
            |d| slept.push(d),
        );
        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(slept, vec![Duration::from_secs(1), Duration::from_secs(2)]);
    }

    #[test]
    fn a_full_queue_refuses_with_a_typed_error() {
        let mut q = BoundedQueue::new(1);
        q.enqueue(Promise::submit(AgentId::new("a"), "task one").unwrap())
            .unwrap();
        let err = q
            .enqueue(Promise::submit(AgentId::new("b"), "task two").unwrap())
            .unwrap_err();
        assert_eq!(err, PromiseError::QueueFull { capacity: 1 });
    }

    // ---- Part 6: the async agent task -----------------------------------

    #[tokio::test]
    async fn an_async_agent_task_resolves_its_promise() {
        let mut promise = Promise::submit(AgentId::new("researcher"), "summarize").unwrap();
        // A tiny think-time keeps the test instant; the point is the `.await`
        // path, not the duration.
        let task = AgentTask::completing(Duration::from_millis(1), "summary ready");
        let output = run_agent_task(&mut promise, task).await.unwrap();
        assert_eq!(output, "summary ready");
        assert_eq!(
            promise.state(),
            &PromiseState::Resolved("summary ready".into())
        );
    }

    #[tokio::test]
    async fn an_async_agent_timeout_is_a_typed_error_and_leaves_the_promise_running() {
        let mut promise = Promise::submit(AgentId::new("slowpoke"), "summarize").unwrap();
        let task = AgentTask::timing_out(Duration::from_millis(1), 30);
        let err = run_agent_task(&mut promise, task).await.unwrap_err();
        assert_eq!(
            err,
            PromiseError::AgentTimeout {
                agent: AgentId::new("slowpoke"),
                waited_secs: 30,
            }
        );
        // The failing turn left it Running (not Resolved): a failed attempt is
        // not a resolution. Part 9's state machine will make this stricter.
        assert_eq!(promise.state(), &PromiseState::Running);
    }

    #[tokio::test(start_paused = true)]
    async fn a_future_is_lazy_until_awaited() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // Building the future must not run it: `started` stays false until the
        // `.await`. An `AtomicBool` lets the test read the flag while the future
        // holds a shared reference to it. `start_paused` means the timer inside
        // the task is virtual, so this test is instant despite a 1s think-time.
        let started = AtomicBool::new(false);
        let mut promise = Promise::submit(AgentId::new("a"), "t").unwrap();
        let fut = async {
            started.store(true, Ordering::SeqCst);
            run_agent_task(
                &mut promise,
                AgentTask::completing(Duration::from_secs(1), "done"),
            )
            .await
        };
        assert!(
            !started.load(Ordering::SeqCst),
            "a future must not run before it is awaited"
        );
        let out = fut.await;
        assert!(started.load(Ordering::SeqCst), "awaiting the future ran it");
        assert_eq!(out.unwrap(), "done");
    }

    // ---- Part 6: the async retry driver ---------------------------------

    #[tokio::test(start_paused = true)]
    async fn async_retry_recovers_after_retryable_failures() {
        // `start_paused` + auto-advance means the backoff sleeps complete in
        // virtual time: the test asserts real retry behavior in no wall time.
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(30)),
            jitter: Jitter::None,
        };
        let mut rng = Rng::new(0);
        let mut calls = 0;

        let result: RetryResult<&str> = run_with_retry_async(&policy, &mut rng, |_attempt| {
            calls += 1;
            async move {
                if calls < 3 {
                    Err(PromiseError::AgentTimeout {
                        agent: AgentId::new("slowpoke"),
                        waited_secs: 30,
                    })
                } else {
                    Ok("recovered")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "recovered");
        assert_eq!(calls, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn async_retry_stops_immediately_on_a_non_retryable_error() {
        let policy = RetryPolicy::default_for_agents();
        let mut rng = Rng::new(0);
        let mut calls = 0;

        let result: RetryResult<&str> = run_with_retry_async(&policy, &mut rng, |_attempt| {
            calls += 1;
            async { Err(PromiseError::InvalidTask(InvalidTask::Empty)) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(calls, 1, "a non-retryable error must not be retried");
    }

    #[tokio::test(start_paused = true)]
    async fn async_retry_exhausts_and_returns_the_last_error() {
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::Fixed {
                delay: Duration::from_millis(5),
            },
            jitter: Jitter::None,
        };
        let mut rng = Rng::new(0);
        let mut calls = 0;

        let result: RetryResult<&str> = run_with_retry_async(&policy, &mut rng, |_attempt| {
            calls += 1;
            async {
                Err(PromiseError::AgentTimeout {
                    agent: AgentId::new("slowpoke"),
                    waited_secs: 30,
                })
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(calls, 3, "three attempts total");
    }
}
