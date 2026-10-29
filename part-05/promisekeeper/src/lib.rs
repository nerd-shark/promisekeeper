//! PromiseKeeper's domain library.
//!
//! Part 4 made failure a first-class value: [`PromiseError`], a typed enum the
//! compiler will not let a caller ignore. Part 5 is where those values earn
//! their keep. A flaky swarm of agents is the normal operating condition, not
//! the exception: models rate-limit, tools time out, upstream APIs blink. This
//! chapter adds the three primitives that keep a struggling dependency from
//! being hammered to death by its own clients:
//!
//! 1. **Retry as a typed decision.** [`RetryPolicy`] reuses
//!    [`PromiseError::is_retryable`] from Part 4 to decide *what* to retry. A
//!    timeout or a rate-limit is worth another attempt; an invalid task is not,
//!    because the same input fails the same way. Retrying a non-retryable error
//!    is a bug, so the policy refuses to.
//! 2. **Backoff with jitter.** [`BackoffStrategy`] is an enum ([`Fixed`] and
//!    [`Exponential`]) so the compiler forces every caller to handle both. The
//!    delay is spread with randomized [`Jitter`] because fifty agents backing
//!    off on the identical schedule retry in lockstep and recreate the
//!    thundering herd they were supposed to relieve.
//! 3. **Backpressure.** [`BoundedQueue`] has a hard capacity. When agents
//!    outrun the engine, submissions are *refused* with a typed error instead
//!    of being buffered until memory runs out. Refusing work early is how a
//!    system protects itself from its own clients.
//!
//! Everything here is synchronous and single-task on purpose. The concurrency
//! machinery (tokio, `Arc`, one task per promise) arrives in Parts 6 through 8.
//! The *policy* types are what matter in this chapter, and they are easier to
//! reason about, and to test, without a runtime in the way.
//!
//! [`Fixed`]: BackoffStrategy::Fixed
//! [`Exponential`]: BackoffStrategy::Exponential

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use uuid::Uuid;

// ---------------------------------------------------------------------------
// Part 2-4 carryover: identifiers, the promise, typed errors, and `track`.
// Unchanged from Part 4 except where noted. Read Part 4 for the full story on
// why failure is modeled as data; this chapter consumes that data.
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
/// Introduced in Part 4. Each variant is a failure mode an agent orchestrator
/// actually hits, carrying the diagnostic data you want when it fires and
/// nothing you would regret seeing in a log. Part 5 adds one variant,
/// [`PromiseError::QueueFull`], the typed signal that backpressure engaged.
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

    /// New in Part 5. The submission queue is at capacity and refused the work.
    /// This is backpressure made visible: the engine is protecting itself by
    /// saying no now rather than buffering until it falls over. Carries the
    /// capacity so a caller can report what limit it hit.
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
    /// changing anything. Part 5 turns this into the retry *condition*: a
    /// timeout, a rate-limit, or a transient tool failure is worth another
    /// attempt; an invalid task or a full queue is not.
    ///
    /// It lives here, next to the variants, so adding a variant forces a
    /// decision about its retryability at the same spot. A full queue is
    /// deliberately *not* retryable from inside the retry loop: the queue is
    /// full because the system is already overloaded, and the correct response
    /// is to shed the load, not to sit in a retry loop adding to it.
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
    /// backoff policy honors this over its own computed delay: a server that
    /// says "retry after 12s" knows more than our exponential curve does.
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
/// *whether* to retry is [`RetryPolicy`]'s job, added in this chapter and kept
/// out of `track` on purpose.
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
// Part 5, piece 1: a tiny deterministic PRNG for jitter.
//
// Jitter needs randomness that is *unpredictable across attempts*, so fifty
// agents do not retry in lockstep. It does NOT need cryptographic randomness,
// and it does NOT need to be unseedable. A seedable generator is strictly
// better for a teaching build: the policy tests can pin a seed and assert the
// exact delays, and production code can seed from the clock. This is a
// SplitMix64, a well-known, tiny, good-enough generator. Fifteen lines instead
// of a dependency, and the tests stay reproducible.
// ---------------------------------------------------------------------------

