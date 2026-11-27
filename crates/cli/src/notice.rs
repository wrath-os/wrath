//! What logins show about the machine: a file per subject in `/wrath/system/notice.d`, there only
//! while there's something to say. grapes has PAM show the directory at console and SSH logins.

use std::path::Path;

const DIR: &str = "wrath/system/notice.d";

/// Where an older wrath kept its one notice, removed so logins don't show it beside the new ones.
const OLD: &str = "wrath/system/notice";

/// Says `text` about `subject` as people log in to the machine at `root`, or nothing when `None`.
pub fn set(root: &Path, subject: &str, text: Option<&str>) -> Result<(), String> {
    let path = root.join(DIR).join(subject);
    let failed = |e: std::io::Error| format!("{}: {e}", path.display());
    let old = root.join(OLD);
    remove(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    match text {
        Some(text) => {
            std::fs::create_dir_all(root.join(DIR)).map_err(failed)?;
            std::fs::write(&path, format!("wrath: {text}\n")).map_err(failed)
        }
        None => remove(&path).map_err(failed),
    }
}

/// Removes the file at `path`, if it's there.
pub(crate) fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What logins show about `subject` on the machine at `root`.
#[cfg(test)]
pub fn get(root: &Path, subject: &str) -> Option<String> {
    std::fs::read_to_string(root.join(DIR).join(subject)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_subject_has_its_own_notice_and_the_old_one_goes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("wrath/system")).unwrap();
        std::fs::write(root.path().join(OLD), "wrath: old\n").unwrap();
        set(root.path(), "upgrade", Some("it failed")).unwrap();
        set(root.path(), "boot", Some("it fell back")).unwrap();
        assert!(!root.path().join(OLD).exists());
        assert_eq!(get(root.path(), "upgrade").unwrap(), "wrath: it failed\n");
        set(root.path(), "upgrade", None).unwrap();
        set(root.path(), "upgrade", None).unwrap();
        assert_eq!(get(root.path(), "upgrade"), None);
        assert_eq!(get(root.path(), "boot").unwrap(), "wrath: it fell back\n");
    }
}
