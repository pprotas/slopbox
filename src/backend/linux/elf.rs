mod bundles;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use goblin::elf::{Elf, dynamic};

use crate::backend::{RuntimePlan, RuntimeSelection};
use crate::command::{protected_system_executable, trusted_executable};
use crate::fs_util::expand_host_home;

const BIN: &str = "/run/slopbox/bin";
const DT_AUXILIARY: u64 = 0x7ffffffd;
const DT_FILTER: u64 = 0x7fffffff;

pub(crate) fn prepare(
    selection: &RuntimeSelection,
    workspace: &Path,
    git_signing: bool,
) -> Result<RuntimePlan> {
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "selected executable runtimes require a non-root host user"
    );
    ensure!(
        cfg!(any(target_arch = "aarch64", target_arch = "x86_64")),
        "selected ELF runtimes currently support aarch64 and x86_64 hosts"
    );
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let mut tools = BTreeMap::new();
    let mut selected_paths = Vec::new();
    for name in ["bash", "bwrap", "env"] {
        tools.insert(name.to_owned(), trusted_executable(name)?);
    }
    if git_signing {
        tools.insert("ssh-keygen".into(), trusted_executable("ssh-keygen")?);
    }
    for path in &selection.executables {
        ensure!(
            !path.as_os_str().as_bytes().iter().any(u8::is_ascii_control),
            "control characters are not supported in runtime paths"
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("runtime executable needs a name")?;
        ensure!(
            name != "slopbox",
            "slopbox is reserved for the session executable"
        );
        let path = if path.components().count() == 1 && !path.is_absolute() {
            trusted_executable(name)?
        } else {
            expand_host_home(path, &home)?
        };
        selected_paths.push(path.clone());
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve runtime executable {}", path.display()))?;
        if let Some(previous) = tools.insert(name.to_owned(), path.clone()) {
            ensure!(
                previous == path,
                "conflicting runtime executable name {name}"
            );
        }
    }
    let mut discovery = Discovery::new(workspace, &home)?;
    for path in &selected_paths {
        discovery.check(&normalized(path)?)?;
    }
    let slopbox = env::current_exe()?.canonicalize()?;
    discovery.selected.extend(tools.values().cloned());
    discovery.selected.insert(slopbox.clone());
    for root in &selection.dependency_roots {
        let root = expand_host_home(root, &home)?;
        discovery.check(&normalized(&root)?)?;
        let root = root.canonicalize().context("resolve dependency root")?;
        discovery.check(&root)?;
        ensure!(root.is_dir(), "dependency root must be a directory");
        ensure!(
            !discovery.workspace.starts_with(&root)
                && !discovery
                    .forbidden
                    .iter()
                    .any(|path| path.starts_with(&root)),
            "dependency root contains workspace, credentials or control state: {}",
            root.display()
        );
        discovery.dependency_roots.push(root);
    }
    discovery.select_bundles(&selection.bundles, &home)?;
    for path in selected_paths {
        discovery.file(&path)?;
    }
    for (name, path) in &tools {
        discovery.executable(path, &tools)?;
        discovery.link(Path::new(BIN).join(name), path.clone())?;
    }
    discovery.executable(&slopbox, &tools)?;
    discovery.discover_bundles()?;
    discovery.link("/bin/sh".into(), tools["bash"].clone())?;
    discovery.link("/usr/bin/env".into(), tools["env"].clone())?;
    discovery.link("/run/slopbox/bwrap".into(), tools["bwrap"].clone())?;
    Ok(discovery.plan())
}

struct Discovery {
    workspace: PathBuf,
    forbidden: Vec<PathBuf>,
    selected: HashSet<PathBuf>,
    dependency_roots: Vec<PathBuf>,
    bundles: BTreeMap<PathBuf, PathBuf>,
    bundled_libraries: HashMap<String, Vec<PathBuf>>,
    cache: HashMap<String, Vec<PathBuf>>,
    files: BTreeSet<PathBuf>,
    links: BTreeMap<PathBuf, PathBuf>,
    visited: HashSet<(PathBuf, Vec<PathBuf>)>,
    scripts: HashSet<PathBuf>,
    depth: usize,
}

