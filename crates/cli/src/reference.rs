//! `wrath dev docs`: the reference pages in `docs/reference/`, made from the code so they say
//! what the code does: the commands from their definitions, the options from the modules, and
//! the module API from the Starlark host's doc comments.

use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;

/// What every generated page starts with.
const GENERATED: &str = "<!-- Made by `wrath dev docs` from the code. Don't edit it by hand. -->";

/// Writes the reference pages, wrath's under `root` and the options under the set whose
/// `modules` they come from, or with `check`, fails when one differs from what the code says.
pub fn write(
    root: &Path,
    modules: &Path,
    cli: &clap::Command,
    check: bool,
) -> Result<ExitCode, String> {
    let own = root.join("docs/reference");
    let set = modules.parent().unwrap_or(root).join("docs/reference");
    let pages = [
        (&own, "commands.md", "Commands", commands(cli)),
        (
            &set,
            "options.md",
            "Options",
            crate::options::markdown(modules)?,
        ),
        (
            &own,
            "module-api.md",
            "The module API",
            wrath_starlark::module_api(),
        ),
    ];
    let mut stale = Vec::new();
    for (dir, file, title, body) in pages {
        let text = format!("{GENERATED}\n\n# {title}\n\n{}", body.trim_end()) + "\n";
        let path = dir.join(file);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            continue;
        }
        if check {
            stale.push(path.display().to_string());
            continue;
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    if stale.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    Err(format!(
        "{} {} out of date; `wrath dev docs` writes them again",
        stale.join(", "),
        if stale.len() == 1 { "is" } else { "are" }
    ))
}

/// Every command that isn't hidden, with what it does, how it's called and its options.
pub fn commands(cli: &clap::Command) -> String {
    let mut cli = cli.clone();
    cli.build();
    let mut out = String::from("## Options every command takes\n\n");
    options(&mut out, &cli, true);
    for sub in visible(&cli) {
        command(&mut out, "wrath", sub);
    }
    out
}

fn command(out: &mut String, parent: &str, cmd: &clap::Command) {
    let name = format!("{parent} {}", cmd.get_name());
    let about = cmd.get_long_about().or(cmd.get_about());
    let usage: Vec<String> = cmd
        .get_arguments()
        .filter(|arg| arg.is_positional() && !arg.is_hide_set())
        .map(ToString::to_string)
        .collect();
    let has_options = cmd
        .get_arguments()
        .any(|arg| !arg.is_positional() && !arg.is_hide_set() && !arg.is_global_set());
    let mut call = name.clone();
    if visible(cmd).next().is_some() {
        call.push_str(" <COMMAND>");
    } else if has_options {
        call.push_str(" [OPTIONS]");
    }
    for arg in &usage {
        call.push(' ');
        call.push_str(arg);
    }
    let _ = writeln!(out, "## `{name}`\n");
    if let Some(about) = about {
        let _ = writeln!(out, "{about}\n");
    }
    let _ = writeln!(out, "```text\n{call}\n```\n");
    options(out, cmd, false);
    for sub in visible(cmd) {
        command(out, &name, sub);
    }
}

/// The arguments of `cmd` that aren't hidden, one line each with what they're for: the global
/// ones, or with `global` false, its own.
fn options(out: &mut String, cmd: &clap::Command, global: bool) {
    let mut any = false;
    let shown = cmd
        .get_arguments()
        .filter(|arg| !arg.is_hide_set() && arg.is_global_set() == global)
        .filter(|arg| !matches!(arg.get_id().as_str(), "help" | "version"));
    for arg in shown {
        let help = arg.get_long_help().or(arg.get_help());
        let help = help.map(|help| format!(": {help}")).unwrap_or_default();
        let short = arg
            .get_short()
            .map(|short| format!("-{short}, "))
            .unwrap_or_default();
        let _ = writeln!(out, "- `{short}{arg}`{help}");
        any = true;
    }
    if any {
        out.push('\n');
    }
}

fn visible(cmd: &clap::Command) -> impl Iterator<Item = &clap::Command> {
    cmd.get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_listed_with_their_usage_and_options_but_not_hidden_ones() {
        let cli = clap::Command::new("wrath")
            .arg(clap::arg!(-C --config <DIR> "The configuration to act on").global(true))
            .subcommand(
                clap::Command::new("build")
                    .about("Build things")
                    .arg(clap::arg!([PACKAGES]... "Packages to build"))
                    .arg(clap::arg!(-o --link <PATH> "Link it here"))
                    .arg(clap::arg!(--test "Hidden").hide(true)),
            )
            .subcommand(clap::Command::new("activate").hide(true));
        let text = commands(&cli);
        assert!(
            text.contains("- `-C, --config <DIR>`: The configuration to act on"),
            "{text}"
        );
        assert!(
            text.contains("## `wrath build`\n\nBuild things\n"),
            "{text}"
        );
        assert!(
            text.contains("wrath build [OPTIONS] [PACKAGES]..."),
            "{text}"
        );
        assert!(
            text.contains("- `-o, --link <PATH>`: Link it here"),
            "{text}"
        );
        assert!(
            !text.contains("--test") && !text.contains("activate"),
            "{text}"
        );
    }
}
