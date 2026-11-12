use std::path::Path;
use std::process::ExitCode;

use distro_build::chain::{self, Arm};
use distro_build::graph::names_of;
use distro_build::sandbox::Status;
use distro_build::store::Lab;
use distro_build::{seed, seed_pack};

/// What must be left behind once the final toolchain is built.
const BOOTSTRAP: [&str; 7] = [
    "seed",
    "wrapper",
    "m4",
    "bison",
    "python",
    "binutils-stage1",
    "gcc-stage1",
];
const FINAL: [&str; 4] = ["glibc", "binutils", "gcc", "hello"];

/// Builds the toolchain for `arm` and checks step 2's gate.
fn toolchain(lab: &Lab, arm: &str) -> Result<(), String> {
    let arm = Arm::parse(arm)?;
    let built = chain::build(lab, Path::new("."), "toolchain.toml", arm)?;
    let mut failed = false;

    for name in FINAL {
        let found = chain::references(&lab.on_host(&built.paths[name]))?;
        for bootstrap in BOOTSTRAP {
            if let Some(file) = found.get(built.paths[bootstrap].hash()) {
                println!(
                    "  LEAK   {name} refers to {bootstrap}, in {}",
                    file.display()
                );
                failed = true;
            }
        }
    }

    for (program, expected) in [
        ("bin/hello", "hello from the store\n"),
        ("bin/hello-cxx", "hello from libstdc++\n"),
    ] {
        let (status, text) = chain::run_alone(
            lab,
            &built.paths["hello"],
            program,
            &program.replace('/', "-"),
        )?;
        if status == Status::Passed && text == expected {
            println!("  ran    {program} with /usr absent");
        } else {
            println!("  FAILED {program}: {status:?}: {text}");
            failed = true;
        }
    }
    if failed {
        Err(format!("arm {} did not pass the gate", arm.name()))
    } else {
        println!("arm {}: gate passed", arm.name());
        Ok(())
    }
}

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
        ["seed"] => seed::prepare(&lab).map(|path| println!("{}", path.inside())),
        ["seed-pack", arm] => {
            Arm::parse(arm).and_then(|arm| seed_pack::pack(&lab, Path::new("."), arm))
        }
        ["seed-addresses", arm] => {
            Arm::parse(arm).and_then(|arm| seed_pack::addresses(&lab, Path::new("."), arm))
        }
        ["toolchain", arm] => toolchain(&lab, arm),
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
        ["repro", arm, only @ ..] => Arm::parse(arm)
            .and_then(|arm| distro_build::experiments::reproduce(&lab, Path::new("."), arm, only)),
        _ => Err(
            "usage: lab seed | lab <seed-pack|seed-addresses|toolchain|boot> <arm> | \
             lab repro <arm> [id...] | lab <graph|graph-closure> <arm> <model> | \
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
