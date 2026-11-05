//! Rendering scenario outcomes as plain text.

use std::fmt::Write;

use crate::runner::Outcome;
use crate::scenario::{Milestone, Scenario};

/// Renders results grouped by milestone, in the order given, with a total at the end.
/// Milestones without results are left out.
pub fn render(milestones: &[Milestone], results: &[(&Scenario, Outcome)]) -> String {
    let width = id_width(results);
    let mut out = String::new();

    for milestone in milestones {
        let group: Vec<_> = results
            .iter()
            .filter(|(s, _)| s.milestone == milestone.id)
            .collect();
        if group.is_empty() {
            continue;
        }
        let passing = group.iter().filter(|(_, o)| o.is_pass()).count();
        let _ = writeln!(
            out,
            "{}  {}  {passing}/{}",
            milestone.id.to_uppercase(),
            milestone.title,
            group.len()
        );
        for (scenario, outcome) in group {
            write_entry(&mut out, scenario, outcome, width);
        }
        out.push('\n');
    }

    let passing = results.iter().filter(|(_, o)| o.is_pass()).count();
    let _ = writeln!(out, "{passing}/{} scenarios passing", results.len());
    out
}

/// Renders results as a flat list, one entry per scenario.
pub fn render_list(results: &[(&Scenario, Outcome)]) -> String {
    let width = id_width(results);
    let mut out = String::new();
    for (scenario, outcome) in results {
        write_entry(&mut out, scenario, outcome, width);
    }
    out
}

fn id_width(results: &[(&Scenario, Outcome)]) -> usize {
    results.iter().map(|(s, _)| s.id.len()).max().unwrap_or(0)
}

fn write_entry(out: &mut String, scenario: &Scenario, outcome: &Outcome, width: usize) {
    let label = match outcome {
        Outcome::Pass => "PASS",
        Outcome::Fail(_) => "FAIL",
        Outcome::Pending(_) => "TODO",
    };
    let _ = writeln!(out, "  {label}  {:width$}  {}", scenario.id, scenario.title);

    let mut detail = |text: &str| {
        let _ = writeln!(out, "        {:width$}  {text}", "");
    };
    match outcome {
        Outcome::Pass => {}
        Outcome::Pending(reason) => detail(reason),
        Outcome::Fail(failure) => {
            detail(&failure.reason);
            if let Some(logs) = &failure.logs {
                detail(&format!("logs: {}", logs.display()));
            }
            if let Some(reproduction) = &failure.reproduction {
                detail(&format!("reproduce: {}", reproduction.command));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::runner::Failure;
    use crate::scenario::Runner;
    use crate::vm::Reproduction;

    fn milestone(id: &str, title: &str) -> Milestone {
        Milestone {
            id: id.into(),
            title: title.into(),
        }
    }

    fn scenario(milestone: &str, id: &str, title: &str) -> Scenario {
        Scenario {
            id: id.into(),
            milestone: milestone.into(),
            title: title.into(),
            runner: Runner::Pending {
                reason: "later".into(),
            },
            source: PathBuf::from(format!("acceptance/{milestone}/{id}.toml")),
        }
    }

    #[test]
    fn renders_grouped_scoreboard() {
        let milestones = [
            milestone("m1", "Harness"),
            milestone("m2", "Compiler core"),
            milestone("m3", "Nothing yet"),
        ];
        let boots = scenario("m1", "harness-boots", "A NixOS image boots");
        let fails = scenario("m1", "failure-reports", "A failure is reported");
        let plan = scenario("m2", "plan", "Plan shows the diff");
        let results = [
            (&boots, Outcome::Pass),
            (
                &fails,
                Outcome::Fail("exit code 2, expected 1: error: unrecognized subcommand".into()),
            ),
            (&plan, Outcome::Pending("no `vm` runner yet".into())),
        ];

        insta::assert_snapshot!(render(&milestones, &results));
    }

    #[test]
    fn renders_failure_with_logs_and_reproduction() {
        let vm = scenario("m1", "vm-boots", "A VM boots");
        let failure = Failure {
            reason: "unit `sshd.service` is not active (state: failed)".into(),
            logs: Some(PathBuf::from("target/distro-check/runs/m1-vm-boots")),
            reproduction: Some(Reproduction {
                image: PathBuf::from("/nix/store/abc-fixture"),
                command: "distro check acceptance/m1/vm-boots.toml --image /nix/store/abc-fixture"
                    .into(),
            }),
        };

        insta::assert_snapshot!(render_list(&[(&vm, Outcome::Fail(failure))]));
    }

    #[test]
    fn renders_empty_suite() {
        assert_eq!(render(&[], &[]), "0/0 scenarios passing\n");
    }
}
