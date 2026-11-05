//! Acceptance scenarios and the progress scoreboard.
//!
//! Milestones are defined by executable scenarios that start out failing. `distro status` loads
//! them, runs what can be run and prints how many pass. `distro check` runs individual scenario
//! files.

mod runner;
mod scenario;
mod scoreboard;
mod vm;

pub use runner::{Failure, Outcome, RunContext, run};
pub use scenario::{
    CommandSpec, LoadError, ManualSpec, Milestone, Runner, SIGNOFF_DIR, Scenario, Suite,
    parse_scenario,
};
pub use scoreboard::{render, render_list};
pub use vm::{Expect, Firmware, Reproduction, VmSpec, WaitFor};
