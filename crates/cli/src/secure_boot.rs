//! Secure Boot keys for one machine, made before it's installed and kept encrypted beside its host
//! key: the PK and KEK, which change what its firmware trusts, for its admins only, and the db key,
//! which signs what it boots, and the PCR key, which signs its TPM policy, for the machine too.
//! The firmware takes keys as signed variable updates, `.auth` files, which are made here as well,
//! so systemd-boot can enroll them when the firmware is in setup mode.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// The archive of what the machine itself gets, encrypted to the configuration's recipients.
pub const MACHINE_ARCHIVE: &str = wrath_eval::SECURE_BOOT_KEYS;

/// The archive of the PK and KEK, encrypted to every recipient but the machine.
pub const OWNER_ARCHIVE: &str = "secure-boot-owner.age";

/// The db key and certificate sign what the machine boots; the PCR key signs its TPM policy; the
/// `.auth` files enroll the certificates.
pub const MACHINE_FILES: [&str; 7] = [
    "db.key", "db.crt", "pcr.key", "pcr.pem", "PK.auth", "KEK.auth", "db.auth",
];

/// What only admins keep: enough to change what the firmware trusts later.
pub const OWNER_FILES: [&str; 4] = ["PK.key", "PK.crt", "KEK.key", "KEK.crt"];

/// The variables the firmware keeps keys in, with their vendor GUID and the key whose
/// signature it accepts in setup mode, as the chain of trust goes: PK signs itself and KEK, KEK
/// signs db.
const VARIABLES: [(&str, &str, &str); 3] = [
    ("PK", GLOBAL_VARIABLE, "PK"),
    ("KEK", GLOBAL_VARIABLE, "PK"),
    ("db", IMAGE_SECURITY_DATABASE, "KEK"),
];

const GLOBAL_VARIABLE: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";
const IMAGE_SECURITY_DATABASE: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";
const CERT_X509: &str = "a5c059a1-94e4-4aa7-87b5-ab155c2bf072";
const CERT_TYPE_PKCS7: &str = "4aafd29d-68df-49ee-8aa9-347d375665a7";

/// Non-volatile, readable at boot and after, and written only with a time-based signature.
const ATTRIBUTES: u32 = 0x01 | 0x02 | 0x04 | 0x20;

/// The keys of a machine, as two tar archives: what the machine gets and what only admins keep.
pub struct Keys {
    pub machine: Vec<u8>,
    pub owner: Vec<u8>,
}

/// Makes machine `name`'s keys in `dir`, a private directory, with `openssl`.
pub fn make(name: &str, dir: &Path) -> Result<Keys, String> {
    for (key, title) in [
        ("PK", "Platform Key"),
        ("KEK", "Key Exchange Key"),
        ("db", "db"),
    ] {
        openssl(
            dir,
            &[
                "req", "-new", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256", "-days",
                "7300", "-subj",
            ],
            &[
                &format!("/CN={name} Secure Boot {title}"),
                "-keyout",
                &format!("{key}.key"),
                "-out",
                &format!("{key}.crt"),
            ],
        )?;
        openssl(
            dir,
            &["x509", "-outform", "DER"],
            &["-in", &format!("{key}.crt"), "-out", &format!("{key}.der")],
        )?;
    }
    openssl(
        dir,
        &[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
        ],
        &["-out", "pcr.key"],
    )?;
    openssl(
        dir,
        &["pkey", "-pubout"],
        &["-in", "pcr.key", "-out", "pcr.pem"],
    )?;

    let owner = owner_guid()?;
    let time = efi_time(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs(),
    );
    for (variable, vendor, signer) in VARIABLES {
        let list = signature_list(owner, &read(dir, &format!("{variable}.der"))?);
        let payload = dir.join(format!("{variable}.payload"));
        fs::write(&payload, signed_bytes(variable, guid(vendor), time, &list))
            .map_err(|e| format!("{}: {e}", payload.display()))?;
        openssl(
            dir,
            &[
                "smime", "-sign", "-binary", "-noattr", "-md", "sha256", "-outform", "DER",
            ],
            &[
                "-signer",
                &format!("{signer}.crt"),
                "-inkey",
                &format!("{signer}.key"),
                "-in",
                &format!("{variable}.payload"),
                "-out",
                &format!("{variable}.p7"),
            ],
        )?;
        let auth = authenticated(time, &read(dir, &format!("{variable}.p7"))?, &list);
        let file = dir.join(format!("{variable}.auth"));
        fs::write(&file, auth).map_err(|e| format!("{}: {e}", file.display()))?;
    }
    Ok(Keys {
        machine: archive(dir, &MACHINE_FILES)?,
        owner: archive(dir, &OWNER_FILES)?,
    })
}

