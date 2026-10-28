use std::{fs, path::Path, time::SystemTime};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

pub fn file_stamp(path: &Path) -> Result<FileStamp> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    Ok(FileStamp {
        modified: metadata.modified().ok(),
        len: metadata.len(),
    })
}

/// Save source-first text while refusing to overwrite an externally changed file.
pub fn save_source(
    path: &Path,
    saved_source: &str,
    current: &str,
    expected_disk: FileStamp,
) -> Result<FileStamp> {
    let actual_disk = file_stamp(path)?;
    if actual_disk != expected_disk {
        bail!("file changed on disk; autosave stopped before overwriting it");
    }
    let disk_source =
        fs::read_to_string(path).with_context(|| format!("failed to verify {}", path.display()))?;
    if disk_source != saved_source {
        bail!("file contents changed on disk; autosave stopped before overwriting them");
    }
    if current != saved_source {
        atomic_write(path, current)?;
    }
    file_stamp(path)
}

/// Atomically replace a file using a temporary file in the same directory.
pub fn atomic_write(path: &Path, source: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to replace symbolic link {}", path.display());
    }
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }

    let mut temporary = NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create temporary file in {}", parent.display()))?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .context("failed to preserve file permissions")?;
    std::io::Write::write_all(temporary.as_file_mut(), source.as_bytes())
        .context("failed to write temporary file")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to sync temporary file")?;

    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace {}", path.display()))?;
    sync_parent_directory(parent);
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) {
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_source_writes_current_text() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "before").unwrap();
        let stamp = file_stamp(&path).unwrap();

        save_source(&path, "before", "after 中文", stamp).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "after 中文");
    }

    #[test]
    fn external_modification_stops_save() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "before").unwrap();
        let stamp = file_stamp(&path).unwrap();
        fs::write(&path, "external change").unwrap();

        assert!(save_source(&path, "before", "editor change", stamp).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "external change");
    }

    #[test]
    fn same_stamp_metadata_does_not_hide_content_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "before").unwrap();
        fs::write(&path, "EXTERN").unwrap();
        let current_stamp = file_stamp(&path).unwrap();

        assert!(save_source(&path, "before", "editor", current_stamp).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "EXTERN");
    }

    #[test]
    fn atomic_write_replaces_content() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "before").unwrap();
        atomic_write(&path, "after 中文").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "after 中文");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_refuses_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.md");
        let link = directory.path().join("link.md");
        fs::write(&target, "original").unwrap();
        symlink(&target, &link).unwrap();

        assert!(atomic_write(&link, "replacement").is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    }
}
