pub(crate) mod diagnostics;
pub(crate) mod init;
pub(crate) mod probe;
pub(crate) mod runtime;
pub(crate) mod tool;

use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Seek};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};

#[cfg(test)]
use super::{BrokerConnections, RuntimePlan, Workspace};
use super::{ExecutionPlan, RuntimeStore};
use crate::command::find_optional_executable;
use crate::fs_util::find_socket;
use crate::harness::MountAccess;
#[cfg(test)]
use crate::harness::PreparedHarness;

pub(crate) fn command(
    bwrap: &Path,
    plan: &ExecutionPlan<'_>,
    child: &[OsString],
) -> Result<Command> {
    let mut system_links = Vec::new();
    for path in [
        Some(Path::new("/bin/sh")),
        env::var_os("NIX_LD").map(|_| Path::new("/lib64/ld-linux-x86-64.so.2")),
        Some(Path::new("/usr/bin/env")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(target) = resolve_host_symlink(path)? {
            system_links.push((path.to_path_buf(), target));
        }
    }
    command_with_system_links(bwrap, plan, child, &system_links)
}

fn command_with_system_links(
    bwrap: &Path,
    plan: &ExecutionPlan<'_>,
    child: &[OsString],
    system_links: &[(PathBuf, PathBuf)],
) -> Result<Command> {
    ensure!(!child.is_empty(), "a command is required");
    let proxies: Vec<_> = [
        ("general", plan.brokers.general.as_ref()),
        ("model", plan.brokers.model.as_ref()),
        (
            "authenticated-http",
            plan.brokers.authenticated_http.as_ref(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, endpoint)| endpoint.map(|endpoint| (name, endpoint)))
    .collect();
    let sandbox_path = &plan.runtime.path;
    let mut command = Command::new(bwrap);
    command.env_clear();
    command.args([
        "--die-with-parent",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-net",
        "--hostname",
        "slopbox",
        "--cap-drop",
        "ALL",
        "--clearenv",
    ]);

    if !plan.private_terminal {
        command.arg("--new-session");
    }

    for directory in [
        Path::new("/nix"),
        Path::new("/nix/store"),
        Path::new("/run"),
        Path::new("/run/current-system"),
        Path::new("/run/slopbox"),
        Path::new("/run/slopbox-host"),
        Path::new("/run/slopbox-host/general"),
        Path::new("/run/slopbox-host/model"),
        Path::new("/run/slopbox-host/authenticated-http"),
        Path::new("/run/slopbox-host/git-signing"),
        Path::new("/run/slopbox-tool-home"),
        Path::new("/etc"),
        Path::new("/etc/ssl"),
        Path::new("/etc/static"),
        Path::new("/etc/static/ssl"),
        Path::new("/bin"),
        Path::new("/lib64"),
        Path::new("/usr"),
        Path::new("/usr/bin"),
        Path::new("/home"),
    ] {
        add_dir(&mut command, directory);
    }

    for directory in &plan.harness.directories {
        add_dir(&mut command, directory);
    }

    match &plan.runtime.store {
        RuntimeStore::Host => {
            command.args(["--ro-bind", "/nix/store", "/nix/store"]);
            add_optional_ro_bind(
                &mut command,
                Path::new("/run/current-system/sw"),
                Path::new("/run/current-system/sw"),
            );
        }
        RuntimeStore::Selected(paths) => {
            for path in paths {
                command.arg("--ro-bind").arg(path).arg(path);
            }
        }
    }

    for path in [
        "/etc/hosts",
        "/etc/passwd",
        "/etc/group",
        "/etc/nsswitch.conf",
        "/etc/ssl/certs",
        "/etc/static/ssl/certs",
    ] {
        add_optional_ro_bind(&mut command, Path::new(path), Path::new(path));
    }

    for (path, target) in system_links {
        command.arg("--symlink").arg(target).arg(path);
    }

    command.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    command.arg("--bind");
    command.arg(plan.private_home);
    command.arg("/home/slopbox");
    command.arg("--bind");
    command.arg(plan.tool_home);
    command.arg("/run/slopbox-tool-home");

    add_parent_dirs(&mut command, plan.workspace.target);
    command.arg(if plan.workspace.writable {
        "--bind"
    } else {
        "--ro-bind"
    });
    command.arg(plan.workspace.source);
    command.arg(plan.workspace.target);
    command.arg("--chdir");
    command.arg(plan.workspace.target);

    if let Some(dev_environment) = &plan.dev_environment {
        command
            .arg("--ro-bind")
            .arg(&dev_environment.script)
            .arg("/run/slopbox/dev-env.sh");
    }
    if plan.clipboard {
        command
            .arg("--ro-bind")
            .arg(plan.session_dir.join("clipboard"))
            .arg(crate::clipboard::MOUNT);
    }
    for mount in &plan.harness.mounts {
        add_parent_dirs(&mut command, &mount.target);
        match mount.access {
            MountAccess::ReadOnly => {
                command
                    .arg("--ro-bind")
                    .arg(&mount.source)
                    .arg(&mount.target);
            }
            MountAccess::ReadWrite => {
                command.arg("--bind").arg(&mount.source).arg(&mount.target);
            }
            MountAccess::TemporaryOverlay => {
                command
                    .arg("--overlay-src")
                    .arg(&mount.source)
                    .arg("--tmp-overlay")
                    .arg(&mount.target);
            }
        }
    }
    let gitconfig = plan.session_dir.join("gitconfig");
    let has_gitconfig = gitconfig.is_file();
    if has_gitconfig {
        command
            .arg("--ro-bind")
            .arg(gitconfig)
            .arg("/run/slopbox/gitconfig");
    }
    let account_ca = plan.session_dir.join("account-ca.pem");
    if account_ca.is_file() {
        ensure!(
            plan.brokers.authenticated_http.is_some(),
            "account TLS broker is unavailable"
        );
        command
            .arg("--ro-bind")
            .arg(account_ca)
            .arg("/run/slopbox/account-ca.pem");
    }
    let github = plan.session_dir.join("github");
    if github.is_dir() {
        command
            .arg("--ro-bind")
            .arg(github)
            .arg("/run/slopbox/github");
    }
    if let Some(signing) = &plan.brokers.git_signing {
        command
            .arg("--ro-bind")
            .arg(signing)
            .arg("/run/slopbox-host/git-signing");
        command
            .arg("--ro-bind")
            .arg(plan.session_dir.join("git-sign"))
            .arg("/run/slopbox/git-sign");
    }
    for (name, endpoint) in &proxies {
        command
            .arg("--ro-bind")
            .arg(&endpoint.socket_dir)
            .arg(format!("/run/slopbox-host/{name}"));
    }

    let current_exe = fs::canonicalize(env::current_exe()?)
        .context("failed to resolve the Slopbox executable")?;
    command
        .arg("--ro-bind")
        .arg(current_exe)
        .arg("/run/slopbox/slopbox");

    for (name, value) in sandbox_environment(sandbox_path, has_gitconfig, plan.workspace.target)
        .into_iter()
        .chain(plan.environment.iter().cloned())
        .chain(plan.harness.environment.iter().cloned())
    {
        command.arg("--setenv").arg(name).arg(value);
    }

    let mut child = child.to_vec();
    child[0] = normalized_executable(&child[0])?;
    let child = if plan.dev_environment.is_some() {
        let bash = find_executable("bash", sandbox_path)?;
        let mut activated = vec![
            bash.into_os_string(),
            OsString::from("-c"),
            OsString::from("source /run/slopbox/dev-env.sh; exec \"$@\""),
            OsString::from("slopbox-dev-env"),
        ];
        activated.extend(child);
        activated
    } else {
        child
    };

    command.arg("--");
    if !proxies.is_empty() {
        command.arg("/run/slopbox/slopbox").arg("__sandbox-init");
        for (name, endpoint) in &proxies {
            command
                .arg(format!("--{name}-gateway-socket"))
                .arg(format!("/run/slopbox-host/{name}/gateway.sock"))
                .arg(format!("--{name}-proxy-port"))
                .arg(endpoint.port.to_string());
        }
        command.arg("--").args(child);
    } else {
        command.args(child);
    }

    close_inherited_descriptors(&mut command);
    Ok(command)
}

fn add_dir(command: &mut Command, path: &Path) {
    command.arg("--dir").arg(path);
}

fn add_parent_dirs(command: &mut Command, path: &Path) {
    let mut parents: Vec<_> = path
        .ancestors()
        .skip(1)
        .filter(|path| *path != Path::new("/"))
        .collect();
    parents.reverse();

    let mut seen = HashSet::new();
    for parent in parents {
        if seen.insert(parent.to_path_buf()) {
            add_dir(command, parent);
        }
    }
}

fn add_optional_ro_bind(command: &mut Command, source: &Path, destination: &Path) {
    if source.exists() {
        command.arg("--ro-bind").arg(source).arg(destination);
    }
}

fn resolve_host_symlink(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }

    let target = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve system link {}", path.display()))?;
    ensure!(
        target.starts_with("/nix/store"),
        "system path {} resolves outside /nix/store",
        path.display()
    );
    Ok(Some(target))
}

pub(crate) fn sandbox_path() -> Result<OsString> {
    let host_path = env::var_os("PATH").context("PATH is not set")?;
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for entry in env::split_paths(&host_path) {
        let Ok(canonical) = fs::canonicalize(&entry) else {
            continue;
        };
        if canonical.starts_with("/nix/store") && seen.insert(canonical.clone()) {
            entries.push(canonical);
        }
    }

    ensure!(
        !entries.is_empty(),
        "PATH contains no usable Nix store directories"
    );
    env::join_paths(entries).context("failed to construct sandbox PATH")
}

pub(crate) fn required_executor(path: &OsStr) -> Result<PathBuf> {
    find_executable("bwrap", path).context(
        "bubblewrap is required on the host before launch; use the packaged slopbox command or run this debug binary with nix develop -c ./target/debug/slopbox",
    )
}

pub(crate) fn find_executable(name: &str, path: &OsStr) -> Result<PathBuf> {
    find_optional_executable(name, path)
        .with_context(|| format!("required executable {name} is not available in the sandbox PATH"))
}

fn sandbox_environment(
    path: &OsStr,
    git_config: bool,
    workspace: &Path,
) -> Vec<(OsString, OsString)> {
    let mut values = vec![
        (OsString::from("HOME"), OsString::from("/home/slopbox")),
        (OsString::from("USER"), OsString::from("slopbox")),
        (OsString::from("LOGNAME"), OsString::from("slopbox")),
        (OsString::from("SHELL"), OsString::from("/bin/sh")),
        (
            OsString::from("PATH"),
            env::join_paths(
                std::iter::once(PathBuf::from("/run/slopbox")).chain(env::split_paths(path)),
            )
            .expect("sandbox PATH entries are valid"),
        ),
        (OsString::from("TMPDIR"), OsString::from("/tmp")),
        (
            OsString::from("XDG_CONFIG_HOME"),
            OsString::from("/home/slopbox/.config"),
        ),
        (
            OsString::from("XDG_CACHE_HOME"),
            OsString::from("/home/slopbox/.cache"),
        ),
        (
            OsString::from("XDG_DATA_HOME"),
            OsString::from("/home/slopbox/.local/share"),
        ),
        (
            OsString::from("XDG_STATE_HOME"),
            OsString::from("/home/slopbox/.local/state"),
        ),
        (
            OsString::from("NO_PROXY"),
            OsString::from("127.0.0.1,localhost,::1"),
        ),
        (
            OsString::from("no_proxy"),
            OsString::from("127.0.0.1,localhost,::1"),
        ),
        (OsString::from("SLOPBOX_SANDBOX"), OsString::from("1")),
        (
            OsString::from("SLOPBOX_WORKSPACE"),
            workspace.as_os_str().to_os_string(),
        ),
    ];

    if git_config {
        values.push((
            OsString::from("GIT_CONFIG_GLOBAL"),
            OsString::from("/run/slopbox/gitconfig"),
        ));
        values.push((OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")));
    }

    for name in ["TERM", "COLORTERM", "LANG", "TZ"] {
        if let Some(value) = env::var_os(name) {
            values.push((OsString::from(name), value));
        }
    }
    if let Some(value) = env::var_os("NIX_LD") {
        let path = PathBuf::from(value);
        if let Ok(path) = fs::canonicalize(path)
            && path.starts_with("/nix/store")
        {
            values.push((OsString::from("NIX_LD"), path.into_os_string()));
        }
    }
    if let Some(value) = env::var_os("NIX_LD_LIBRARY_PATH") {
        let canonical: Option<Vec<_>> = env::split_paths(&value)
            .map(|path| fs::canonicalize(path).ok())
            .collect();
        if let Some(canonical) = canonical
            && canonical.iter().all(|path| path.starts_with("/nix/store"))
        {
            values.push((
                OsString::from("NIX_LD_LIBRARY_PATH"),
                env::join_paths(canonical).expect("canonical Nix library paths are valid"),
            ));
        }
    }
    for (name, value) in env::vars_os() {
        if name.as_bytes().starts_with(b"LC_") {
            values.push((name, value));
        }
    }

    values
}

fn normalized_executable(executable: &OsStr) -> Result<OsString> {
    let path = Path::new(executable);
    if !path.is_absolute() {
        return Ok(executable.to_os_string());
    }

    let canonical = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve executable {}", path.display()))?;
    if canonical.starts_with("/nix/store") {
        return Ok(canonical.into_os_string());
    }

    Ok(executable.to_os_string())
}

fn find_submounts(workspace: &Path) -> Result<Vec<PathBuf>> {
    let mountinfo = BufReader::new(
        fs::File::open("/proc/self/mountinfo").context("failed to read mount table")?,
    );
    let mut submounts = Vec::new();

    for line in mountinfo.lines() {
        let line = line.context("failed to read mount table")?;
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 5 {
            continue;
        }
        let mountpoint = PathBuf::from(decode_mountinfo_path(fields[4])?);
        if mountpoint != workspace && mountpoint.starts_with(workspace) {
            submounts.push(mountpoint);
        }
    }

    submounts.sort();
    submounts.dedup();
    Ok(submounts)
}

fn decode_mountinfo_path(value: &str) -> Result<OsString> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'\\' {
            ensure!(
                index + 3 < bytes.len(),
                "invalid mountinfo escape in {value}"
            );
            let digits = &bytes[index + 1..index + 4];
            ensure!(
                digits.iter().all(|byte| (b'0'..=b'7').contains(byte)),
                "invalid mountinfo escape in {value}"
            );
            output.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + digits[2] - b'0');
            index += 4;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }

    Ok(OsString::from_vec(output))
}

pub(crate) fn validate_workspace_target(workspace: &Path) -> Result<()> {
    ensure!(
        !workspace.starts_with("/home/slopbox"),
        "workspace conflicts with the sandbox-private home path"
    );
    Ok(())
}

pub(crate) fn validate_workspace_contents(workspace: &Path) -> Result<()> {
    if let Some(socket) = find_socket(workspace)? {
        bail!(
            "workspace contains a Unix socket at {}; stop the service and remove its socket, or select a directory without host IPC",
            socket.display()
        );
    }
    let submounts = find_submounts(workspace)?;
    ensure!(
        submounts.is_empty(),
        "workspace contains submounts that would widen its filesystem grant: {}; select a workspace without nested mounts",
        display_paths(&submounts)
    );
    Ok(())
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn clone_or_copy_file(
    input: &mut fs::File,
    output: &mut fs::File,
) -> std::io::Result<()> {
    let cloned = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
    if cloned == 0 {
        return Ok(());
    }
    input.rewind()?;
    output.set_len(0)?;
    output.rewind()?;
    std::io::copy(input, output)?;
    Ok(())
}

fn close_inherited_descriptors(command: &mut Command) {
    // Do not leak descriptors opened by the supervisor into bubblewrap or its child.
    unsafe {
        command.pre_exec(|| {
            let result = libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            );
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ENOSYS) {
                    return Err(error);
                }
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_mounts_and_forwards_only_selected_broker_planes() {
        use crate::backend::ProxyEndpoint;

        let session = tempfile::tempdir().unwrap();
        let runtime = RuntimePlan {
            store: RuntimeStore::Selected(vec!["/nix/store/fixture-runtime".into()]),
            path: "/nix/store/fixture-runtime/bin".into(),
        };
        let harness = PreparedHarness::default();
        let cases = [
            (BrokerConnections::default(), vec![]),
            (
                BrokerConnections {
                    general: Some(ProxyEndpoint {
                        socket_dir: "/host/general".into(),
                        port: 10001,
                    }),
                    authenticated_http: Some(ProxyEndpoint {
                        socket_dir: "/host/authenticated-http".into(),
                        port: 10003,
                    }),
                    ..BrokerConnections::default()
                },
                vec!["general", "authenticated-http"],
            ),
            (
                BrokerConnections {
                    model: Some(ProxyEndpoint {
                        socket_dir: "/host/model".into(),
                        port: 10002,
                    }),
                    ..BrokerConnections::default()
                },
                vec!["model"],
            ),
        ];
        for (brokers, selected) in cases {
            let ca = session.path().join("account-ca.pem");
            if brokers.authenticated_http.is_some() {
                fs::write(&ca, "public trust").unwrap();
            } else if ca.exists() {
                fs::remove_file(&ca).unwrap();
            }
            let plan = ExecutionPlan {
                workspace: Workspace {
                    source: Path::new("/host/workspace"),
                    target: Path::new("/workspace with spaces"),
                    writable: false,
                },
                private_home: Path::new("/host/home"),
                tool_home: Path::new("/host/tool-home"),
                session_dir: session.path(),
                dev_environment: None,
                runtime: &runtime,
                harness: &harness,
                brokers: &brokers,
                environment: &[],
                private_terminal: false,
                clipboard: false,
            };
            let command = command_with_system_links(
                Path::new("/not-executed/bwrap"),
                &plan,
                &["sh".into()],
                &[("/bin/sh".into(), "/nix/store/fixture-shell/bin/sh".into())],
            )
            .unwrap();
            let arguments: Vec<_> = command
                .get_args()
                .map(|value| value.to_str().unwrap())
                .collect();
            assert!(arguments.windows(3).any(|args| args == ["--symlink", "/nix/store/fixture-shell/bin/sh", "/bin/sh"]));
            for flag in [
                "--unshare-user",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--unshare-net",
                "--clearenv",
                "--new-session",
            ] {
                assert!(arguments.contains(&flag), "missing {flag}");
            }
            assert!(
                arguments
                    .windows(2)
                    .any(|args| args == ["--cap-drop", "ALL"])
            );
            assert!(
                arguments
                    .windows(3)
                    .any(|args| args == ["--ro-bind", "/host/workspace", "/workspace with spaces"])
            );
            assert!(arguments.windows(3).any(|args| args
                == [
                    "--ro-bind",
                    "/nix/store/fixture-runtime",
                    "/nix/store/fixture-runtime"
                ]));
            assert!(
                !arguments
                    .windows(3)
                    .any(|args| args == ["--ro-bind", "/nix/store", "/nix/store"])
            );
            assert_eq!(arguments.contains(&"__sandbox-init"), !selected.is_empty());
            assert_eq!(
                arguments.windows(3).any(|args| args
                    == [
                        "--ro-bind",
                        ca.to_str().unwrap(),
                        "/run/slopbox/account-ca.pem"
                    ]),
                brokers.authenticated_http.is_some()
            );
            for (name, port) in [
                ("general", "10001"),
                ("model", "10002"),
                ("authenticated-http", "10003"),
            ] {
                let enabled = selected.contains(&name);
                assert_eq!(
                    arguments.windows(3).any(|args| args
                        == [
                            "--ro-bind",
                            &format!("/host/{name}"),
                            &format!("/run/slopbox-host/{name}")
                        ]),
                    enabled
                );
                assert_eq!(
                    arguments.windows(2).any(|args| args
                        == [
                            &format!("--{name}-gateway-socket"),
                            &format!("/run/slopbox-host/{name}/gateway.sock")
                        ]),
                    enabled
                );
                assert_eq!(
                    arguments
                        .windows(2)
                        .any(|args| args == [&format!("--{name}-proxy-port"), port]),
                    enabled
                );
            }
        }
    }

    #[test]
    fn direct_command_preserves_arguments_and_private_terminal_session() {
        let session = tempfile::tempdir().unwrap();
        let runtime = RuntimePlan {
            store: RuntimeStore::Host,
            path: "/nix/store/fixture-runtime/bin".into(),
        };
        let harness = PreparedHarness::default();
        let brokers = BrokerConnections::default();
        let plan = ExecutionPlan {
            workspace: Workspace {
                source: Path::new("/host/staged-workspace"),
                target: Path::new("/workspace"),
                writable: true,
            },
            private_home: Path::new("/host/home"),
            tool_home: Path::new("/host/tool-home"),
            session_dir: session.path(),
            dev_environment: None,
            runtime: &runtime,
            harness: &harness,
            brokers: &brokers,
            environment: &[],
            private_terminal: true,
            clipboard: false,
        };
        let child = vec![
            OsString::from("sh"),
            OsString::from("-c"),
            OsString::from("printf '%s' 'a quoted argument'"),
        ];
        let command = command_with_system_links(
            Path::new("/not-executed/bwrap"),
            &plan,
            &child,
            &[("/bin/sh".into(), "/nix/store/fixture-shell/bin/sh".into())],
        )
        .unwrap();
        let arguments: Vec<_> = command.get_args().collect();
        assert!(!arguments.contains(&OsStr::new("--new-session")));
        assert!(!arguments.contains(&OsStr::new("__sandbox-init")));
        assert!(
            arguments
                .windows(3)
                .any(|args| args == ["--bind", "/host/staged-workspace", "/workspace"])
        );
        assert!(
            arguments
                .windows(3)
                .any(|args| args == ["--ro-bind", "/nix/store", "/nix/store"])
        );
        let separator = arguments
            .iter()
            .position(|argument| *argument == "--")
            .unwrap();
        assert_eq!(arguments[separator + 1..], child);
    }

    #[test]
    fn system_links_reject_non_store_targets() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sh");
        assert!(resolve_host_symlink(&path).unwrap().is_none());
        fs::write(&path, "not a trusted system executable").unwrap();
        assert!(
            resolve_host_symlink(&path)
                .unwrap_err()
                .to_string()
                .contains("resolves outside /nix/store")
        );
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(resolve_host_symlink(&link).is_err());
    }

    #[test]
    fn decodes_mountinfo_paths() {
        assert_eq!(
            decode_mountinfo_path(r#"/tmp/a\040b\011c\134d"#).unwrap(),
            OsString::from("/tmp/a b\tc\\d")
        );
    }

    #[test]
    fn workspace_check_rejects_host_ipc_until_the_socket_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("service.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let error = validate_workspace_contents(root.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("stop the service and remove its socket")
        );
        fs::remove_file(socket).unwrap();
        validate_workspace_contents(root.path()).unwrap();
    }

    #[test]
    fn missing_bubblewrap_explains_how_to_launch_the_debug_binary() {
        let directory = tempfile::tempdir().unwrap();
        let error = required_executor(directory.path().as_os_str()).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("bubblewrap is required on the host before launch"));
        assert!(message.contains("nix develop -c ./target/debug/slopbox"));
        assert!(message.contains("required executable bwrap"));
    }
}
