use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use goblin::mach::{Mach, MachO, SingleArch, constants::cputype, header};

use super::super::Boundary;
use super::{SYSTEM_EXECUTABLE_DIRECTORIES, bundles};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Entry,
    Bundled,
    Library,
}

struct Object {
    path: PathBuf,
    executable: Option<PathBuf>,
    runpaths: Vec<PathBuf>,
    kind: Kind,
}

pub(super) fn discover(
    entries: Vec<PathBuf>,
    bundled: Vec<PathBuf>,
    directories: &[PathBuf],
    boundary: &Boundary,
    roots: &[PathBuf],
    dependency_roots: &[PathBuf],
) -> Result<BTreeSet<PathBuf>> {
    ensure!(
        entries.len() + bundled.len() <= 1024,
        "too many native runtime objects"
    );
    let selected: BTreeSet<_> = entries
        .iter()
        .map(|path| path.canonicalize())
        .collect::<std::io::Result<_>>()?;
    let mut queue: Vec<_> = entries
        .into_iter()
        .map(|path| Object {
            path,
            executable: None,
            runpaths: Vec::new(),
            kind: Kind::Entry,
        })
        .chain(bundled.into_iter().map(|path| Object {
            path,
            executable: None,
            runpaths: Vec::new(),
            kind: Kind::Bundled,
        }))
        .collect();
    let mut files = BTreeSet::new();
    let mut visited = BTreeSet::new();
    while let Some(mut object) = queue.pop() {
        boundary.check(&object.path)?;
        let canonical = file_aliases(&object.path, boundary, &mut files)?;
        boundary.check(&canonical)?;
        let metadata = fs::metadata(&canonical)?;
        ensure!(
            metadata.is_file() && (object.kind != Kind::Entry || metadata.mode() & 0o111 != 0),
            "native selection is not an executable file: {}",
            object.path.display()
        );
        ensure!(
            metadata.mode() & 0o6000 == 0,
            "native selection has setuid/setgid bits: {}",
            object.path.display()
        );
        ensure!(
            metadata.nlink() == 1
                || (metadata.uid() == 0
                    && metadata.mode() & 0o022 == 0
                    && SYSTEM_EXECUTABLE_DIRECTORIES
                        .iter()
                        .any(|root| canonical.starts_with(root))),
            "native executable has mutable hard-link aliases: {}",
            object.path.display()
        );
        files.insert(object.path.clone());
        files.insert(canonical.clone());
        if !visited.insert((
            canonical.clone(),
            object.executable.clone(),
            object.runpaths.clone(),
            object.kind,
        )) {
            continue;
        }
        ensure!(
            visited.len() <= 1024 && files.len() <= 4096,
            "native runtime graph is too large"
        );
        ensure!(
            metadata.len() <= 256 * 1024 * 1024,
            "native runtime file is too large"
        );
        let bytes = fs::read(&canonical)?;
        if bytes.starts_with(b"#!") {
            ensure!(object.kind == Kind::Entry, "native library is a script");
            let end = bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(bytes.len());
            ensure!(end <= 4096, "native script interpreter line is too long");
            let words: Vec<_> = std::str::from_utf8(&bytes[2..end])?
                .split_whitespace()
                .collect();
            let interpreter = match words.as_slice() {
                ["/usr/bin/env", name] if !name.starts_with('-') && !name.contains('/') => {
                    directories
                        .iter()
                        .map(|root| root.join(name))
                        .find(|path| super::super::executable(path))
                        .context("script interpreter is missing from the selected runtime PATH")?
                }
                [interpreter] => PathBuf::from(interpreter),
                _ => anyhow::bail!(
                    "native scripts require an absolute interpreter or a simple /usr/bin/env NAME shebang"
                ),
            };
            boundary.check(&interpreter)?;
            ensure!(
                selected.contains(&interpreter.canonicalize()?),
                "select the script interpreter explicitly: {}",
                interpreter.display()
            );
            queue.push(Object {
                path: interpreter,
                executable: None,
                runpaths: Vec::new(),
                kind: Kind::Entry,
            });
            continue;
        }
        let cpu = if cfg!(target_arch = "aarch64") {
            cputype::CPU_TYPE_ARM64
        } else {
            cputype::CPU_TYPE_X86_64
        };
        object.path = canonical;
        let mut binaries = Vec::new();
        match Mach::parse(&bytes).context("native file is not a supported Mach-O or script")? {
            Mach::Binary(binary) => binaries.push(binary),
            Mach::Fat(fat) => {
                for (index, arch) in fat.iter_arches().enumerate() {
                    if arch?.cputype == cpu {
                        let SingleArch::MachO(binary) = fat.get(index)? else {
                            anyhow::bail!("native runtime contains an archive")
                        };
                        binaries.push(binary);
                    }
                }
                ensure!(
                    !binaries.is_empty(),
                    "native runtime file lacks the host architecture"
                );
            }
        }
        for binary in binaries {
            dependencies(
                &binary,
                cpu,
                &object,
                roots,
                dependency_roots,
                boundary,
                &mut queue,
            )?;
        }
    }
    Ok(files)
}

