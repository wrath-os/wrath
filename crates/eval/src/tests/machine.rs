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
      = help: declare it in `[machine.values]`, so each machine is given it
    "#);
}

const ZONE: (&str, &str) = (
    "system.toml",
    "[system]\nhostname = \"box\"\n\n[machine.values]\nzone = { type = \"string\" }\n",
);

#[test]
fn declared_machine_values_are_filled_in_on_the_machine() {
    let evaluation = ok(&[
        ZONE,
        (
            "services/motd.toml",
            "[system]\nmotd = \"in ${machine.zone}\"\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("system.motd"),
        Some(&Value::Runtime(vec![
            RuntimePart::Text("in ".into()),
            RuntimePart::Machine("machine.zone".into()),
        ]))
    );
    let declared: Vec<&str> = evaluation
        .machine_values_declared()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(declared, ["zone"]);
    insta::assert_snapshot!(why(&evaluation, "system.motd"), @r#"
    system.motd = { runtime = "in ${machine.zone}" }
      set at services/motd.toml:2:8
      uses machine.zone, filled in on the machine
    "#);
}

#[test]
fn machine_values_declared_in_an_included_file_count() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [\"common.toml\"]\n\n[system]\nmotd = \"${machine.zone}\"\n",
        ),
        (
            "common.toml",
            "[system]\nhostname = \"box\"\n\n[machine.values]\nzone = { type = \"string\" }\n",
        ),
    ]);

    assert!(evaluation.machine_values_declared().contains_key("zone"));
}

#[test]
fn error_bad_machine_value_declarations() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = \"box\"\n\n[machine]\nvalue = {}\n\n[machine.values]\n\
         zone = { type = \"int\" }\nrack = \"string\"\nrow = { type = \"string\", doc = 1 }\n",
    )]), @r#"
    error: unexpected `value` in `[machine]`
     --> system.toml:5:1
      |
    5 | value = {}
      | ^^^^^ only `values` and `requires` belong here

    error: a machine value is declared with its type
     --> system.toml:9:8
      |
    9 | rack = "string"
      |        ^^^^^^^^ expected `{ type = "string" }`

    error: a machine value's `doc` is a string
      --> system.toml:10:32
       |
    10 | row = { type = "string", doc = 1 }
       |                                ^ expected a string

    error: machine values are strings
     --> system.toml:8:17
      |
    8 | zone = { type = "int" }
      |                 ^^^^^ expected `"string"`
    "#);
}

#[test]
fn a_configuration_states_what_a_machine_needs() {
    let evaluation = ok(&[(
        "system.toml",
        "[system]\nhostname = \"box\"\n\n[machine.requires]\ngpu = \"nvidia\"\n\
         arch = { within = [\"x86_64\", \"aarch64\"] }\nmemory_gb = { at_least = 64 }\n\
         cpu.count = { at_least = 8 }\n",
    )]);

    let requirements: Vec<String> = evaluation
        .requirements()
        .iter()
        .map(|requirement| {
            format!(
                "{}: {} ({})",
                requirement.fact,
                requirement.check,
                evaluation.sources().location(requirement.at)
            )
        })
        .collect();
    assert_eq!(
        requirements,
        [
            "arch: one of x86_64, aarch64 (system.toml:6:1)",
            "cpu.count: at least 8 (system.toml:8:5)",
            "gpu: nvidia (system.toml:5:1)",
            "memory_gb: at least 64 (system.toml:7:1)",
        ]
    );
}

#[test]
fn error_bad_requirements() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = \"box\"\n\n[machine.requires]\ngpu = { at_least = 1 }\n\
         memory_gb = \"lots\"\ncpu.cuont = 4\ndisk = 1\nmem = { at_least = 1 }\narch = { within = [1] }\n\
         \"cpu.count\" = { at_least = 1, most = 2 }\n",
    )]), @r#"
    error: `arch` is a name
      --> system.toml:10:20
       |
    10 | arch = { within = [1] }
       |                    ^ expected a string

    error: unknown fact `cpu.cuont`
     --> system.toml:7:5
      |
    7 | cpu.cuont = 4
      |     ^^^^^ required here
      = help: did you mean `cpu.count`?

    error: unexpected `most` in a requirement on `cpu.count`
      --> system.toml:11:31
       |
    11 | "cpu.count" = { at_least = 1, most = 2 }
       |                               ^^^^ only `at_least` belongs here

    error: unknown fact `disk`
     --> system.toml:8:1
      |
    8 | disk = 1
      | ^^^^ required here
      = a machine can be required to have arch, cpu.count, gpu, memory_gb

    error: unexpected `at_least` in a requirement on `gpu`
     --> system.toml:5:9
      |
    5 | gpu = { at_least = 1 }
      |         ^^^^^^^^ only `within` belongs here

    error: unknown fact `mem`
     --> system.toml:9:1
      |
    9 | mem = { at_least = 1 }
      | ^^^ required here
      = a machine can be required to have arch, cpu.count, gpu, memory_gb

    error: `memory_gb` is required as a number, or `{ at_least = N }`
     --> system.toml:6:13
      |
    6 | memory_gb = "lots"
      |             ^^^^^^ expected a number, or `{ at_least = N }`
    "#);
}
