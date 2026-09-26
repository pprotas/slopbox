#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub(crate) use native::validate_arguments as validate_native_arguments;

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Mount, MountAccess, PrepareContext, PreparedHarness};
use crate::ToolNetwork;
use crate::command::{find_optional_executable, shell_quote};
use crate::fs_util::{expand_host_home, find_socket, read_text, write_private};
use crate::policy::{HarnessMode, Profile};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    read_only_mounts: Vec<ConfiguredReadOnlyMount>,
    #[serde(default)]
    temporary_overlay_mounts: Vec<ConfiguredReadOnlyMount>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfiguredReadOnlyMount {
    source: PathBuf,
    target: PathBuf,
}

#[derive(Default)]
pub(crate) struct Resources {
    pub mounts: Vec<(PathBuf, PathBuf)>,
    pub temporary_overlays: Vec<(PathBuf, PathBuf)>,
    pub extensions: Vec<PathBuf>,
    pub skills: Vec<PathBuf>,
    pub prompts: Vec<PathBuf>,
    pub themes: Vec<PathBuf>,
}

pub(crate) struct Pi {
    mode: HarnessMode,
    home: PathBuf,
    pub resources: Resources,
}

impl Pi {
    pub fn inspect(mode: HarnessMode, home: &Path, config: &Config) -> Result<Self> {
        Ok(Self {
            mode,
            home: home.to_path_buf(),
            resources: discover_host_pi_resources(mode, home, config)?,
        })
    }

    pub fn initialize_home(home: &Path) -> Result<()> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(home.join(".pi/agent/sessions"))
            .context("failed to initialize private home directory .pi/agent/sessions")
    }

    pub fn prepare(&self, context: &PrepareContext<'_>) -> Result<PreparedHarness> {
        #[cfg(target_os = "macos")]
        if let Some(config) = context.native {
            return self.prepare_native(context, config);
        }
        let mut prepared = PreparedHarness {
            directories: vec![
                "/run/slopbox-pi-agent".into(),
                "/run/slopbox-host-pi".into(),
            ],
            environment: environment(
                context.profile,
                context.general_network,
                context.tool_network,
            ),
            ..PreparedHarness::default()
        };
        for provider in context.model_providers {
            let (name, value) = match provider {
                crate::provider::Kind::OpenRouter => ("OPENROUTER_API_KEY", "slopbox:openrouter"),
                crate::provider::Kind::OpenAiCodex => ("SLOPBOX_CODEX_ENABLED", "1"),
            };
            prepared.environment.push((name.into(), value.into()));
        }
        if !context.dry_run {
            let host_agent = if self.mode != HarnessMode::None {
                self.home.join(".pi/agent")
            } else {
                context.session_dir.join("disabled-host-pi-agent")
            };
            configure_pi(context.session_dir, &host_agent)?;
            prepare_pi_mount_targets(&context.private_home.join(".pi/agent"))?;
            prepared.executable =
                configure_pi_wrapper(context.session_dir, context.host_path, &self.resources)?;
            for name in ["settings.json", "auth.json"] {
                prepared.mounts.push(Mount {
                    source: context.session_dir.join("pi-state").join(name),
                    target: Path::new("/home/slopbox/.pi/agent").join(name),
                    access: MountAccess::ReadWrite,
                });
            }
            prepared.mounts.push(Mount {
                source: context.session_dir.join("pi-agent"),
                target: "/run/slopbox-pi-agent".into(),
                access: MountAccess::ReadOnly,
            });
            prepared.mounts.push(Mount {
                source: context.session_dir.join("pi-agent/AGENTS.md"),
                target: "/home/slopbox/.pi/agent/AGENTS.md".into(),
                access: MountAccess::ReadOnly,
            });
        }
        for (source, target) in &self.resources.mounts {
            prepared.mounts.push(Mount {
                source: source.clone(),
                target: target.clone(),
                access: MountAccess::ReadOnly,
            });
        }
        for (source, target) in &self.resources.temporary_overlays {
            prepared.mounts.push(Mount {
                source: source.clone(),
                target: target.clone(),
                access: MountAccess::TemporaryOverlay,
            });
        }
        if prepared.executable.is_some() {
            prepared.mounts.push(Mount {
                source: context.session_dir.join("pi-wrapper"),
                target: "/run/slopbox/pi".into(),
                access: MountAccess::ReadOnly,
            });
        }
        Ok(prepared)
    }

    pub fn accepts_image_paste(&self, command: &[OsString]) -> bool {
        command.first().is_some_and(|executable| executable == "pi")
    }
}

pub(crate) fn launch_command(arguments: Vec<OsString>) -> Vec<OsString> {
    let mut command = vec![OsString::from("pi")];
    command.extend(arguments);
    command
}

fn environment(
    profile: Profile,
    general_network: bool,
    tool_network: ToolNetwork,
) -> Vec<(OsString, OsString)> {
    let mut values = vec![
        (
            "PI_CODING_AGENT_DIR".into(),
            "/home/slopbox/.pi/agent".into(),
        ),
        (
            "PI_CODING_AGENT_SESSION_DIR".into(),
            "/home/slopbox/.pi/agent/sessions".into(),
        ),
        (
            "SLOPBOX_PI_TOOL_NETWORK".into(),
            match tool_network {
                ToolNetwork::None => "none",
                ToolNetwork::General => "general",
            }
            .into(),
        ),
    ];
    if profile != Profile::Developer || !general_network {
        values.push(("PI_OFFLINE".into(), "1".into()));
    }
    values
}

