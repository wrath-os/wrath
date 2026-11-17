//! The pinned toolchain everything is built from, unpacked into the store and pointed at where it
//! ended up.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::sandbox::{self, Limits, Status, Step};
use crate::store::{Lab, STORE_DIR, StorePath, hex, with_path};

/// Points the unpacked binaries at their new location.
const PREPARE: &str = r#"
shopt -s nullglob
LD_BINARY=$out/lib/ld-linux-x86-64.so.2
LIBSTDCXX_SO_DIR=$(echo $out/lib/gcc/*/*)
LD_LIBRARY_PATH=$out/lib $LD_BINARY $out/bin/mv $out/lib/libstdc++.* $LIBSTDCXX_SO_DIR/
LD_LIBRARY_PATH=$out/lib $LD_BINARY $out/bin/cp $out/bin/patchelf .
LD_LIBRARY_PATH=$out/lib $LD_BINARY $out/bin/cp $out/lib/libgcc_s.so.1 .
LD_LIBRARY_PATH=.:$out/lib:$LIBSTDCXX_SO_DIR $LD_BINARY \
    ./patchelf --set-rpath $out/lib --force-rpath $out/lib/libgcc_s.so.1
for i in $out/bin/* $out/libexec/gcc/*/*/*; do
    if [ -L "$i" ]; then continue; fi
    if [ -z "${i##*/liblto*}" ]; then continue; fi
    LD_LIBRARY_PATH=$out/lib:$LIBSTDCXX_SO_DIR $LD_BINARY \
        ./patchelf --set-interpreter $LD_BINARY --set-rpath $out/lib:$LIBSTDCXX_SO_DIR \
        --force-rpath "$i"