impl Discovery {
    fn new(workspace: &Path, home: &Path) -> Result<Self> {
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let mut forbidden = vec![
            home.join(".ssh"),
            home.join(".gnupg"),
            home.join(".password-store"),
            home.join(".pi/agent/auth.json"),
            home.join(".aws"),
            home.join(".azure"),
            home.join(".kube"),
            config.join("gcloud"),
            "/etc/ssh".into(),
            "/etc/shadow".into(),
            "/etc/gshadow".into(),
            "/etc/sudoers".into(),
            config.join("slopbox"),
            data.join("slopbox"),
        ];
        for path in forbidden.clone() {
            let resolved = path
                .ancestors()
                .find_map(|ancestor| {
                    Some(
                        ancestor
                            .canonicalize()
                            .ok()?
                            .join(path.strip_prefix(ancestor).ok()?),
                    )
                })
                .context("resolve protected runtime paths")?;
            forbidden.push(normalized(&resolved)?);
        }
        let mut discovery = Self {
            workspace: workspace.canonicalize()?,
            forbidden,
            selected: HashSet::new(),
            dependency_roots: Vec::new(),
            bundles: BTreeMap::new(),
            bundled_libraries: HashMap::new(),
            cache: HashMap::new(),
            files: BTreeSet::new(),
            links: BTreeMap::new(),
            visited: HashSet::new(),
            scripts: HashSet::new(),
            depth: 0,
        };
        let cache = Path::new("/etc/ld.so.cache");
        if cache.exists() {
            crate::command::protected_system_path(cache)
                .context("loader cache is not protected host state")?;
            discovery.file(cache)?;
            let ldconfig = ["/sbin/ldconfig", "/usr/sbin/ldconfig"]
                .into_iter()
                .find_map(|path| protected_system_executable(Path::new(path)).ok())
                .context("reading the loader cache requires a protected root-owned ldconfig")?;
            let output = Command::new(ldconfig)
                .env_clear()
                .current_dir("/")
                .args(["-p", "-C", "/etc/ld.so.cache"])
                .output()
                .context("read loader cache")?;
            ensure!(
                output.status.success(),
                "could not read the host loader cache"
            );
            for line in String::from_utf8(output.stdout)?.lines() {
                if let Some((left, right)) = line.split_once(" => ") {
                    let name = left
                        .split_whitespace()
                        .next()
                        .context("invalid loader cache entry")?;
                    let path = PathBuf::from(right.trim());
                    ensure!(path.is_absolute(), "loader cache contains a relative path");
                    discovery.cache.entry(name.into()).or_default().push(path);
                }
            }
        }
        Ok(discovery)
    }

    fn check(&self, path: &Path) -> Result<()> {
        normalized(path)?;
        ensure!(
            !path.starts_with(&self.workspace),
            "runtime overlaps workspace: {}",
            path.display()
        );
        ensure!(
            !self.forbidden.iter().any(|root| path.starts_with(root)),
            "runtime overlaps host credentials or control state: {}",
            path.display()
        );
        ensure!(
            !["/tmp", "/dev", "/proc", "/sys", "/run", "/home/slopbox"]
                .iter()
                .any(|root| path.starts_with(root)),
            "runtime overlaps a private or special guest path: {}",
            path.display()
        );
        Ok(())
    }

    fn authorize(&self, path: &Path) -> Result<()> {
        self.check(path)?;
        ensure!(
            self.selected.contains(path)
                || self.bundles.values().any(|root| path.starts_with(root))
                || self
                    .dependency_roots
                    .iter()
                    .any(|root| path.starts_with(root))
                || crate::command::protected_system_path(path).is_ok(),
            "runtime dependency is outside host-authorized roots: {}; select its installation prefix in runtime.dependency_roots",
            path.display()
        );
        Ok(())
    }