fn prepare_pi_mount_targets(agent_dir: &Path) -> Result<()> {
    ensure_not_symlink(agent_dir)?;
    for name in ["settings.json", "auth.json", "AGENTS.md"] {
        let target = agent_dir.join(name);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&target)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    fs::symlink_metadata(&target)?.is_file(),
                    "Pi configuration mount target is not a regular file: {}",
                    target.display()
                );
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create {}", target.display()));
            }
        }
    }
    Ok(())
}

fn configure_pi(session_dir: &Path, host_agent: &Path) -> Result<()> {
    let trusted_agent = session_dir.join("pi-agent");
    configure_pi_directory(&trusted_agent, host_agent)?;

    let runtime_agent = session_dir.join("pi-state");
    DirBuilder::new().mode(0o700).create(&runtime_agent)?;
    let settings = trusted_pi_settings(&host_agent.join("settings.json"))?;
    write_private(&runtime_agent.join("settings.json"), &settings)?;
    write_private(&runtime_agent.join("auth.json"), "{}\n")?;
    Ok(())
}

fn configure_pi_directory(agent_dir: &Path, host_agent_dir: &Path) -> Result<()> {
    const EXTENSION: &str = include_str!("../../assets/pi-extension.ts");

    ensure_not_symlink(agent_dir)?;
    let extensions_dir = agent_dir.join("extensions");
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    builder
        .create(&extensions_dir)
        .with_context(|| format!("failed to create {}", extensions_dir.display()))?;
    write_private(&extensions_dir.join("slopbox.ts"), EXTENSION)?;

    let host_agents = host_agent_dir.join("AGENTS.md");
    let agents = if host_agents.is_file() {
        read_text(&host_agents, "Pi AGENTS.md")?
    } else {
        String::new()
    };
    write_private(&agent_dir.join("AGENTS.md"), &agents)?;

    Ok(())
}

const SAFE_PI_SETTING_PATHS: &[&[&str]] = &[
    &["defaultProvider"],
    &["defaultModel"],
    &["defaultThinkingLevel"],
    &["hideThinkingBlock"],
    &["showCacheMissNotices"],
    &["thinkingBudgets", "minimal"],
    &["thinkingBudgets", "low"],
    &["thinkingBudgets", "medium"],
    &["thinkingBudgets", "high"],
    &["thinkingBudgets", "xhigh"],
    &["thinkingBudgets", "max"],
    &["theme"],
    &["quietStartup"],
    &["collapseChangelog"],
    &["doubleEscapeAction"],
    &["treeFilterMode"],
    &["editorPaddingX"],
    &["outputPad"],
    &["autocompleteMaxVisible"],
    &["showHardwareCursor"],
    &["tuiMode"],
    &["fullscreenExitOutput"],
    &["fullscreenScrollbar"],
    &["warnings", "anthropicExtraUsage"],
    &["compaction", "enabled"],
    &["compaction", "reserveTokens"],
    &["compaction", "keepRecentTokens"],
    &["branchSummary", "reserveTokens"],
    &["branchSummary", "skipPrompt"],
    &["retry", "enabled"],
    &["retry", "maxRetries"],
    &["retry", "baseDelayMs"],
    &["retry", "provider", "timeoutMs"],
    &["retry", "provider", "maxRetries"],
    &["retry", "provider", "maxRetryDelayMs"],
    &["steeringMode"],
    &["followUpMode"],
    &["httpIdleTimeoutMs"],
    &["terminal", "showImages"],
    &["terminal", "imageWidthCells"],
    &["terminal", "clearOnShrink"],
    &["images", "autoResize"],
    &["images", "blockImages"],
    &["defaultTools"],
    &["enabledModels"],
    &["markdown", "codeBlockIndent"],
    &["markdown", "mermaid"],
];

fn trusted_pi_settings(host_settings: &Path) -> Result<String> {
    let source = if host_settings.is_file() {
        let contents = read_text(host_settings, "Pi settings")?;
        serde_json::from_str::<Value>(&contents)
            .with_context(|| format!("invalid host Pi settings {}", host_settings.display()))?
    } else {
        Value::Object(Map::new())
    };
    ensure!(source.is_object(), "host Pi settings must be a JSON object");

    let mut filtered = Value::Object(Map::new());
    for path in SAFE_PI_SETTING_PATHS {
        if let Some(value) = value_at_path(&source, path) {
            insert_value_at_path(&mut filtered, path, value.clone());
        }
    }
    insert_value_at_path(
        &mut filtered,
        &["defaultProjectTrust"],
        Value::String("never".into()),
    );
    insert_value_at_path(&mut filtered, &["transport"], Value::String("sse".into()));

    let mut output = serde_json::to_string_pretty(&filtered)?;
    output.push('\n');
    Ok(output)
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, key| value.get(key))
}

