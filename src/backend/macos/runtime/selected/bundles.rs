use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::super::{Boundary, Host, absolute};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(super) struct Resources {
    pub roots: Vec<PathBuf>,
    pub native_files: Vec<PathBuf>,
}

pub(super) fn discover(
    requested: &[PathBuf],
    host: &Host,
    boundary: &Boundary,
    executables: &BTreeSet<PathBuf>,
) -> Result<Resources> {
    ensure!(requested.len() <= 256, "too many native runtime bundles");
    let mut roots = BTreeSet::new();
    for requested in requested {
        let path = crate::fs_util::expand_host_home(requested, &host.home)?;
        boundary.check(&path)?;
        let canonical = path
            .canonicalize()
            .context("resolve native runtime bundle")?;
        boundary.check(&canonical)?;
        ensure!(
            canonical.is_dir(),
            "native runtime bundle must be a directory"
        );
        let identity = super::super::file_id(&canonical)?;
        for broad in [
            "/",
            "/Applications",
            "/System",
            "/Library",
            "/Users",
            "/Volumes",
            "/usr",
            "/usr/bin",
            "/usr/sbin",
            "/usr/lib",
            "/usr/libexec",
            "/usr/share",
            "/usr/local",
            "/usr/local/bin",
            "/usr/local/lib",
            "/usr/local/share",
            "/opt",
            "/opt/homebrew",
            "/opt/homebrew/bin",
            "/opt/homebrew/lib",
            "/opt/homebrew/share",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/opt",
            "/bin",
            "/sbin",
            "/private",
            "/private/tmp",
            "/private/var",
            "/private/var/tmp",
            "/nix",
            "/nix/store",
        ] {
            ensure!(
                super::super::file_id(Path::new(broad))? != identity,
                "select an application bundle, not a broad host directory: {}",
                path.display()
            );
        }
        // Use the logical installation path, not an alternate spelling through the data volume.
        ensure!(
            !canonical.starts_with("/System/Volumes"),
            "native bundles cannot use system-volume aliases"
        );
        roots.insert(path);
        roots.insert(canonical);
    }
    let mut pending = Vec::new();
    for root in &roots {
        let canonical = root.canonicalize()?;
        ensure_no_submounts(&canonical)?;
        pending.push((canonical.clone(), 0, fs::metadata(&canonical)?.dev()));
    }
    let mut visited = BTreeSet::new();
    let mut native_files = Vec::new();
    while let Some((path, depth, device)) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        ensure!(
            visited.len() <= 100_000 && depth <= 128,
            "native bundle tree is too large"
        );
        boundary.check(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(
            metadata.dev() == device,
            "native bundle contains a filesystem boundary"
        );
        if metadata.is_symlink() {
            let target = path
                .parent()
                .context("bundle link has no parent")?
                .join(fs::read_link(&path)?);
            let lexical = normalized(&target)?;
            boundary.check(&lexical)?;
            let canonical = target
                .canonicalize()
                .context("resolve native bundle link")?;
            boundary.check(&canonical)?;
            ensure!(
                lexical.canonicalize()? == canonical,
                "bundle link traverses an alias before '..'"
            );
            ensure!(
                roots.iter().any(|root| canonical.starts_with(root))
                    || executables.contains(&canonical),
                "native bundle link escapes selected resources: {} -> {}",
                path.display(),
                canonical.display()
            );
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                ensure!(pending.len() < 100_000, "native bundle tree is too large");
                pending.push((entry?.path(), depth + 1, device));
            }
        } else {
            ensure!(
                metadata.is_file(),
                "native bundle contains a special file: {}",
                path.display()
            );
            ensure!(
                metadata.mode() & 0o6000 == 0,
                "native bundle contains setuid/setgid bits: {}",
                path.display()
            );
            ensure!(
                metadata.nlink() == 1,
                "native bundle file has hard-link aliases: {}",
                path.display()
            );
            let mut magic = Vec::new();
            fs::File::open(&path)?.take(4).read_to_end(&mut magic)?;
            let thin = matches!(magic.as_slice(), b"\xcf\xfa\xed\xfe" | b"\xfe\xed\xfa\xcf");
            let fat = matches!(
                magic.as_slice(),
                b"\xca\xfe\xba\xbe"
                    | b"\xbe\xba\xfe\xca"
                    | b"\xca\xfe\xba\xbf"
                    | b"\xbf\xba\xfe\xca"
            );
            if thin
                || (fat
                    && (metadata.mode() & 0o111 != 0
                        || matches!(
                            path.extension().and_then(|extension| extension.to_str()),
                            Some("node" | "dylib")
                        )))
            {
                native_files.push(path);
            }
        }
    }
    Ok(Resources {
        roots: roots.into_iter().collect(),
        native_files,
    })
}

pub(super) fn normalized(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "native resource path must be absolute");
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                ensure!(result.pop(), "native resource traverses above root");
            }
            Component::CurDir => {}
            part => result.push(part.as_os_str()),
        }
    }
    absolute(&result)?;
    Ok(result)
}

fn ensure_no_submounts(root: &Path) -> Result<()> {
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    ensure!(
        (0..4096).contains(&count),
        "cannot enumerate native filesystem mounts"
    );
    let capacity = count as usize + 16;
    let mut mounts = Vec::<libc::statfs>::with_capacity(capacity);
    let count = unsafe {
        libc::getfsstat(
            mounts.as_mut_ptr(),
            (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int,
            libc::MNT_NOWAIT,
        )
    };
    ensure!(
        count >= 0 && (count as usize) < capacity,
        "native filesystem mounts changed during inspection"
    );
    unsafe {
        mounts.set_len(count as usize);
    }
    for mount in mounts {
        let path = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) }.to_str()?;
        let path = Path::new(path);
        ensure!(
            path == root || !path.starts_with(root),
            "native bundle contains a submount: {}",
            path.display()
        );
    }
    Ok(())
}
