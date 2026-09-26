use super::*;

impl Pi {
    pub(super) fn prepare_native(
        &self,
        context: &PrepareContext<'_>,
        config: &crate::backend::macos::Config,
    ) -> Result<PreparedHarness> {
        ensure!(!context.dry_run, "unsupported native Pi preparation");
        let config = config.resolve()?;
        self.validate_native_resources()?;
        let host_agent = if self.mode == HarnessMode::None {
            context.session_dir.join("disabled-host-pi-agent")
        } else {
            self.home.join(".pi/agent")
        };
        configure_pi(context.session_dir, &host_agent)?;
        let agent = context.session_dir.join("pi-state");
        let trusted = context.session_dir.join("pi-agent");
        let history = context.private_home.join(".pi/agent/sessions");
        let private_home = context.session_dir.join("home");
        let resources = self.native_resources(&private_home, &agent)?;
        fs::create_dir_all(private_home.join(".pi"))?;
        std::os::unix::fs::symlink(&agent, private_home.join(".pi/agent"))?;
        std::os::unix::fs::symlink(&history, agent.join("sessions"))?;
        let mut settings: Value =
            serde_json::from_str(&fs::read_to_string(agent.join("settings.json"))?)?;
        if settings.get("defaultProvider").is_none()
            && let Some(provider) = context.model_providers.first()
        {
            settings["defaultProvider"] = match provider {
                crate::provider::Kind::OpenRouter => "openrouter",
                crate::provider::Kind::OpenAiCodex => "openai-codex",
            }
            .into();
        }
        settings["enableInstallTelemetry"] = false.into();
        write_private(
            &agent.join("settings.json"),
            &serde_json::to_string(&settings)?,
        )?;
        // Normal Pi tools and trusted imports; only bash is overridden.
        std::os::unix::fs::symlink(trusted.join("AGENTS.md"), agent.join("AGENTS.md"))?;
        let mut arguments = vec![
            config.node.to_string_lossy().into_owned(),
            config.pi_cli.to_string_lossy().into_owned(),
            "--no-extensions".into(),
            "--no-skills".into(),
            "--no-prompt-templates".into(),
            "--no-themes".into(),
            "--no-approve".into(),
            "--extension".into(),
            trusted
                .join("extensions/slopbox.ts")
                .to_string_lossy()
                .into_owned(),
        ];
        append_resource_arguments(&mut arguments, &resources);
        let script = format!(
            "#!/bin/sh\nexec {} \"$@\"\n",
            arguments
                .iter()
                .map(|argument| shell_quote(argument))
                .collect::<Vec<_>>()
                .join(" ")
        );
        write_private(&context.session_dir.join("pi-wrapper"), &script)?;
        let mut environment = vec![
            ("PI_CODING_AGENT_DIR".into(), agent.clone().into_os_string()),
            (
                "PI_CODING_AGENT_SESSION_DIR".into(),
                history.clone().into_os_string(),
            ),
            ("PI_OFFLINE".into(), "1".into()),
            ("PI_TELEMETRY".into(), "0".into()),
            (
                "SLOPBOX_PI_TOOL_NETWORK".into(),
                if context.tool_network == ToolNetwork::General {
                    "general"
                } else {
                    "none"
                }
                .into(),
            ),
        ];
        for provider in context.model_providers {
            let (key, value) = match provider {
                crate::provider::Kind::OpenRouter => ("OPENROUTER_API_KEY", "slopbox:openrouter"),
                crate::provider::Kind::OpenAiCodex => ("SLOPBOX_CODEX_ENABLED", "1"),
            };
            environment.push((key.into(), value.into()));
        }
        let mut resource_mounts: Vec<_> = resources
            .mounts
            .into_iter()
            .map(|(source, target)| Mount {
                source,
                target,
                access: MountAccess::ReadOnly,
            })
            .collect();
        resource_mounts.extend([
            Mount {
                source: trusted.clone(),
                target: trusted,
                access: MountAccess::ReadOnly,
            },
            Mount {
                source: agent.clone(),
                target: agent,
                access: MountAccess::ReadWrite,
            },
            Mount {
                source: history.clone(),
                target: history,
                access: MountAccess::ReadWrite,
            },
        ]);
        Ok(PreparedHarness {
            executable: Some(config.node),
            directories: Vec::new(),
            environment,
            mounts: resource_mounts,
        })
    }
}

