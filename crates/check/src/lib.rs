//! Acceptance scenarios and the progress scoreboard.
//!
//! Milestones are defined by executable scenarios that start out failing. `distro status` loads
//! them, runs what can be run and prints how many pass.

mod runner;
mod scenario;
mod scoreboard;

pub use runner::{Outcome, RunContext, run};
pub use scenario::{
    CommandSpec, LoadError, ManualSpec, Milestone, Runner, SIGNOFF_DIR, Scenario, Suite,
    parse_scenario,
};
pub use scoreboard::render;
