use crate::backend::ExecutionPlan;
use crate::harness::MountAccess;
use anyhow::{Result, ensure};
use std::path::Path;

fn quoted(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("native paths must be UTF-8"))?;
    ensure!(
        !text.chars().any(char::is_control),
        "native paths cannot contain control characters"
    );
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

pub(super) fn render(
    plan: &ExecutionPlan<'_>,
    tool: bool,
    socket: &Path,
    executable: &Path,
) -> Result<String> {
    let mut profile = r#"(version 1)
(deny default)
(allow process-fork)
(deny syscall-unix (syscall-number SYS_setsid SYS_setpgid))
(allow file-read*
    (literal "/") (literal "/System") (literal "/System/Volumes")
    (literal "/System/Volumes/Preboot") (literal "/System/Volumes/Preboot/Cryptexes")
    (subpath "/System/Volumes/Preboot/Cryptexes/OS")
    (subpath "/System/Library") (subpath "/usr/lib")
    (subpath "/usr/share/locale") (subpath "/usr/share/zoneinfo")
    (literal "/etc/ssl/openssl.cnf") (literal "/etc/ssl/cert.pem")
    (literal "/private/etc/ssl/openssl.cnf") (literal "/private/etc/ssl/cert.pem")
    (literal "/bin/bash"))
(allow file-read* file-write-data (literal "/dev/null"))
(allow file-read-metadata
    (literal "/var") (literal "/private") (literal "/private/var")
    (literal "/etc") (literal "/etc/ssl")
    (literal "/private/etc") (literal "/private/etc/ssl"))
(allow sysctl-read
    (sysctl-name "hw.pagesize" "hw.pagesize_compat" "hw.memsize" "hw.ncpu"
        "kern.osrelease" "kern.ostype" "kern.osversion" "kern.argmax"
        "kern.hostname" "kern.version" "hw.machine"))
"#
    .to_owned();
    let node = &plan.runtime.native.config.node;
    let package = plan
        .runtime
        .native
        .config
        .pi_cli
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let home = if tool {
        plan.tool_home
    } else {
        plan.private_home
    };
    profile.push_str(&format!(
        "(allow file-read* file-write* (subpath {}))\n",
        quoted(home)?
    ));
    profile.push_str(&format!(
        "(allow file-read* {} (subpath {}))\n",
        if plan.workspace.writable {
            "file-write*"
        } else {
            ""
        },
        quoted(plan.workspace.source)?
    ));
    profile.push_str(&format!("(allow file-read* (literal {}))\n", quoted(node)?));
    for path in [node.as_path(), package, home, plan.workspace.source]
        .into_iter()
        .flat_map(|path| path.ancestors().skip(1))
    {
        profile.push_str(&format!(
            "(allow file-read-metadata (literal {}))\n",
            quoted(path)?
        ));
    }
    if tool {
        let mut parents = std::collections::BTreeSet::new();
        for path in &plan.runtime.native.tools.read_roots {
            profile.push_str(&format!("(allow file-read* (subpath {}))\n", quoted(path)?));
            parents.extend(path.ancestors().skip(1));
        }
        if let Some(environment) = plan.dev_environment {
            profile.push_str(&format!(
                "(allow file-read* (literal {}))\n",
                quoted(&environment.script)?
            ));
            parents.extend(environment.script.ancestors().skip(1));
        }
        for parent in parents {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(parent)?
            ));
        }
        for link in &plan.runtime.native.tools.read_links {
            profile.push_str(&format!("(allow file-read* (literal {}))\n", quoted(link)?));
        }
        let link_parents: std::collections::BTreeSet<_> = plan
            .runtime
            .native
            .tools
            .read_links
            .iter()
            .flat_map(|path| path.ancestors().skip(1))
            .collect();
        for parent in link_parents {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(parent)?
            ));
        }
        profile.push_str(&format!(
            "(allow file-read* file-write* (subpath {}))\n",
            quoted(plan.tool_cache)?
        ));
        for parent in plan.tool_cache.ancestors().skip(1) {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(parent)?
            ));
        }
        profile.push_str(
            "(allow process-exec)\n(allow file-read* (subpath \"/bin\") (subpath \"/usr/bin\") (literal \"/private/etc/ssl/openssl.cnf\") (literal \"/private/etc/ssl/cert.pem\"))\n",
        );
    } else {
        profile.push_str(&format!("(allow process-exec (literal \"/bin/bash\") (literal {}))\n(allow file-read* (subpath {}))\n", quoted(node)?, quoted(package)?));
        profile.push_str(&format!(
            "(allow file-read* (literal {}))\n",
            quoted(&plan.session_dir.join("pi-wrapper"))?
        ));
        for mount in &plan.harness.mounts {
            ensure!(
                mount.source == mount.target && mount.access != MountAccess::TemporaryOverlay,
                "native mount remapping is unsupported"
            );
            for parent in mount.source.ancestors().skip(1) {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    quoted(parent)?
                ));
            }
            profile.push_str(&format!(
                "(allow file-read* {} (subpath {}))\n",
                if mount.access == MountAccess::ReadWrite {
                    "file-write*"
                } else {
                    ""
                },
                quoted(&mount.source)?
            ));
        }
        profile.push_str(&format!("(allow system-socket (socket-domain AF_UNIX))\n(allow network-outbound (remote unix-socket (literal {})))\n", quoted(socket)?));
        if plan.private_terminal {
            profile.push_str("(allow file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n");
        }
    }
    let gitconfig = plan.session_dir.join("gitconfig");
    if gitconfig.is_file() {
        profile.push_str(&format!(
            "(allow file-read* (literal {}))\n",
            quoted(&gitconfig)?
        ));
        for parent in gitconfig.ancestors().skip(1) {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(parent)?
            ));
        }
    }
    let github = plan.session_dir.join("github");
    if tool && github.is_dir() {
        let endpoint = plan
            .brokers
            .authenticated_http
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("GitHub account broker is unavailable"))?;
        let direct_socket = endpoint.socket_dir.join("direct.sock");
        profile.push_str(&format!("(allow file-read* (subpath {}))\n(allow system-socket (socket-domain AF_UNIX))\n(allow network-outbound (remote unix-socket (literal {})))\n", quoted(&github)?, quoted(&direct_socket)?));
        for parent in github
            .ancestors()
            .skip(1)
            .chain(direct_socket.ancestors().skip(1))
        {
            profile.push_str(&format!(
                "(allow file-read-metadata (literal {}))\n",
                quoted(parent)?
            ));
        }
    }
    if let Some(signing) = &plan.brokers.git_signing {
        for path in [executable, &executable.with_file_name("git-sign")] {
            profile.push_str(&format!(
                "(allow file-read* process-exec (literal {}))\n",
                quoted(path)?
            ));
            for parent in path.ancestors().skip(1) {
                profile.push_str(&format!(
                    "(allow file-read-metadata (literal {}))\n",
                    quoted(parent)?
                ));
            }
        }
        profile.push_str(&format!(
            "(allow system-socket (socket-domain AF_UNIX))\n(allow network-outbound (remote unix-socket (literal {})))\n",
            quoted(&signing.join("gateway.sock"))?
        ));
    }
    let general = !tool
        || plan
            .harness
            .environment
            .iter()
            .any(|(name, value)| name == "SLOPBOX_PI_TOOL_NETWORK" && value == "general");
    for endpoint in [
        plan.brokers.general.as_ref().filter(|_| general),
        plan.brokers.model.as_ref().filter(|_| !tool),
        plan.brokers.authenticated_http.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        profile.push_str(&format!("(allow network-outbound (require-all (socket-domain AF_INET) (remote tcp \"localhost:{}\")))\n", endpoint.port));
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::macos::{
        Config,
        runtime::{DeveloperTools, NativeRuntime},
    };
    use crate::backend::{BrokerConnections, RuntimePlan, Workspace};
    use crate::harness::PreparedHarness;

    #[test]
    fn project_closure_and_activation_are_read_only_and_tool_only() {
        use crate::backend::PreparedDevEnvironment;

        let environment = PreparedDevEnvironment {
            script: "/control/session/dev-env.sh".into(),
            profile: "/nix/store/selected-profile".into(),
            bash: "/nix/store/selected-bash/bin/bash".into(),
            store_paths: vec![
                "/nix/store/selected-profile".into(),
                "/nix/store/selected-bash".into(),
            ],
        };
        let mut tools = DeveloperTools::default();
        tools
            .read_roots
            .extend(environment.store_paths.iter().cloned());
        let runtime = RuntimePlan {
            path: Default::default(),
            native: NativeRuntime {
                config: Config {
                    node: "/runtime/node".into(),
                    pi_cli: "/runtime/pi/dist/cli.js".into(),
                    tool_timeout_seconds: 10,
                },
                tools,
            },
        };
        let plan = ExecutionPlan {
            session_dir: Path::new("/control/session"),
            private_home: Path::new("/harness-home"),
            tool_home: Path::new("/tool-home"),
            tool_cache: Path::new("/tool-cache"),
            workspace: Workspace {
                source: Path::new("/workspace"),
                target: Path::new("/workspace"),
                writable: true,
            },
            dev_environment: Some(&environment),
            runtime: &runtime,
            harness: &PreparedHarness::default(),
            brokers: &BrokerConnections::default(),
            environment: &[],
            private_terminal: false,
            clipboard: false,
        };
        for tool in [false, true] {
            let profile =
                render(&plan, tool, Path::new("/tool.sock"), Path::new("/worker")).unwrap();
            for path in &environment.store_paths {
                assert_eq!(
                    profile.contains(&format!(
                        "(allow file-read* (subpath {}))",
                        quoted(path).unwrap()
                    )),
                    tool
                );
            }
            assert_eq!(
                profile.contains(&format!(
                    "(allow file-read* (literal {}))",
                    quoted(&environment.script).unwrap()
                )),
                tool
            );
            for directory in ["/nix/store", "/nix/var/nix", "/control", "/control/session"] {
                assert!(
                    !profile.contains(&format!("(subpath \"{directory}\")")),
                    "{profile}"
                );
            }
            assert!(!profile.lines().any(|line| line.contains("file-write")
                && (line.contains("/nix/") || line.contains("/control/"))));
            assert_eq!(
                profile
                    .matches("(allow file-read-metadata (literal \"/nix/store\"))")
                    .count(),
                usize::from(tool)
            );
        }
    }

    #[test]
    fn account_configuration_is_read_only_and_signing_requires_a_broker() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("session");
        std::fs::create_dir(&session).unwrap();
        let gitconfig = session.join("gitconfig");
        std::fs::write(&gitconfig, "generated").unwrap();
        let executable = root.path().join("control/worker");
        let helper = executable.with_file_name("git-sign");
        let signing = root.path().join("signing");
        let account = root.path().join("account");
        let github = crate::github::prepare(
            &session,
            &crate::backend::ProxyEndpoint {
                socket_dir: account.clone(),
                port: 12345,
            },
        )
        .unwrap();
        let runtime = RuntimePlan {
            path: Default::default(),
            native: NativeRuntime {
                config: Config {
                    node: "/runtime/node".into(),
                    pi_cli: "/runtime/pi/dist/cli.js".into(),
                    tool_timeout_seconds: 10,
                },
                tools: DeveloperTools::default(),
            },
        };
        for enabled in [false, true] {
            let brokers = BrokerConnections {
                authenticated_http: Some(crate::backend::ProxyEndpoint {
                    socket_dir: account.clone(),
                    port: 12345,
                }),
                git_signing: enabled.then(|| signing.clone()),
                ..Default::default()
            };
            let plan = ExecutionPlan {
                session_dir: &session,
                private_home: Path::new("/harness-home"),
                tool_home: Path::new("/tool-home"),
                tool_cache: Path::new("/tool-cache"),
                workspace: Workspace {
                    source: Path::new("/workspace"),
                    target: Path::new("/workspace"),
                    writable: true,
                },
                dev_environment: None,
                runtime: &runtime,
                harness: &PreparedHarness::default(),
                brokers: &brokers,
                environment: &[],
                private_terminal: false,
                clipboard: false,
            };
            for tool in [false, true] {
                let profile = render(&plan, tool, Path::new("/tool.sock"), &executable).unwrap();
                assert!(profile.contains(&format!(
                    "(allow file-read* (literal {}))",
                    quoted(&gitconfig).unwrap()
                )));
                assert_eq!(
                    profile.contains(&format!("(subpath {})", quoted(&github).unwrap())),
                    tool
                );
                assert_eq!(
                    profile.contains(&quoted(&account.join("direct.sock")).unwrap()),
                    tool
                );
                assert!(!profile.contains(&format!("(subpath {})", quoted(&account).unwrap())));
                for path in [&executable, &helper, &signing.join("gateway.sock")] {
                    assert_eq!(profile.contains(&quoted(path).unwrap()), enabled);
                }
                for directory in [&session, executable.parent().unwrap(), &signing] {
                    assert!(
                        !profile.contains(&format!("(subpath {})", quoted(directory).unwrap()))
                    );
                }
                assert!(!profile.lines().any(|line| line.contains("file-write")
                    && line.contains(root.path().to_str().unwrap())));
            }
        }
    }
}