impl Pi {
    // Reuse Linux's discovery/filtering plan, replacing namespace paths with
    // canonical host paths. Seatbelt grants these only to the harness, read-only.
    fn native_resources(&self, private_home: &Path, agent: &Path) -> Result<Resources> {
        self.validate_native_resources()?;
        let mut mappings = Vec::new();
        let mut resources = Resources::default();
        for (source, target) in &self.resources.mounts {
            let source = source
                .canonicalize()
                .with_context(|| format!("resolve Pi resource {}", source.display()))?;
            self.validate_native_resource_root(&source)?;
            if !resources
                .mounts
                .iter()
                .any(|(existing, _)| existing == &source)
            {
                resources.mounts.push((source.clone(), source.clone()));
            }
            mappings.push((source, target.clone()));
        }
        let translate = |paths: &[PathBuf]| -> Result<Vec<PathBuf>> {
            paths
                .iter()
                .map(|path| {
                    mappings
                        .iter()
                        .filter_map(|(source, target)| {
                            path.strip_prefix(target)
                                .ok()
                                .map(|relative| (target, source.join(relative)))
                        })
                        .max_by_key(|(target, _)| target.components().count())
                        .map(|(_, path)| path)
                        .with_context(|| format!("unmapped native Pi resource {}", path.display()))
                })
                .collect()
        };
        resources.extensions = translate(&self.resources.extensions)?;
        resources.skills = translate(&self.resources.skills)?;
        resources.prompts = translate(&self.resources.prompts)?;
        resources.themes = translate(&self.resources.themes)?;
        // Preserve conventional ~/.pi/agent and configured ~/ paths for plugins.
        // These aliases do not grant access to anything outside the reviewed roots.
        for (source, target) in mappings {
            let Ok(relative) = target.strip_prefix("/home/slopbox") else {
                continue;
            };
            let alias = if let Ok(relative) = relative.strip_prefix(".pi/agent") {
                agent.join(relative)
            } else {
                private_home.join(relative)
            };
            for parent in alias
                .ancestors()
                .skip(1)
                .take_while(|path| path.starts_with(private_home))
            {
                ensure_not_symlink(parent)?;
            }
            fs::create_dir_all(alias.parent().context("invalid Pi resource alias")?)?;
            std::os::unix::fs::symlink(&source, &alias)
                .with_context(|| format!("create native Pi resource alias {}", alias.display()))?;
        }
        Ok(resources)
    }

    pub(crate) fn validate_native_resources(&self) -> Result<()> {
        ensure!(
            self.resources.temporary_overlays.is_empty(),
            "native Pi temporary_overlay_mounts are not supported; remove them or use Linux"
        );
        // AGENTS.md is copied as text, so its canonical source must be checked
        // too: a host-resource symlink must not turn into a copied credential.
        let agents = self.home.join(".pi/agent/AGENTS.md");
        if self.mode != HarnessMode::None && agents.is_file() {
            self.validate_native_resource_root(&agents.canonicalize()?)?;
        }
        for (source, _) in &self.resources.mounts {
            self.validate_native_resource_root(&source.canonicalize()?)?;
        }
        Ok(())
    }

    fn validate_native_resource_root(&self, source: &Path) -> Result<()> {
        use crate::backend::macos::runtime::resolve_ancestors;
        let home = self.home.canonicalize()?;
        let config = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let mut protected = vec![config.join("slopbox"), data.join("slopbox")];
        for name in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".docker",
            ".kube",
            ".password-store",
            ".netrc",
            ".gitconfig",
            ".cargo",
            "Library",
            ".pi/agent/auth.json",
            ".pi/agent/models.json",
            ".pi/agent/settings.json",
            ".pi/agent/sessions",
        ] {
            protected.push(home.join(name));
        }
        let mut protected = protected
            .iter()
            .map(|path| resolve_ancestors(path))
            .collect::<Result<Vec<_>>>()?;
        // runtime_root separately refuses symlink/private-directory violations.
        protected.push(PathBuf::from(format!(
            "/private/var/tmp/slopbox-{}",
            unsafe { libc::getuid() }
        )));
        for path in protected {
            ensure!(
                !source.starts_with(&path) && !path.starts_with(source),
                "native Pi resource overlaps host credentials or private state: {}",
                source.display()
            );
        }
        Ok(())
    }
}

