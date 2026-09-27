//! Host runtime discovery, not policy or a package manager. PATH selects binaries
//! within recognized installations; it never creates a recursive filesystem grant.
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::Config;
use crate::backend::{PreparedDevEnvironment, RuntimePlan};
use crate::policy::RuntimeMode;

mod nix;
pub(crate) mod selected;

pub(crate) struct NativeRuntime {
    pub config: Option<Config>,
    pub tools: DeveloperTools,
    pub selected_files: Vec<PathBuf>,
    pub system_data: Vec<PathBuf>,
}

#[derive(Default, Debug)]
pub(crate) struct DeveloperTools {
    pub read_roots: Vec<PathBuf>,
    pub read_links: Vec<PathBuf>,
    path: Vec<PathBuf>,
    environment: Vec<(String, PathBuf)>,
}

// Explicit inputs make discovery testable without changing the test process's
// environment or granting access to real host credentials.
struct Host {
    home: PathBuf,
    workspace: PathBuf,
    config: PathBuf,
    data: PathBuf,
    path: OsString,
    brew: Vec<PathBuf>,
    mise: PathBuf,
    rustup: PathBuf,
    rust_toolchain: Option<String>,
    developer: Option<PathBuf>,
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
            path: env::var_os("PATH").unwrap_or_default(),
            brew: vec!["/opt/homebrew".into(), "/usr/local".into()],
            mise: data.join("mise/installs"),
            rustup: env::var_os("RUSTUP_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".rustup")),
            rust_toolchain: env::var("RUSTUP_TOOLCHAIN").ok(),
            developer: None,
            home,
            workspace: workspace.to_owned(),
            config,
            data,
        })
    }
}

fn developer_directory(
    override_: Option<&Path>,
    selected: &Path,
    command_line_tools: &Path,
) -> Result<Option<PathBuf>> {
    if let Some(path) = override_ {
        absolute(path)?;
        // Nix's DEVELOPER_DIR selects a build SDK, not a native host toolchain.
        if !path.starts_with("/nix/store") {
            return Ok(Some(path.to_owned()));
        }
    }
    match fs::canonicalize(selected) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(command_line_tools
            .is_dir()
            .then(|| command_line_tools.to_owned())),
        Err(error) => Err(error).context("resolve selected Apple developer directory"),
    }
}

struct Boundary {
    home: PathBuf,
    protected: Vec<PathBuf>,
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
        Ok(Self { home, protected })
    }

    fn check(&self, path: &Path) -> Result<()> {
        absolute(path)?;
        ensure!(
            !self.home.starts_with(path)
                && self.protected.iter().all(|other| !overlaps(path, other)),
            "native runtime root overlaps private state or workspace: {}",
            path.display()
        );
        Ok(())
    }

    fn installation(&self, path: &Path) -> Result<Option<PathBuf>> {
        self.check(path)?;
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        };
        // Install roots must be real directories, not aliases to arbitrary data.
        ensure!(
            metadata.is_dir(),
            "native installation is not a directory: {}",
            path.display()
        );
        let canonical = path.canonicalize()?;
        self.check(&canonical)?;
        Ok(Some(canonical))
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

impl DeveloperTools {
    pub(super) fn discover(workspace: &Path, node: &Path) -> Result<Self> {
        let mut host = Host::read(workspace)?;
        host.developer = developer_directory(
            env::var_os("DEVELOPER_DIR").map(PathBuf::from).as_deref(),
            Path::new("/private/var/select/developer_dir"),
            Path::new("/Library/Developer/CommandLineTools"),
        )?;
        Self::from_host(&host, node)
    }