fn insert_value_at_path(target: &mut Value, path: &[&str], value: Value) {
    let Some((key, remainder)) = path.split_first() else {
        *target = value;
        return;
    };
    let object = target
        .as_object_mut()
        .expect("filtered Pi settings path is an object");
    if remainder.is_empty() {
        object.insert((*key).to_owned(), value);
        return;
    }
    let child = object
        .entry((*key).to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    insert_value_at_path(child, remainder, value);
}

fn discover_host_pi_resources(
    harness: HarnessMode,
    home: &Path,
    pi_config: &Config,
) -> Result<Resources> {
    const GUEST_ROOT: &str = "/run/slopbox-host-pi";

    let mut plan = Resources::default();
    if harness == HarnessMode::None {
        return Ok(plan);
    }

    let agent_dir = home.join(".pi/agent");

    if harness == HarnessMode::Trusted {
        let extensions = agent_dir.join("extensions");
        if extensions.is_dir() {
            let guest = PathBuf::from(GUEST_ROOT).join("extensions");
            add_host_pi_resource_mount(&mut plan, &extensions, &guest, "extensions");
            collect_conventional_extensions(&extensions, &guest, &mut plan.extensions)?;
        }
    }
    for name in ["skills", "prompts", "themes"] {
        let source = agent_dir.join(name);
        if source.is_dir() {
            let guest = PathBuf::from(GUEST_ROOT).join(name);
            add_host_pi_resource_mount(&mut plan, &source, &guest, name);
            match name {
                "skills" => plan.skills.push(guest),
                "prompts" => plan.prompts.push(guest),
                "themes" => plan.themes.push(guest),
                _ => unreachable!(),
            }
        }
    }

    if harness == HarnessMode::Trusted {
        let bin = agent_dir.join("bin");
        if bin.is_dir() {
            let guest = PathBuf::from(GUEST_ROOT).join("bin");
            add_host_pi_resource_mount(&mut plan, &bin, &guest, "bin");
        }
        let settings_path = agent_dir.join("settings.json");
        if settings_path.is_file() {
            let settings: Value = serde_json::from_str(&read_text(&settings_path, "Pi settings")?)
                .with_context(|| format!("invalid host Pi settings {}", settings_path.display()))?;
            discover_npm_package_resources(&agent_dir, &settings, &mut plan)?;
        }
        let (mounts, temporary_overlays) = configured_pi_mounts(pi_config, home)?;
        plan.mounts.extend(mounts);
        plan.temporary_overlays.extend(temporary_overlays);
    }

    plan.extensions.sort();
    plan.extensions.dedup();
    plan.skills.sort();
    plan.skills.dedup();
    plan.prompts.sort();
    plan.prompts.dedup();
    plan.themes.sort();
    plan.themes.dedup();
    Ok(plan)
}

fn add_host_pi_resource_mount(plan: &mut Resources, source: &Path, guest: &Path, agent_name: &str) {
    plan.mounts
        .push((source.to_path_buf(), guest.to_path_buf()));
    plan.mounts.push((
        source.to_path_buf(),
        PathBuf::from("/home/slopbox/.pi/agent").join(agent_name),
    ));
}

type ConfiguredPiMounts = (Vec<(PathBuf, PathBuf)>, Vec<(PathBuf, PathBuf)>);

fn configured_pi_mounts(config: &Config, home: &Path) -> Result<ConfiguredPiMounts> {
    let read_only = resolve_configured_pi_mounts(&config.read_only_mounts, home)?;
    let temporary_overlays = resolve_configured_pi_mounts(&config.temporary_overlay_mounts, home)?;
    for (_, target) in &temporary_overlays {
        ensure!(
            !read_only.iter().any(|(_, existing)| existing == target),
            "duplicate configured Pi resource target {}",
            target.display()
        );
    }
    Ok((read_only, temporary_overlays))
}

fn resolve_configured_pi_mounts(
    mounts: &[ConfiguredReadOnlyMount],
    home: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let mut resolved = Vec::new();
    for mount in mounts {
        let source = expand_host_home(&mount.source, home)?;
        let source = fs::canonicalize(&source).with_context(|| {
            format!(
                "failed to resolve configured Pi resource {}",
                source.display()
            )
        })?;
        reject_credential_mount(&source, home)?;
        if source.is_dir() {
            if let Some(socket) = find_socket(&source)? {
                bail!(
                    "configured Pi resource contains a Unix socket: {}",
                    socket.display()
                );
            }
        } else {
            ensure!(
                source.is_file(),
                "configured Pi resource is not a regular file"
            );
        }

        let target = expand_guest_home(&mount.target)?;
        ensure_safe_pi_mount_target(&target)?;
        ensure!(
            !resolved
                .iter()
                .any(|(_, existing): &(PathBuf, PathBuf)| existing == &target),
            "duplicate configured Pi resource target {}",
            target.display()
        );
        resolved.push((source, target));
    }
    Ok(resolved)
}

fn expand_guest_home(path: &Path) -> Result<PathBuf> {
    if path == Path::new("~") {
        return Ok(PathBuf::from("/home/slopbox"));
    }
    if let Ok(relative) = path.strip_prefix("~/") {
        return Ok(PathBuf::from("/home/slopbox").join(relative));
    }
    ensure!(
        path.is_absolute(),
        "configured Pi resource target must be absolute or start with ~/"
    );
    Ok(path.to_path_buf())
}

fn ensure_safe_pi_mount_target(target: &Path) -> Result<()> {
    use std::path::Component;

    ensure!(
        !target
            .components()
            .any(|component| matches!(component, Component::ParentDir)),
        "configured Pi resource target contains .."
    );
    ensure!(
        [
            Path::new("/home/slopbox/.cache"),
            Path::new("/home/slopbox/.config"),
            Path::new("/home/slopbox/.local/share"),
        ]
        .iter()
        .any(|root| target == *root || target.starts_with(root)),
        "configured Pi resource target must be inside the sandbox cache, config, or data home"
    );
    Ok(())
}

fn reject_credential_mount(source: &Path, home: &Path) -> Result<()> {
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let forbidden = [
        home.join(".ssh"),
        home.join(".gnupg"),
        home.join(".password-store"),
        home.join(".pi/agent/auth.json"),
        data_home.join("slopbox/credentials"),
    ];
    ensure!(
        forbidden
            .iter()
            .filter_map(|path| fs::canonicalize(path).ok())
            .all(|path| !source.starts_with(path)),
        "refusing to mount a credential directory as a Pi resource"
    );
    Ok(())
}

fn collect_conventional_extensions(
    source: &Path,
    guest: &Path,
    extensions: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(source)
        .with_context(|| format!("failed to inspect Pi extensions {}", source.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_file() && is_pi_extension_file(&path) {
            extensions.push(guest.join(entry.file_name()));
        } else if path.is_dir() {
            for index in ["index.ts", "index.js"] {
                if path.join(index).is_file() {
                    extensions.push(guest.join(entry.file_name()).join(index));
                    break;
                }
            }
        }
    }
    Ok(())
}

fn discover_npm_package_resources(
    agent_dir: &Path,
    settings: &Value,
    plan: &mut Resources,
) -> Result<()> {
    const GUEST_NPM: &str = "/run/slopbox-host-pi/npm";

    let Some(packages) = settings.get("packages").and_then(Value::as_array) else {
        return Ok(());
    };
    let npm_root = agent_dir.join("npm");
    let node_modules = npm_root.join("node_modules");
    if !node_modules.is_dir() {
        return Ok(());
    }

    let mut found_package = false;
    for package in packages {
        let (source, filters) = match package {
            Value::String(source) => (source.as_str(), None),
            Value::Object(package) => match package.get("source").and_then(Value::as_str) {
                Some(source) => (source, Some(package)),
                None => continue,
            },
            _ => continue,
        };
        let Some(name) = npm_package_name(source) else {
            eprintln!("slopbox: skipping unsupported trusted Pi package source {source}");
            continue;
        };
        let package_root = node_modules.join(&name);
        if !package_root.is_dir() {
            eprintln!("slopbox: trusted Pi package {source} is not installed; skipping it");
            continue;
        }
        found_package = true;
        let guest_root = PathBuf::from(GUEST_NPM).join("node_modules").join(&name);
        discover_package_manifest_resources(&package_root, &guest_root, filters, plan)?;
    }

    if found_package {
        add_host_pi_resource_mount(plan, &npm_root, Path::new(GUEST_NPM), "npm");
    }
    Ok(())
}

fn npm_package_name(source: &str) -> Option<String> {
    let mut specification = source.strip_prefix("npm:")?;
    if specification.starts_with('@') {
        let slash = specification.find('/')?;
        if let Some(version) = specification[slash + 1..].rfind('@') {
            specification = &specification[..slash + 1 + version];
        }
    } else if let Some((name, _)) = specification.split_once('@') {
        specification = name;
    }
    if specification.is_empty()
        || specification
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(specification.to_owned())
}

fn discover_package_manifest_resources(
    package_root: &Path,
    guest_root: &Path,
    filters: Option<&Map<String, Value>>,
    plan: &mut Resources,
) -> Result<()> {
    let manifest_path = package_root.join("package.json");
    let manifest: Value = if manifest_path.is_file() {
        serde_json::from_str(&read_text(&manifest_path, "Pi package manifest")?)
            .with_context(|| format!("invalid Pi package manifest {}", manifest_path.display()))?
    } else {
        Value::Object(Map::new())
    };

    let pi = manifest.get("pi").and_then(Value::as_object);
    let has_configured_resources =
        ["extensions", "skills", "prompts", "themes"]
            .iter()
            .any(|key| {
                filters.is_some_and(|filters| filters.contains_key(*key))
                    || pi.is_some_and(|pi| pi.contains_key(*key))
            });
    let configured = |key: &str| {
        filters
            .and_then(|filters| filters.get(key))
            .or_else(|| pi.and_then(|pi| pi.get(key)))
    };
    if has_configured_resources {
        add_manifest_resources(
            package_root,
            guest_root,
            configured("extensions"),
            &mut plan.extensions,
            true,
        )?;
        add_manifest_resources(
            package_root,
            guest_root,
            configured("skills"),
            &mut plan.skills,
            false,
        )?;
        add_manifest_resources(
            package_root,
            guest_root,
            configured("prompts"),
            &mut plan.prompts,
            false,
        )?;
        add_manifest_resources(
            package_root,
            guest_root,
            configured("themes"),
            &mut plan.themes,
            false,
        )?;
    } else {
        let source = package_root.join("extensions");
        if source.is_dir() {
            collect_conventional_extensions(
                &source,
                &guest_root.join("extensions"),
                &mut plan.extensions,
            )?;
        }
        for (name, resources) in [
            ("skills", &mut plan.skills),
            ("prompts", &mut plan.prompts),
            ("themes", &mut plan.themes),
        ] {
            if package_root.join(name).is_dir() {
                resources.push(guest_root.join(name));
            }
        }
    }
    Ok(())
}

fn add_manifest_resources(
    package_root: &Path,
    guest_root: &Path,
    configured: Option<&Value>,
    resources: &mut Vec<PathBuf>,
    extensions: bool,
) -> Result<()> {
    let Some(patterns) = configured.and_then(Value::as_array) else {
        return Ok(());
    };
    if patterns
        .iter()
        .filter_map(Value::as_str)
        .any(|pattern| pattern.starts_with(['!', '-']))
    {
        eprintln!(
            "slopbox: skipping Pi package resource category with unsupported exclusion patterns"
        );
        return Ok(());
    }
    let canonical_root = fs::canonicalize(package_root)?;
    for pattern in patterns.iter().filter_map(Value::as_str) {
        let pattern = pattern.strip_prefix('+').unwrap_or(pattern);
        let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
        let full_pattern = package_root.join(pattern);
        let full_pattern = full_pattern
            .to_str()
            .context("Pi package resource pattern is not UTF-8")?;
        let matches = glob::glob(full_pattern)
            .with_context(|| format!("invalid Pi package resource pattern {pattern}"))?;
        for matched in matches {
            let matched = matched?;
            let canonical = fs::canonicalize(&matched)?;
            ensure!(
                canonical.starts_with(&canonical_root),
                "Pi package resource escapes its package root: {}",
                matched.display()
            );
            let relative = matched.strip_prefix(package_root)?;
            let guest = guest_root.join(relative);
            if extensions {
                if matched.is_file() && is_pi_extension_file(&matched) {
                    resources.push(guest);
                } else if matched.is_dir() {
                    collect_extension_tree(&matched, &guest, resources)?;
                }
            } else {
                resources.push(guest);
            }
        }
    }
    Ok(())
}

fn collect_extension_tree(
    source: &Path,
    guest: &Path,
    extensions: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(source)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let guest_path = guest.join(entry.file_name());
        if path.is_dir() {
            collect_extension_tree(&path, &guest_path, extensions)?;
        } else if path.is_file() && is_pi_extension_file(&path) {
            extensions.push(guest_path);
        }
    }
    Ok(())
}

fn is_pi_extension_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| matches!(extension, "ts" | "js" | "mjs" | "cjs"))
}