pub(crate) fn validate_arguments(arguments: &[OsString]) -> Result<()> {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str().context("Pi arguments must be UTF-8")?;
        match argument {
            "--" => break,
            "--provider" | "--model" | "--thinking" | "--models" | "--name" | "-n" | "--tools"
            | "-t" | "--exclude-tools" | "-xt" | "--use-theme" | "--tui-mode" => {
                arguments.next().context("missing Pi option value")?;
            }
            "--mode" => {
                ensure!(
                    arguments
                        .next()
                        .is_some_and(|value| value == "rpc" || value == "json"),
                    "native Pi supports interactive, RPC, or JSON mode"
                );
            }
            "-p" | "--print" | "-c" | "--continue" | "-r" | "--resume" | "--no-session"
            | "--no-tools" | "-nt" | "--no-builtin-tools" | "-nbt" | "--verbose" | "--help"
            | "-h" | "--version" | "-v" => {}
            "install" | "remove" | "uninstall" | "update" | "config" => {
                bail!("Pi package commands are not enabled in the native harness")
            }
            value if !value.starts_with('-') => {}
            _ => bail!("Pi option {argument} is not enabled in the narrow native launcher"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PathBuf, crate::backend::macos::Config) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("host-home");
        fs::create_dir_all(home.join(".pi/agent")).unwrap();
        let package = base.join("runtime/pi");
        fs::create_dir_all(package.join("dist")).unwrap();
        fs::write(package.join("dist/cli.js"), "fixture").unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"name":"@earendil-works/pi-coding-agent"}"#,
        )
        .unwrap();
        let node = base.join("runtime/node");
        fs::write(&node, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        let config = crate::backend::macos::Config {
            node,
            pi_cli: package.join("dist/cli.js"),
            tool_timeout_seconds: 120,
        };
        (root, home, config)
    }

    #[test]
    fn native_preparation_uses_normal_pi_tools_and_shared_resource_selection() {
        let (root, home, config) = fixture();
        let agent = home.join(".pi/agent");
        for directory in [
            "extensions",
            "skills/example",
            "prompts",
            "themes",
            "npm/node_modules/example",
        ] {
            fs::create_dir_all(agent.join(directory)).unwrap();
        }
        fs::write(
            agent.join("extensions/example.ts"),
            "export default () => {};",
        )
        .unwrap();
        fs::write(agent.join("skills/example/SKILL.md"), "fixture skill").unwrap();
        fs::write(
            agent.join("npm/node_modules/example/package.json"),
            r#"{"pi":{"extensions":["main.js"]}}"#,
        )
        .unwrap();
        fs::write(
            agent.join("npm/node_modules/example/main.js"),
            "export default () => {};",
        )
        .unwrap();
        fs::write(agent.join("settings.json"), r#"{"defaultProvider":"openrouter","defaultModel":"fixture","packages":["npm:example"],"apiKey":"secret","httpProxy":"http://untrusted"}"#).unwrap();
        fs::write(agent.join("auth.json"), "host credential canary").unwrap();
        fs::write(agent.join("AGENTS.md"), "host instructions").unwrap();
        let persistent = root.path().join("persistent");
        Pi::initialize_home(&persistent).unwrap();
        for mode in [HarnessMode::None, HarnessMode::Data, HarnessMode::Trusted] {
            let pi = Pi::inspect(mode, &home, &Config::default()).unwrap();
            let session = tempfile::tempdir_in(root.path()).unwrap();
            let prepared = pi
                .prepare(&PrepareContext {
                    native: Some(&config),
                    session_dir: session.path(),
                    private_home: &persistent,
                    host_path: OsStr::new("/usr/bin:/bin"),
                    profile: Profile::Developer,
                    general_network: false,
                    tool_network: ToolNetwork::None,
                    model_providers: &[crate::provider::Kind::OpenRouter],
                    dry_run: false,
                })
                .unwrap();
            let script = fs::read_to_string(session.path().join("pi-wrapper")).unwrap();
            assert!(!script.contains("--tools"));
            assert!(!script.contains("--no-builtin-tools"));
            assert!(!script.contains("--no-context-files"));
            assert!(script.contains("--no-extensions") && script.contains("--no-approve"));
            assert_eq!(
                script.contains("extensions/example.ts"),
                mode == HarnessMode::Trusted
            );
            assert_eq!(
                script.contains("npm/node_modules/example/main.js"),
                mode == HarnessMode::Trusted
            );
            for flag in ["'--skill'", "'--prompt-template'", "'--theme'"] {
                assert_eq!(script.contains(flag), mode != HarnessMode::None, "{script}");
            }
            assert!(!script.contains("/run/slopbox-host-pi"));
            let settings: Value = serde_json::from_str(
                &fs::read_to_string(session.path().join("pi-state/settings.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(settings["defaultProvider"], "openrouter");
            assert_eq!(
                settings["defaultModel"] == "fixture",
                mode != HarnessMode::None
            );
            for key in ["apiKey", "httpProxy", "packages"] {
                assert!(settings.get(key).is_none());
            }
            assert_eq!(
                fs::read_to_string(session.path().join("pi-state/auth.json")).unwrap(),
                "{}\n"
            );
            let alias = session.path().join("home/.pi/agent");
            assert_eq!(
                fs::read_to_string(alias.join("AGENTS.md")).unwrap(),
                if mode == HarnessMode::None {
                    ""
                } else {
                    "host instructions"
                }
            );
            assert_eq!(
                alias.join("extensions/example.ts").exists(),
                mode == HarnessMode::Trusted
            );
            assert_eq!(
                alias.join("sessions").canonicalize().unwrap(),
                persistent
                    .join(".pi/agent/sessions")
                    .canonicalize()
                    .unwrap()
            );
            for mount in &prepared.mounts {
                assert_eq!(mount.source, mount.target);
                if mount.source.starts_with(&home) {
                    assert_eq!(mount.access, MountAccess::ReadOnly);
                    assert_ne!(mount.source, agent);
                    assert_ne!(mount.source, agent.join("auth.json"));
                    assert_ne!(mount.source, agent.join("settings.json"));
                }
            }
        }
        assert_eq!(
            fs::read_to_string(agent.join("auth.json")).unwrap(),
            "host credential canary"
        );
    }

    #[test]
    fn native_resource_roots_cannot_import_credentials_through_ancestors_or_aliases() {
        let (_root, home, _) = fixture();
        let pi = Pi::inspect(HarnessMode::None, &home, &Config::default()).unwrap();
        let extensions = home.join(".pi/agent/extensions");
        fs::create_dir_all(&extensions).unwrap();
        pi.validate_native_resource_root(&extensions).unwrap();
        // Protect even missing credential paths, including their parent roots.
        for source in [
            &home,
            &home.join(".pi"),
            &home.join(".pi/agent"),
            &home.join(".ssh"),
            &home.join(".ssh/key"),
        ] {
            assert!(
                pi.validate_native_resource_root(source).is_err(),
                "{}",
                source.display()
            );
        }
        fs::create_dir_all(extensions.join("private-state")).unwrap();
        std::os::unix::fs::symlink(extensions.join("private-state"), home.join(".ssh")).unwrap();
        assert!(pi.validate_native_resource_root(&extensions).is_err());
        fs::write(home.join(".pi/agent/auth.json"), "credential canary").unwrap();
        std::os::unix::fs::symlink(
            home.join(".pi/agent/auth.json"),
            home.join(".pi/agent/AGENTS.md"),
        )
        .unwrap();
        let mut pi = pi;
        pi.mode = HarnessMode::Data;
        assert!(pi.validate_native_resources().is_err());
    }

    #[test]
    fn native_configured_read_only_resources_preserve_aliases_without_host_writes() {
        let (root, home, _) = fixture();
        let source = home.join("plugin-data");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("marker"), "unchanged").unwrap();
        let pi = Pi::inspect(
            HarnessMode::Trusted,
            &home,
            &Config {
                read_only_mounts: vec![ConfiguredReadOnlyMount {
                    source: source.clone(),
                    target: "~/.cache/plugin".into(),
                }],
                ..Config::default()
            },
        )
        .unwrap();
        let private = root.path().join("private");
        let resources = pi
            .native_resources(&private, &private.join("agent"))
            .unwrap();
        assert_eq!(resources.mounts, [(source.clone(), source.clone())]);
        assert_eq!(
            private.join(".cache/plugin").canonicalize().unwrap(),
            source
        );
        assert_eq!(
            fs::read_to_string(source.join("marker")).unwrap(),
            "unchanged"
        );
    }

    #[test]
    fn native_temporary_overlays_fail_explicitly_before_alias_creation() {
        let (root, home, _) = fixture();
        let mut pi = Pi::inspect(HarnessMode::None, &home, &Config::default()).unwrap();
        pi.resources
            .temporary_overlays
            .push((home, "/home/slopbox/.cache/plugin".into()));
        let private = root.path().join("private");
        let error = pi
            .native_resources(&private, &private.join("agent"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("temporary_overlay_mounts"));
        assert!(!private.exists());
    }

    #[test]
    fn native_arguments_cannot_enable_resources_or_change_the_session_layout() {
        for arguments in [
            vec!["-e", "workspace.ts"],
            vec!["--extension=workspace.ts"],
            vec!["--approve"],
            vec!["--session-dir", "/host"],
            vec!["install", "package"],
        ] {
            assert!(
                validate_arguments(
                    &arguments
                        .into_iter()
                        .map(OsString::from)
                        .collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        for arguments in [
            vec!["--mode", "rpc"],
            vec!["--tools", "read,bash"],
            vec!["--exclude-tools", "write,edit"],
            vec!["--model", "openrouter/openai/gpt-4o", "-p", "hello"],
            vec!["--", "--extension=only-prompt-text"],
        ] {
            validate_arguments(
                &arguments
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
    }
}
