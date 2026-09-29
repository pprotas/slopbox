use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(any(target_os = "linux", test))]
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

pub(crate) fn expand_host_home(path: &Path, home: &Path) -> Result<PathBuf> {
    if path == Path::new("~") {
        return Ok(home.to_path_buf());
    }
    if let Ok(relative) = path.strip_prefix("~/") {
        return Ok(home.join(relative));
    }
    ensure!(
        path.is_absolute(),
        "configured path must be absolute or start with ~/"
    );
    Ok(path.to_path_buf())
}

pub(crate) fn read_text(path: &Path, description: &str) -> Result<String> {
    const MAX_TEXT_SIZE: u64 = 1024 * 1024;
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {description} {}", path.display()))?;
    ensure!(
        metadata.len() <= MAX_TEXT_SIZE,
        "{description} exceeds the 1 MiB import limit"
    );
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {description} {}", path.display()))
}

pub(crate) fn write_private(path: &Path, contents: &str) -> Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    output.write_all(contents.as_bytes())?;
    output.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to install {}", path.display()))?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn find_socket(root: &Path) -> Result<Option<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).with_context(|| {
            format!(
                "failed to inspect workspace directory {}",
                directory.display()
            )
        })?;
        for entry in entries {
            let entry =
                entry.with_context(|| format!("failed to inspect {}", directory.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to inspect file type for {}", path.display()))?;
            if file_type.is_socket() {
                return Ok(Some(path));
            }
            if file_type.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_unix_socket() {
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("service.sock");
        let _listener = UnixListener::bind(&socket).unwrap();

        assert_eq!(find_socket(directory.path()).unwrap(), Some(socket));
    }

    #[test]
    fn ignores_symlink_to_socket() {
        use std::os::unix::fs::symlink;
        use std::os::unix::net::UnixListener;

        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let socket = outside.path().join("service.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        symlink(&socket, directory.path().join("link.sock")).unwrap();

        assert_eq!(find_socket(directory.path()).unwrap(), None);
    }
}
