//! Rendering scenario outcomes as a plain-text scoreboard.

use std::fmt::Write;

use crate::runner::Outcome;
use crate::scenario::{Milestone, Scenario};

/// Renders results grouped by milestone, in the order given, with a total at the end.
/// Milestones without results are left out.
pub fn render(milestones: &[Milestone], results: &[(&Scenario, Outcome)]) -> String {
    let id_width = results.iter().map(|(s, _)| s.id.len()).max().unwrap_or(0);
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
            let (label, detail) = match outcome {
                Outcome::Pass => ("PASS", None),
                Outcome::Fail(reason) => ("FAIL", Some(reason)),
                Outcome::Pending(reason) => ("TODO", Some(reason)),
            };
            let _ = writeln!(
                out,
                "  {label}  {:id_width$}  {}",
                scenario.id, scenario.title
            );
            if let Some(detail) = detail {
                let _ = writeln!(out, "        {:id_width$}  {detail}", "");
            }
        }
        out.push('\n');
    }

    let passing = results.iter().filter(|(_, o)| o.is_pass()).count();
    let _ = writeln!(out, "{passing}/{} scenarios passing", results.len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Runner;

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
            runner: Runner::Unsupported { kind: "vm" },
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
    fn renders_empty_suite() {
        assert_eq!(render(&[], &[]), "0/0 scenarios passing\n");
    }
}
