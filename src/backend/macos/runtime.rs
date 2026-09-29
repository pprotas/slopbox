use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use crate::backend::RuntimePlan;
use anyhow::{Context, Result, ensure};

pub(crate) mod selected;

#[derive(Default)]
pub(crate) struct NativeRuntime {
    pub selected_files: Vec<PathBuf>,
    pub selected_roots: Vec<PathBuf>,
    pub system_data: Vec<PathBuf>,
}

// Explicit inputs make discovery testable without changing the test process's
// environment or granting access to real host credentials.
struct Host {
    home: PathBuf,
    workspace: PathBuf,
    config: PathBuf,
    data: PathBuf,
}

impl Host {
    fn read(workspace: &Path) -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        absolute(&home)?;
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        absolute(&config)?;
        absolute(&data)?;
        Ok(Self {
            home,
            workspace: workspace.to_owned(),
            config,
            data,
        })
    }
}

struct Boundary {
    home: PathBuf,
    protected: Vec<PathBuf>,
    protected_ids: BTreeSet<(u64, u64)>,
    container_ids: BTreeSet<(u64, u64)>,
    ancestry: RefCell<BTreeMap<PathBuf, bool>>,
}

impl Boundary {
    fn new(host: &Host) -> Result<Self> {
        let home = host
            .home
            .canonicalize()
            .context("resolve native host home")?;
        let mut protected = vec![
            host.workspace.clone(),
            host.config.clone(),
            host.data.join("slopbox"),
            PathBuf::from("/System/Volumes"),
        ];
        for name in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".docker",
            ".kube",
            ".password-store",
            ".pi",
            ".claude",
            ".codex",
            ".config",
            ".cargo",
            ".netrc",
            ".gitconfig",
            "Library",
        ] {
            protected.push(home.join(name));
        }
        // A missing credential/config directory must not make its parent safe.
        // Keep lexical paths AND resolve existing ancestors (including symlinks).
        for path in protected.clone() {
            protected.push(resolve_ancestors(&path)?);
        }
        protected.push(PathBuf::from(format!(
            "/private/var/tmp/slopbox-{}",
            unsafe { libc::getuid() }
        )));
        // realpath preserves case aliases and APFS volume aliases.
        let mut protected_ids = BTreeSet::new();
        let mut container_ids = BTreeSet::new();
        for path in &protected {
            if let Some(id) = file_id(path)? {
                protected_ids.insert(id);
            }
        }
        for path in protected.iter().chain(std::iter::once(&home)) {
            for ancestor in path.ancestors() {
                if let Some(id) = file_id(ancestor)? {
                    container_ids.insert(id);
                }
            }
        }
        Ok(Self {
            home,
            protected,
            protected_ids,
            container_ids,
            ancestry: RefCell::new(BTreeMap::new()),
        })
    }

    fn check(&self, path: &Path) -> Result<()> {
        absolute(path)?;
        ensure!(
            !self.home.starts_with(path)
                && self.protected.iter().all(|other| !overlaps(path, other))
                && !file_id(path)?.is_some_and(|id| self.container_ids.contains(&id))
                && !self.private_ancestor(path)?,
            "native runtime root overlaps private state or workspace: {}",
            path.display()
        );
        Ok(())
    }

    fn private_ancestor(&self, path: &Path) -> Result<bool> {
        if let Some(private) = self.ancestry.borrow().get(path) {
            return Ok(*private);
        }
        let private = file_id(path)?.is_some_and(|id| self.protected_ids.contains(&id))
            || match path.parent() {
                Some(parent) => self.private_ancestor(parent)?,
                None => false,
            };
        self.ancestry.borrow_mut().insert(path.to_owned(), private);
        Ok(private)
    }
}

fn file_id(path: &Path) -> Result<Option<(u64, u64)>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("inspect native resource identity {}", path.display())),
    }
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn absolute(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && !path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            && path
                .to_str()
                .is_some_and(|text| !text.chars().any(char::is_control)),
        "native runtime paths must be absolute, UTF-8 and contain no parent traversal or controls"
    );
    Ok(())
}

pub(crate) fn resolve_ancestors(path: &Path) -> Result<PathBuf> {
    absolute(path)?;
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().context("missing native path root")?;
            Ok(resolve_ancestors(parent)?.join(path.file_name().context("invalid native path")?))
        }
        Err(error) => {
            Err(error).with_context(|| format!("resolve protected path {}", path.display()))
        }
    }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
}

pub(crate) fn prepare_generic_home(home: &Path) -> Result<()> {
    // Guest-owned contents must never be traversed or repaired by host initialization.
    match fs::DirBuilder::new().mode(0o700).create(home) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(home)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0,
        "native private home is not a private directory"
    );
    Ok(())
}

pub(crate) fn prepare_runtime(selected: Option<RuntimePlan>) -> Result<RuntimePlan> {
    selected.context("native macOS requires host-selected [runtime] resources")
}