/// A small, seedable pseudo-random generator used to spread backoff delays.
///
/// This is deliberately not cryptographically secure and deliberately not the
/// `rand` crate. Jitter's only requirement is that two clients computing the
/// same nominal delay land on *different* actual delays; a cheap, seedable
/// generator meets that bar and lets tests pin the sequence.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seed the generator. Seed it from the clock in production
    /// (`Rng::new(SystemTime::now()...)`); seed it with a constant in tests so
    /// the delays are reproducible.
    pub fn new(seed: u64) -> Self {
        // Avoid an all-zero state, which SplitMix64 tolerates but which reads
        // like a bug. Any nonzero constant works.
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
        // 53 bits of mantissa precision, the standard trick.
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ---------------------------------------------------------------------------
// Part 5, piece 2: backoff strategies, modeled as an enum.
//
// Modeling this as an enum (rather than, say, a trait object or a closure) is
// the Part 2 "enum" idea paying off again: the compiler forces every place
// that computes a delay to handle BOTH variants. Add a third strategy later
// (decorrelated jitter, say) and every match that forgot it fails to compile.
// Part 9 leans on this same property for the lifecycle state machine.
// ---------------------------------------------------------------------------

/// How to space out retry attempts.
///
/// Two strategies, each a variant so a caller must handle both:
///
/// - [`BackoffStrategy::Fixed`] waits the same amount every time. Fine for a
///   dependency that recovers on a known cadence; dangerous under load because
///   every client retries on the same beat.
/// - [`BackoffStrategy::Exponential`] doubles (or multiplies) the wait each
///   attempt, up to a ceiling. This is the right default for an unknown
///   dependency: back off harder the longer it stays down.
///
/// Neither is safe *without jitter*. The [`RetryPolicy`] that owns a strategy
/// is what applies the jitter; the strategy itself only computes the nominal
/// delay for a given attempt.
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
    /// cap at `max`. Spares callers from spelling out `multiplier: 2`.
    pub fn exponential(initial: Duration, max: Duration) -> Self {
        BackoffStrategy::Exponential {
            initial,
            multiplier: 2,
            max,
        }
    }

    /// The *nominal* delay before a given retry attempt, before jitter.
    ///
    /// `attempt` is 1-based: attempt 1 is the first retry (the first call was
    /// attempt 0 and is not this function's concern). For the exponential
    /// strategy the delay is `initial * multiplier^(attempt - 1)`, clamped to
    /// `max`, with saturating arithmetic so a long-lived policy cannot overflow
    /// its way to a negative or wrapped delay.
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

// ---------------------------------------------------------------------------
// Part 5, piece 3: jitter.
//
// This is the non-obvious, high-value idea of the chapter. Backoff alone does
// not solve the thundering herd; it just moves the herd's stampede a few
// seconds later. If fifty agents all fail at t=0 and all back off exactly 2s,
// they all retry at t=2 and knock the dependency over again. Jitter breaks the
// synchronization by randomizing each client's actual delay around the nominal
// one. The canonical reference is the AWS Architecture Blog, "Exponential
// Backoff And Jitter".
// ---------------------------------------------------------------------------

/// How much to randomize a nominal backoff delay.
#[derive(Debug, Clone, PartialEq)]
pub enum Jitter {
    /// No jitter. Every client with the same strategy retries in lockstep. This
    /// variant exists mostly so tests can assert the un-jittered delay and so
    /// the danger is a thing you opt into, not a silent default.
    None,
    /// "Full" jitter: the actual delay is a uniform random value in
    /// `[0, nominal]`. This is the AWS-recommended default and the most
    /// effective at de-synchronizing a herd, at the cost of occasionally
    /// retrying almost immediately.
    Full,
    /// "Equal" jitter: half the nominal delay plus a random value in
    /// `[0, nominal/2]`. Keeps a floor under the delay (a retry never fires
    /// instantly) while still spreading the herd.
    Equal,
}

impl Jitter {
    /// Apply this jitter to a nominal delay, drawing from `rng`.
    ///
    /// Takes `&mut Rng` because drawing a random number advances the generator.
    /// Passing the generator in (rather than calling a global) is what makes
    /// the whole policy testable: a test seeds its own `Rng` and gets a delay
    /// it can assert.
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

// ---------------------------------------------------------------------------
// Part 5, piece 4: the retry policy and its driver.
//
// The policy is data: how many attempts, which backoff, how much jitter. The
// *driver* (`run_with_retry`) is the loop that reads the policy, calls the
// operation, inspects the typed error, and decides whether to wait and try
// again. Keeping the two separate means the policy is trivially testable and
// the driver is the one place the retry decision lives.
//
// The driver takes its "sleep" as a closure so tests do not actually sleep.
// This is the standard trick for testing time-dependent code without a clock
// dependency: in production you pass `std::thread::sleep`; in a test you pass a
// closure that records the requested durations and returns instantly.
// ---------------------------------------------------------------------------

/// A retry policy: the data that decides whether, how often, and how slowly to
/// retry a failing operation.
///
/// This carries no behavior of its own beyond small helpers. The actual
/// retrying is done by [`run_with_retry`], which reads the policy. That split
/// keeps the policy a plain, serializable (in Part 9) data type.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `max_attempts: 3` means one initial
    /// try plus up to two retries.
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
    ///
    /// The precedence is deliberate: if the upstream sent a `RateLimited`
    /// error carrying `retry_after_secs`, that number wins. The server knows
    /// when it will be ready; our exponential curve is only a guess. Jitter is
    /// still applied on top so even server-directed retries do not perfectly
    /// synchronize.
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
/// with a non-retryable error on some attempt and we stopped immediately.
///
/// Modeling "we gave up" and "we were told not to bother" as the same
/// `Err(PromiseError)` keeps the caller's handling simple: a `?` propagates the
/// real underlying failure either way, and the retry machinery is invisible to
/// code that just wants the result.
pub type RetryResult<T> = Result<T, PromiseError>;

/// Drive an operation under a [`RetryPolicy`].
///
/// `operation` is called with the attempt number (0-based: 0 is the first try)
/// and returns `Result<T, PromiseError>`. On success, `run_with_retry` returns
/// immediately. On a *retryable* failure it computes a delay from the policy,
/// calls `sleep` with that delay, and tries again until attempts run out. On a
/// *non-retryable* failure it returns that error at once, without sleeping,
/// because retrying it is a bug.
///
/// `sleep` is injected so this function is testable without a real clock. In
/// production, pass `|d| std::thread::sleep(d)`. In a test, pass a closure that
/// pushes `d` onto a `Vec` and returns, so the test asserts the backoff
/// schedule in microseconds.
///
/// `rng` is injected for the same reason jitter takes it: a seeded generator
/// makes the jittered delays reproducible in tests.
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
    // `max_attempts` of 0 is nonsense; treat it as "try once" so a
    // misconfiguration never means "never run the operation at all".
    let attempts = policy.max_attempts.max(1);

    let mut last_error: Option<PromiseError> = None;
    for attempt in 0..attempts {
        match operation(attempt) {
            Ok(value) => return Ok(value),
            Err(err) => {
                // Retrying a non-retryable error is the bug this guard exists
                // to prevent. Invalid task, full queue: the same call will fail
                // the same way, so stop now and hand the error back.
                if !err.is_retryable() {
                    return Err(err);
                }

                // This was the last attempt; do not sleep, just surface the
                // error. Sleeping after the final attempt is a classic
                // off-by-one that wastes the caller's time for no benefit.
                let is_last = attempt + 1 >= attempts;
                if is_last {
                    return Err(err);
                }

                // Compute the backoff, honoring a server-directed retry-after
                // when the error carried one, then wait.
                let server_hint = err.server_retry_after();
                // `attempt` is 0-based here; the backoff curve is 1-based
                // (attempt 1 is the first retry), so pass `attempt + 1`.
                let delay = policy.delay_before(attempt + 1, server_hint, rng);
                last_error = Some(err);
                sleep(delay);
            }
        }
    }

    // The loop only falls through here if `attempts` was somehow exhausted
    // without an early return, which the `is_last` guard already covers. Return
    // the last error we saw; the `unwrap_or` is defensive and should be
    // unreachable in practice.
    Err(last_error.unwrap_or(PromiseError::RateLimited {
        retry_after_secs: 0,
    }))
}

// ---------------------------------------------------------------------------
// Part 5, piece 5: backpressure.
//
// Retry and backoff protect the dependency from US. Backpressure protects US
// from our CLIENTS. When agents submit faster than the engine drains, an
// unbounded queue just trades a fast failure for a slow one: memory climbs
// until the process dies, taking every in-flight promise with it. A BOUNDED
// queue refuses the overflow with a typed error, so the caller learns "slow
// down" immediately and the engine stays alive. The resilience-vs-throughput
// tradeoff is right here: a smaller capacity sheds load sooner but rejects more
// bursts; a larger one absorbs bursts but risks a bigger blast radius.
// ---------------------------------------------------------------------------

/// A fixed-capacity FIFO queue for pending promises.
///
/// This is the backpressure primitive. `enqueue` returns a typed
/// [`PromiseError::QueueFull`] once the queue is at capacity instead of growing
/// without bound. The engine (built for real in Parts 6 through 8) drains it
/// with `dequeue`.
///
/// It is a thin wrapper over `VecDeque` on purpose: the capacity check is the
/// whole point, and burying it in something cleverer would hide the one line
/// that makes this a backpressure mechanism rather than a plain queue.
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

    /// Try to add a promise. Returns the promise back on success so the caller
    /// still has its id, or [`PromiseError::QueueFull`] if the queue is at
    /// capacity. Returning the id (rather than `()`) is what lets the CLI print
    /// "queued: <id>" only for work that was actually accepted.
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Part 4 carryover tests (failure is a value) --------------------

    #[test]
    fn a_new_promise_starts_pending() {
        let promise = Promise::submit(AgentId::new("researcher"), "summarize the filings").unwrap();
        assert_eq!(promise.state(), &PromiseState::Pending);
        assert_eq!(promise.agent().as_str(), "researcher");
        assert_eq!(promise.task(), "summarize the filings");
    }

    #[test]
    fn submit_rejects_a_blank_task_with_a_typed_error() {
        let err = Promise::submit(AgentId::new("researcher"), "   ").unwrap_err();
        assert_eq!(err, PromiseError::InvalidTask(InvalidTask::Empty));
        assert!(!err.is_retryable());
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

    // ---- Retryability: the typed decision -------------------------------

    #[test]
    fn invalid_task_and_full_queue_are_not_retryable() {
        assert!(!PromiseError::InvalidTask(InvalidTask::Empty).is_retryable());
        assert!(!PromiseError::QueueFull { capacity: 8 }.is_retryable());
    }

    #[test]
    fn timeout_rate_limit_and_tool_failure_are_retryable() {
        assert!(PromiseError::AgentTimeout {
            agent: AgentId::new("a"),
            waited_secs: 1
        }
        .is_retryable());
        assert!(PromiseError::RateLimited {
            retry_after_secs: 5
        }
        .is_retryable());
        assert!(PromiseError::tool_failed("web_fetch", "timeout").is_retryable());
    }

    #[test]
    fn only_rate_limited_surfaces_a_server_retry_after() {
        let rate = PromiseError::RateLimited {
            retry_after_secs: 12,
        };
        assert_eq!(rate.server_retry_after(), Some(Duration::from_secs(12)));
        assert_eq!(
            PromiseError::AgentTimeout {
                agent: AgentId::new("a"),
                waited_secs: 9
            }
            .server_retry_after(),
            None
        );
    }

    // ---- Backoff math ---------------------------------------------------

    #[test]
    fn fixed_backoff_is_the_same_every_attempt() {
        let b = BackoffStrategy::Fixed {
            delay: Duration::from_secs(2),
        };
        assert_eq!(b.nominal_delay(1), Duration::from_secs(2));
        assert_eq!(b.nominal_delay(5), Duration::from_secs(2));
    }

    #[test]
    fn exponential_backoff_doubles_and_caps() {
        let b = BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(10));
        assert_eq!(b.nominal_delay(1), Duration::from_secs(1)); // 1 * 2^0
        assert_eq!(b.nominal_delay(2), Duration::from_secs(2)); // 1 * 2^1
        assert_eq!(b.nominal_delay(3), Duration::from_secs(4)); // 1 * 2^2
        assert_eq!(b.nominal_delay(4), Duration::from_secs(8)); // 1 * 2^3
        assert_eq!(b.nominal_delay(5), Duration::from_secs(10)); // capped from 16
        assert_eq!(b.nominal_delay(50), Duration::from_secs(10)); // still capped, no overflow
    }

    // ---- Jitter: the herd-breaker ---------------------------------------

    #[test]
    fn no_jitter_returns_the_nominal_delay_unchanged() {
        let mut rng = Rng::new(1);
        let d = Jitter::None.apply(Duration::from_secs(4), &mut rng);
        assert_eq!(d, Duration::from_secs(4));
    }

    #[test]
    fn full_jitter_stays_within_zero_and_nominal() {
        let mut rng = Rng::new(42);
        let nominal = Duration::from_secs(8);
        for _ in 0..1000 {
            let d = Jitter::Full.apply(nominal, &mut rng);
            assert!(d <= nominal, "full jitter exceeded nominal: {d:?}");
        }
    }

    #[test]
    fn equal_jitter_keeps_a_floor_of_half_nominal() {
        let mut rng = Rng::new(7);
        let nominal = Duration::from_secs(8);
        let half = Duration::from_secs(4);
        for _ in 0..1000 {
            let d = Jitter::Equal.apply(nominal, &mut rng);
            assert!(d >= half, "equal jitter dropped below half: {d:?}");
            assert!(d <= nominal, "equal jitter exceeded nominal: {d:?}");
        }
    }

    #[test]
    fn two_clients_with_different_seeds_do_not_retry_in_lockstep() {
        // The whole point of jitter: same strategy, same nominal delay, but the
        // actual delays differ, so the herd desynchronizes.
        let mut rng_a = Rng::new(1);
        let mut rng_b = Rng::new(2);
        let nominal = Duration::from_secs(10);
        let a = Jitter::Full.apply(nominal, &mut rng_a);
        let b = Jitter::Full.apply(nominal, &mut rng_b);
        assert_ne!(a, b, "two seeds produced identical jitter; herd not broken");
    }

    // ---- The retry driver -----------------------------------------------

    #[test]
    fn retry_returns_immediately_on_first_success() {
        let policy = RetryPolicy::default_for_agents();
        let mut rng = Rng::new(0);
        let mut slept: Vec<Duration> = Vec::new();

        let result: RetryResult<&str> =
            run_with_retry(&policy, &mut rng, |_attempt| Ok("done"), |d| slept.push(d));

        assert_eq!(result.unwrap(), "done");
        assert!(slept.is_empty(), "a first-try success must not sleep");
    }

    #[test]
    fn retry_stops_immediately_on_a_non_retryable_error() {
        let policy = RetryPolicy::default_for_agents();
        let mut rng = Rng::new(0);
        let mut slept: Vec<Duration> = Vec::new();
        let mut calls = 0;

        let result: RetryResult<&str> = run_with_retry(
            &policy,
            &mut rng,
            |_attempt| {
                calls += 1;
                Err(PromiseError::InvalidTask(InvalidTask::Empty))
            },
            |d| slept.push(d),
        );

        assert!(result.is_err());
        assert_eq!(calls, 1, "a non-retryable error must not be retried");
        assert!(slept.is_empty(), "a non-retryable error must not sleep");
    }

    #[test]
    fn retry_succeeds_on_the_third_attempt() {
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(30)),
            jitter: Jitter::None, // deterministic for the assertion
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
        assert_eq!(calls, 3);
        // Two failures => two sleeps, at 1s and 2s (exponential, no jitter).
        assert_eq!(slept, vec![Duration::from_secs(1), Duration::from_secs(2)]);
    }

    #[test]
    fn retry_exhausts_attempts_and_returns_the_last_error() {
        let policy = RetryPolicy {
            max_attempts: 3,
            backoff: BackoffStrategy::Fixed {
                delay: Duration::from_millis(5),
            },
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
                Err(PromiseError::AgentTimeout {
                    agent: AgentId::new("slowpoke"),
                    waited_secs: 30,
                })
            },
            |d| slept.push(d),
        );

        assert!(result.is_err());
        assert_eq!(calls, 3, "three attempts total");
        // Three attempts => only two sleeps; we never sleep after the last.
        assert_eq!(slept.len(), 2, "must not sleep after the final attempt");
    }

    #[test]
    fn retry_honors_a_server_retry_after_over_computed_backoff() {
        let policy = RetryPolicy {
            max_attempts: 2,
            backoff: BackoffStrategy::exponential(Duration::from_secs(1), Duration::from_secs(30)),
            jitter: Jitter::None,
        };
        let mut rng = Rng::new(0);
        let mut slept: Vec<Duration> = Vec::new();
        let mut calls = 0;

        let _result: RetryResult<&str> = run_with_retry(
            &policy,
            &mut rng,
            |_attempt| {
                calls += 1;
                Err(PromiseError::RateLimited {
                    retry_after_secs: 12,
                })
            },
            |d| slept.push(d),
        );

        // The computed exponential delay for the first retry would be 1s, but
        // the server said 12s, and the server wins.
        assert_eq!(slept, vec![Duration::from_secs(12)]);
    }

    // ---- Backpressure ---------------------------------------------------

    #[test]
    fn a_bounded_queue_accepts_up_to_capacity() {
        let mut q = BoundedQueue::new(2);
        let p1 = Promise::submit(AgentId::new("a"), "task one").unwrap();
        let p2 = Promise::submit(AgentId::new("b"), "task two").unwrap();
        assert!(q.enqueue(p1).is_ok());
        assert!(q.enqueue(p2).is_ok());
        assert_eq!(q.len(), 2);
        assert!(q.is_full());
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
        // The refusal message names the capacity, never a task.
        let rendered = err.to_string();
        assert!(rendered.contains("capacity 1"));
    }

    #[test]
    fn draining_the_queue_makes_room_again() {
        let mut q = BoundedQueue::new(1);
        q.enqueue(Promise::submit(AgentId::new("a"), "task one").unwrap())
            .unwrap();
        assert!(q.is_full());
        let drained = q.dequeue().expect("one promise was queued");
        assert_eq!(drained.agent().as_str(), "a");
        assert!(q.is_empty());
        // Room again: the next enqueue succeeds.
        assert!(q
            .enqueue(Promise::submit(AgentId::new("b"), "task two").unwrap())
            .is_ok());
    }

    #[test]
    fn queue_full_error_never_leaks_the_task() {
        let mut q = BoundedQueue::new(1);
        q.enqueue(Promise::submit(AgentId::new("a"), "first task").unwrap())
            .unwrap();
        let err = q
            .enqueue(Promise::submit(AgentId::new("b"), "secret task text").unwrap())
            .unwrap_err();
        assert!(!err.to_string().contains("secret task text"));
    }
}
