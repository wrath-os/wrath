//! Measuring how long a plan takes with many modules (spike S1).
//!
//! Generates synthetic service modules in every module language, each shaped like a real one:
//! a map of instances, a unit and a file per instance, a system user and a firewall port
//! contributed to other modules. One more module iterates over every service. A generated
//! configuration enables half of them. Then it times what `plan` does after a one-line change:
//! load the modules, evaluate the changed configuration, build its model and diff it against
//! the model before the change.

use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use distro_model::{Model, plan};

use crate::system::{self, ModuleOrder};

#[derive(Clone, Copy)]
enum Language {
    Luau,
    Starlark,
}

impl Language {
    const ALL: [Language; 2] = [Language::Luau, Language::Starlark];

    /// The module directory name, also printed in the report.
    fn name(self) -> &'static str {
        match self {
            Language::Luau => "luau",
            Language::Starlark => "starlark",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Language::Luau => "luau",
            Language::Starlark => "star",
        }
    }
}

pub struct Options<'a> {
    /// Directory holding a subdirectory of real modules per language.
    pub modules: &'a Path,
    pub synthetic: usize,
    pub runs: usize,
    pub budget: Duration,
}

/// Runs the measurement for every language and reports whether all stayed within budget.
pub fn plan_time(options: &Options<'_>) -> Result<bool, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    let configs = work.path().join("configs");
    write_configs(&configs, options.synthetic)?;
    let mut within = true;
    for language in Language::ALL {
        let name = language.name();
        let extension = language.extension();
        let modules = work.path().join(name);
        copy_modules(&options.modules.join(name), &modules)?;
        for index in 0..options.synthetic {
            let source = synthetic_module(language, index);
            write(&modules.join(format!("svc{index:03}.{extension}")), &source)?;
        }
        write(
            &modules.join(format!("inventory.{extension}")),
            inventory_module(language),
        )?;

        let before = system::evaluate_dir(&configs.join("before"), &modules, ModuleOrder::Sorted)
            .map(|evaluation| Model::from_evaluation(&evaluation))?;
        let mut times = Vec::with_capacity(options.runs);
        let mut changes = 0;
        for _ in 0..options.runs {
            let start = Instant::now();
            let evaluation =
                system::evaluate_dir(&configs.join("after"), &modules, ModuleOrder::Sorted)?;
            let after = Model::from_evaluation(&evaluation);
            let diff = plan(&before, &after);
            times.push(start.elapsed());
            changes = diff.to_string().lines().count();
        }
        times.sort();
        let median = times[times.len() / 2];
        let fits = median <= options.budget;
        within &= fits;
        let module_count = fs::read_dir(&modules).map_err(|e| e.to_string())?.count();
        println!(
            "{} {name:<9} {module_count} modules: plan after a one-line change takes {} ms \
             (median of {}, fastest {} ms, slowest {} ms); budget {} ms; plan has {changes} lines",
            if fits { "PASS" } else { "FAIL" },
            median.as_millis(),
            options.runs,
            times[0].as_millis(),
            times[times.len() - 1].as_millis(),
            options.budget.as_millis(),
        );
    }
    Ok(within)
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

fn synthetic_module(language: Language, index: usize) -> String {
    let name = format!("svc{index:03}");
    let port = 10_000 + index;
    match language {
        Language::Luau => format!(
            r#"-- A synthetic service for measuring plan time.

local instance = types.record({{
	enable = option(types.bool, {{ default = false }}),
	port = option(types.port, {{ default = {port} }}),
	user = option(types.str, {{ default = "{name}" }}),
	extra_args = option(types.list(types.str), {{ default = {{}} }}),
	settings = option(types.freeform, {{ optional = true }}),
}})

return {{
	name = "{name}",
	options = {{ ["services.{name}"] = types.map(instance) }},
	writes = {{ "users", "networking.firewall.allowed_tcp_ports" }},
	render = function(ctx)
		local ports = {{}}
		for instance_name, cfg in pairs(ctx.read("services.{name}")) do
			if cfg.enable then
				local config = "/etc/{name}/" .. instance_name .. ".json"
				ctx.file(config, formats.json(cfg.settings or {{}}))
				ctx.unit("{name}-" .. instance_name .. ".service", {{
					Unit = {{ Description = "{name} (" .. instance_name .. ")" }},
					Service = {{
						User = cfg.user,
						ExecStart = "{name} --config " .. config .. " " .. table.concat(cfg.extra_args, " "),
					}},
					Install = {{ WantedBy = "multi-user.target" }},
				}})
				ctx.default("users." .. cfg.user, {{ system = true, description = "{name}" }})
				table.insert(ports, cfg.port)
			end
		end
		if #ports > 0 then
			ctx.set("networking.firewall.allowed_tcp_ports", ports)
		end
	end,
}}
"#
        ),
        Language::Starlark => format!(
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
        ),
    }
}

/// A module that reads every service, as an inventory page would.
fn inventory_module(language: Language) -> &'static str {
    match language {
        Language::Luau => {
            r#"-- Lists every configured service instance.

return {
	name = "inventory",
	options = {},
	writes = {},
	render = function(ctx)
		local lines = {}
		for service, instances in pairs(ctx.read("services")) do
			for instance_name in pairs(instances) do
				table.insert(lines, service .. "." .. instance_name)
			end
		end
		ctx.file("/etc/distro/inventory", table.concat(lines, "\n") .. "\n")
	end,
}
"#
        }
        Language::Starlark => {
            r#"# Lists every configured service instance.

def render(ctx):
    lines = []
    for service, instances in ctx.read("services").items():
        for instance_name in instances:
            lines.append(service + "." + instance_name)
    ctx.file("/etc/distro/inventory", "\n".join(lines) + "\n")

module(name = "inventory", options = {}, writes = [], render = render)
"#
        }
    }
}
