#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use goblin::mach::{Mach, MachO, SingleArch, constants::cputype, header};

use super::{Boundary, DeveloperTools, Host, NativeRuntime, absolute};
use crate::backend::{RuntimePlan, RuntimeSelection};

const SYSTEM_EXECUTABLE_DIRECTORIES: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];

pub(crate) fn prepare(selection: &RuntimeSelection, workspace: &Path) -> Result<RuntimePlan> {
    let host = Host::read(workspace)?;
    prepare_for(selection, &host)
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
        selection.bundles.is_empty() && selection.dependency_roots.is_empty(),
        "native selected runtimes currently support executable files with system libraries only; bundles and dependency_roots are not implemented on macOS"
    );
    ensure!(
        selection.executables.len() <= 256,
        "too many selected native executables"
    );
    let boundary = Boundary::new(host)?;
    let mut files = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut directories = Vec::new();
    let mut queue = vec![
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
            SYSTEM_EXECUTABLE_DIRECTORIES
                .into_iter().map(|root| Path::new(root).join(requested)).find(|path| super::executable(path))
                .with_context(|| format!("native command {} is not a system executable; select its absolute installation path", requested.display()))?
        };
        absolute(&path)?;
        super::push_unique(
            &mut directories,
            path.parent()
                .context("executable has no parent")?
                .to_owned(),
        );
        queue.push(path);
    }
    while let Some(path) = queue.pop() {
        boundary.check(&path)?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("resolve native executable {}", path.display()))?;
        boundary.check(&canonical)?;
        let metadata = fs::metadata(&canonical)?;
        ensure!(
            metadata.is_file() && metadata.mode() & 0o111 != 0,
            "native selection is not an executable file: {}",
            path.display()
        );
        ensure!(
            metadata.mode() & 0o6000 == 0,
            "native selection has setuid/setgid bits: {}",
            path.display()
        );
        // Root ownership and mode bits alone do not exclude writable macOS ACL aliases.
        ensure!(
            metadata.nlink() == 1
                || (metadata.uid() == 0
                    && metadata.mode() & 0o022 == 0
                    && SYSTEM_EXECUTABLE_DIRECTORIES
                        .iter()
                        .any(|root| canonical.starts_with(root))),
            "native executable has mutable hard-link aliases: {}",
            path.display()
        );
        files.insert(path.clone());
        files.insert(canonical.clone());
        if !visited.insert(canonical.clone()) {
            continue;
        }
        ensure!(files.len() <= 1024, "native runtime graph is too large");
        ensure!(
            metadata.len() <= 256 * 1024 * 1024,
            "native executable is too large"
        );
        let bytes = fs::read(&canonical)?;
        if bytes.starts_with(b"#!") {
            let end = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(bytes.len());
            ensure!(end <= 4096, "native script interpreter line is too long");
            let interpreter = std::str::from_utf8(&bytes[2..end])?.trim();
            ensure!(
                !interpreter.contains(char::is_whitespace),
                "native scripts require a simple absolute shebang without arguments"
            );
            let interpreter = PathBuf::from(interpreter);
            absolute(&interpreter)?;
            ensure!(
                matches!(interpreter.to_str(), Some("/bin/sh" | "/bin/bash")),
                "native script interpreter must be /bin/sh or /bin/bash; invoke other explicitly selected interpreters directly"
            );
            continue;
        }
        let cpu = if cfg!(target_arch = "aarch64") {
            cputype::CPU_TYPE_ARM64
        } else {
            cputype::CPU_TYPE_X86_64
        };
        match Mach::parse(&bytes)
            .context("selected executable is not a supported Mach-O or shell script")?
        {
            Mach::Binary(binary) => validate_binary(&binary, cpu)?,
            Mach::Fat(fat) => {
                let mut found = false;
                for (index, arch) in fat.iter_arches().enumerate() {
                    if arch?.cputype == cpu {
                        let SingleArch::MachO(binary) = fat.get(index)? else {
                            anyhow::bail!("native runtime contains an archive")
                        };
                        validate_binary(&binary, cpu)?;
                        found = true;
                    }
                }
                ensure!(found, "native executable lacks the host architecture");
            }
        }
    }
    for directory in ["/usr/bin", "/bin"] {
        super::push_unique(&mut directories, PathBuf::from(directory));
    }
    let path: OsString = std::env::join_paths(directories)?;
    Ok(RuntimePlan {
        path,
        native: NativeRuntime {
            config: None,
            tools: DeveloperTools::default(),
            selected_files: files.into_iter().collect(),
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

fn validate_binary(binary: &MachO<'_>, cpu: u32) -> Result<()> {
    ensure!(
        binary.is_64
            && binary.header.cputype == cpu
            && binary.header.filetype == header::MH_EXECUTE,
        "native runtime requires a host-architecture 64-bit Mach-O executable"
    );
    for library in binary.libs.iter().skip(1) {
        let path = Path::new(library);
        absolute(path)?;
        ensure!(
            path.starts_with("/usr/lib") || path.starts_with("/System/Library"),
            "native executable needs a non-system library not supported by this runtime: {library}"
        );
    }
    Ok(())
}