    fn file(&mut self, path: &Path) -> Result<PathBuf> {
        let path = normalized(path)?;
        self.check(&path)?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("resolve runtime file {}", path.display()))?;
        self.authorize(&canonical)?;
        let metadata = fs::metadata(&canonical)?;
        ensure!(
            metadata.is_file(),
            "runtime dependency is not a regular file: {}",
            path.display()
        );
        ensure!(
            metadata.nlink() == 1 || (metadata.uid() == 0 && metadata.mode() & 0o022 == 0),
            "runtime file has mutable hard-link aliases: {}",
            path.display()
        );
        ensure!(
            self.files.len() < 4096,
            "runtime dependency graph is too large"
        );
        ensure!(
            !self.links.contains_key(&canonical),
            "runtime file conflicts with a link: {}",
            canonical.display()
        );
        self.files.insert(canonical.clone());
        self.link(path, canonical.clone())?;
        Ok(canonical)
    }

    fn link(&mut self, path: PathBuf, target: PathBuf) -> Result<()> {
        if path == target {
            return Ok(());
        }
        ensure!(
            !self.files.contains(&path),
            "runtime link conflicts with a file: {}",
            path.display()
        );
        if let Some(previous) = self.links.insert(path.clone(), target.clone()) {
            ensure!(
                previous == target,
                "conflicting runtime link: {}",
                path.display()
            );
        }
        Ok(())
    }

    fn executable(&mut self, path: &Path, tools: &BTreeMap<String, PathBuf>) -> Result<()> {
        let path = if path == Path::new("/bin/sh") {
            &tools["bash"]
        } else {
            path
        };
        let canonical = self.file(path)?;
        ensure!(
            fs::metadata(&canonical)?.mode() & 0o111 != 0,
            "runtime executable is not executable: {}",
            path.display()
        );
        let bytes = contents(&canonical)?;
        if bytes.starts_with(b"\x7fELF") {
            drop(bytes);
            self.object(&canonical, &[])?;
        } else if bytes.starts_with(b"#!") {
            ensure!(
                self.scripts.len() < 32,
                "runtime script interpreter chain is too deep"
            );
            ensure!(
                self.scripts.insert(canonical.clone()),
                "cyclic script interpreter: {}",
                path.display()
            );
            let line = bytes
                .split(|byte| *byte == b'\n')
                .next()
                .context("missing shebang")?;
            ensure!(line.len() <= 255, "runtime script shebang is too long");
            let line = std::str::from_utf8(line).context("non-UTF-8 shebang")?;
            let mut words = line[2..].split_whitespace();
            let interpreter = Path::new(words.next().context("empty shebang")?);
            ensure!(
                interpreter.is_absolute(),
                "runtime shebang interpreter must be absolute"
            );
            if interpreter == Path::new("/usr/bin/env") {
                let name = words
                    .next()
                    .context("env shebang needs an explicitly selected interpreter")?;
                ensure!(
                    words.next().is_none() && !name.starts_with('-'),
                    "only simple env name shebangs are supported"
                );
                let target = tools.get(name).with_context(|| {
                    format!("script interpreter {name} is not selected in host runtime.executables")
                })?;
                self.executable(target, tools)?;
            } else {
                // The optional shebang argument does not select additional runtime files.
                self.executable(interpreter, tools)?;
            }
            self.scripts.remove(&canonical);
        } else {
            bail!(
                "runtime executable is neither ELF nor a supported script: {}",
                path.display()
            );
        }
        Ok(())
    }

    fn object(&mut self, path: &Path, inherited: &[PathBuf]) -> Result<()> {
        let path = normalized(path)?;
        ensure!(
            self.visited.len() < 4096 && inherited.len() < 128,
            "runtime dependency graph is too large"
        );
        if !self.visited.insert((path.clone(), inherited.to_vec())) {
            return Ok(());
        }
        self.depth += 1;
        ensure!(self.depth <= 128, "runtime dependency chain is too deep");
        let canonical = self.file(&path)?;
        let bytes = contents(&canonical)?;
        let elf = parse(&bytes, &path)?;
        ensure!(
            elf.is_64 && elf.little_endian,
            "only native 64-bit little-endian ELF runtimes are supported"
        );
        #[cfg(target_arch = "aarch64")]
        ensure!(
            elf.header.e_machine == goblin::elf::header::EM_AARCH64,
            "runtime ELF architecture differs from the host"
        );
        #[cfg(target_arch = "x86_64")]
        ensure!(
            elf.header.e_machine == goblin::elf::header::EM_X86_64,
            "runtime ELF architecture differs from the host"
        );
        if let Some(dynamic) = &elf.dynamic {
            ensure!(
                !dynamic
                    .dyns
                    .iter()
                    .any(|entry| entry.d_tag == dynamic::DT_FLAGS_1
                        && entry.d_val & dynamic::DF_1_NODEFLIB != 0),
                "ELF NODEFLIB search semantics are not supported"
            );
            ensure!(
                !dynamic.dyns.iter().any(|entry| matches!(
                    entry.d_tag,
                    dynamic::DT_AUDIT | dynamic::DT_DEPAUDIT | DT_FILTER | DT_AUXILIARY
                )),
                "ELF audit/filter dependencies are not supported"
            );
        }
        if let Some(interpreter) = elf.interpreter {
            self.object(Path::new(interpreter), &[])?;
        }
        let origin = path.parent().context("ELF file has no parent")?;
        let rpaths = if elf.runpaths.is_empty() {
            search_paths(&elf.rpaths, origin)?
        } else {
            Vec::new()
        };
        let mut ancestors = rpaths.clone();
        for path in inherited {
            if !ancestors.contains(path) {
                ancestors.push(path.clone());
            }
        }
        let search = if elf.runpaths.is_empty() {
            ancestors.clone()
        } else {
            search_paths(&elf.runpaths, origin)?
        };
        for name in &elf.libraries {
            if name.contains('/') {
                self.object(&expand(name, origin)?, &ancestors)?;
                continue;
            }
            ensure!(
                !name.contains('$'),
                "unsupported dynamic token in ELF dependency"
            );
            let mut candidates = Vec::new();
            for directory in &search {
                let path = directory.join(name);
                if self.compatible(&path, &elf)? {
                    candidates.push(path);
                    break;
                }
            }
            if candidates.is_empty() {
                for path in self.cache.get(*name).into_iter().flatten() {
                    if self.compatible(path, &elf)? {
                        candidates.push(path.clone());
                    }
                }
            }
            if candidates.is_empty() {
                for directory in ["/lib64", "/usr/lib64", "/lib", "/usr/lib"] {
                    let path = Path::new(directory).join(name);
                    if self.compatible(&path, &elf)? {
                        candidates.push(path);
                        break;
                    }
                }
            }
            if candidates.is_empty() {
                // dlopen callers can supply search paths we cannot infer statically.
                for path in self.bundled_libraries.get(*name).into_iter().flatten() {
                    if self.compatible(path, &elf)? {
                        candidates.push(path.clone());
                    }
                }
            }
            ensure!(
                !candidates.is_empty(),
                "unresolved runtime dependency {name} for {}",
                path.display()
            );
            for candidate in candidates {
                self.object(&candidate, &ancestors)?;
            }
        }
        self.depth -= 1;
        Ok(())
    }

    fn compatible(&self, path: &Path, requester: &Elf<'_>) -> Result<bool> {
        self.check(&normalized(path)?)?;
        if !path.is_file() {
            return Ok(false);
        }
        self.authorize(&path.canonicalize()?)?;
        let bytes = contents(path)?;
        let elf = parse(&bytes, path)?;
        Ok(elf.is_64 == requester.is_64
            && elf.little_endian == requester.little_endian
            && elf.header.e_machine == requester.header.e_machine)
    }
}