fn file_aliases(
    path: &Path,
    boundary: &Boundary,
    files: &mut BTreeSet<PathBuf>,
) -> Result<PathBuf> {
    let mut path = path.to_owned();
    let mut seen = BTreeSet::new();
    for _ in 0..40 {
        boundary.check(&path)?;
        files.insert(path.clone());
        path = path
            .parent()
            .context("native file has no parent")?
            .canonicalize()?
            .join(path.file_name().context("native file has no name")?);
        boundary.check(&path)?;
        ensure!(seen.insert(path.clone()), "native file link cycle");
        files.insert(path.clone());
        if !fs::symlink_metadata(&path)?.is_symlink() {
            return Ok(path);
        }
        let target = path.parent().unwrap().join(fs::read_link(&path)?);
        let normalized = bundles::normalized(&target)?;
        ensure!(
            target.canonicalize()? == normalized.canonicalize()?,
            "native file link traverses an alias before '..'"
        );
        path = normalized;
    }
    anyhow::bail!("too many native file links")
}

fn dependencies(
    binary: &MachO<'_>,
    cpu: u32,
    object: &Object,
    roots: &[PathBuf],
    dependency_roots: &[PathBuf],
    boundary: &Boundary,
    queue: &mut Vec<Object>,
) -> Result<()> {
    ensure!(
        binary.is_64 && binary.header.cputype == cpu,
        "native runtime requires host-architecture 64-bit Mach-O files"
    );
    ensure!(
        if object.kind == Kind::Entry {
            binary.header.filetype == header::MH_EXECUTE
        } else if object.kind == Kind::Library {
            binary.header.filetype == header::MH_DYLIB
        } else {
            matches!(
                binary.header.filetype,
                header::MH_EXECUTE | header::MH_DYLIB | header::MH_BUNDLE
            )
        },
        "unsupported native Mach-O file type"
    );
    let executable = if binary.header.filetype == header::MH_EXECUTE {
        Some(object.path.clone())
    } else {
        object.executable.clone()
    };
    let mut runpaths = Vec::new();
    for path in &binary.rpaths {
        let path = expand(path, &object.path, executable.as_deref())?;
        boundary.check(&path)?;
        super::super::push_unique(&mut runpaths, path);
    }
    for path in &object.runpaths {
        super::super::push_unique(&mut runpaths, path.clone());
    }
    ensure!(runpaths.len() <= 128, "native runpath stack is too large");
    for library in binary.libs.iter().skip(1) {
        let candidates = if let Some(suffix) = library.strip_prefix("@rpath/") {
            runpaths.iter().map(|root| root.join(suffix)).collect()
        } else {
            vec![expand(library, &object.path, executable.as_deref())?]
        };
        let mut resolved = false;
        for candidate in candidates {
            let path = bundles::normalized(&candidate)?;
            boundary.check(&path)?;
            if path.starts_with("/usr/lib") || path.starts_with("/System/Library") {
                resolved = true;
                break;
            }
            if !path.try_exists()? {
                continue;
            }
            let canonical = path.canonicalize()?;
            boundary.check(&canonical)?;
            ensure!(
                candidate.canonicalize()? == canonical,
                "native library path traverses an alias before '..'"
            );
            ensure!(
                roots
                    .iter()
                    .chain(dependency_roots)
                    .any(|root| canonical.starts_with(root)),
                "native non-system library is outside selected resources: {library}"
            );
            ensure!(
                queue.len() < 1024,
                "native runtime dependency queue is too large"
            );
            queue.push(Object {
                path,
                executable: executable.clone(),
                runpaths: runpaths.clone(),
                kind: Kind::Library,
            });
            resolved = true;
            break;
        }
        ensure!(
            resolved,
            "native non-system library could not be resolved from declared runpaths: {library}"
        );
    }
    Ok(())
}

fn expand(value: &str, loader: &Path, executable: Option<&Path>) -> Result<PathBuf> {
    let path = if let Some(suffix) = value
        .strip_prefix("@loader_path")
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
    {
        loader
            .parent()
            .context("native loader has no parent")?
            .join(suffix.trim_start_matches('/'))
    } else if let Some(suffix) = value
        .strip_prefix("@executable_path")
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
    {
        executable
            .context("bundled plugin needs an explicit executable context for @executable_path")?
            .parent()
            .context("native executable has no parent")?
            .join(suffix.trim_start_matches('/'))
    } else {
        let path = PathBuf::from(value);
        ensure!(
            path.is_absolute(),
            "unsupported native library path: {value}"
        );
        path
    };
    let normalized = bundles::normalized(&path)?;
    if path != normalized {
        let exists = path.try_exists()?;
        ensure!(
            exists == normalized.try_exists()?,
            "native library traversal changes path lookup"
        );
        if exists {
            ensure!(
                path.canonicalize()? == normalized.canonicalize()?,
                "native library path traverses an alias before '..'"
            );
        }
    }
    Ok(normalized)
}