fn append_resource_arguments(arguments: &mut Vec<String>, resources: &Resources) {
    for extension in &resources.extensions {
        arguments.push("--extension".to_owned());
        arguments.push(extension.to_string_lossy().into_owned());
    }
    for skill in &resources.skills {
        arguments.push("--skill".to_owned());
        arguments.push(skill.to_string_lossy().into_owned());
    }
    for prompt in &resources.prompts {
        arguments.push("--prompt-template".to_owned());
        arguments.push(prompt.to_string_lossy().into_owned());
    }
    for theme in &resources.themes {
        arguments.push("--theme".to_owned());
        arguments.push(theme.to_string_lossy().into_owned());
    }
}

fn configure_pi_wrapper(
    session_dir: &Path,
    sandbox_path: &OsStr,
    resources: &Resources,
) -> Result<Option<PathBuf>> {
    let wrapper = session_dir.join("pi-wrapper");
    let Some(pi) = find_optional_executable("pi", sandbox_path) else {
        let _ = fs::remove_file(wrapper);
        return Ok(None);
    };
    let pi = fs::canonicalize(&pi)
        .with_context(|| format!("failed to resolve Pi executable {}", pi.display()))?;
    ensure!(
        pi.starts_with("/nix/store"),
        "Pi resolves outside /nix/store"
    );
    let mut arguments = vec![
        "--no-extensions".to_owned(),
        "--no-skills".to_owned(),
        "--no-prompt-templates".to_owned(),
        "--no-themes".to_owned(),
        "--extension".to_owned(),
        "/run/slopbox-pi-agent/extensions/slopbox.ts".to_owned(),
    ];
    append_resource_arguments(&mut arguments, resources);

    let mut script = "#!/bin/sh\n".to_owned();
    if !resources.extensions.is_empty()
        || !resources.skills.is_empty()
        || !resources.prompts.is_empty()
        || !resources.themes.is_empty()
    {
        let summary = format!(
            "slopbox: trusted host Pi resources: {} extension(s), {} skill path(s), {} prompt path(s), {} theme path(s)",
            resources.extensions.len(),
            resources.skills.len(),
            resources.prompts.len(),
            resources.themes.len()
        );
        script.push_str("printf '%s\\n' ");
        script.push_str(&shell_quote(&summary));
        script.push_str(" >&2\n");
        for extension in &resources.extensions {
            let message = format!("slopbox: trusted Pi extension: {}", extension.display());
            script.push_str("printf '%s\\n' ");
            script.push_str(&shell_quote(&message));
            script.push_str(" >&2\n");
        }
    }
    script.push_str("exec ");
    script.push_str(&shell_quote(&pi.to_string_lossy()));
    for argument in arguments {
        script.push(' ');
        script.push_str(&shell_quote(&argument));
    }
    script.push_str(" \"$@\"\n");
    write_private(&wrapper, &script)?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700))?;
    Ok(Some(pi))
}