    fn from_host(host: &Host, node: &Path) -> Result<Self> {
        let boundary = Boundary::new(host)?;
        let mut tools = Self::default();
        // Unlike the whole Homebrew prefix, Cellar contains package installations,
        // not mutable etc/, var/, or arbitrary files placed in prefix/bin.
        let mut brew_bins = Vec::new();
        for prefix in &host.brew {
            if let Some(cellar) = boundary.installation(&prefix.join("Cellar"))? {
                tools.homebrew_opt(prefix, &cellar, &boundary)?;
                push_unique(&mut tools.read_roots, cellar);
                brew_bins.extend([prefix.join("bin"), prefix.join("sbin")]);
            }
        }
        tools.rustup(host, &boundary)?;
        tools.apple(host, &boundary)?;

        // mise shims/configuration are not imported. Only activated installation
        // paths (or the explicitly selected Node installation) are recognized.
        let mut candidates: Vec<_> = env::split_paths(&host.path)
            .filter(|path| absolute(path).is_ok())
            .collect();
        candidates.push(node.parent().context("Node has no parent")?.to_owned());
        if let Some(installs) = boundary.installation(&host.mise)? {
            for candidate in &candidates {
                let Ok(canonical) = candidate.canonicalize() else {
                    continue;
                };
                let Ok(relative) = canonical.strip_prefix(&installs) else {
                    continue;
                };
                let parts: Vec<_> = relative.components().collect();
                if parts.len() >= 3 {
                    let version = installs.join(parts[0]).join(parts[1]);
                    if let Some(root) = boundary.installation(&version)? {
                        push_unique(&mut tools.read_roots, root);
                    }
                }
            }
        }
        // PATH order selects among already-approved installations. Canonical
        // package bin directories avoid granting prefix/bin (or arbitrary PATH).
        candidates.extend(brew_bins.iter().cloned());
        for candidate in candidates {
            if boundary.check(&candidate).is_err() {
                continue;
            }
            if let Ok(path) = candidate.canonicalize()
                && path.is_dir()
                && tools.contains(&path)
            {
                push_unique(&mut tools.path, path);
            } else if brew_bins.contains(&candidate) {
                let entries = match fs::read_dir(&candidate) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let mut binaries = entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<Vec<_>>>()?;
                binaries.sort();
                for binary in binaries {
                    if let Ok(target) = binary.canonicalize()
                        && tools.contains(&target)
                        && executable(&target)
                    {
                        push_unique(&mut tools.path, target.parent().unwrap().to_owned());
                    }
                }
            }
        }
        push_unique(&mut tools.path, node.parent().unwrap().to_owned());
        for path in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
            push_unique(&mut tools.path, PathBuf::from(path));
        }
        Ok(tools)
    }

    fn contains(&self, path: &Path) -> bool {
        self.read_roots.iter().any(|root| path.starts_with(root))
    }

    fn homebrew_opt(&mut self, prefix: &Path, cellar: &Path, boundary: &Boundary) -> Result<()> {
        let Some(opt) = boundary.installation(&prefix.join("opt"))? else {
            return Ok(());
        };
        let mut entries = fs::read_dir(&opt)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let link = entry.path();
            if absolute(&link).is_err() || !entry.file_type()?.is_symlink() {
                continue;
            }
            let Ok(target) = link.canonicalize() else {
                continue;
            };
            let Ok(keg) = target.strip_prefix(cellar) else {
                continue;
            };
            if keg.components().count() != 2 || !target.is_dir() {
                continue;
            }
            boundary.check(&target)?;
            // dyld follows opt/<formula> install names. Allow the link itself;
            // file contents remain covered only by the canonical Cellar grant.
            push_unique(&mut self.read_links, link);
        }
        Ok(())
    }

    fn rustup(&mut self, host: &Host, boundary: &Boundary) -> Result<()> {
        boundary.check(&host.rustup)?;
        // Read only the host-selected version, never export settings or overrides.
        #[derive(Default, Deserialize)]
        struct Settings {
            default_toolchain: Option<String>,
            default_host_triple: Option<String>,
        }
        let settings_path = host.rustup.join("settings.toml");
        let settings: Settings = match fs::metadata(&settings_path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && metadata.len() <= 64 * 1024,
                    "invalid rustup settings"
                );
                toml::from_str(&fs::read_to_string(settings_path)?)
                    .context("read host rustup toolchain selection")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(error) => return Err(error).context("inspect host rustup settings"),
        };
        let selected = host
            .rust_toolchain
            .as_ref()
            .or(settings.default_toolchain.as_ref());
        if let Some(name) = selected {
            ensure!(
                Path::new(name).components().count() == 1
                    && matches!(
                        Path::new(name).components().next(),
                        Some(Component::Normal(_))
                    )
                    && !name.chars().any(char::is_control),
                "only named installed rustup toolchains are supported"
            );
            let toolchains = host.rustup.join("toolchains");
            let mut root = boundary.installation(&toolchains.join(name))?;
            if root.is_none() {
                // Rustup stores release/channel shorthands with a host triple suffix.
                let triple = settings
                    .default_host_triple
                    .unwrap_or_else(|| format!("{}-apple-darwin", env::consts::ARCH));
                ensure!(
                    Path::new(&triple).file_name() == Some(OsStr::new(&triple)),
                    "invalid rustup default host triple"
                );
                root = boundary.installation(&toolchains.join(format!("{name}-{triple}")))?;
            }
            let root = root.with_context(|| {
                format!(
                    "could not resolve installed Rust toolchain {name}; select a fully qualified name from `rustup toolchain list`"
                )
            })?;
            for name in ["cargo", "rustc"] {
                ensure!(
                    executable(&root.join("bin").join(name)),
                    "selected Rust toolchain lacks {name}"
                );
            }
            // Real Cargo/rustc instead of ~/.cargo/bin rustup proxies. This keeps
            // host Rustup/Cargo configuration and credentials out of both roles.
            push_unique(&mut self.path, root.join("bin"));
            push_unique(&mut self.read_roots, root);
        }
        Ok(())
    }

    fn apple(&mut self, host: &Host, boundary: &Boundary) -> Result<()> {
        let Some(developer) = &host.developer else {
            return Ok(());
        };
        boundary.check(developer)?;
        let developer = developer
            .canonicalize()
            .context("resolve Apple developer directory")?;
        boundary.check(&developer)?;
        let (compiler, sdk) = if developer
            .join("Toolchains/XcodeDefault.xctoolchain")
            .is_dir()
        {
            (
                developer.join("Toolchains/XcodeDefault.xctoolchain"),
                developer.join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"),
            )
        } else {
            (developer.join("usr"), developer.join("SDKs/MacOSX.sdk"))
        };
        let compiler = boundary
            .installation(&compiler)?
            .context("unsupported Apple toolchain layout")?;
        let bin = if compiler.ends_with("usr") {
            compiler.join("bin")
        } else {
            compiler.join("usr/bin")
        };
        let sdk = sdk.canonicalize().context("resolve selected macOS SDK")?;
        ensure!(
            sdk.starts_with(&developer),
            "selected SDK escapes the developer installation"
        );
        let sdk = boundary
            .installation(&sdk)?
            .context("selected macOS SDK is missing")?;
        for name in ["clang", "clang++", "ar"] {
            let path = bin.join(name);
            ensure!(
                executable(&path) && path.canonicalize()?.starts_with(&compiler),
                "invalid Apple tool {name}"
            );
        }
        push_unique(&mut self.read_roots, compiler);
        push_unique(&mut self.read_roots, sdk.clone());
        push_unique(&mut self.path, bin.clone());
        self.environment.extend([
            ("DEVELOPER_DIR".into(), developer),
            ("SDKROOT".into(), sdk),
            ("CC".into(), bin.join("clang")),
            // Keep the clang++ name: canonicalizing it to clang changes the driver.
            ("CXX".into(), bin.join("clang++")),
            ("AR".into(), bin.join("ar")),
            (
                format!(
                    "CARGO_TARGET_{}_APPLE_DARWIN_LINKER",
                    env::consts::ARCH.to_uppercase()
                ),
                bin.join("clang"),
            ),
        ]);
        Ok(())
    }

    pub(crate) fn environment(&self, cache: &Path) -> Result<Vec<String>> {
        let path = env::join_paths(&self.path).context("construct native tool PATH")?;
        let mut values = vec![
            format!("PATH={}", path.to_str().context("non-UTF-8 tool PATH")?),
            format!(
                "CARGO_HOME={}",
                cache.to_str().context("non-UTF-8 tool cache")?
            ),
            "GIT_CONFIG_NOSYSTEM=1".into(),
            "GIT_CONFIG_GLOBAL=/dev/null".into(),
            "SSL_CERT_FILE=/etc/ssl/cert.pem".into(),
        ];
        for (name, path) in &self.environment {
            values.push(format!(
                "{name}={}",
                path.to_str().context("non-UTF-8 tool environment")?
            ));
        }
        Ok(values)
    }
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
}

