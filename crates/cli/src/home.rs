//! A user's home on its own: `~/.config/distro`, switched by the user without root.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_backend_nixos::{self as backend, Target, home as homes};
use distro_model::Model;

use crate::generations::{self, Switch};

/// The user's own configuration, `~/.config/distro`, when someone other than root runs a
/// command and it has a `home.toml`.
pub fn dir() -> Option<PathBuf> {
    if is_root() {
        return None;
    }
    let config = home_dir()?.join(".config/distro");
    config.join("home.toml").is_file().then_some(config)
}

fn is_root() -> bool {
    std::env::var("USER").is_ok_and(|user| user == "root")
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn profile() -> Result<PathBuf, String> {
    home_dir()
        .map(|home| homes::profile(&home))
        .ok_or_else(|| "HOME isn't set, so there's no home to switch".to_owned())
}

/// The model the home runs now, or an empty one before its first `apply`.
pub fn current() -> Result<Model, String> {
    match homes::current_model(&profile()?) {
        Some(path) => crate::read_model(&path),
        None => Ok(Model::default()),
    }
}

/// Refuses a home the system already puts in place: it has one owner.
fn refuse_system_home() -> Result<(), String> {
    let user = std::env::var("USER").map_err(|_| "USER isn't set".to_owned())?;
    let Some(path) = backend::current_model() else {
        return Ok(());
    };
    let system = crate::read_model(Path::new(path))?;
    if system.homes.contains_key(&user) {
        return Err(format!(
            "the system configuration manages {user}'s home, so it has two owners; remove \
             `users/{user}.toml` from the system configuration to make it yours"
        ));
    }
    Ok(())
}

/// Builds the home for `model` and switches to it, unless it runs that model already. Packages
/// the lock next to `config` doesn't have yet are locked first.
pub fn apply(
    root: &Path,
    config: &Path,
    model: Model,
    how: Switch,
    yes: bool,
) -> Result<ExitCode, String> {
    refuse_system_home()?;
    let model = generations::locked(root, config, model)?;
    let profile = profile()?;
    let hash = model.hash();
    if homes::current_model_hash(&profile).as_deref() == Some(hash.as_str()) {
        println!("your home already has model {}", &hash[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    let first = homes::current_model(&profile).is_none();
    print!("{}", distro_model::plan(&current()?, &model));
    if !generations::confirmed(yes, "switch to it?")? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let built = generations::build(root, &model, Target::Home)?;
    switch(&profile, &built, how)?;
    if first && how == Switch::Apply {
        println!(
            "your packages are in ~/.local/state/distro/profiles/home/profile/bin; login shells \
             find them once your shell's rc has `. ~/.config/distro/profile.sh`"
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Lists the home's generations, each with what changed from the one before it.
pub fn history() -> Result<ExitCode, String> {
    let profile = profile()?;
    let mut before: Option<Model> = None;
    for generation in homes::history(&profile)? {
        println!("{generation}");
        let model = crate::read_model(&homes::generation_model(&profile, generation.number)).ok();
        if let (Some(before), Some(after)) = (&before, &model) {
            println!("      {}", distro_model::plan(before, after).summary(4));
        }
        before = model;
    }
    Ok(ExitCode::SUCCESS)
}

/// Switches the home back from the one being tried, or else to its previous generation.
pub fn rollback() -> Result<ExitCode, String> {
    let profile = profile()?;
    if homes::trying() {
        homes::end_trial(&profile)?;
        println!("stopped trying; your home is back to its current generation");
        return Ok(ExitCode::SUCCESS);
    }
    let generation = homes::rollback(&profile)?;
    println!("rolled your home back to generation {}", generation.number);
    Ok(ExitCode::SUCCESS)
}

/// Says which generation of the home is current and whether it's the configuration's `model`.
pub fn status(model: &Model) -> Result<ExitCode, String> {
    let profile = profile()?;
    let current = homes::history(&profile)?.into_iter().find(|g| g.current);
    match current {
        Some(generation) => println!(
            "your home is at generation {}, from {}",
            generation.number, generation.created
        ),
        None => println!("your home has no generations yet; `distro apply` makes the first"),
    }
    let running = homes::current_model_hash(&profile);
    println!("{}", generations::matching(running.as_deref(), model));
    Ok(ExitCode::SUCCESS)
}

/// Switches the home to `built`, a home built already, such as by another machine.
pub fn switch_to(built: &Path, how: Switch) -> Result<ExitCode, String> {
    refuse_system_home()?;
    switch(&profile()?, built, how)?;
    Ok(ExitCode::SUCCESS)
}

/// Makes the home being tried the current generation.
pub fn keep() -> Result<ExitCode, String> {
    homes::keep_trial(&profile()?)?;
    println!("kept the home you were trying as your current generation");
    Ok(ExitCode::SUCCESS)
}

fn switch(profile: &Path, built: &Path, how: Switch) -> Result<(), String> {
    match how {
        Switch::Apply => {
            homes::activate(profile, built)?;
            println!("switched your home to {}", built.display());
        }
        Switch::Try(seconds) => {
            homes::try_home(profile, built, seconds)?;
            println!("trying {}", built.display());
            println!(
                "it switches back in {seconds}s unless you run `distro keep`; `distro rollback` \
                 switches back now"
            );
        }
    }
    Ok(())
}

/// Whether `history` and `rollback` are about the user's own home: someone other than root runs
/// them, and has a home configuration or generations of one.
pub fn is_context() -> bool {
    dir().is_some() || (!is_root() && profile().is_ok_and(|profile| profile.exists()))
}
