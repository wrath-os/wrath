//! What a machine detects about itself, and whether it has what a configuration requires.

use std::collections::BTreeMap;
use std::path::Path;

use distro_eval::{Requirement, Sources};

/// The facts requirements are about, by name: each with the values this machine has. A machine
/// can have several of some, such as GPUs, and none of a fact it can't detect.
pub fn detect() -> BTreeMap<&'static str, Vec<String>> {
    BTreeMap::from([
        ("arch", vec![std::env::consts::ARCH.to_owned()]),
        ("cpu.count", vec![cpu_count().to_string()]),
        ("gpu", gpus(Path::new("/sys/bus/pci/devices"))),
        (
            "memory_gb",
            memory_gb().iter().map(u64::to_string).collect(),
        ),
    ])
}

pub fn cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// The memory the kernel sees, in whole GiB, rounded to the nearest. It's a little less than
/// what's installed, so a requirement of 64 can need `at_least = 62`.
fn memory_gb() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    Some((kib + (1 << 19)) >> 20)
}

/// The vendors of the display controllers in `devices`, a directory of PCI devices.
fn gpus(devices: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(devices) else {
        return Vec::new();
    };
    let read = |device: &Path, name: &str| {
        std::fs::read_to_string(device.join(name)).map(|text| text.trim().to_owned())
    };
    let mut vendors: Vec<String> = entries
        .filter_map(|entry| {
            let device = entry.ok()?.path();
            // PCI class 0x03 is a display controller.
            if !read(&device, "class").ok()?.starts_with("0x03") {
                return None;
            }
            Some(
                match read(&device, "vendor").ok()?.as_str() {
                    "0x10de" => "nvidia",
                    "0x1002" => "amd",
                    "0x8086" => "intel",
                    other => other,
                }
                .to_owned(),
            )
        })
        .collect();
    vendors.sort();
    vendors.dedup();
    vendors
}

/// Checks a machine with `facts` meets every requirement, saying what it lacks when it doesn't.
/// `what` names what's required, such as `the role gpu-worker`.
pub fn check(
    requirements: &[Requirement],
    sources: &Sources,
    facts: &BTreeMap<&str, Vec<String>>,
    what: &str,
) -> Result<(), String> {
    let unmet: Vec<String> = requirements
        .iter()
        .filter_map(|requirement| {
            let has = facts.get(requirement.fact).map_or(&[][..], Vec::as_slice);
            if requirement.check.met_by(has) {
                return None;
            }
            let has = match has {
                [] => "none".to_owned(),
                has => has.join(", "),
            };
            Some(format!(
                "  {}: {what} requires {}, this machine has {has} ({})",
                requirement.fact,
                requirement.check,
                sources.location(requirement.at)
            ))
        })
        .collect();
    if unmet.is_empty() {
        return Ok(());
    }
    Err(format!(
        "this machine doesn't fit {what}\n{}",
        unmet.join("\n")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpus_are_the_vendors_of_display_controllers() {
        let dir = tempfile::tempdir().unwrap();
        let device = |name: &str, class: &str, vendor: &str| {
            let device = dir.path().join(name);
            std::fs::create_dir_all(&device).unwrap();
            std::fs::write(device.join("class"), format!("{class}\n")).unwrap();
            std::fs::write(device.join("vendor"), format!("{vendor}\n")).unwrap();
        };
        device("0000:01:00.0", "0x030000", "0x10de");
        device("0000:02:00.0", "0x030200", "0x10de");
        device("0000:00:02.0", "0x038000", "0x8086");
        device("0000:00:1f.3", "0x040300", "0x8086");

        assert_eq!(gpus(dir.path()), ["intel", "nvidia"]);
        assert!(gpus(&dir.path().join("missing")).is_empty());
    }

    #[test]
    fn a_machine_that_lacks_a_requirement_is_told_what_and_where() {
        let mut sources = Sources::default();
        let file = sources.add(
            "roles/gpu-worker.toml",
            "[machine.requires]\ngpu = \"nvidia\"\nmemory_gb = { at_least = 64 }\narch = \"x86_64\"\n",
        );
        let at = |line_start: usize| distro_eval::Span::new(file, line_start..line_start + 1);
        let requirements = [
            Requirement {
                fact: "gpu",
                check: distro_eval::Check::Is("nvidia".into()),
                at: at(19),
            },
            Requirement {
                fact: "memory_gb",
                check: distro_eval::Check::AtLeast(64),
                at: at(34),
            },
            Requirement {
                fact: "arch",
                check: distro_eval::Check::Is("x86_64".into()),
                at: at(64),
            },
        ];
        let facts = BTreeMap::from([
            ("arch", vec!["x86_64".to_owned()]),
            ("gpu", vec!["amd".to_owned(), "intel".to_owned()]),
            ("memory_gb", vec![]),
        ]);

        let err = check(&requirements, &sources, &facts, "the role gpu-worker").unwrap_err();
        assert_eq!(
            err,
            "this machine doesn't fit the role gpu-worker\n  \
             gpu: the role gpu-worker requires nvidia, this machine has amd, intel \
             (roles/gpu-worker.toml:2:1)\n  \
             memory_gb: the role gpu-worker requires at least 64, this machine has none \
             (roles/gpu-worker.toml:3:1)"
        );
        let facts = BTreeMap::from([
            ("arch", vec!["x86_64".to_owned()]),
            ("gpu", vec!["nvidia".to_owned()]),
            ("memory_gb", vec!["126".to_owned()]),
        ]);
        assert_eq!(
            check(&requirements, &sources, &facts, "the role gpu-worker"),
            Ok(())
        );
    }

    #[test]
    fn every_fact_a_requirement_can_name_is_detected() {
        let facts = detect();
        let names: Vec<&str> = distro_eval::FACTS.iter().map(|(name, _)| *name).collect();
        assert_eq!(facts.keys().copied().collect::<Vec<_>>(), names);
        assert_eq!(facts["arch"], [std::env::consts::ARCH]);
    }
}
