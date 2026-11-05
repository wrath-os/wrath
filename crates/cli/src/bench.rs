//! Measuring how long a plan takes with many modules (spike S1).
//!
//! Generates synthetic service modules, each shaped like a real one: a map of instances, a unit
//! and a file per instance, a system user and a firewall port contributed to other modules. One
//! more module iterates over every service. A generated configuration enables half of them.
//! Then it times what `plan` does after a one-line change: load the modules, evaluate the changed
//! configuration, build its model and diff it against the model before the change.

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use distro_model::{Model, plan};

use crate::system::{self, ModuleOrder};

pub struct Options<'a> {
    /// The real modules, which the synthetic ones are added to.
    pub modules: &'a Path,
    pub synthetic: usize,
    pub runs: usize,
    pub budget: Duration,
}

/// Runs the measurement and reports whether the median stayed within budget.
pub fn plan_time(options: &Options<'_>) -> Result<bool, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    let configs = work.path().join("configs");
    write_configs(&configs, options.synthetic)?;
    let modules = work.path().join("modules");
    copy_modules(options.modules, &modules)?;
    for index in 0..options.synthetic {
        write(
            &modules.join(format!("svc{index:03}.star")),
            &synthetic_module(index),
        )?;
    }
    write(&modules.join("inventory.star"), INVENTORY_MODULE)?;

    let before = system::evaluate_dir(&configs.join("before"), &modules, ModuleOrder::Sorted)
        .map(|evaluation| Model::from_evaluation(&evaluation))?;
    let mut times = Vec::with_capacity(options.runs);
    let mut changes = 0;
    let mut module_count = 0;
    for _ in 0..options.runs {
        let start = Instant::now();
        let evaluation =
            system::evaluate_dir(&configs.join("after"), &modules, ModuleOrder::Sorted)?;
        let after = Model::from_evaluation(&evaluation);
        let diff = plan(&before, &after);
        times.push(start.elapsed());
        changes = diff.to_string().lines().count();
        module_count = evaluation.modules().len();
    }
    times.sort();
    let median = times[times.len() / 2];
    let fits = median <= options.budget;
    println!(
        "{} {module_count} modules: plan after a one-line change takes {} ms \
         (median of {}, fastest {} ms, slowest {} ms); budget {} ms; plan has {changes} lines",
        if fits { "PASS" } else { "FAIL" },
        median.as_millis(),
        options.runs,
        times[0].as_millis(),
        times[times.len() - 1].as_millis(),
        options.budget.as_millis(),
    );
    Ok(fits)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn copy_modules(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if let Some(name) = path.file_name() {
            fs::copy(&path, to.join(name)).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// A configuration that enables every other synthetic service, before and after changing one
/// port.
fn write_configs(dir: &Path, synthetic: usize) -> Result<(), String> {
    let mut services = String::new();
    for index in (0..synthetic).step_by(2) {
        // Writing to a String cannot fail.
        let _ = writeln!(
            services,
            "[services.svc{index:03}.main]\nenable = true\nextra_args = [\"--verbose\"]\n\n\
             [services.svc{index:03}.main.settings]\nlog = \"info\"\nworkers = \"${{facts.cpu.count}}\"\n"
        );
    }
    for (name, port) in [("before", 8080), ("after", 8081)] {
        let config = dir.join(name);
        write(
            &config.join("system.toml"),
            "[system]\nhostname = \"bench\"\n",
        )?;
        write(&config.join("services/many.toml"), &services)?;
        write(
            &config.join("services/web.toml"),
            &format!(
                "[services.nginx.main]\nenable = true\nport = {port}\n\n[services.nginx.main.sites.www]\ndomains = [\"${{system.hostname}}.lan\"]\nroot = \"/srv/www\"\n"
            ),
        )?;
    }
    Ok(())
}

fn synthetic_module(index: usize) -> String {
    let name = format!("svc{index:03}");
    let port = 10_000 + index;
    format!(
        r#"# A synthetic service for measuring plan time.

instance = types.record(
    enable = option(types.bool, default = False),
    port = option(types.port, default = {port}),
    user = option(types.str, default = "{name}"),
    extra_args = option(types.list(types.str), default = []),
    settings = option(types.freeform, optional = True),
)

def render(ctx):
    ports = []
    for instance_name, cfg in ctx.read("services.{name}").items():
        if not cfg["enable"]:
            continue
        config = "/etc/{name}/" + instance_name + ".json"
        ctx.file(config, formats.json(cfg.get("settings", {{}})))
        ctx.unit("{name}-" + instance_name + ".service", {{
            "Unit": {{"Description": "{name} (" + instance_name + ")"}},
            "Service": {{
                "User": cfg["user"],
                "ExecStart": "{name} --config " + config + " " + " ".join(cfg["extra_args"]),
            }},
            "Install": {{"WantedBy": "multi-user.target"}},
        }})
        ctx.default("users." + cfg["user"], {{"system": True, "description": "{name}"}})
        ports.append(cfg["port"])
    if ports:
        ctx.set("networking.firewall.allowed_tcp_ports", ports)

module(
    name = "{name}",
    options = {{"services.{name}": types.map(instance)}},
    writes = ["users", "networking.firewall.allowed_tcp_ports"],
    render = render,
)
"#
    )
}

/// A module that reads every service, as an inventory page would.
const INVENTORY_MODULE: &str = r#"# Lists every configured service instance.

def render(ctx):
    lines = []
    for service, instances in ctx.read("services").items():
        for instance_name in instances:
            lines.append(service + "." + instance_name)
    ctx.file("/etc/distro/inventory", "\n".join(lines) + "\n")

module(name = "inventory", options = {}, writes = [], render = render)
"#;