pub(crate) fn prepare_tool_cache(home: &Path) -> Result<PathBuf> {
    // Only this sandbox-owned cache persists, not the session's HOME or host
    // ~/.cargo. Refuse symlinks before host creation/chmod can follow them.
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
        "native tool state directory is not private"
    );
    let cache = home.join("cargo");
    match fs::DirBuilder::new().mode(0o700).create(&cache) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&cache)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0,
        "native Cargo cache is not private"
    );
    Ok(cache)
}

pub(crate) fn prepare_dev_environment(
    session_dir: &Path,
    workspace: &Path,
    mode: crate::DevEnvironment,
) -> Result<Option<PreparedDevEnvironment>> {
    if crate::backend::nix::enabled(workspace, mode)? {
        Ok(Some(nix::prepare(session_dir, workspace)?))
    } else {
        Ok(None)
    }
}

pub(crate) fn prepare_runtime(
    mode: RuntimeMode,
    path: &OsStr,
    environment: Option<&PreparedDevEnvironment>,
    workspace: &Path,
    selected: Option<RuntimePlan>,
) -> Result<RuntimePlan> {
    if let Some(runtime) = selected {
        return Ok(runtime);
    }
    ensure!(
        matches!(mode, RuntimeMode::Host | RuntimeMode::Project),
        "unsupported native runtime capability"
    );
    ensure!(
        mode != RuntimeMode::Project || environment.is_some(),
        "runtime=project requires an activated flake development environment"
    );
    let config = crate::session::macos_config()?
        .0
        .context("missing native runtime")?
        .resolve()?;
    let mut tools = if mode == RuntimeMode::Host {
        DeveloperTools::discover(workspace, &config.node)?
    } else {
        DeveloperTools::default()
    };
    if let Some(environment) = environment {
        let project = nix::tools(environment, workspace)?;
        tools.read_roots.extend(project.read_roots);
        // Nix supplies the compiler/SDK selection, even with host tools as fallback.
        tools.environment.clear();
        if mode == RuntimeMode::Project {
            tools.path = project.path;
        }
    }
    Ok(RuntimePlan {
        path: path.to_owned(),
        native: NativeRuntime {
            config: Some(config),
            tools,
            selected_files: Vec::new(),
            system_data: Vec::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    mod homebrew;

    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        _root: tempfile::TempDir,
        host: Host,
        node: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let home = base.join("home");
            let workspace = base.join("workspace");
            fs::create_dir(&home).unwrap();
            fs::create_dir(&workspace).unwrap();
            let host = Host {
                config: home.join(".config"),
                data: home.join(".local/share"),
                rustup: home.join(".rustup"),
                mise: home.join(".local/share/mise/installs"),
                home,
                workspace,
                brew: vec![base.join("brew")],
                path: OsString::new(),
                rust_toolchain: None,
                developer: None,
            };
            let node = base.join("node/bin/node");
            binary(&node);
            Self {
                _root: root,
                host,
                node,
            }
        }
        fn discover(&self) -> DeveloperTools {
            DeveloperTools::from_host(&self.host, &self.node).unwrap()
        }
    }

    fn binary(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn path_is_not_a_filesystem_grant_and_package_links_cannot_expand_it() {
        let mut fixture = Fixture::new();
        let host = &mut fixture.host;
        let brew = &host.brew[0];
        let package = brew.join("Cellar/git/1/bin");
        binary(&package.join("git"));
        fs::create_dir_all(brew.join("bin")).unwrap();
        symlink(package.join("git"), brew.join("bin/git")).unwrap();
        binary(&host.home.join(".ssh/secret-tool"));
        binary(&brew.join("bin/unpackaged-tool"));
        binary(&host.workspace.join("bin/project-tool"));
        symlink(
            host.home.join(".ssh/secret-tool"),
            brew.join("bin/secret-link"),
        )
        .unwrap();
        fs::create_dir_all(brew.join("etc")).unwrap();
        fs::write(brew.join("etc/gitconfig"), "canary").unwrap();
        host.path = env::join_paths([
            Path::new(""),
            Path::new("."),
            Path::new("/"),
            &host.home,
            &host.home.join(".ssh"),
            &host.workspace,
            &brew.join("bin"),
        ])
        .unwrap();
        let tools = DeveloperTools::from_host(host, &fixture.node).unwrap();
        assert_eq!(tools.read_roots, [brew.join("Cellar")]);
        assert!(tools.path.contains(&package));
        assert!(!tools.path.contains(&brew.join("bin")));
        assert!(!tools.contains(&brew.join("etc/gitconfig")));
        assert!(!tools.contains(&host.home.join(".ssh/secret-tool")));
        assert!(!tools.contains(&host.workspace));
    }

    #[test]
    fn homebrew_opt_only_adds_literal_reads_for_links_to_installed_kegs() {
        let fixture = Fixture::new();
        let brew = &fixture.host.brew[0];
        let cellar = brew.join("Cellar");
        let keg = cellar.join("pcre2/1");
        fs::create_dir_all(keg.join("lib")).unwrap();
        let opt = brew.join("opt");
        fs::create_dir(&opt).unwrap();
        symlink("../Cellar/pcre2/1", opt.join("pcre2")).unwrap();
        symlink(&keg, opt.join("pcre2-alias")).unwrap();
        fs::create_dir_all(fixture.host.home.join(".ssh")).unwrap();
        fs::write(fixture.host.home.join(".ssh/key"), "credential canary").unwrap();
        symlink(fixture.host.home.join(".ssh"), opt.join("private")).unwrap();
        symlink(fixture.host.home.join(".ssh/key"), opt.join("private-file")).unwrap();
        symlink(&cellar, opt.join("cellar")).unwrap();
        symlink(cellar.join("pcre2"), opt.join("formula")).unwrap();
        symlink(keg.join("lib"), opt.join("not-a-keg")).unwrap();
        symlink("missing", opt.join("dangling")).unwrap();
        symlink("cycle", opt.join("cycle")).unwrap();
        fs::create_dir(opt.join("unpackaged")).unwrap();
        fs::write(opt.join("config"), "not an installation").unwrap();

        let tools = fixture.discover();
        assert_eq!(tools.read_roots, [cellar]);
        assert_eq!(
            tools.read_links,
            [opt.join("pcre2"), opt.join("pcre2-alias")]
        );
        assert!(!tools.contains(&opt));
        assert!(!tools.path.iter().any(|path| path.starts_with(&opt)));
    }

    #[test]
    fn homebrew_opt_directory_cannot_be_a_symlink_to_private_state() {
        let fixture = Fixture::new();
        let brew = &fixture.host.brew[0];
        fs::create_dir_all(brew.join("Cellar/pcre2/1")).unwrap();
        fs::create_dir_all(fixture.host.home.join(".ssh")).unwrap();
        symlink(fixture.host.home.join(".ssh"), brew.join("opt")).unwrap();
        assert!(DeveloperTools::from_host(&fixture.host, &fixture.node).is_err());
    }

    #[test]
    fn boundary_rejects_ancestors_descendants_missing_state_and_symlink_aliases() {
        let fixture = Fixture::new();
        let host = &fixture.host;
        let boundary = Boundary::new(host).unwrap();
        for path in [
            PathBuf::from("/"),
            host.home.clone(),
            host.home.parent().unwrap().to_owned(),
            host.config.clone(),
            host.home.join(".ssh/child"),
            host.data.join("slopbox"),
            host.workspace.clone(),
            host.workspace.join("toolchain"),
            host.data.clone(),
        ] {
            assert!(boundary.check(&path).is_err(), "{}", path.display());
        }
        fs::create_dir_all(host.home.join(".ssh")).unwrap();
        fs::create_dir_all(&host.brew[0]).unwrap();
        symlink(host.home.join(".ssh"), host.brew[0].join("Cellar")).unwrap();
        assert!(boundary.installation(&host.brew[0].join("Cellar")).is_err());
        assert!(DeveloperTools::from_host(host, &fixture.node).is_err());
        for path in ["", ".", "bin", "/tmp/../usr/bin", "/tmp/control\n"] {
            assert!(absolute(Path::new(path)).is_err());
        }
    }

    #[test]
    fn protected_alias_into_a_package_tree_rejects_the_tree() {
        let fixture = Fixture::new();
        let cellar = fixture.host.brew[0].join("Cellar");
        fs::create_dir_all(cellar.join("state")).unwrap();
        symlink(cellar.join("state"), fixture.host.home.join(".ssh")).unwrap();
        assert!(DeveloperTools::from_host(&fixture.host, &fixture.node).is_err());
    }

    #[test]
    fn rustup_uses_installed_binaries_without_exporting_host_configuration() {
        let mut fixture = Fixture::new();
        let root = fixture.host.rustup.join("toolchains/1.90.0-test");
        for name in ["cargo", "rustc"] {
            binary(&root.join("bin").join(name));
        }
        fs::write(
            fixture.host.rustup.join("settings.toml"),
            "default_toolchain = '1.90.0-test'\n",
        )
        .unwrap();
        let tools = fixture.discover();
        assert_eq!(tools.read_roots, std::slice::from_ref(&root));
        assert_eq!(tools.path[0], root.join("bin"));
        assert!(!tools.contains(&fixture.host.rustup.join("settings.toml")));
        assert!(!tools.contains(&fixture.host.home.join(".cargo/credentials.toml")));
        let env = tools.environment(Path::new("/private/cache")).unwrap();
        assert!(env.contains(&"CARGO_HOME=/private/cache".into()));
        assert!(!env.iter().any(|value| value.starts_with("RUSTUP_HOME=")));
        for name in ["../bad", "/tmp/bad", ".", "", "not-installed"] {
            fixture.host.rust_toolchain = Some(name.into());
            assert!(DeveloperTools::from_host(&fixture.host, &fixture.node).is_err());
        }
    }

    #[test]
    fn rustup_resolves_version_shorthand_without_falling_back_to_the_default() {
        let mut fixture = Fixture::new();
        let triple = format!("{}-apple-darwin", env::consts::ARCH);
        let selected = fixture
            .host
            .rustup
            .join(format!("toolchains/1.91.0-{triple}"));
        let default = fixture
            .host
            .rustup
            .join(format!("toolchains/1.90.0-{triple}"));
        for root in [&selected, &default] {
            for name in ["cargo", "rustc"] {
                binary(&root.join("bin").join(name));
            }
        }
        fs::write(
            fixture.host.rustup.join("settings.toml"),
            format!("default_toolchain = '1.90.0-{triple}'\n"),
        )
        .unwrap();
        fixture.host.rust_toolchain = Some("1.91.0".into());
        let tools = fixture.discover();
        assert_eq!(tools.read_roots, std::slice::from_ref(&selected));
        assert_eq!(tools.path[0], selected.join("bin"));
        assert!(!tools.contains(&fixture.host.rustup.join("settings.toml")));

        fixture.host.rust_toolchain = Some(format!("1.91.0-{triple}"));
        assert_eq!(fixture.discover().read_roots, [selected]);
        fixture.host.rust_toolchain = Some("1.92.0".into());
        assert!(DeveloperTools::from_host(&fixture.host, &fixture.node).is_err());
    }

    #[test]
    fn rustup_resolves_channel_shorthand_using_the_configured_host() {
        let triple = if env::consts::ARCH == "aarch64" {
            "x86_64-apple-darwin"
        } else {
            "aarch64-apple-darwin"
        };
        for (channel, from_environment) in [
            ("stable", false),
            ("beta", true),
            ("nightly-2026-09-20", true),
        ] {
            let mut fixture = Fixture::new();
            let selected = fixture
                .host
                .rustup
                .join(format!("toolchains/{channel}-{triple}"));
            for name in ["cargo", "rustc"] {
                binary(&selected.join("bin").join(name));
            }
            fs::write(
                fixture.host.rustup.join("settings.toml"),
                format!("default_toolchain = '{channel}'\ndefault_host_triple = '{triple}'\n"),
            )
            .unwrap();
            if from_environment {
                fixture.host.rust_toolchain = Some(channel.into());
            }
            let tools = fixture.discover();
            assert_eq!(tools.read_roots, [selected]);
        }
    }

    #[test]
    fn rustup_shorthand_cannot_grant_a_symlinked_toolchain() {
        let mut fixture = Fixture::new();
        let toolchains = fixture.host.rustup.join("toolchains");
        fs::create_dir_all(&toolchains).unwrap();
        symlink(
            &fixture.host.workspace,
            toolchains.join(format!("1.91.0-{}-apple-darwin", env::consts::ARCH)),
        )
        .unwrap();
        fixture.host.rust_toolchain = Some("1.91.0".into());
        let error = DeveloperTools::from_host(&fixture.host, &fixture.node).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("native installation is not a directory")
        );
    }

    #[test]
    fn mise_only_exposes_activated_versions_not_shims_or_its_database() {
        let mut fixture = Fixture::new();
        let selected = fixture.host.mise.join("node/24");
        let other = fixture.host.mise.join("node/23");
        binary(&selected.join("bin/node"));
        binary(&other.join("bin/node"));
        fixture.node = selected.join("bin/node");
        fixture.host.path = env::join_paths([
            selected.join("bin"),
            fixture.host.mise.parent().unwrap().join("shims"),
        ])
        .unwrap();
        let tools = fixture.discover();
        assert_eq!(tools.read_roots, [selected]);
        assert!(!tools.contains(&other));
        assert!(!tools.contains(fixture.host.mise.parent().unwrap()));
    }

    #[test]
    fn nix_developer_directory_uses_system_selected_tools() {
        let mut fixture = Fixture::new();
        let base = fixture.host.home.parent().unwrap().to_owned();
        let developer = base.join("Developer");
        let bin = developer.join("usr/bin");
        binary(&bin.join("clang"));
        binary(&bin.join("ar"));
        symlink("clang", bin.join("clang++")).unwrap();
        let sdk = developer.join("SDKs/MacOSX.sdk");
        fs::create_dir_all(&sdk).unwrap();
        let selected = base.join("selected-developer");
        symlink(&developer, &selected).unwrap();

        fixture.host.developer = developer_directory(
            Some(Path::new("/nix/store/test-apple-sdk-14.4")),
            &selected,
            &base.join("CommandLineTools"),
        )
        .unwrap();
        assert_eq!(fixture.host.developer.as_deref(), Some(developer.as_path()));
        let tools = fixture.discover();
        assert_eq!(tools.read_roots, [developer.join("usr"), sdk.clone()]);
        let environment = tools.environment(Path::new("/private/cache")).unwrap();
        for (name, path) in [
            ("DEVELOPER_DIR", developer),
            ("SDKROOT", sdk),
            ("CC", bin.join("clang")),
            ("CXX", bin.join("clang++")),
        ] {
            assert!(environment.contains(&format!("{name}={}", path.display())));
        }
        assert!(!tools.path.iter().any(|path| path.starts_with("/nix/store")));
    }

    #[test]
    fn nix_developer_directory_uses_clt_when_selection_is_absent() {
        let fixture = Fixture::new();
        let base = fixture.host.home.parent().unwrap();
        let selected = base.join("selected-developer");
        let clt = base.join("CommandLineTools");
        fs::create_dir(&clt).unwrap();
        for override_ in [None, Some(Path::new("/nix/store/test-apple-sdk-14.4"))] {
            assert_eq!(
                developer_directory(override_, &selected, &clt).unwrap(),
                Some(clt.clone())
            );
        }
        fs::remove_dir(&clt).unwrap();
        assert_eq!(
            developer_directory(
                Some(Path::new("/nix/store/test-apple-sdk-14.4")),
                &selected,
                &clt,
            )
            .unwrap(),
            None
        );
        fs::create_dir(&clt).unwrap();
        fs::write(&selected, "not a directory").unwrap();
        assert!(developer_directory(None, &selected.join("child"), &clt).is_err());
    }

    #[test]
    fn native_developer_overrides_remain_explicit_and_validated() {
        let mut fixture = Fixture::new();
        let base = fixture.host.home.parent().unwrap().to_owned();
        let selected = base.join("selected-developer");
        let clt = base.join("CommandLineTools");
        fs::create_dir(&clt).unwrap();
        symlink(&clt, &selected).unwrap();
        for path in [
            base.join("Xcode-Beta.app/Contents/Developer"),
            base.join("missing-developer"),
            PathBuf::from("/nix/store-other/sdk"),
        ] {
            assert_eq!(
                developer_directory(Some(&path), &selected, &clt).unwrap(),
                Some(path)
            );
        }
        for path in [
            "",
            "relative/sdk",
            "/nix/store/sdk/../other",
            "/nix/store/sdk\n",
        ] {
            assert!(developer_directory(Some(Path::new(path)), &selected, &clt).is_err());
        }
        for path in [
            base.join("missing-developer"),
            fixture.host.workspace.clone(),
        ] {
            fixture.host.developer = developer_directory(Some(&path), &selected, &clt).unwrap();
            assert!(DeveloperTools::from_host(&fixture.host, &fixture.node).is_err());
        }
    }

    #[test]
    fn apple_xcode_and_clt_layouts_select_cxx_and_rust_linker_without_xcrun() {
        for xcode in [false, true] {
            let mut fixture = Fixture::new();
            let developer = fixture.host.home.parent().unwrap().join("Developer");
            let compiler = developer.join(if xcode {
                "Toolchains/XcodeDefault.xctoolchain"
            } else {
                "usr"
            });
            let bin = compiler.join(if xcode { "usr/bin" } else { "bin" });
            binary(&bin.join("clang"));
            binary(&bin.join("ar"));
            symlink("clang", bin.join("clang++")).unwrap();
            let sdks = developer.join(if xcode {
                "Platforms/MacOSX.platform/Developer/SDKs"
            } else {
                "SDKs"
            });
            fs::create_dir_all(sdks.join("MacOSX27.sdk")).unwrap();
            symlink("MacOSX27.sdk", sdks.join("MacOSX.sdk")).unwrap();
            fixture.host.developer = Some(developer.clone());
            let tools = fixture.discover();
            assert_eq!(tools.read_roots, [compiler, sdks.join("MacOSX27.sdk")]);
            assert!(!tools.contains(&developer.join("usr/bin/xcodebuild")) || !xcode);
            let env = tools.environment(Path::new("/private/cache")).unwrap();
            assert!(env.contains(&format!("CXX={}", bin.join("clang++").display())));
            assert!(env.contains(&format!(
                "CARGO_TARGET_{}_APPLE_DARWIN_LINKER={}",
                env::consts::ARCH.to_uppercase(),
                bin.join("clang").display()
            )));
            assert!(!tools.contains(Path::new("/Library/Preferences")));
        }
    }

    #[test]
    fn cargo_cache_persists_but_refuses_symlinks_and_nonprivate_directories() {
        let fixture = Fixture::new();
        let private = fixture.host.home.join("private");
        let cache = prepare_tool_cache(&private).unwrap();
        fs::write(cache.join("marker"), "persistent").unwrap();
        assert_eq!(prepare_tool_cache(&private).unwrap(), cache);
        assert_eq!(fs::read(cache.join("marker")).unwrap(), b"persistent");
        fs::remove_dir_all(&cache).unwrap();
        symlink(&fixture.host.home, &cache).unwrap();
        assert!(prepare_tool_cache(&private).is_err());
        fs::remove_file(&cache).unwrap();
        fs::set_permissions(&private, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(prepare_tool_cache(&private).is_err());
    }
}