fn contents(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 256 * 1024 * 1024,
        "runtime file exceeds the 256 MiB inspection limit"
    );
    fs::read(path).with_context(|| format!("read runtime file {}", path.display()))
}

fn parse<'a>(bytes: &'a [u8], path: &Path) -> Result<Elf<'a>> {
    Elf::parse(bytes).map_err(|_| anyhow::anyhow!("invalid ELF runtime file: {}", path.display()))
}

fn search_paths(values: &[&str], origin: &Path) -> Result<Vec<PathBuf>> {
    values
        .iter()
        .flat_map(|value| value.split(':'))
        .map(|entry| expand(entry, origin))
        .collect()
}

fn expand(value: &str, origin: &Path) -> Result<PathBuf> {
    let origin = origin.to_str().context("ELF origin is not UTF-8")?;
    let mut expanded = String::new();
    let mut remaining = value;
    while let Some(index) = remaining.find('$') {
        expanded.push_str(&remaining[..index]);
        let token = &remaining[index..];
        remaining = if let Some(tail) = token.strip_prefix("${ORIGIN}") {
            tail
        } else if let Some(tail) = token.strip_prefix("$ORIGIN") {
            ensure!(
                !tail.starts_with(
                    |character: char| character.is_ascii_alphanumeric() || character == '_'
                ),
                "unsupported ELF dynamic path token"
            );
            tail
        } else {
            bail!("unsupported ELF dynamic path token");
        };
        expanded.push_str(origin);
    }
    expanded.push_str(remaining);
    normalized(Path::new(&expanded))
}

