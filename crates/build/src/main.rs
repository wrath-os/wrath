use std::path::Path;
use std::process::ExitCode;

use distro_build::chain::Arm;
use distro_build::graph::names_of;
use distro_build::store::Lab;

/// The recipe files of a system, from `recipes/` here.
fn boot_recipes() -> Result<Vec<String>, String> {
    distro_build::graph::system_recipes(Path::new("."))
}

fn graph(lab: &Lab, arm: &str, model: &str, inputs: Option<&Path>) -> Result<(), String> {
    let arm = Arm::parse(arm)?;
    distro_build::graph::run(
        lab,
        Path::new("."),
        &names_of(&boot_recipes()?),
        arm,
        Path::new(model),
        inputs,
    )
}

fn main() -> ExitCode {
    let lab = Lab::new("work");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["boot", arm] => Arm::parse(arm).and_then(|arm| {
            distro_build::pilot::run(
                &lab,
                Path::new("."),
                "boot",
                &names_of(&boot_recipes()?),
                arm,
            )
            .map(|_| ())
        }),
        ["graph", arm, model] => graph(&lab, arm, model, None),
        ["graph-apply", arm, model, inputs] => graph(&lab, arm, model, Some(Path::new(inputs))),
        ["graph-rollback", apply] => apply_number(apply)
            .and_then(|apply| distro_build::graph::rollback(&lab, apply))
            .map(|seq| println!("apply {seq}")),
        ["graph-diff", first, second] => apply_number(first)
            .and_then(|first| distro_build::graph::diff(&lab, first, apply_number(second)?))
            .map(|text| print!("{text}")),
        ["graph-closure", arm, model] => Arm::parse(arm)
            .and_then(|arm| {
                distro_build::graph::closure(
                    &lab,
                    Path::new("."),
                    &names_of(&boot_recipes()?),
                    arm,
                    Path::new(model),
                )
            })
            .map(|text| print!("{text}")),
        _ => Err(
            "usage: lab boot <arm> | lab <graph|graph-closure> <arm> <model> | \
             lab graph-apply <arm> <model> <inputs> | lab graph-rollback <apply> | \
             lab graph-diff <apply> <apply>"
                .to_owned(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn apply_number(text: &str) -> Result<i64, String> {
    text.parse()
        .map_err(|_| format!("`{text}` isn't an apply number"))
}
