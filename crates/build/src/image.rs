//! A bootable disk image of one generation: a GPT with an EFI system partition holding
//! systemd-boot, a unified kernel image and a boot entry, and an ext4 root with the store and the
//! generation as generation 1. Written to a USB stick, it boots a PC; the VM tests boot it too.

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::chain::{fresh, runtime_closure};
use crate::graph::ROOT_LABEL;
use crate::seed::run;
use crate::store::{Lab, STORE_DIR, StorePath, with_path};

/// The EFI system partition's size, in MiB.
const ESP_MIB: u64 = 256;

/// Room on the root for logs and homes beyond what's there, in MiB.
const ROOM_MIB: u64 = 2048;

/// Makes `out` a disk image that boots `generation`, with `options` added to the kernel's own,
/// such as where its console is, and, when given, the `configuration` it was built from in
/// `/etc/wrath`, so wrath on it can plan, and the system `wrath install` puts on the disks when
/// that isn't the one it boots. `work` holds what's put together on the way.
pub fn image(
    lab: &Lab,
    generation: &StorePath,
    configuration: Option<&Path>,
    install: Option<&StorePath>,
    options: &str,
    work: &Path,
    out: &Path,
) -> Result<(), String> {
    fresh(work)?;
    let root = work.join("root");
    let esp = work.join("esp");
    root_tree(lab, generation, &root)?;
    if let Some(install) = install {
        add_install(lab, install, &root)?;
    }
    if let Some(configuration) = configuration {
        keep_configuration(configuration, &root)?;
    }
    esp_tree(lab, generation, options, &esp)?;

    let esp_image = work.join("esp.img");
    let root_image = work.join("root.img");
    make_esp(&esp, &esp_image)?;
    let root_mib = size_mib(&root)? + ROOM_MIB;
    make_root(&root, root_mib, &root_image)?;
    join(&esp_image, &root_image, root_mib, out)
}

/// The root file system: every store path `generation` needs, the generation as generation 1
/// and the current one, and the directories wrath expects before it puts the system in place.
pub fn root_tree(lab: &Lab, generation: &StorePath, root: &Path) -> Result<(), String> {
    let store = root.join("wrath/store");
    fs::create_dir_all(&store).map_err(with_path(&store))?;
    let closure = runtime_closure(lab, generation)?;
    for path in &closure {
        run(
            Command::new("cp")
                .args(["-a", "--reflink=auto"])
                .arg(lab.on_host(path))
                .arg(&store),
            &format!("copying {}", path.inside()),
        )?;
    }
    crate::records::copy_system(lab, generation, &closure, &root.join(WORK))?;
    let generations = root.join(GENERATIONS);
    fs::create_dir_all(&generations).map_err(with_path(&generations))?;
    symlink(generation.inside(), generations.join("1")).map_err(with_path(&generations))?;
    symlink("generations/1", root.join(CURRENT)).map_err(with_path(root))?;
    for dir in [
        "bin", "etc", "dev", "proc", "sys", "run", "tmp", "var/lib", "root", "home", "srv",
    ] {
        fs::create_dir_all(root.join(dir)).map_err(with_path(root))?;
    }
    Ok(())
}

/// Where a machine keeps its generations, under the root: each a link named by its number to a
/// system in `/wrath/store`, kept beside the store so a root erased at every boot keeps them.
pub const GENERATIONS: &str = "wrath/system/generations";

/// Where a machine's builder keeps its records, under the root, such as which build made each
/// path in the store, so it knows what it has without building it again.
pub const WORK: &str = "wrath/work";

/// The link to the current generation, under the root, as `generations/N`.
pub const CURRENT: &str = "wrath/system/current";

/// Where an image names the system `wrath install` puts on the disks, under its root.
pub const INSTALL: &str = "wrath/system/install";

