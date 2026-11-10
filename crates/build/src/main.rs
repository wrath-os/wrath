use std::path::Path;
use std::process::ExitCode;

use distro_build::chain::{self, Arm, Views};
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

/// Builds the pilot for `arm` and checks step 3's gate.
fn pilot(lab: &Lab, arm: &str) -> Result<(), String> {
    let arm = Arm::parse(arm)?;
    if distro_build::pilot::run(lab, Path::new("."), "pilot", &["pilot.toml"], arm)? {
        Ok(())
    } else {
        Err(format!("arm {} did not pass the pilot", arm.name()))
    }
}

/// Builds the whole set for `arm`. Its results are read from the tables, so failures are only news.
fn full(lab: &Lab, arm: &str) -> Result<(), String> {
    let arm = Arm::parse(arm)?;
    distro_build::pilot::run(
        lab,
        Path::new("."),
        "full",
        &["pilot.toml", "full.toml"],
        arm,
    )?;
    Ok(())
}

const SPREAD_RECIPES: [&str; 3] = ["toolchain.toml", "pilot.toml", "full.toml"];
const BOOT_RECIPES: [&str; 9] = distro_build::graph::SYSTEM_RECIPES;

fn parse_counts(first: &str, second: &str) -> Result<(usize, usize), String> {
    let parse = |text: &str| text.parse::<usize>().map_err(|e| e.to_string());
    Ok((parse(first)?, parse(second)?))
}

fn graph(lab: &Lab, arm: &str, model: &str, inputs: Option<&Path>) -> Result<(), String> {
    let arm = Arm::parse(arm)?;
    distro_build::graph::run(
        lab,
        Path::new("."),
        &BOOT_RECIPES,
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
        ["pilot", arm] => pilot(&lab, arm),
        ["full", arm] => full(&lab, arm),
        ["boot", arm] => Arm::parse(arm).and_then(|arm| {
            distro_build::pilot::run(&lab, Path::new("."), "boot", &BOOT_RECIPES, arm).map(|_| ())
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
                    &BOOT_RECIPES,
                    arm,
                    Path::new(model),
                )
            })
            .map(|text| print!("{text}")),
        ["graph-bound", arm, repository] => Arm::parse(arm)
            .and_then(|arm| {
                distro_build::graph::bound(&lab, Path::new(repository), &SPREAD_RECIPES, arm)
            })
            .map(|text| print!("{text}")),
        ["graph-write-test", processes, records, path] => parse_counts(processes, records)
            .and_then(|(processes, records)| {
                distro_build::graph::write_test(processes, records, Path::new(path))
            })
            .map(|text| print!("{text}")),
        ["graph-writer", path, writer, records] => {
            parse_counts(writer, records).and_then(|(writer, records)| {
                distro_build::graph::write_records(Path::new(path), writer, records)
            })
        }
        ["cargo-sources", lock] => std::fs::read_to_string(lock)
            .map_err(|e| e.to_string())
            .and_then(|text| distro_build::cargo::crates(&text))
            .and_then(|crates| {
                distro_build::cargo::pin(&crates, Path::new("sources.lock"))?;
                print!("{}", distro_build::cargo::source_lines(&crates));
                Ok(())
            }),
        ["recheck", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::pilot::recheck(&lab, Path::new("."), &BOOT_RECIPES, arm)),
        ["shrink", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::pilot::shrink(&lab, Path::new("."), &BOOT_RECIPES, arm))
            .and_then(|text| {
                print!("{text}");
                std::fs::write("results/shrink-b.md", text).map_err(|e| e.to_string())
            }),
        ["split", arm, ids @ ..] => Arm::parse(arm)
            .and_then(|arm| {
                distro_build::pilot::split(&lab, Path::new("."), &BOOT_RECIPES, arm, ids, false)
            })
            .map(|text| print!("{text}")),
        ["slots", arm, ids @ ..] => Arm::parse(arm)
            .and_then(|arm| {
                distro_build::pilot::split(&lab, Path::new("."), &BOOT_RECIPES, arm, ids, true)
            })
            .map(|text| print!("{text}")),
        ["closure", arm, ids @ ..] => Arm::parse(arm)
            .and_then(|arm| {
                distro_build::pilot::closure(&lab, Path::new("."), &BOOT_RECIPES, arm, ids)
            })
            .map(|text| print!("{text}")),
        ["repro", arm, only @ ..] => Arm::parse(arm)
            .and_then(|arm| distro_build::experiments::reproduce(&lab, Path::new("."), arm, only)),
        ["narrow", arm, views] => Arm::parse(arm).and_then(|arm| {
            distro_build::experiments::narrow(&lab, Path::new("."), arm, Views::parse(views)?)
        }),
        ["layouts", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::layouts::fetch_sizes(&lab, Path::new("."), arm)),
        ["slots-fetch", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::slots::fetch_sizes(&lab, Path::new("."), arm)),
        ["resolve-bench"] => {
            distro_build::evalbench::run(Path::new("work/resolve-bench"), &[1_000, 10_000, 100_000])
                .and_then(|text| {
                    print!("{text}");
                    std::fs::write("results/resolve-bench.md", text).map_err(|e| e.to_string())
                })
        }
        ["spread", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::experiments::spread(&lab, Path::new("."), arm)),
        ["paths", arm] => Arm::parse(arm)
            .and_then(|arm| distro_build::pilot::paths(&lab, Path::new("."), "pilot.toml", arm))
            .map(|text| print!("{text}")),
        _ => Err(
            "usage: lab seed | lab <toolchain|pilot|full|paths|repro|spread> <a|b|c|d|e> | \
             lab narrow <a|b|c> <view>"
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