fn ensure_not_symlink(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        ensure!(
            !metadata.file_type().is_symlink(),
            "refusing to write Pi configuration through symlink {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_keeps_host_resources_read_only_and_configuration_session_owned() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("host-home");
        let agent = home.join(".pi/agent");
        fs::create_dir_all(agent.join("extensions")).unwrap();
        fs::create_dir_all(agent.join("skills")).unwrap();
        fs::write(
            agent.join("extensions/example.ts"),
            "export default () => {};\n",
        )
        .unwrap();
        fs::write(agent.join("AGENTS.md"), "host instructions").unwrap();
        fs::write(agent.join("settings.json"), r#"{"defaultModel":"fixture"}"#).unwrap();
        fs::write(agent.join("auth.json"), "host credential canary").unwrap();
        let cache = home.join(".cache/browser");
        fs::create_dir_all(&cache).unwrap();
        let config = Config {
            read_only_mounts: vec![ConfiguredReadOnlyMount {
                source: cache.clone(),
                target: "~/.cache/browser".into(),
            }],
            temporary_overlay_mounts: vec![ConfiguredReadOnlyMount {
                source: cache,
                target: "~/.cache/overlay".into(),
            }],
        };
        let private_home = root.path().join("private-home");
        Pi::initialize_home(&private_home).unwrap();
        for mode in [HarnessMode::None, HarnessMode::Data, HarnessMode::Trusted] {
            let pi = Pi::inspect(mode, &home, &config).unwrap();
            let session = tempfile::tempdir_in(root.path()).unwrap();
            let prepared = pi
                .prepare(&PrepareContext {
                    #[cfg(target_os = "macos")]
                    native: None,
                    session_dir: session.path(),
                    private_home: &private_home,
                    host_path: root.path().as_os_str(),
                    profile: Profile::Developer,
                    general_network: false,
                    tool_network: ToolNetwork::None,
                    model_providers: &[],
                    dry_run: false,
                })
                .unwrap();
            assert!(prepared.executable.is_none());
            assert!(prepared.mounts.contains(&Mount {
                source: session.path().join("pi-agent"),
                target: "/run/slopbox-pi-agent".into(),
                access: MountAccess::ReadOnly,
            }));
            assert_eq!(
                prepared.mounts.iter().any(|mount| {
                    mount.target == Path::new("/home/slopbox/.cache/browser")
                        && mount.source == fs::canonicalize(home.join(".cache/browser")).unwrap()
                        && mount.access == MountAccess::ReadOnly
                }),
                mode == HarnessMode::Trusted
            );
            let writable: Vec<_> = prepared
                .mounts
                .iter()
                .filter(|mount| mount.access == MountAccess::ReadWrite)
                .collect();
            assert_eq!(writable.len(), 2);
            for (mount, name) in writable.iter().zip(["settings.json", "auth.json"]) {
                assert_eq!(mount.source, session.path().join("pi-state").join(name));
                assert_eq!(
                    mount.target,
                    Path::new("/home/slopbox/.pi/agent").join(name)
                );
            }
            assert_eq!(
                fs::read_to_string(session.path().join("pi-state/auth.json")).unwrap(),
                "{}\n"
            );
            let settings =
                fs::read_to_string(session.path().join("pi-state/settings.json")).unwrap();
            assert_eq!(settings.contains("fixture"), mode != HarnessMode::None);
            assert_eq!(
                fs::read_to_string(session.path().join("pi-agent/AGENTS.md")).unwrap(),
                if mode == HarnessMode::None {
                    ""
                } else {
                    "host instructions"
                }
            );
            assert_eq!(
                pi.resources.extensions.is_empty(),
                mode != HarnessMode::Trusted
            );
            assert_eq!(pi.resources.skills.is_empty(), mode == HarnessMode::None);
            assert_eq!(
                prepared
                    .mounts
                    .iter()
                    .any(|mount| mount.access == MountAccess::TemporaryOverlay),
                mode == HarnessMode::Trusted
            );
            assert!(
                prepared
                    .mounts
                    .iter()
                    .filter(|mount| mount.source.starts_with(&home))
                    .all(|mount| mount.access != MountAccess::ReadWrite)
            );
            assert!(
                !prepared
                    .mounts
                    .iter()
                    .any(|mount| mount.source == agent.join("auth.json"))
            );
            assert!(
                prepared
                    .environment
                    .contains(&("PI_OFFLINE".into(), "1".into()))
            );
            assert!(
                prepared
                    .environment
                    .contains(&("SLOPBOX_PI_TOOL_NETWORK".into(), "none".into()))
            );
        }
        assert_eq!(
            fs::read_to_string(agent.join("auth.json")).unwrap(),
            "host credential canary"
        );
    }

    #[test]
    fn inspection_and_preview_do_not_generate_runtime_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("absent-host-home");
        let session = root.path().join("absent-session");
        let private_home = root.path().join("absent-private-home");
        let pi = Pi::inspect(HarnessMode::Trusted, &home, &Config::default()).unwrap();
        let prepared = pi
            .prepare(&PrepareContext {
                #[cfg(target_os = "macos")]
                native: None,
                session_dir: &session,
                private_home: &private_home,
                host_path: OsStr::new("/absent"),
                profile: Profile::Developer,
                general_network: true,
                tool_network: ToolNetwork::General,
                model_providers: &[],
                dry_run: true,
            })
            .unwrap();
        assert!(prepared.mounts.is_empty());
        assert!(prepared.executable.is_none());
        assert!(!home.exists());
        assert!(!session.exists());
        assert!(!private_home.exists());
        assert!(
            !prepared
                .environment
                .iter()
                .any(|(name, _)| name == "PI_OFFLINE")
        );
    }

    #[test]
    fn launch_preserves_arguments_and_only_direct_pi_gets_image_paste() {
        let root = tempfile::tempdir().unwrap();
        let pi = Pi::inspect(HarnessMode::None, root.path(), &Config::default()).unwrap();
        let arguments = vec![
            OsString::from("--session"),
            OsString::from("a path with spaces"),
            OsString::from("--"),
            OsString::from("prompt"),
        ];
        let command = launch_command(arguments.clone());
        assert_eq!(command[0], "pi");
        assert_eq!(command[1..], arguments);
        assert!(pi.accepts_image_paste(&command));
        assert!(!pi.accepts_image_paste(&["sh".into(), "-c".into(), "pi".into()]));
        assert!(!pi.accepts_image_paste(&[]));
    }

    #[test]
    fn model_configuration_uses_only_selected_provider_markers() {
        use crate::provider::Kind;

        let root = tempfile::tempdir().unwrap();
        let pi = Pi::inspect(HarnessMode::None, root.path(), &Config::default()).unwrap();
        for (providers, expected) in [
            (vec![], vec![]),
            (
                vec![Kind::OpenRouter],
                vec![("OPENROUTER_API_KEY", "slopbox:openrouter")],
            ),
            (
                vec![Kind::OpenAiCodex],
                vec![("SLOPBOX_CODEX_ENABLED", "1")],
            ),
            (
                vec![Kind::OpenRouter, Kind::OpenAiCodex],
                vec![
                    ("OPENROUTER_API_KEY", "slopbox:openrouter"),
                    ("SLOPBOX_CODEX_ENABLED", "1"),
                ],
            ),
        ] {
            let prepared = pi
                .prepare(&PrepareContext {
                    #[cfg(target_os = "macos")]
                    native: None,
                    session_dir: root.path(),
                    private_home: root.path(),
                    host_path: OsStr::new("/absent"),
                    profile: Profile::Developer,
                    general_network: false,
                    tool_network: ToolNetwork::None,
                    model_providers: &providers,
                    dry_run: true,
                })
                .unwrap();
            let model_environment: Vec<_> = prepared
                .environment
                .iter()
                .filter(|(name, _)| name == "OPENROUTER_API_KEY" || name == "SLOPBOX_CODEX_ENABLED")
                .map(|(name, value)| (name.to_str().unwrap(), value.to_str().unwrap()))
                .collect();
            assert_eq!(model_environment, expected);
        }
    }

    #[test]
    fn only_online_developer_sessions_allow_pi_catalog_refreshes() {
        let offline = |profile, general| {
            environment(profile, general, ToolNetwork::General)
                .iter()
                .any(|(name, _)| name == "PI_OFFLINE")
        };

        assert!(!offline(Profile::Developer, true));
        assert!(offline(Profile::Developer, false));
        assert!(offline(Profile::Contained, true));
        assert!(offline(Profile::Adversarial, true));
    }

    #[test]
    fn writes_trusted_pi_configuration_without_a_credential() {
        let root = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("pi-agent");
        let host_agent_dir = root.path().join("host-agent");
        fs::create_dir(&host_agent_dir).unwrap();
        fs::write(host_agent_dir.join("AGENTS.md"), "trusted instructions\n").unwrap();
        configure_pi_directory(&agent_dir, &host_agent_dir).unwrap();

        assert!(!agent_dir.join("models.json").exists());
        assert!(!agent_dir.join("auth.json").exists());
        assert!(!agent_dir.join("settings.json").exists());
        let extension = fs::read_to_string(agent_dir.join("extensions/slopbox.ts")).unwrap();
        assert!(extension.contains("tool-run"));
        assert!(extension.contains("project_trust"));
        assert!(extension.contains("openai-codex"));
        assert!(extension.contains("SYNTHETIC_CODEX_JWT"));
        assert_eq!(
            fs::read_to_string(agent_dir.join("AGENTS.md")).unwrap(),
            "trusted instructions\n"
        );
    }

    #[test]
    fn concurrent_sessions_keep_their_own_pi_configuration() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().unwrap();
        let host_agent = root.path().join("host-agent");
        fs::create_dir(&host_agent).unwrap();
        fs::write(host_agent.join("AGENTS.md"), "first instructions").unwrap();
        fs::write(
            host_agent.join("settings.json"),
            r#"{"defaultModel":"first"}"#,
        )
        .unwrap();
        let first = tempfile::tempdir_in(root.path()).unwrap();
        configure_pi(first.path(), &host_agent).unwrap();
        let first_agent = first.path().join("pi-agent");
        let inode = fs::metadata(&first_agent).unwrap().ino();
        fs::write(first.path().join("pi-state/auth.json"), "first marker").unwrap();

        fs::write(host_agent.join("AGENTS.md"), "second instructions").unwrap();
        fs::write(
            host_agent.join("settings.json"),
            r#"{"defaultModel":"second"}"#,
        )
        .unwrap();
        let second = tempfile::tempdir_in(root.path()).unwrap();
        configure_pi(second.path(), &host_agent).unwrap();
        let without_resources = tempfile::tempdir_in(root.path()).unwrap();
        configure_pi(without_resources.path(), &root.path().join("absent")).unwrap();

        assert_eq!(fs::metadata(&first_agent).unwrap().ino(), inode);
        assert_eq!(
            fs::read_to_string(first_agent.join("AGENTS.md")).unwrap(),
            "first instructions"
        );
        assert!(
            fs::read_to_string(first.path().join("pi-state/settings.json"))
                .unwrap()
                .contains("first")
        );
        assert_eq!(
            fs::read_to_string(first.path().join("pi-state/auth.json")).unwrap(),
            "first marker"
        );
        assert_eq!(
            fs::read_to_string(second.path().join("pi-agent/AGENTS.md")).unwrap(),
            "second instructions"
        );
        assert!(
            fs::read_to_string(second.path().join("pi-state/settings.json"))
                .unwrap()
                .contains("second")
        );
        assert_eq!(
            fs::read_to_string(second.path().join("pi-state/auth.json")).unwrap(),
            "{}\n"
        );
        assert!(
            fs::read_to_string(without_resources.path().join("pi-agent/AGENTS.md"))
                .unwrap()
                .is_empty()
        );
        assert!(
            !fs::read_to_string(without_resources.path().join("pi-state/settings.json"))
                .unwrap()
                .contains("second")
        );
        assert_eq!(
            fs::metadata(&first_agent).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let second_path = second.path().to_path_buf();
        drop(second);
        drop(without_resources);
        assert!(!second_path.exists());
        assert!(first_agent.join("extensions/slopbox.ts").is_file());
        let first_path = first.path().to_path_buf();
        drop(first);
        assert!(!first_path.exists());
    }

    #[test]
    fn pi_mount_targets_are_stable_and_must_be_regular_files() {
        use std::os::unix::fs::{MetadataExt, symlink};

        let root = tempfile::tempdir().unwrap();
        prepare_pi_mount_targets(root.path()).unwrap();
        let agents = root.path().join("AGENTS.md");
        fs::write(&agents, "existing mount target").unwrap();
        let inode = fs::metadata(&agents).unwrap().ino();
        prepare_pi_mount_targets(root.path()).unwrap();
        assert_eq!(fs::metadata(&agents).unwrap().ino(), inode);
        assert_eq!(
            fs::read_to_string(&agents).unwrap(),
            "existing mount target"
        );

        let auth = root.path().join("auth.json");
        fs::remove_file(&auth).unwrap();
        symlink(&agents, &auth).unwrap();
        assert!(prepare_pi_mount_targets(root.path()).is_err());
        fs::remove_file(&auth).unwrap();
        fs::create_dir(&auth).unwrap();
        assert!(prepare_pi_mount_targets(root.path()).is_err());
    }

    #[test]
    fn refuses_to_replace_existing_pi_configuration() {
        let root = tempfile::tempdir().unwrap();
        let agent = root.path().join("pi-agent");
        configure_pi_directory(&agent, &root.path().join("absent")).unwrap();
        fs::write(agent.join("AGENTS.md"), "existing session").unwrap();

        assert!(configure_pi_directory(&agent, &root.path().join("absent")).is_err());
        assert_eq!(
            fs::read_to_string(agent.join("AGENTS.md")).unwrap(),
            "existing session"
        );
        assert!(agent.join("extensions/slopbox.ts").is_file());
    }

    #[test]
    fn filters_host_pi_settings() {
        let root = tempfile::tempdir().unwrap();
        let settings = root.path().join("settings.json");
        fs::write(
            &settings,
            r#"{
                "defaultModel": "gpt-5.5",
                "defaultThinkingLevel": "high",
                "externalEditor": "host-command",
                "httpProxy": "http://host-proxy",
                "packages": ["untrusted-package"],
                "extensions": ["host-extension.ts"],
                "sessionDir": "/host/sessions",
                "defaultProjectTrust": "always",
                "transport": "websocket",
                "retry": {"maxRetries": 7, "unknown": "removed"}
            }"#,
        )
        .unwrap();

        let filtered: Value =
            serde_json::from_str(&trusted_pi_settings(&settings).unwrap()).unwrap();
        assert_eq!(filtered["defaultModel"], "gpt-5.5");
        assert_eq!(filtered["defaultThinkingLevel"], "high");
        assert_eq!(filtered["retry"]["maxRetries"], 7);
        assert_eq!(filtered["defaultProjectTrust"], "never");
        assert_eq!(filtered["transport"], "sse");
        for removed in [
            "externalEditor",
            "httpProxy",
            "packages",
            "extensions",
            "sessionDir",
        ] {
            assert!(filtered.get(removed).is_none(), "{removed}");
        }
        assert!(filtered["retry"].get("unknown").is_none());
    }

    #[test]
    fn resolves_configured_read_only_pi_mounts() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let browser = home.join(".cache/browser");
        fs::create_dir_all(&browser).unwrap();
        fs::write(browser.join("runtime"), "fixture").unwrap();

        let mounts = resolve_configured_pi_mounts(
            &[ConfiguredReadOnlyMount {
                source: PathBuf::from("~/.cache/browser"),
                target: PathBuf::from("~/.cache/browser"),
            }],
            &home,
        )
        .unwrap();
        assert_eq!(
            mounts,
            [(
                fs::canonicalize(browser).unwrap(),
                PathBuf::from("/home/slopbox/.cache/browser")
            )]
        );

        fs::create_dir_all(home.join(".ssh")).unwrap();
        assert!(
            resolve_configured_pi_mounts(
                &[ConfiguredReadOnlyMount {
                    source: PathBuf::from("~/.ssh"),
                    target: PathBuf::from("~/.cache/ssh"),
                }],
                &home,
            )
            .is_err()
        );
    }

    #[test]
    fn discovers_filtered_npm_package_resources() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        fs::create_dir_all(package.join("skills/example")).unwrap();
        fs::write(package.join("index.ts"), "export default () => {};\n").unwrap();
        fs::write(package.join("skills/example/SKILL.md"), "# Example\n").unwrap();
        fs::write(
            package.join("package.json"),
            r#"{
                "pi": {
                    "extensions": ["./index.ts"],
                    "skills": ["./skills"]
                }
            }"#,
        )
        .unwrap();

        let mut filters = Map::new();
        filters.insert("extensions".into(), Value::Array(Vec::new()));
        let mut plan = Resources::default();
        discover_package_manifest_resources(
            &package,
            Path::new("/run/test/package"),
            Some(&filters),
            &mut plan,
        )
        .unwrap();

        assert!(plan.extensions.is_empty());
        assert_eq!(plan.skills, [PathBuf::from("/run/test/package/skills")]);
        assert_eq!(npm_package_name("npm:plain@1.2.3").unwrap(), "plain");
        assert_eq!(
            npm_package_name("npm:@scope/package@1.2.3").unwrap(),
            "@scope/package"
        );
    }

    #[test]
    fn refuses_symlinked_pi_configuration_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let agent_dir = root.path().join("pi-agent");
        symlink(outside.path(), &agent_dir).unwrap();

        assert!(configure_pi_directory(&agent_dir, root.path()).is_err());
    }
}