done
for i in $out/lib/*.so*; do
    if [ -L "$i" ]; then continue; fi
    rpath=$($out/bin/patchelf --print-rpath "$i" 2>/dev/null) || continue
    if [ -n "$rpath" ]; then
        $out/bin/patchelf --set-rpath $out/lib --force-rpath "$i"
    fi
done
export PATH=$out/bin
ln -s bash $out/bin/sh
ln -s bzip2 $out/bin/bunzip2
printf '#!%s/bin/sh\nexec %s/bin/gzip -d "$@"\n' $out $out > $out/bin/gunzip
printf '#!%s/bin/sh\nexec %s/bin/grep -E "$@"\n' $out $out > $out/bin/egrep
printf '#!%s/bin/sh\nexec %s/bin/grep -F "$@"\n' $out $out > $out/bin/fgrep
chmod +x $out/bin/gunzip $out/bin/egrep $out/bin/fgrep
cp -r $out/include-glibc/. $out/include/
rm -rf $out/include-glibc
"#;

pub(crate) fn sha256_of(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(with_path(path))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(with_path(path))?;
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let output = command.output().map_err(|e| format!("{what}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Where `seed.pin` is when it isn't beside the work directory: in the set being built.
static PIN: OnceLock<PathBuf> = OnceLock::new();

/// Builds from the seed the file at `path` pins. Only the first call counts.
pub fn set_pin(path: PathBuf) {
    let _ = PIN.set(path);
}

/// Which seed to build from: `seed.pin` holds its hash, and a URL if it can be downloaded.
fn pin(lab: &Lab) -> Result<(String, Option<String>), String> {
    let path = PIN
        .get()
        .cloned()
        .unwrap_or_else(|| lab.work.with_file_name("seed.pin"));
    let text = fs::read_to_string(&path).map_err(with_path(&path))?;
    Ok(parse_pin(&text))
}

fn parse_pin(text: &str) -> (String, Option<String>) {
    let mut fields = text.split_whitespace();
    let hash = fields.next().unwrap_or_default().to_owned();
    (hash, fields.next().map(str::to_owned))
}

/// What to do when something must be built here and the seed isn't here to build it from.
pub const MISSING: &str = "building here starts from the seed, and this machine doesn't have \
it; use a cache that has what's missing, or build where the seed is and apply from there with \
`wrath apply --host`";

fn tarball_path(lab: &Lab, sha256: &str) -> PathBuf {
    lab.work.join(format!("seed/{sha256}.tar.xz"))
}

/// Whether the seed is ready or can be: unpacked, its tarball here, or a URL to download it from.
pub fn available(lab: &Lab) -> Result<bool, String> {
    let (sha256, url) = pin(lab)?;
    Ok(url.is_some() || tarball_path(lab, &sha256).exists() || ready(lab)?)
}

/// The seed's tarball, downloaded the first time if it has a URL and always checked against its
/// hash.
fn tarball(lab: &Lab, sha256: &str, url: Option<&str>) -> Result<PathBuf, String> {
    let path = tarball_path(lab, sha256);
    let dir = lab.work.join("seed");
    fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    if !path.exists() {
        let url = url.ok_or(MISSING)?;
        let partial = dir.join("download");
        run(
            Command::new("curl")
                .args(["-q", "--fail", "--location", "--silent", "--show-error"])
                .args(["--max-time", "600", "--output"])
                .arg(&partial)
                .arg(url),
            "downloading the seed",
        )?;
        fs::rename(&partial, &path).map_err(with_path(&path))?;
    }
    let actual = sha256_of(&path)?;
    if actual != sha256 {
        return Err(format!("the seed's hash is {actual}, expected {sha256}"));
    }
    Ok(path)
}

/// The seed's store path, which keys name it by, whether it's unpacked or not. It covers how the
/// seed is prepared and where the store is, which the prepared seed's programs have in them.
pub fn path(lab: &Lab) -> Result<StorePath, String> {
    Ok(named(&pin(lab)?.0))
}

fn named(sha256: &str) -> StorePath {
    StorePath::derive(
        "seed",
        &[sha256.as_bytes(), PREPARE.as_bytes(), STORE_DIR.as_bytes()],
    )
}

/// Whether the seed is unpacked and ready to run, as builds need it.
pub fn ready(lab: &Lab) -> Result<bool, String> {
    let seed = path(lab)?;
    Ok(lab
        .work
        .join(format!("seed/ready-{}", seed.hash()))
        .exists())
}

/// The seed's store path, unpacked and ready to run. Safe to call from several processes.
pub fn prepare(lab: &Lab) -> Result<StorePath, String> {
    let (sha256, url) = pin(lab)?;
    let seed = named(&sha256);
    fs::create_dir_all(&lab.work).map_err(with_path(&lab.work))?;
    let lock = File::create(lab.work.join("seed.lock")).map_err(|e| format!("seed.lock: {e}"))?;
    lock.lock().map_err(|e| format!("seed.lock: {e}"))?;
    let ready = lab.work.join(format!("seed/ready-{}", seed.hash()));
    if ready.exists() {
        return Ok(seed);
    }

    let tarball = tarball(lab, &sha256, url.as_deref())?;
    let dir = lab.on_host(&seed);
    if dir.exists() {
        run(
            Command::new("chmod").args(["-R", "u+w"]).arg(&dir),
            "clearing the seed",
        )?;
        fs::remove_dir_all(&dir).map_err(with_path(&dir))?;
    }
    fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    run(
        Command::new("tar")
            .arg("-xJf")
            .arg(&tarball)
            .arg("-C")
            .arg(&dir),
        "unpacking the seed",
    )?;
    run(
        Command::new("chmod").args(["-R", "u+w"]).arg(&dir),
        "making the seed writable",
    )?;

    let inside = seed.inside();
    let build_dir = lab.work.join("seed/build");
    fs::create_dir_all(&build_dir).map_err(with_path(&build_dir))?;
    // The script rewrites every program in the seed, so it can't run from one of them.
    fs::copy(dir.join("bin/bash"), build_dir.join("bash"))
        .map_err(|e| format!("copying bash: {e}"))?;
    let log = lab.work.join("seed/prepare.log");
    let outcome = sandbox::run(
        lab,
        &Step {
            name: "prepare the seed".to_owned(),
            argv: vec![
                format!("{inside}/lib/ld-linux-x86-64.so.2"),
                "--library-path".to_owned(),
                format!("{inside}/lib"),
                "/build/bash".to_owned(),
                "-euc".to_owned(),
                PREPARE.to_owned(),
            ],
            inputs: Vec::new(),
            output: Some(seed.clone()),
            view: None,
            build_dir,
            log: log.clone(),
            env: BTreeMap::from([("out".to_owned(), inside)]),
            limits: Limits::DEFAULT,
            links: Vec::new(),
            at: None,
        },
    )?;
    if outcome.status != Status::Passed {
        let text = fs::read_to_string(&log).unwrap_or_default();
        return Err(format!("preparing the seed {:?}:\n{text}", outcome.status));
    }
    fs::write(&ready, "").map_err(with_path(&ready))?;
    Ok(seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_may_have_no_url() {
        assert_eq!(parse_pin("abc\n"), ("abc".to_owned(), None));
    }

    #[test]
    fn a_pin_keeps_where_to_download_it() {
        assert_eq!(
            parse_pin("abc  https://x/y\n"),
            ("abc".to_owned(), Some("https://x/y".to_owned()))
        );
    }

    #[test]
    fn the_seed_is_available_with_a_url_its_tarball_or_unpacked_and_otherwise_not() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let pin = dir.path().join("seed.pin");
        fs::write(&pin, "abc https://x/y\n").unwrap();
        assert!(available(&lab).unwrap());
        fs::write(&pin, "abc\n").unwrap();
        assert!(!available(&lab).unwrap());
        assert_eq!(
            tarball(&lab, "abc", None).unwrap_err(),
            MISSING,
            "a build says what to do instead"
        );
        fs::write(tarball_path(&lab, "abc"), "").unwrap();
        assert!(available(&lab).unwrap());
        fs::remove_file(tarball_path(&lab, "abc")).unwrap();
        let ready = lab.work.join(format!("seed/ready-{}", named("abc").hash()));
        fs::write(ready, "").unwrap();
        assert!(available(&lab).unwrap());
    }
}
