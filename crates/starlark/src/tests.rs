use std::collections::BTreeMap;
use std::path::Path;

use distro_eval::{
    DirFs, Evaluation, Input, MemFs, Module, OptionPath, OptionSpec, Schema, Sources, Type, Value,
    describe_outputs, evaluate, explain,
};

use super::load;

const MODULES: [&str; 8] = [
    "firewall",
    "fprintd",
    "nginx",
    "pam",
    "pam-base",
    "syncthing",
    "system",
    "users",
];

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

fn base_schema() -> Schema {
    let mut schema = Schema::default();
    schema
        .declare("system.hostname", OptionSpec::new(Type::Str))
        .unwrap();
    schema
}

/// Evaluates the S4 fixture with the built-in modules.
fn evaluate_fixture() -> Evaluation {
    let mut sources = Sources::default();
    let modules: Vec<Box<dyn Module>> = MODULES
        .iter()
        .map(|name| {
            let path = format!("modules/{name}.star");
            let text = std::fs::read_to_string(repo().join(&path)).unwrap();
            load(&mut sources, &path, text)
                .unwrap_or_else(|e| panic!("{}", distro_eval::render(&[e], &sources)))
        })
        .collect();
    evaluate(Input {
        sources,
        // Relative, so locations in snapshots don't depend on where the repository is.
        config: &DirFs::new("../../fixtures/configs/s4"),
        schema: Schema::default(),
        modules: &modules,
        facts: &Value::Table(BTreeMap::new()),
    })
    .unwrap_or_else(|e| panic!("{e}"))
}

fn run(source: &str) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let module = load(&mut sources, "test.star", source.to_owned())
        .map_err(|e| distro_eval::render(&[e], &sources))?;
    evaluate(Input {
        sources,
        config: &MemFs::new([("system.toml", "[system]\nhostname = \"box\"\n")]),
        schema: base_schema(),
        modules: &[module],
        facts: &Value::Table(BTreeMap::new()),
    })
    .map_err(|e| e.to_string())
}

#[test]
fn the_built_in_modules_render_the_fixture() {
    let evaluation = evaluate_fixture();

    insta::assert_snapshot!("s4_outputs", describe_outputs(&evaluation));
    let why = |path: &str| explain(&evaluation, Some(&OptionPath::parse(path).unwrap())).unwrap();
    insta::assert_snapshot!(why("security.pam.services.sudo.auth"), @r#"
    security.pam.services.sudo.auth = [{ control = "required", module = "pam_env.so", name = "env" }, { control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }, { args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }]
      set by module `fprintd` at modules/fprintd.star:14:9: [{ after = "env", before = "unix", control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }]
      set by module `pam-base` at modules/pam-base.star:31:9: [{ control = "required", module = "pam_env.so", name = "env" }, { after = "env", args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }, { after = "unix", control = "required", module = "pam_deny.so", name = "deny" }]
      overrides [], default
      removes ["deny"], set at ../../fixtures/configs/s4/system.toml:9:8
    "#);
}

#[test]
fn settings_point_at_the_exact_call() {
    let evaluation = evaluate_fixture();

    insta::assert_snapshot!(
        explain(&evaluation, Some(&OptionPath::parse("networking.firewall.allowed_tcp_ports").unwrap())).unwrap(),
        @"
    networking.firewall.allowed_tcp_ports = [80, 22000]
      set by module `nginx` at modules/nginx.star:60:9: [80]
      set by module `syncthing` at modules/syncthing.star:44:9: [22000]
      overrides [], default
    "
    );
}

#[test]
fn errors_point_at_the_expression() {
    insta::assert_snapshot!(run(r#"
def render(ctx):
    ctx.read("system.hostnam")

module(name = "typo", options = {}, writes = [], render = render)
"#).unwrap_err(), @r#"
    error: unknown option `system.hostnam`; did you mean `system.hostname`?
     --> test.star:3:5
      |
    3 |     ctx.read("system.hostnam")
      |     ^^^^^^^^^^^^^^^^^^^^^^^^^^ here
    "#);
}

#[test]
fn values_read_are_copies() {
    let evaluation = run(r#"
def render(ctx):
    system = ctx.read("system")
    system["hostname"] = "other"
    ctx.file("/hostname", ctx.read("system.hostname"))

module(name = "mutate", options = {}, writes = [], render = render)
"#)
    .unwrap();

    assert_eq!(
        evaluation.modules()[0].output.files["/hostname"].text,
        "box"
    );
}

#[test]
fn a_module_that_runs_too_long_is_stopped() {
    let message = run(r#"
def render(ctx):
    for _ in range(1000000000):
        pass

module(name = "spin", options = {}, writes = [], render = render)
"#)
    .unwrap_err();

    assert!(message.contains("cancel"), "{message}");
}

#[test]
fn a_module_must_declare_itself() {
    let message = run("x = 1\n").unwrap_err();

    assert!(message.contains("never calls `module(...)`"), "{message}");
}

#[test]
fn types_and_options_are_checked() {
    let message = run(r#"
module(name = "x", options = {"a": types.list(option(types.str))}, writes = [], render = None)
"#)
    .unwrap_err();

    assert!(message.contains("expected a type"), "{message}");
}
