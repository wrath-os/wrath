//! Machine values: facts and given values, filled in on the machine when it switches.

use super::{err, ok, why};
use crate::{RuntimePart, Value};

const SYSTEM: (&str, &str) = ("system.toml", "[system]\nhostname = \"box\"\n");

#[test]
fn machine_values_are_filled_in_on_the_machine() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "services/motd.toml",
            "[system]\nmotd = \"${machine.name} has ${facts.cpu.count} CPUs\"\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("system.motd"),
        Some(&Value::Runtime(vec![
            RuntimePart::Machine("machine.name".into()),
            RuntimePart::Text(" has ".into()),
            RuntimePart::Machine("facts.cpu.count".into()),
            RuntimePart::Text(" CPUs".into()),
        ]))
    );
    let used: Vec<String> = evaluation
        .machine_values_used()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(used, ["facts.cpu.count", "machine.name"]);
    insta::assert_snapshot!(why(&evaluation, "system.motd"), @r#"
    system.motd = { runtime = "${machine.name} has ${facts.cpu.count} CPUs" }
      set at services/motd.toml:2:8
      uses machine.name, filled in on the machine
      uses facts.cpu.count, filled in on the machine
    "#);
}

#[test]
fn error_machine_value_in_an_option_fixed_at_build() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        ("services/web.toml", "[services.nginx.main]\nworkers = \"${facts.cpu.count}\"\n"),
    ]), @r#"
    error: invalid value for `services.nginx.main.workers`
     --> services/web.toml:2:11
      |
    2 | workers = "${facts.cpu.count}"
      |           ^^^^^^^^^^^^^^^^^^^^ `services.nginx.main.workers` is fixed when the system is built, so it cannot hold a value only known at service start
    "#);
}

#[test]
fn error_unknown_machine_values() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        ("services/motd.toml", "[system]\nmotd = \"${facts.cpu.cuont} ${machine.zone}\"\n"),
    ]), @r#"
    error: unknown machine value `facts.cpu.cuont`
     --> services/motd.toml:2:9
      |
    2 | motd = "${facts.cpu.cuont} ${machine.zone}"
      |         ^^^^^^^^^^^^^^^^^^ referenced here
      = help: did you mean `facts.cpu.count`?

    error: unknown machine value `machine.zone`
     --> services/motd.toml:2:28
      |
    2 | motd = "${facts.cpu.cuont} ${machine.zone}"
      |                            ^^^^^^^^^^^^^^^ referenced here
      = help: did you mean `machine.name`?
    "#);
}