/// systemd's signer, in a system.
const SBSIGN: &str = "systemd/lib/systemd/systemd-sbsign";

/// systemd's tool for the PCR values an image gives, and signing a TPM policy for them.
const MEASURE: &str = "systemd/lib/systemd/systemd-measure";

/// Where the switch keeps the public half of the machine's PCR key, under its root, for enrolling
/// the disks to the policies it signs. The stub hands it only to the initrd.
pub const PCR_PUBLIC_KEY: &str = "var/lib/wrath/tpm2-pcr-public-key.pem";

/// Signs what a machine boots with its db key, and its images' TPM policies with its PCR key,
/// both decrypted from the system it switches to into a directory only root reads, which goes when
/// the signer does.
pub struct Signer {
    dir: tempfile::TempDir,
    sbsign: PathBuf,
    measure: PathBuf,
    /// The start of the db certificate's SHA-256, which names what it signed, so a new key signs
    /// everything again.
    pub fingerprint: String,
    /// The `.auth` files that enroll the machine's keys, by name.
    pub enrollment: Vec<(String, Vec<u8>)>,
}

impl Signer {
    /// The signer for the machine at `root`, from the keys `system` carries for it, when `system`
    /// boots with Secure Boot. A system that has no keys for this machine boots unsigned, which is
    /// said.
    pub fn of(root: &Path, system: &Path) -> Result<Option<Self>, String> {
        let spec = system.join("activation.json");
        let Ok(text) = fs::read_to_string(&spec) else {
            return Ok(None);
        };
        let activation: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", spec.display()))?;
        if activation["secure_boot"] != Value::Bool(true) {
            return Ok(None);
        }
        let machine = machine_name(root, system);
        let keys = activation["secrets"].as_object().and_then(|secrets| {
            secrets
                .values()
                .find(|secret| secret["machine"].as_str() == Some(machine.as_str()))
        });
        let Some(keys) = keys else {
            crate::style::note(&format!(
                "this system boots with Secure Boot, but has no keys for {machine}, so what it \
                 boots isn't signed; `wrath secret machine {machine}` makes them"
            ));
            return Ok(None);
        };
        let at_root = |path: &str| root.join(path.trim_start_matches('/'));
        let mut age = Command::new(at_root(activation["age"].as_str().unwrap_or_default()));
        age.arg("--decrypt");
        for identity in activation["identities"].as_array().into_iter().flatten() {
            let identity = at_root(identity.as_str().unwrap_or_default());
            if identity.exists() {
                age.arg("--identity").arg(identity);
            }
        }
        let output = age
            .arg(system.join(keys["source"].as_str().unwrap_or_default()))
            .output()
            .map_err(|e| format!("cannot run age: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cannot decrypt {machine}'s Secure Boot keys: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let runtime = Path::new("/run");
        let dir = tempfile::Builder::new()
            .prefix("wrath-secure-boot")
            .tempdir_in(if runtime.is_dir() {
                runtime.to_path_buf()
            } else {
                std::env::temp_dir()
            })
            .map_err(|e| format!("cannot make a private directory: {e}"))?;
        let mut enrollment = Vec::new();
        let mut fingerprint = String::new();
        for (name, bytes) in unpack(&output.stdout)? {
            match name.as_str() {
                "db.key" | "db.crt" | "pcr.key" | "pcr.pem" => {
                    let path = dir.path().join(&name);
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)
                        .and_then(|mut file| file.write_all(&bytes))
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if name == "db.crt" {
                        let digest = <sha2::Sha256 as sha2::Digest>::digest(&bytes);
                        fingerprint = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
                    }
                }
                _ if name.ends_with(".auth") => enrollment.push((name, bytes)),
                _ => {}
            }
        }
        Ok(Some(Self {
            dir,
            sbsign: system.join(SBSIGN),
            measure: system.join(MEASURE),
            fingerprint,
            enrollment,
        }))
    }

    /// The PCR key's public half, in PEM.
    pub fn pcr_public_key(&self) -> Result<Vec<u8>, String> {
        read(self.dir.path(), "pcr.pem")
    }

    /// The PCR key's public half and a policy signed with it for the PCR 11 values an image of
    /// `parts` gives in its initrd only, `enter-initrd`: once the initrd hands over, PCR 11 moves
    /// on and the disks' TPM key no longer unseals.
    pub fn pcr_policy(
        &self,
        parts: &wrath_build::uki::Parts,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let dir = self.dir.path();
        let public = self.pcr_public_key()?;
        let measured = [
            ("linux", parts.linux),
            ("osrel", parts.os_release),
            ("cmdline", parts.cmdline),
            ("initrd", parts.initrd),
            ("pcrpkey", public.as_slice()),
        ];
        let mut command = Command::new(&self.measure);
        command.arg("sign");
        for (section, bytes) in measured {
            let path = dir.join(format!("section.{section}"));
            fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            command.arg(format!("--{section}={}", path.display()));
        }
        let output = command
            .arg(format!("--private-key={}", dir.join("pcr.key").display()))
            .arg(format!("--public-key={}", dir.join("pcr.pem").display()))
            .args(["--phase=enter-initrd", "--bank=sha256", "--json=short"])
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.measure.display()))?;
        for (section, _) in measured {
            let _ = fs::remove_file(dir.join(format!("section.{section}")));
        }
        if !output.status.success() {
            return Err(format!(
                "cannot sign the TPM policy: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok((public, output.stdout))
    }

    /// A path in the signer's private directory, off the ESP, for an image while it's signed.
    pub fn scratch(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Writes `from`, signed, to `to`.
    pub fn sign(&self, from: &Path, to: &Path) -> Result<(), String> {
        let key = self.dir.path().join("db.key");
        let certificate = self.dir.path().join("db.crt");
        let output = Command::new(&self.sbsign)
            .arg("sign")
            .arg(format!("--private-key={}", key.display()))
            .arg(format!("--certificate={}", certificate.display()))
            .arg(format!("--output={}", to.display()))
            .arg(from)
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.sbsign.display()))?;
        if !output.status.success() {
            return Err(format!(
                "cannot sign {}: {}",
                from.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
impl Signer {
    /// A signer that appends `signed` to what it signs and signs every TPM policy as `policy`,
    /// for tests: no keys, no systemd.
    pub(crate) fn fake(enrollment: Vec<(String, Vec<u8>)>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, text: &str| {
            let path = dir.path().join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            path
        };
        let sbsign = script(
            "sbsign",
            "#!/bin/sh\nfor arg; do case $arg in --output=*) out=${arg#--output=};; esac; done\n\
             eval \"in=\\${$#}\"\ncat \"$in\" > \"$out\" && printf signed >> \"$out\"\n",
        );
        let measure = script("measure", "#!/bin/sh\nprintf policy\n");
        fs::write(dir.path().join("pcr.pem"), "public key").unwrap();
        Self {
            dir,
            sbsign,
            measure,
            fingerprint: "0123abcd".to_owned(),
            enrollment,
        }
    }
}

/// The machine at `root`'s name: the one it was given at install, or else its hostname, as
/// `system` sets it or the machine has it.
fn machine_name(root: &Path, system: &Path) -> String {
    let given = crate::machine::Given::read(
        &root.join(crate::machine::MACHINE_FILE.trim_start_matches('/')),
    )
    .ok()
    .and_then(|given| given.name);
    let hostname = |path: PathBuf| {
        fs::read_to_string(path)
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    };
    given
        .or_else(|| hostname(system.join("etc/hostname")))
        .or_else(|| hostname(root.join("etc/hostname")))
        .unwrap_or_else(|| {
            rustix::system::uname()
                .nodename()
                .to_string_lossy()
                .into_owned()
        })
}

/// The files of a tar archive [`make`] made, by name.
pub fn unpack(archive: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut files = Vec::new();
    let mut tar = tar::Archive::new(archive);
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = entry.path().map_err(|e| e.to_string())?;
        let name = name.to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        files.push((name, bytes));
    }
    Ok(files)
}

fn archive(dir: &Path, names: &[&str]) -> Result<Vec<u8>, String> {
    let mut tar = tar::Builder::new(Vec::new());
    for name in names {
        let bytes = read(dir, name)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .map_err(|e| e.to_string())?;
    }
    tar.into_inner().map_err(|e| e.to_string())
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = dir.join(name);
    fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn openssl(dir: &Path, args: &[&str], more: &[&str]) -> Result<(), String> {
    let output = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .args(more)
        .output()
        .map_err(|e| format!("cannot run openssl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "openssl {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

/// Who the certificates are listed as belonging to, which the firmware only keeps.
fn owner_guid() -> Result<[u8; 16], String> {
    let mut bytes = [0; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(bytes)
}

/// A GUID as EFI lays it out: the first three fields little-endian, the rest as written.
fn guid(text: &str) -> [u8; 16] {
    let hex: Vec<u8> = text
        .split('-')
        .flat_map(|part| {
            (0..part.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&part[at..at + 2], 16).expect("a GUID is hex"))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut bytes = [0; 16];
    bytes[..4].copy_from_slice(&[hex[3], hex[2], hex[1], hex[0]]);
    bytes[4..6].copy_from_slice(&[hex[5], hex[4]]);
    bytes[6..8].copy_from_slice(&[hex[7], hex[6]]);
    bytes[8..].copy_from_slice(&hex[8..]);
    bytes
}

/// `seconds` since the epoch as an `EFI_TIME` in UTC, which a time-based signature carries.
fn efi_time(seconds: u64) -> [u8; 16] {
    let days = (seconds / 86_400) as i64;
    let in_day = seconds % 86_400;
    // Days to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let of_era = z - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * of_year + 2) / 153;
    let day = of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let mut time = [0; 16];
    time[..2].copy_from_slice(&(year as u16).to_le_bytes());
    time[2] = month as u8;
    time[3] = day as u8;
    time[4] = (in_day / 3600) as u8;
    time[5] = (in_day % 3600 / 60) as u8;
    time[6] = (in_day % 60) as u8;
    time
}

/// An `EFI_SIGNATURE_LIST` with one X.509 certificate, `cert` in DER, owned by `owner`.
fn signature_list(owner: [u8; 16], cert: &[u8]) -> Vec<u8> {
    let entry = 16 + cert.len() as u32;
    let mut list = Vec::with_capacity(28 + entry as usize);
    list.extend_from_slice(&guid(CERT_X509));
    list.extend_from_slice(&(28 + entry).to_le_bytes());
    list.extend_from_slice(&0u32.to_le_bytes());
    list.extend_from_slice(&entry.to_le_bytes());
    list.extend_from_slice(&owner);
    list.extend_from_slice(cert);
    list
}

/// What a time-based signature on setting `variable` to `data` covers: its name in UTF-16
/// without the terminator, its vendor, its attributes, the time and the data.
fn signed_bytes(variable: &str, vendor: [u8; 16], time: [u8; 16], data: &[u8]) -> Vec<u8> {
    let mut bytes: Vec<u8> = variable.encode_utf16().flat_map(u16::to_le_bytes).collect();
    bytes.extend_from_slice(&vendor);
    bytes.extend_from_slice(&ATTRIBUTES.to_le_bytes());
    bytes.extend_from_slice(&time);
    bytes.extend_from_slice(data);
    bytes
}

/// An `.auth` file: `EFI_VARIABLE_AUTHENTICATION_2`, the time and a PKCS #7 signature, followed by
/// the data it signs.
fn authenticated(time: [u8; 16], pkcs7: &[u8], data: &[u8]) -> Vec<u8> {
    let mut auth = Vec::with_capacity(16 + 24 + pkcs7.len() + data.len());
    auth.extend_from_slice(&time);
    auth.extend_from_slice(&(24 + pkcs7.len() as u32).to_le_bytes());
    // WIN_CERT_REVISION_2_0, WIN_CERT_TYPE_EFI_GUID.
    auth.extend_from_slice(&0x0200u16.to_le_bytes());
    auth.extend_from_slice(&0x0ef1u16.to_le_bytes());
    auth.extend_from_slice(&guid(CERT_TYPE_PKCS7));
    auth.extend_from_slice(pkcs7);
    auth.extend_from_slice(data);
    auth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_are_laid_out_as_efi_does() {
        assert_eq!(
            guid(GLOBAL_VARIABLE),
            [
                0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03,
                0x2b, 0x8c
            ]
        );
    }

    #[test]
    fn a_time_is_its_utc_date_and_clock() {
        // 2026-11-19 10:58:40 UTC.
        let time = efi_time(1_795_085_920);
        assert_eq!(u16::from_le_bytes([time[0], time[1]]), 2026);
        assert_eq!(&time[2..7], &[11, 19, 10, 58, 40]);
        assert_eq!(&time[7..], &[0; 9]);
        // A leap day.
        assert_eq!(&efi_time(951_782_400)[..4], &[0xd0, 0x07, 2, 29]);
    }

    #[test]
    fn a_signature_list_holds_one_certificate_and_says_its_sizes() {
        let list = signature_list([7; 16], b"cert");
        assert_eq!(&list[..16], &guid(CERT_X509));
        assert_eq!(&list[16..28], &[48, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0]);
        assert_eq!(&list[28..44], &[7; 16]);
        assert_eq!(&list[44..], b"cert");
    }

    #[test]
    fn what_is_signed_names_the_variable_and_ends_with_the_data() {
        let bytes = signed_bytes("db", guid(IMAGE_SECURITY_DATABASE), [1; 16], b"list");
        assert_eq!(&bytes[..4], b"d\0b\0");
        assert_eq!(&bytes[4..20], &guid(IMAGE_SECURITY_DATABASE));
        assert_eq!(&bytes[20..24], &[0x27, 0, 0, 0]);
        assert_eq!(&bytes[24..40], &[1; 16]);
        assert_eq!(&bytes[40..], b"list");
    }

    #[test]
    fn an_auth_file_carries_the_signature_ahead_of_the_data() {
        let auth = authenticated([1; 16], b"sig", b"list");
        assert_eq!(&auth[..16], &[1; 16]);
        assert_eq!(&auth[16..24], &[27, 0, 0, 0, 0x00, 0x02, 0xf1, 0x0e]);
        assert_eq!(&auth[24..40], &guid(CERT_TYPE_PKCS7));
        assert_eq!(&auth[40..], b"siglist");
    }

    #[test]
    fn a_machines_keys_come_in_two_archives() {
        let dir = tempfile::tempdir().unwrap();
        let keys = make("t-01", dir.path()).unwrap();
        let names = |archive: &[u8]| -> Vec<String> {
            unpack(archive)
                .unwrap()
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        assert_eq!(names(&keys.machine), MACHINE_FILES);
        assert_eq!(names(&keys.owner), OWNER_FILES);
        let files = unpack(&keys.machine).unwrap();
        let db = &files.iter().find(|(name, _)| name == "db.auth").unwrap().1;
        // The signed list is the db certificate, after the time and the signature.
        let cert = fs::read(dir.path().join("db.der")).unwrap();
        assert!(db.ends_with(&cert));
    }
}