/// Adds `system`, and what it needs that the root doesn't have yet, to an image's store, named as
/// [`INSTALL`]. An image of a configuration with disks boots a system without them, which can't
/// wait for disks that aren't there yet, and installs this one.
pub fn add_install(lab: &Lab, system: &StorePath, root: &Path) -> Result<(), String> {
    let store = root.join("wrath/store");
    let closure = runtime_closure(lab, system)?;
    for path in &closure {
        let host = lab.on_host(path);
        if host
            .file_name()
            .is_some_and(|name| store.join(name).exists())
        {
            continue;
        }
        run(
            Command::new("cp")
                .args(["-a", "--reflink=auto"])
                .arg(&host)
                .arg(&store),
            &format!("copying {}", path.inside()),
        )?;
    }
    crate::records::copy_system(lab, system, &closure, &root.join(WORK))?;
    let link = root.join(INSTALL);
    symlink(system.inside(), &link).map_err(with_path(&link))
}

/// Copies `configuration` into the root as the system's own, in `/etc/wrath`, leaving out what
/// was built or written in it, such as a VCS's files.
pub fn keep_configuration(configuration: &Path, root: &Path) -> Result<(), String> {
    let to = root.join("etc/wrath");
    fs::create_dir_all(&to).map_err(with_path(&to))?;
    run(
        Command::new("cp")
            .args(["-r", "--no-preserve=ownership"])
            .arg(configuration.join("."))
            .arg(&to),
        "copying the configuration",
    )?;
    for vcs in [".git", ".jj"] {
        crate::chain::remove(&to.join(vcs))?;
    }
    Ok(())
}

/// Where a link in a generation points, as a path in the store on the host, and its name on the
/// ESP: the store path and file, such as `<hash>-bzImage`, which is how wrath names them when it
/// writes boot entries on the machine.
fn boot_file(lab: &Lab, generation: &StorePath, name: &str) -> Result<(PathBuf, String), String> {
    let link = lab.on_host(generation).join(name);
    let target = fs::read_link(&link).map_err(with_path(&link))?;
    let inside = target
        .strip_prefix(STORE_DIR)
        .map_err(|_| format!("{} isn't in the store", target.display()))?;
    let on_esp = inside.to_string_lossy().replace('/', "-");
    Ok((lab.work.join("store").join(inside), on_esp))
}

/// The image's own unified kernel image on the ESP. The machine makes its own once it switches,
/// named after the system, and this one goes then.
const IMAGE_UKI: &str = "wrath-1.efi";

/// What a system calls itself in the boot menu, from its `/etc/os-release`: its `PRETTY_NAME`, or
/// its `NAME`, or `wrath`.
pub fn release_name(os_release: &str) -> String {
    let field = |key: &str| {
        os_release.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.strip_prefix('=')?;
            let value = value.trim_matches('"');
            (!value.is_empty()).then(|| value.to_owned())
        })
    };
    field("PRETTY_NAME")
        .or_else(|| field("NAME"))
        .unwrap_or_else(|| "wrath".to_owned())
}

