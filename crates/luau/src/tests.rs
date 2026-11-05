use std::collections::BTreeMap;
use std::path::Path;

use distro_eval::{
    DirFs, Input, MemFs, Module, OptionPath, OptionSpec, Schema, Sources, Type, Value,
    describe_outputs, evaluate, explain,
};

use super::load;

const MODULES: [&str; 6] = [
    "firewall",
    "fprintd",
    "pam",
    "pam-base",
    "syncthing",
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

fn load_modules(sources: &mut Sources) -> Vec<Box<dyn Module>> {
    MODULES
        .iter()
        .map(|name| {
            let path = format!("modules/luau/{name}.luau");
            let text = std::fs::read_to_string(repo().join(&path)).unwrap();
            load(sources, &path, text).unwrap_or_else(|e| panic!("{}", e.message))
        })
        .collect()
}

/// Loads one module from source and evaluates it against a minimal config.
fn run(source: &str) -> Result<distro_eval::Evaluation, String> {
    let mut sources = Sources::default();
    let module = load(&mut sources, "test.luau", source.to_owned())
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

fn load_error(source: &str) -> String {
    let mut sources = Sources::default();
    match load(&mut sources, "test.luau", source.to_owned()) {
        Ok(_) => panic!("the module loaded"),
        Err(e) => distro_eval::render(&[e], &sources),
    }
}

#[test]
fn the_s4_modules_render_the_fixture() {
    let mut sources = Sources::default();
    let modules = load_modules(&mut sources);
    let evaluation = evaluate(Input {
        sources,
        // Relative, so locations in snapshots don't depend on where the repository is.
        config: &DirFs::new("../../fixtures/configs/s4"),
        schema: base_schema(),
        modules: &modules,
        facts: &Value::Table(BTreeMap::new()),
    })
    .unwrap_or_else(|e| panic!("{e}"));

    insta::assert_snapshot!("s4_outputs", describe_outputs(&evaluation));
    let why = |path: &str| explain(&evaluation, Some(&OptionPath::parse(path).unwrap())).unwrap();
    insta::assert_snapshot!(why("security.pam.services.sudo.auth"), @r#"
    security.pam.services.sudo.auth = [{ control = "required", module = "pam_env.so", name = "env" }, { control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }, { args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }]
      set by module `fprintd` at modules/luau/fprintd.luau:21:4: [{ after = "env", before = "unix", control = "sufficient", module = "pam_fprintd.so", name = "fprintd" }]
      set by module `pam-base` at modules/luau/pam-base.luau:36:4: [{ control = "required", module = "pam_env.so", name = "env" }, { after = "env", args = ["likeauth", "try_first_pass"], control = "sufficient", module = "pam_unix.so", name = "unix" }, { after = "unix", control = "required", module = "pam_deny.so", name = "deny" }]
      overrides [], default
      removes ["deny"], set at ../../fixtures/configs/s4/system.toml:9:8
    "#);
}

#[test]
fn pairs_iterates_in_sorted_order() {
    let evaluation = run(r#"
return {
    name = "order",
    options = {},
    writes = {},
    render = function(ctx)
        local t = {}
        t.zeta = 1
        t.alpha = 2
        t[10] = 3
        t[2] = 4
        local keys = {}
        for k in pairs(t) do
            table.insert(keys, tostring(k))
        end
        ctx.file("/keys", table.concat(keys, " "))
    end,
}
"#)
    .unwrap();

    assert_eq!(
        evaluation.modules()[0].output.files["/keys"].text,
        "2 10 alpha zeta"
    );
}

#[test]
fn addresses_time_and_randomness_are_not_available() {
    for (expression, error) in [
        ("tostring({})", "tostring cannot show a table"),
        (
            "string.format('%s', ipairs)",
            "string.format cannot show a function",
        ),
        (
            "string.format('%s', {})",
            "string.format cannot show a table",
        ),
        ("os.time()", "attempt to index nil with 'time'"),
        ("math.random()", "attempt to call a nil value"),
        ("next({})", "attempt to call a nil value"),
    ] {
        let source = format!(
            "return {{ name = 'x', options = {{}}, writes = {{}}, render = function(ctx)\n local _ = {expression}\nend }}"
        );
        let message = run(&source).unwrap_err();
        assert!(message.contains(error), "{expression}: {message}");
    }
}

#[test]
fn direct_table_iteration_is_rejected_at_load() {
    insta::assert_snapshot!(load_error("local t = {}\nfor k, v in t do end\nreturn {}\n"), @"
    error: iteration order is not defined here
     --> test.luau:2:13
      |
    2 | for k, v in t do end
      |             ^ iterate with `pairs(...)` or `ipairs(...)`
      = help: `pairs` visits keys in sorted order
    ");
}

#[test]
fn values_read_are_read_only() {
    let message = run(r#"
return {
    name = "mutate",
    options = {},
    writes = {},
    render = function(ctx)
        local system = ctx.read("system")
        system.hostname = "other"
    end,
}
"#)
    .unwrap_err();

    insta::assert_snapshot!(message, @"
    error: attempt to modify a readonly table
     --> test.luau:8:9
      |
    8 |         system.hostname = \"other\"
      |         ^^^^^^^^^^^^^^^^^^^^^^^^^ here
    ");
}

#[test]
fn errors_point_at_the_module_line() {
    insta::assert_snapshot!(run(r#"
return {
    name = "typo",
    options = {},
    writes = {},
    render = function(ctx)
        ctx.read("system.hostnam")
    end,
}
"#).unwrap_err(), @"
    error: unknown option `system.hostnam`; did you mean `system.hostname`?
     --> test.luau:7:9
      |
    7 |         ctx.read(\"system.hostnam\")
      |         ^^^^^^^^^^^^^^^^^^^^^^^^^^ here
    ");
}

#[test]
fn a_module_that_runs_too_long_is_stopped() {
    let message = run(
        "return { name = 'spin', options = {}, writes = {}, render = function(ctx)\n while true do end\nend }",
    )
    .unwrap_err();

    assert!(message.contains("ran for longer than"), "{message}");
}

#[test]
fn module_tables_are_checked() {
    assert!(
        load_error("return { name = 'x', render = function() end, extra = 1 }")
            .contains("unknown key `extra` in the module table")
    );
    assert!(
        load_error("return { options = {}, render = function() end }")
            .contains("the module table needs a `name`")
    );
}
