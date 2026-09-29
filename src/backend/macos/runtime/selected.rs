mod bundles;
mod objects;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::{Boundary, Host, NativeRuntime, absolute};
use crate::backend::{RuntimePlan, RuntimeSelection};

const SYSTEM_EXECUTABLE_DIRECTORIES: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

pub(crate) fn prepare(selection: &RuntimeSelection, workspace: &Path) -> Result<RuntimePlan> {
    prepare_for(selection, &Host::read(workspace)?)
}

fn prepare_for(selection: &RuntimeSelection, host: &Host) -> Result<RuntimePlan> {
    ensure!(
        unsafe { libc::getuid() } != 0 && unsafe { libc::geteuid() } != 0,
        "selected runtimes require a non-root host user"
    );
    ensure!(
        !selection.executables.is_empty(),
        "runtime.executables must not be empty"
    );
    ensure!(
        selection.executables.len() <= 256,
        "too many selected native executables"
    );
    ensure!(
        selection.dependency_roots.len() <= 256,
        "too many native dependency roots"
    );
    let boundary = Boundary::new(host)?;
    let mut dependencies = Vec::new();
    for root in &selection.dependency_roots {
        let path = crate::fs_util::expand_host_home(root, &host.home)?;
        boundary.check(&path)?;
        let canonical = path
            .canonicalize()
            .context("resolve native dependency root")?;
        boundary.check(&canonical)?;
        ensure!(
            !canonical.starts_with("/System/Volumes"),
            "native dependency roots cannot use system-volume aliases"
        );
        ensure!(
            canonical.is_dir(),
            "native dependency root must be a directory"
        );
        dependencies.push(canonical);
    }
    let mut directories = Vec::new();
    let mut entries = vec![
        PathBuf::from("/bin/bash"),
        PathBuf::from("/bin/sh"),
        PathBuf::from("/usr/bin/env"),
    ];
    for requested in &selection.executables {
        let path = if requested.is_absolute() || requested.starts_with("~") {
            crate::fs_util::expand_host_home(requested, &host.home)?
        } else {
            ensure!(
                requested.components().count() == 1 && requested.file_name().is_some(),
                "select native executables by absolute path or system command name"
            );
            SYSTEM_EXECUTABLE_DIRECTORIES.into_iter().map(|root| Path::new(root).join(requested))
                .find(|path| super::executable(path))
                .with_context(|| format!("native command {} is not a system executable; select its absolute installation path", requested.display()))?
        };
        absolute(&path)?;
        boundary.check(&path)?;
        super::push_unique(
            &mut directories,
            path.parent()
                .context("executable has no parent")?
                .to_owned(),
        );
        entries.push(path);
    }
    for directory in ["/usr/bin", "/bin"] {
        super::push_unique(&mut directories, PathBuf::from(directory));
    }
    let selected: BTreeSet<_> = entries
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let resources = bundles::discover(&selection.bundles, host, &boundary, &selected)?;
    let files = objects::discover(
        entries,
        resources.native_files,
        &directories,
        &boundary,
        &resources.roots,
        &dependencies,
    )?;
    Ok(RuntimePlan {
        path: std::env::join_paths(directories)?,
        native: NativeRuntime {
            selected_files: files.into_iter().collect(),
            selected_roots: resources.roots,
            system_data: system_locale_data()?,
        },
    })
}

fn system_locale_data() -> Result<Vec<PathBuf>> {
    let root = Path::new("/usr/share/icu");
    let mut files = Vec::new();
    for entry in fs::read_dir(root).context("read system ICU resources")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("icudt") || !name.ends_with(".dat") {
            continue;
        }
        let path = entry.path();
        let canonical = path.canonicalize()?;
        ensure!(
            canonical.starts_with(root),
            "system ICU resource escapes its directory"
        );
        for ancestor in path.ancestors().chain(canonical.ancestors()) {
            let metadata = fs::metadata(ancestor)?;
            ensure!(
                metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
                "system ICU resource has mutable ancestry"
            );
        }
        ensure!(
            entry.file_type()?.is_file(),
            "system ICU resource is not a regular file"
        );
        files.push(canonical);
        ensure!(files.len() <= 32, "too many system ICU resources");
    }
    ensure!(!files.is_empty(), "system ICU data is missing");
    files.sort();
    Ok(files)
}