/// The ESP's files: systemd-boot as the firmware's default loader, and an entry starting the
/// generation with `options` after its own kernel options, from a unified kernel image made from
/// its systemd's stub, or from its kernel and initrd when that has none.
pub fn esp_tree(
    lab: &Lab,
    generation: &StorePath,
    options: &str,
    esp: &Path,
) -> Result<(), String> {
    for dir in ["EFI/BOOT", "EFI/wrath", "loader/entries"] {
        fs::create_dir_all(esp.join(dir)).map_err(with_path(esp))?;
    }
    let (systemd, _) = boot_file(lab, generation, "systemd")?;
    let loader = systemd.join("lib/systemd/boot/efi/systemd-bootx64.efi");
    copy(&loader, &esp.join("EFI/BOOT/BOOTX64.EFI"))?;
    let system = lab.on_host(generation);
    let params_file = system.join("kernel-params");
    let own = fs::read_to_string(&params_file).map_err(with_path(&params_file))?;
    let params = [own.trim(), options.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let params = format!("{params} init={}/init", generation.inside());
    let release = fs::read_to_string(system.join("etc/os-release")).unwrap_or_default();
    let (kernel, kernel_name) = boot_file(lab, generation, "kernel")?;
    let (initrd, initrd_name) = boot_file(lab, generation, "initrd")?;
    let stub = systemd.join("lib/systemd/boot/efi/linuxx64.efi.stub");
    let starts = if stub.is_file() {
        let read = |path: &Path| fs::read(path).map_err(with_path(path));
        let image = crate::uki::assemble(
            &read(&stub)?,
            &crate::uki::Parts {
                os_release: release.as_bytes(),
                cmdline: params.as_bytes(),
                initrd: &read(&initrd)?,
                linux: &read(&kernel)?,
                pcr_public_key: None,
                pcr_signature: None,
            },
        )
        .map_err(|e| format!("{}: {e}", stub.display()))?;
        let at = esp.join("EFI/wrath").join(IMAGE_UKI);
        fs::write(&at, image).map_err(with_path(&at))?;
        crate::uki::entry_line(&read(&loader)?, &format!("/EFI/wrath/{IMAGE_UKI}"))
    } else {
        copy(&kernel, &esp.join("EFI/wrath").join(&kernel_name))?;
        copy(&initrd, &esp.join("EFI/wrath").join(&initrd_name))?;
        format!(
            "linux /EFI/wrath/{kernel_name}\ninitrd /EFI/wrath/{initrd_name}\noptions {params}\n"
        )
    };
    let text = format!(
        "title {}\nversion generation 1\nsort-key wrath\n{starts}",
        release_name(&release)
    );
    write(&esp.join("loader/entries/wrath-1.conf"), &text)?;
    write(
        &esp.join("loader/loader.conf"),
        "default wrath-1.conf\ntimeout 3\neditor no\n",
    )
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    fs::copy(from, to).map(drop).map_err(with_path(from))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(with_path(path))
}

fn sparse(path: &Path, mib: u64) -> Result<(), String> {
    let file = fs::File::create(path).map_err(with_path(path))?;
    file.set_len(mib << 20).map_err(with_path(path))
}

fn size_mib(dir: &Path) -> Result<u64, String> {
    let output = Command::new("du")
        .arg("-sm")
        .arg(dir)
        .output()
        .map_err(|e| format!("du: {e}"))?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .and_then(|mib| mib.parse().ok())
        .ok_or_else(|| format!("du couldn't size {}", dir.display()))
}

fn make_esp(esp: &Path, image: &Path) -> Result<(), String> {
    sparse(image, ESP_MIB)?;
    run(
        Command::new("mkfs.vfat")
            .args(["-F", "32", "-n", "ESP"])
            .arg(image),
        "making the ESP",
    )?;
    let mut copy = Command::new("mcopy");
    copy.arg("-s").arg("-i").arg(image);
    for entry in fs::read_dir(esp).map_err(with_path(esp))? {
        copy.arg(entry.map_err(with_path(esp))?.path());
    }
    run(copy.arg("::/"), "filling the ESP")
}

/// An ext4 file system of `root`, owned by root inside, as an unprivileged user can make it.
fn make_root(root: &Path, mib: u64, image: &Path) -> Result<(), String> {
    sparse(image, mib)?;
    run(
        Command::new("bwrap")
            .args(["--unshare-user", "--uid", "0", "--gid", "0"])
            .args(["--bind", "/", "/", "--dev-bind", "/dev", "/dev", "--"])
            .args(["mke2fs", "-q", "-t", "ext4", "-L", ROOT_LABEL, "-d"])
            .arg(root)
            .arg(image),
        "making the root file system",
    )
}

/// The disk: a GPT with the ESP from 1 MiB and the root after it.
fn join(esp: &Path, root: &Path, root_mib: u64, out: &Path) -> Result<(), String> {
    let partial = out.with_extension("partial");
    sparse(&partial, 1 + ESP_MIB + root_mib + 1)?;
    let table = format!(
        "label: gpt\nstart=1MiB, size={ESP_MIB}MiB, type=uefi\nstart={}MiB, size={root_mib}MiB, \
         type=linux\n",
        1 + ESP_MIB
    );
    let mut sfdisk = Command::new("sfdisk")
        .arg("--quiet")
        .arg(&partial)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("sfdisk: {e}"))?;
    sfdisk
        .stdin
        .as_mut()
        .ok_or("sfdisk has no input")?
        .write_all(table.as_bytes())
        .map_err(|e| format!("sfdisk: {e}"))?;
    let output = sfdisk
        .wait_with_output()
        .map_err(|e| format!("sfdisk: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "partitioning: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    for (part, seek) in [(esp, 1), (root, 1 + ESP_MIB)] {
        run(
            Command::new("dd")
                .arg(format!("if={}", part.display()))
                .arg(format!("of={}", partial.display()))
                .args(["bs=1M", "conv=notrunc,sparse", "status=none"])
                .arg(format!("seek={seek}")),
            "writing the disk",
        )?;
    }
    fs::rename(&partial, out).map_err(with_path(out))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// A store with a generation whose kernel, initrd and systemd are packages of their own, and
    /// a package nothing uses.
    fn store() -> (tempfile::TempDir, Lab, StorePath) {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let path = |tag: char| StorePath::from_hash(&tag.to_string().repeat(40));
        let (generation, linux, initrd, systemd) = (path('a'), path('b'), path('c'), path('d'));
        let host = |path: &StorePath| lab.on_host(path);
        fs::create_dir_all(host(&linux)).unwrap();
        fs::write(host(&linux).join("bzImage"), "kernel").unwrap();
        fs::create_dir_all(host(&initrd)).unwrap();
        fs::write(host(&initrd).join("initrd"), "initrd").unwrap();
        let efi = host(&systemd).join("lib/systemd/boot/efi");
        fs::create_dir_all(&efi).unwrap();
        fs::write(efi.join("systemd-bootx64.efi"), "loader").unwrap();
        fs::create_dir_all(host(&path('e'))).unwrap();
        let g = host(&generation);
        fs::create_dir_all(&g).unwrap();
        symlink(format!("{}/bzImage", linux.inside()), g.join("kernel")).unwrap();
        symlink(format!("{}/initrd", initrd.inside()), g.join("initrd")).unwrap();
        symlink(systemd.inside(), g.join("systemd")).unwrap();
        fs::write(g.join("kernel-params"), "root=LABEL=wrath-root rw\n").unwrap();
        (dir, lab, generation)
    }

    #[test]
    fn the_root_has_what_the_generation_needs_and_it_as_generation_one() {
        let (dir, lab, generation) = store();
        let root = dir.path().join("root");
        root_tree(&lab, &generation, &root).unwrap();
        let mut stored: Vec<String> = fs::read_dir(root.join("wrath/store"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .map(|name| name[..1].to_owned())
            .collect();
        stored.sort();
        assert_eq!(stored, ["a", "b", "c", "d"]);
        assert_eq!(
            fs::read_link(root.join("wrath/system/generations/1")).unwrap(),
            Path::new(&generation.inside())
        );
        assert_eq!(
            fs::read_link(root.join("wrath/system/current")).unwrap(),
            Path::new("generations/1")
        );
        assert!(root.join("var/lib").is_dir());
        assert!(!root.join("usr").exists());
    }

    #[test]
    fn the_root_brings_the_records_of_the_generations_builds() {
        let (dir, lab, generation) = store();
        let record = |path: &str| {
            let file = dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "").unwrap();
        };
        // The kernel, a tool that built it and isn't in the image, and a package nothing uses.
        let (linux, tool, unused) = ("b".repeat(40), "7".repeat(40), "e".repeat(40));
        for key in [&linux, &tool, &unused] {
            record(&format!("fixed/{key}-built"));
        }
        crate::records::keep(
            &lab,
            &generation,
            [("u1", linux.as_str()), ("u2", tool.as_str())],
            &Default::default(),
        )
        .unwrap();
        let root = dir.path().join("root");
        root_tree(&lab, &generation, &root).unwrap();
        let work = root.join(WORK);
        assert!(work.join(format!("fixed/{linux}-built")).is_file());
        assert!(work.join(format!("fixed/{tool}-built")).is_file());
        assert!(!work.join(format!("fixed/{unused}-built")).exists());
        assert!(work.join("builds").join(generation.hash()).is_file());
    }

    #[test]
    fn a_system_to_install_comes_along_with_what_it_needs_and_is_named() {
        let (dir, lab, generation) = store();
        let root = dir.path().join("root");
        root_tree(&lab, &generation, &root).unwrap();
        // The system to install shares the live one's packages and has one of its own.
        let installed = StorePath::from_hash(&"f".repeat(40));
        let own = StorePath::from_hash(&"9".repeat(40));
        fs::create_dir_all(lab.on_host(&own)).unwrap();
        let host = lab.on_host(&installed);
        fs::create_dir_all(&host).unwrap();
        symlink(own.inside(), host.join("extra")).unwrap();
        let shared = lab.on_host(&generation).join("kernel");
        symlink(fs::read_link(shared).unwrap(), host.join("kernel")).unwrap();

        add_install(&lab, &installed, &root).unwrap();

        let stored: BTreeSet<String> = fs::read_dir(root.join("wrath/store"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy()[..1].to_owned())
            .filter(|name| name != ".")
            .collect();
        assert_eq!(
            stored,
            BTreeSet::from(["9", "a", "b", "c", "d", "f"].map(String::from))
        );
        assert_eq!(
            fs::read_link(root.join(INSTALL)).unwrap(),
            Path::new(&installed.inside())
        );
    }

    #[test]
    fn the_root_keeps_the_configuration_without_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let configuration = dir.path().join("config");
        fs::create_dir_all(configuration.join("users")).unwrap();
        fs::create_dir_all(configuration.join(".jj/repo")).unwrap();
        fs::write(configuration.join("system.toml"), "[system]\n").unwrap();
        fs::write(configuration.join("users/demo.toml"), "\n").unwrap();
        let root = dir.path().join("root");
        keep_configuration(&configuration, &root).unwrap();
        let etc = root.join("etc/wrath");
        assert_eq!(
            fs::read_to_string(etc.join("system.toml")).unwrap(),
            "[system]\n"
        );
        assert!(etc.join("users/demo.toml").is_file());
        assert!(!etc.join(".jj").exists());
    }

    #[test]
    fn the_esp_boots_the_generation_with_the_options_after_its_own() {
        let (dir, lab, generation) = store();
        let esp = dir.path().join("esp");
        esp_tree(&lab, &generation, "console=ttyS0", &esp).unwrap();
        let read = |path: &str| fs::read_to_string(esp.join(path)).unwrap();
        assert_eq!(read("EFI/BOOT/BOOTX64.EFI"), "loader");
        let kernel = format!("{}-bzImage", "b".repeat(40));
        assert_eq!(read(&format!("EFI/wrath/{kernel}")), "kernel");
        let entry = read("loader/entries/wrath-1.conf");
        assert!(
            entry.contains(&format!("linux /EFI/wrath/{kernel}\n")),
            "{entry}"
        );
        assert!(
            entry.contains(&format!(
                "options root=LABEL=wrath-root rw console=ttyS0 init={}/init\n",
                generation.inside()
            )),
            "{entry}"
        );
        assert!(read("loader/loader.conf").starts_with("default wrath-1.conf\n"));
    }

    #[test]
    fn the_esp_boots_an_image_with_the_command_line_inside_when_systemd_has_a_stub() {
        let (dir, lab, generation) = store();
        let systemd = lab.on_host(&StorePath::from_hash(&"d".repeat(40)));
        let stub = systemd.join("lib/systemd/boot/efi/linuxx64.efi.stub");
        fs::write(stub, crate::uki::tests::stub()).unwrap();
        fs::write(
            systemd.join("lib/systemd/boot/efi/systemd-bootx64.efi"),
            "#### LoaderInfo: systemd-boot 262.2 ####",
        )
        .unwrap();
        let esp = dir.path().join("esp");
        esp_tree(&lab, &generation, "console=ttyS0", &esp).unwrap();
        let entry = fs::read_to_string(esp.join("loader/entries/wrath-1.conf")).unwrap();
        assert!(entry.ends_with("uki /EFI/wrath/wrath-1.efi\n"), "{entry}");
        let image = fs::read(esp.join("EFI/wrath/wrath-1.efi")).unwrap();
        let cmdline = format!(
            "root=LABEL=wrath-root rw console=ttyS0 init={}/init",
            generation.inside()
        );
        let has = |part: &[u8]| image.windows(part.len()).any(|w| w == part);
        assert!(has(cmdline.as_bytes()) && has(b"kernel") && has(b"initrd"));
        assert_eq!(fs::read_dir(esp.join("EFI/wrath")).unwrap().count(), 1);
    }
}
