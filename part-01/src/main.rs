//! Part 1 taste: see a promise move through its lifecycle.
//!
//! This is a throwaway toy, not PromiseKeeper proper. The real project
//! (cargo workspace, the `pk` CLI, real types) begins in Part 3. The point
//! here is simply to end Part 1 with something that runs: a promise moving
//! from Pending to Running to Resolved, with the result carried in the type.
//!
//! By Part 9 this grows into a state machine where a resolved promise
//! cannot exist without a result, and a rejected one cannot exist without
//! an error, enforced by the compiler.

#[derive(Debug)]
enum PromiseState {
    Pending,
    Running,
    Resolved(String),
}

fn main() {
    let mut state = PromiseState::Pending;
    println!("{state:?}"); // Pending
    state = PromiseState::Running;
    println!("{state:?}"); // Running
    state = PromiseState::Resolved("summary complete".into());
    println!("{state:?}"); // Resolved("summary complete")

    // The whole reason `Resolved` carries data: we can read the result,
    // and the compiler guarantees it's only there when the promise resolved.
    if let PromiseState::Resolved(result) = &state {
        println!("result: {result}");
    }
}
