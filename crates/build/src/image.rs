//! A bootable disk image of one generation: a GPT with an EFI system partition holding
//! systemd-boot, the kernel, the initrd and a boot entry, and an ext4 root with the store and the
//! generation as generation 1. Written to a USB stick, it boots a PC; the VM tests boot it too.

use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::chain::{fresh, runtime_closure};
use crate::graph::ROOT_LABEL;
use crate::seed::run;
use crate::store::{Lab, StorePath, with_path};

/// The EFI system partition's size, in MiB.
const ESP_MIB: u64 = 256;

/// Room on the root for logs and homes beyond what's there, in MiB.
const ROOM_MIB: u64 = 2048;

/// Makes `out` a disk image that boots `generation`, with `options` added to the kernel's own,
/// such as where its console is, and, when given, the `configuration` it was built from in
/// `/etc/distro`, so distro on it can plan. `work` holds what's put together on the way.
pub fn image(
    lab: &Lab,
    generation: &StorePath,
    configuration: Option<&Path>,
    options: &str,
    work: &Path,
    out: &Path,
) -> Result<(), String> {
    fresh(work)?;
    let root = work.join("root");
    let esp = work.join("esp");
    root_tree(lab, generation, &root)?;
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
/// and the current one, and the directories distro expects before it puts the system in place.
pub fn root_tree(lab: &Lab, generation: &StorePath, root: &Path) -> Result<(), String> {
    let store = root.join("store");
    fs::create_dir_all(&store).map_err(with_path(&store))?;
    for path in runtime_closure(lab, generation)? {
        run(
            Command::new("cp")
                .args(["-a", "--reflink=auto"])
                .arg(lab.on_host(&path))
                .arg(&store),
            &format!("copying {}", path.inside()),
        )?;
    }
    let generations = root.join("system/generations");
    fs::create_dir_all(&generations).map_err(with_path(&generations))?;
    symlink(generation.inside(), generations.join("1")).map_err(with_path(&generations))?;
    symlink("generations/1", root.join("system/current")).map_err(with_path(root))?;
    for dir in [
        "bin", "etc", "dev", "proc", "sys", "run", "tmp", "var/lib", "root", "home", "srv",
    ] {
        fs::create_dir_all(root.join(dir)).map_err(with_path(root))?;
    }
    Ok(())
}

/// Copies `configuration` into the root as the system's own, in `/etc/distro`, leaving out what
/// was built or written in it, such as a VCS's files.
pub fn keep_configuration(configuration: &Path, root: &Path) -> Result<(), String> {
    let to = root.join("etc/distro");
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
/// ESP: the store path and file, such as `<hash>-bzImage`, which is how distro names them when it
/// writes boot entries on the machine.
fn boot_file(lab: &Lab, generation: &StorePath, name: &str) -> Result<(PathBuf, String), String> {
    let link = lab.on_host(generation).join(name);
    let target = fs::read_link(&link).map_err(with_path(&link))?;
    let inside = target
        .strip_prefix("/store")
        .map_err(|_| format!("{} isn't in the store", target.display()))?;
    let on_esp = inside.to_string_lossy().replace('/', "-");
    Ok((lab.work.join("store").join(inside), on_esp))
}

/// The boot entry for `generation`, as distro on the machine would write it for generation 1.
pub fn entry(kernel: &str, initrd: &str, params: &str, generation: &StorePath) -> String {
    format!(
        "title distro\nversion generation 1\nsort-key distro\nlinux /EFI/distro/{kernel}\n\
         initrd /EFI/distro/{initrd}\noptions {params} init={}/init\n",
        generation.inside()
    )
}

/// The ESP's files: systemd-boot as the firmware's default loader, the kernel and the initrd,
/// and an entry starting the generation with `options` after its own kernel options.
pub fn esp_tree(
    lab: &Lab,
    generation: &StorePath,
    options: &str,
    esp: &Path,
) -> Result<(), String> {
    for dir in ["EFI/BOOT", "EFI/distro", "loader/entries"] {
        fs::create_dir_all(esp.join(dir)).map_err(with_path(esp))?;
    }
    let (systemd, _) = boot_file(lab, generation, "systemd")?;
    let loader = systemd.join("lib/systemd/boot/efi/systemd-bootx64.efi");
    copy(&loader, &esp.join("EFI/BOOT/BOOTX64.EFI"))?;
    let mut names = Vec::new();
    for part in ["kernel", "initrd"] {
        let (from, name) = boot_file(lab, generation, part)?;
        copy(&from, &esp.join("EFI/distro").join(&name))?;
        names.push(name);
    }
    let params_file = lab.on_host(generation).join("kernel-params");
    let own = fs::read_to_string(&params_file).map_err(with_path(&params_file))?;
    let params = [own.trim(), options.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let text = entry(&names[0], &names[1], &params, generation);
    write(&esp.join("loader/entries/distro-1.conf"), &text)?;
    write(
        &esp.join("loader/loader.conf"),
        "default distro-1.conf\ntimeout 3\neditor no\n",
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
        fs::write(g.join("kernel-params"), "root=LABEL=distro-root rw\n").unwrap();
        (dir, lab, generation)
    }

    #[test]
    fn the_root_has_what_the_generation_needs_and_it_as_generation_one() {
        let (dir, lab, generation) = store();
        let root = dir.path().join("root");
        root_tree(&lab, &generation, &root).unwrap();
        let mut stored: Vec<String> = fs::read_dir(root.join("store"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy()[..1].to_owned())
            .collect();
        stored.sort();
        assert_eq!(stored, ["a", "b", "c", "d"]);
        assert_eq!(
            fs::read_link(root.join("system/generations/1")).unwrap(),
            Path::new(&generation.inside())
        );
        assert_eq!(
            fs::read_link(root.join("system/current")).unwrap(),
            Path::new("generations/1")
        );
        assert!(root.join("var/lib").is_dir());
        assert!(!root.join("usr").exists());
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
        let etc = root.join("etc/distro");
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
        assert_eq!(read(&format!("EFI/distro/{kernel}")), "kernel");
        let entry = read("loader/entries/distro-1.conf");
        assert!(
            entry.contains(&format!("linux /EFI/distro/{kernel}\n")),
            "{entry}"
        );
        assert!(
            entry.contains(&format!(
                "options root=LABEL=distro-root rw console=ttyS0 init={}/init\n",
                generation.inside()
            )),
            "{entry}"
        );
        assert!(read("loader/loader.conf").starts_with("default distro-1.conf\n"));
    }
}
