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

type Loader = fn(&mut Sources, &str, String) -> Result<Box<dyn Module>, distro_eval::Diagnostic>;

/// Evaluates the S4 fixture with the modules of one language.
fn evaluate_fixture(dir: &str, extension: &str, loader: Loader) -> Evaluation {
    let mut sources = Sources::default();
    let modules: Vec<Box<dyn Module>> = MODULES
        .iter()
        .map(|name| {
            let path = format!("modules/{dir}/{name}.{extension}");
            let text = std::fs::read_to_string(repo().join(&path)).unwrap();
            loader(&mut sources, &path, text)
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
    let module = load(&mut sources, "test.ts", source.to_owned())
        .map_err(|e| distro_eval::render(&[e], &sources))?;
    let mut schema = Schema::default();
    schema
        .declare("system.hostname", OptionSpec::new(Type::Str))
        .unwrap();
    evaluate(Input {
        sources,
        config: &MemFs::new([("system.toml", "[system]\nhostname = \"box\"\n")]),
        schema,
        modules: &[module],
        facts: &Value::Table(BTreeMap::new()),
    })
    .map_err(|e| e.to_string())
}

#[test]
fn typescript_and_luau_modules_render_the_same_system() {
    let typescript = evaluate_fixture("typescript", "ts", load);
    let luau = evaluate_fixture("luau", "luau", distro_luau::load);

    assert_eq!(describe_outputs(&typescript), describe_outputs(&luau));
    assert_eq!(
        typescript.options().keys().collect::<Vec<_>>(),
        luau.options().keys().collect::<Vec<_>>()
    );
    for (path, resolved) in typescript.options() {
        assert_eq!(resolved.value, luau.options()[path].value, "{path}");
    }
}

#[test]
fn settings_point_at_the_call() {
    let evaluation = evaluate_fixture("typescript", "ts", load);

    insta::assert_snapshot!(
        explain(&evaluation, Some(&OptionPath::parse("networking.firewall.allowed_tcp_ports").unwrap())).unwrap(),
        @"
    networking.firewall.allowed_tcp_ports = [80, 22000]
      set by module `nginx` at modules/typescript/nginx.ts:49:4: [80]
      set by module `syncthing` at modules/typescript/syncthing.ts:48:4: [22000]
      overrides [], default
    "
    );
}

#[test]
fn errors_point_at_the_module_line() {
    insta::assert_snapshot!(run(r#"
export default module({
	name: "typo",
	options: {},
	writes: [],
	render(ctx: Ctx) {
		ctx.read("system.hostnam");
	},
});
"#).unwrap_err(), @r#"
    error: unknown option `system.hostnam`; did you mean `system.hostname`?
     --> test.ts:7:3
      |
    7 |         ctx.read("system.hostnam");
      |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^ here
    "#);
}

#[test]
fn type_syntax_errors_point_at_the_line() {
    let mut sources = Sources::default();
    let error = match load(
        &mut sources,
        "test.ts",
        "const x: = 1;\nexport default x;\n".to_owned(),
    ) {
        Ok(_) => panic!("the module loaded"),
        Err(e) => distro_eval::render(&[e], &sources),
    };

    insta::assert_snapshot!(error, @"
    error: Unexpected token `=`. Expected an identifier, void, yield, null, await, break, a string literal, a numeric literal, true, false, `, -, import, this, typeof, {, [, (
     --> test.ts:1:1
      |
    1 | const x: = 1;
      | ^^^^^^^^^^^^^ here
    ");
}

#[test]
fn time_and_randomness_are_not_available() {
    for (expression, error) in [
        ("Date.now()", "Date is not defined"),
        ("Math.random()", "Math.random is not available in modules"),
        ("new WeakRef({})", "WeakRef is not defined"),
    ] {
        let source = format!(
            "export default module({{ name: 'x', options: {{}}, writes: [], render() {{ {expression}; }} }});"
        );
        let message = run(&source).unwrap_err();
        assert!(message.contains(error), "{expression}: {message}");
    }
}

#[test]
fn values_read_are_frozen() {
    let message = run(r#"
export default module({
	name: "mutate",
	options: {},
	writes: [],
	render(ctx: Ctx) {
		const system = ctx.read<{ hostname: string }>("system");
		system.hostname = "other";
	},
});
"#)
    .unwrap_err();

    assert!(
        message.contains("read-only") || message.contains("not writable"),
        "{message}"
    );
}

#[test]
fn a_module_that_runs_too_long_is_stopped() {
    let message = run(
        "export default module({ name: 'spin', options: {}, writes: [], render() { while (true) {} } });",
    )
    .unwrap_err();

    assert!(message.contains("interrupted"), "{message}");
}