fn normalized(path: &Path) -> Result<PathBuf> {
    ensure!(
        !path.as_os_str().as_bytes().iter().any(u8::is_ascii_control),
        "control characters are not supported in runtime paths"
    );
    ensure!(
        path.is_absolute(),
        "relative ELF search paths are not supported"
    );
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => output.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(output.pop(), "runtime path escapes root");
            }
            Component::Prefix(_) => bail!("unsupported runtime path prefix"),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_paths_are_absolute_and_do_not_inherit_the_working_directory() {
        let origin = Path::new("/opt/tool/bin");
        assert_eq!(
            expand("${ORIGIN}/../lib", origin).unwrap(),
            Path::new("/opt/tool/lib")
        );
        assert_eq!(
            expand("$ORIGIN/lib", origin).unwrap(),
            Path::new("/opt/tool/bin/lib")
        );
        for value in [
            "",
            ".",
            "relative/lib",
            "$LIB/lib",
            "$PLATFORM",
            "$ORIGINAL/lib",
            "$ORIGIN_suffix/lib",
            "/lib/\x1b[31m",
            "/../../escape",
        ] {
            assert!(expand(value, origin).is_err(), "{value}");
        }
    }

    #[test]
    fn credential_aliases_are_protected_before_the_leaf_exists() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let alias = root.path().join("alias");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        std::os::unix::fs::symlink(&home, &alias).unwrap();
        let discovery = Discovery::new(&workspace, &alias).unwrap();
        let error = discovery.check(&home.join(".ssh/future-key")).unwrap_err();
        assert!(error.to_string().contains("credentials"), "{error}");
    }

    #[test]
    fn files_cannot_overlap_workspace_or_private_paths() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let discovery = Discovery::new(&workspace, &home).unwrap();
        for path in [
            workspace.join("tool"),
            home.join(".ssh/key"),
            home.join(".gnupg/key"),
            "/proc/self/exe".into(),
            "/run/user/socket".into(),
            "/home/slopbox/bin/tool".into(),
        ] {
            assert!(discovery.check(&path).is_err(), "{}", path.display());
        }
    }
}
