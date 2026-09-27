use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::backend::native::runtime::{prepare_dev_environment, prepare_runtime};
use crate::backend::native::{
    self, clone_or_copy_file, required_executor, sandbox_path, validate_workspace_contents,
};
use crate::backend::{self, BrokerConnections, ExecutionPlan, ProxyEndpoint, Workspace};
#[cfg(target_os = "linux")]
use crate::command::find_optional_executable;
use crate::command::shell_quote;
use crate::fs_util::{expand_host_home, read_text, write_private};
use crate::gateway::{AuthenticatedHttpRoute, GatewaySession, HttpAuthentication};
use crate::git_config::GitUrlRewrite;
use crate::git_signing::{GitSigningIdentity, GitSigningSession};
use crate::harness::{PrepareContext, pi};
use crate::launch::{Agent, LaunchConfig};
use crate::policy::{
    HarnessMode, NetworkMode, Policy, PolicyRequest, Profile, RuntimeMode, WorkspaceMode,
};
use crate::{DevEnvironment, ToolNetwork};

mod access;

static APPLY_TEMP_ID: AtomicU64 = AtomicU64::new(0);

pub struct RunOptions {
    pub workspace: Option<PathBuf>,
    pub profile: Option<Profile>,
    pub dev_env: DevEnvironment,
    pub tool_network: ToolNetwork,
    pub no_host_pi_resources: bool,
    pub command: Vec<OsString>,
    pub dry_run: bool,
    pub approval_view: bool,
    pub launch_config: Option<LaunchConfig>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlopboxConfig {
    #[cfg(target_os = "macos")]
    macos: Option<native::Config>,
    #[serde(default)]
    profile: Profile,
    #[serde(default)]
    policy: PolicyRequest,
    #[serde(default)]
    pi: pi::Config,
    #[serde(default)]
    runtime: Option<backend::RuntimeSelection>,
    #[serde(default)]
    secrets: HashMap<String, SecretConfig>,
    #[serde(default)]
    http_routes: Vec<HttpRouteConfig>,
    #[serde(default)]
    git: GitConfig,
    #[serde(default)]
    defaults: access::Selection,
    #[serde(default)]
    workspaces: Vec<access::Rule>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitConfig {
    #[serde(default)]
    identities: Vec<GitIdentityConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitIdentityConfig {
    id: Option<String>,
    workspace: Option<PathBuf>,
    name: String,
    email: String,
    signing_key_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case", deny_unknown_fields)]
enum SecretConfig {
    Sops { file: PathBuf, key: String },
    Environment { variable: String },
    Command { argv: Vec<String> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpRouteConfig {
    name: String,
    workspace: Option<PathBuf>,
    upstream: String,
    methods: Vec<String>,
    #[serde(default)]
    allow_private_addresses: bool,
    #[serde(default)]
    direct: bool,
    #[serde(default)]
    proxy: bool,
    authentication: HttpAuthenticationConfig,
    #[serde(default)]
    git_urls: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum HttpAuthenticationConfig {
    Basic { username: String, secret: String },
    Bearer { secret: String },
    Token { secret: String },
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    #[serde(default)]
    policy: PolicyRequest,
}

pub struct StageInfo {
    pub id: String,
    pub workspace: PathBuf,
}

struct NetworkPlan {
    general: bool,
    model: bool,
    authenticated_http: bool,
    tool: ToolNetwork,
}

struct WorkspaceMount {
    source: PathBuf,
    writable: bool,
}

struct Paths {
    workspace: PathBuf,
    box_root: PathBuf,
    private_home: PathBuf,
    tool_home: PathBuf,
    config_root: PathBuf,
    state_root: PathBuf,
}

pub fn run(options: RunOptions) -> Result<ExitStatus> {
    ensure!(!options.command.is_empty(), "a command is required");
    backend::ensure_supported()?;
    ensure!(
        options.dry_run || !options.approval_view || crate::terminal::available(),
        "the approval view requires a host terminal on stdin, stdout, and stderr"
    );

    let private_terminal = crate::terminal::available();
    let host_path = sandbox_path()?;
    let executor = required_executor(&host_path)?;
    let paths = prepare_paths(options.workspace.as_deref())?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    ensure!(
        options.launch_config.is_none() || config.runtime.is_none(),
        "selected executable runtimes cannot replace an integrated Pi launch"
    );
    let project_config = load_project_config(&paths.workspace)?;
    let launch_config = match options.launch_config {
        Some(config) => Some(config),
        None => crate::launch::load(&paths.config_root, &paths.workspace)?,
    };
    let (profile, policy) = select_policy(
        &config,
        &project_config,
        options.profile,
        options.no_host_pi_resources,
        launch_config.as_ref(),
    );
    policy.ensure_implemented(profile)?;
    validate_runtime_selection(&config, policy)?;
    if config.runtime.is_some() {
        ensure!(
            !backend::nix::enabled(&paths.workspace, options.dev_env)?,
            "selected executable runtimes cannot activate a project flake; select the Nix runtime or use --dev-env none"
        );
    }
    let git_identity = configured_git_identity(&config, &paths.workspace)?;
    let git_rewrites = configured_git_rewrites(&config, &paths.workspace)?;
    #[cfg(target_os = "linux")]
    let selected_runtime = config
        .runtime
        .as_ref()
        .map(|selection| native::elf::prepare(selection, &paths.workspace, git_identity.is_some()))
        .transpose()?;
    #[cfg(target_os = "macos")]
    let selected_runtime = config
        .runtime
        .as_ref()
        .map(|selection| native::runtime::selected::prepare(selection, &paths.workspace))
        .transpose()?;
    #[cfg(target_os = "macos")]
    {
        native::validate(
            config.macos.as_ref(),
            selected_runtime.is_some(),
            &paths.workspace,
            policy,
            &options.command,
            options.dev_env,
            options.dry_run,
        )?;
    }
    let host_home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let pi = pi::Pi::inspect(policy.harness, &host_home, &config.pi)?;
    #[cfg(target_os = "macos")]
    pi.validate_native_resources()?;
    for rewrite in &git_rewrites {
        eprintln!(
            "slopbox: Git URL {} uses authenticated route {}",
            rewrite.url, rewrite.route
        );
    }
    if let Some(identity) = &git_identity {
        eprintln!(
            "slopbox: Git identity: {} <{}> ({})",
            identity.name, identity.email, identity.fingerprint
        );
    }
    let authenticated_http_routes = if options.dry_run {
        Vec::new()
    } else {
        configured_http_routes(&config, &paths.workspace)?
    };
    for route in &authenticated_http_routes {
        eprintln!("slopbox: authenticated HTTP route: {}", route.name());
    }
    let direct_accounts = authenticated_http_routes
        .iter()
        .any(AuthenticatedHttpRoute::is_direct);
    let general_network = policy.network != NetworkMode::None;
    let network = NetworkPlan {
        general: general_network,
        model: policy.credentials == crate::policy::CredentialMode::Brokered,
        authenticated_http: !authenticated_http_routes.is_empty(),
        tool: if general_network {
            options.tool_network
        } else {
            ToolNetwork::None
        },
    };

    validate_workspace_contents(&paths.workspace)?;

    let workspace_mount = prepare_workspace_mount(&paths, policy.workspace, options.dry_run)?;
    create_private_home(&paths.private_home)?;
    pi::Pi::initialize_home(&paths.private_home)?;
    #[cfg(target_os = "linux")]
    {
        create_private_home(&paths.tool_home)?;
        pi::Pi::initialize_home(&paths.tool_home)?;
    }
    #[cfg(target_os = "macos")]
    let tool_cache = native::runtime::prepare_tool_cache(&paths.tool_home)?;
    let session_dir = tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&paths.box_root)
        .context("failed to create session runtime directory")?;
    #[cfg(target_os = "macos")]
    let (private_home, tool_home) = (
        session_dir.path().join("home"),
        session_dir.path().join("tool-home"),
    );
    #[cfg(target_os = "linux")]
    let (private_home, tool_home) = (paths.private_home.clone(), paths.tool_home.clone());
    #[cfg(target_os = "macos")]
    {
        create_private_home(&private_home)?;
        create_private_home(&tool_home)?;
    }
    #[cfg(target_os = "linux")]
    let dev_environment = if options.dry_run {
        None
    } else {
        prepare_dev_environment(session_dir.path(), &workspace_mount.source, options.dev_env)?
    };

    let git_signing = if options.dry_run {
        None
    } else {
        git_identity.map(GitSigningSession::start).transpose()?
    };
    let gateway =
        if options.dry_run || (!network.general && !network.model && !network.authenticated_http) {
            None
        } else {
            Some(GatewaySession::start(
                &paths.box_root,
                authenticated_http_routes,
            )?)
        };
    let brokers = BrokerConnections {
        general: gateway
            .as_ref()
            .filter(|_| network.general)
            .map(|gateway| ProxyEndpoint {
                socket_dir: gateway.general_socket_dir(),
                port: gateway.general_proxy_port(),
            }),
        model: gateway
            .as_ref()
            .filter(|_| network.model)
            .map(|gateway| ProxyEndpoint {
                socket_dir: gateway.model_socket_dir(),
                port: gateway.model_proxy_port(),
            }),
        authenticated_http: gateway.as_ref().filter(|_| network.authenticated_http).map(
            |gateway| ProxyEndpoint {
                socket_dir: gateway.authenticated_http_socket_dir(),
                port: gateway.authenticated_http_proxy_port(),
            },
        ),
        git_signing: git_signing
            .as_ref()
            .map(|signing| signing.socket_dir().to_path_buf()),
    };
    #[cfg(target_os = "macos")]
    let (mut native_session, brokers) = {
        let mut brokers = brokers;
        let session = native::Session::start(&executor, &mut brokers)?;
        (session, brokers)
    };
    #[cfg(target_os = "macos")]
    let private_home = if selected_runtime.is_some() {
        // Keep per-run application paths short and inside owned session state.
        let home = native_session.directory().join("home");
        create_private_home(&home)?;
        home
    } else {
        private_home
    };
    #[cfg(target_os = "macos")]
    let dev_environment = prepare_dev_environment(
        native_session.directory(),
        &workspace_mount.source,
        options.dev_env,
    )?;
    if !options.dry_run && (git_signing.is_some() || !git_rewrites.is_empty()) {
        let broker_base = brokers
            .authenticated_http
            .as_ref()
            .map(|endpoint| format!("http://127.0.0.1:{}", endpoint.port))
            .unwrap_or_default();
        configure_git(
            session_dir.path(),
            git_signing.as_ref(),
            &git_rewrites,
            &broker_base,
            #[cfg(target_os = "macos")]
            &native_session.executable(),
        )?;
    }
    #[cfg(target_os = "macos")]
    let generic = config.runtime.is_some();
    #[cfg(target_os = "linux")]
    let generic = false;
    let harness = if generic {
        crate::harness::PreparedHarness::default()
    } else {
        pi.prepare(&PrepareContext {
            #[cfg(target_os = "macos")]
            native: config.macos.as_ref(),
            session_dir: session_dir.path(),
            private_home: &paths.private_home,
            host_path: if config.runtime.is_some() {
                OsStr::new("")
            } else {
                &host_path
            },
            profile,
            general_network,
            tool_network: network.tool,
            model_providers: if network.model {
                gateway
                    .as_ref()
                    .map(GatewaySession::providers)
                    .unwrap_or_default()
            } else {
                &[]
            },
            dry_run: options.dry_run,
        })?
    };
    let runtime = prepare_runtime(
        policy.runtime,
        &host_path,
        dev_environment.as_ref(),
        #[cfg(target_os = "linux")]
        harness.executable.as_deref(),
        #[cfg(target_os = "linux")]
        git_signing.is_some(),
        #[cfg(target_os = "linux")]
        options.dry_run,
        #[cfg(target_os = "macos")]
        &paths.workspace,
        selected_runtime,
    )?;
    let mut clipboard = if cfg!(target_os = "linux")
        && private_terminal
        && pi.accepts_image_paste(&options.command)
        && !options.dry_run
    {
        Some(crate::clipboard::Clipboard::new(
            session_dir.path().join("clipboard"),
            crate::command::trusted_executable(
                "wl-paste",
                #[cfg(target_os = "macos")]
                &paths.workspace,
            )
            .ok(),
        )?)
    } else {
        None
    };
    let mut environment = broker_environment(&brokers);
    if let Some(ca) = gateway.as_ref().and_then(GatewaySession::account_ca) {
        let path = session_dir.path().join("account-ca.pem");
        write_private(&path, ca)?;
        #[cfg(target_os = "linux")]
        let path = PathBuf::from("/run/slopbox/account-ca.pem");
        environment.push(("SLOPBOX_ACCOUNT_CA".into(), path.into_os_string()));
    }
    if direct_accounts {
        let endpoint = brokers
            .authenticated_http
            .as_ref()
            .context("direct account broker is unavailable")?;
        let directory = crate::github::prepare(session_dir.path(), endpoint)?;
        #[cfg(target_os = "linux")]
        let directory = {
            let _ = directory;
            PathBuf::from("/run/slopbox/github")
        };
        environment.push(("SLOPBOX_GITHUB_CONFIG".into(), directory.into_os_string()));
    }
    let plan = ExecutionPlan {
        session_dir: session_dir.path(),
        private_home: &private_home,
        tool_home: &tool_home,
        #[cfg(target_os = "macos")]
        tool_cache: &tool_cache,
        workspace: Workspace {
            source: &workspace_mount.source,
            target: &paths.workspace,
            writable: workspace_mount.writable,
        },
        dev_environment: dev_environment.as_ref(),
        runtime: &runtime,
        harness: &harness,
        brokers: &brokers,
        environment: &environment,
        private_terminal,
        clipboard: clipboard.is_some(),
    };
    #[cfg(target_os = "linux")]
    let mut command = native::command(&executor, &plan, &options.command)?;
    #[cfg(target_os = "macos")]
    let (mut job, mut command) = native_session.command(&plan, &options.command)?;
    if options.dry_run {
        if private_terminal {
            eprintln!(
                "slopbox: this preview omits private-PTY setup; do not run the printed command directly"
            );
        }
        print_command(&command);
        return Ok(success_status());
    }

    let result = if private_terminal {
        crate::terminal::run(
            &mut command,
            options.approval_view,
            clipboard.as_mut(),
            crate::approval::Context {
                workspace: &paths.workspace,
                box_root: &paths.box_root,
                session: gateway.as_ref().map(GatewaySession::session_id),
                general_network,
            },
        )
    } else {
        command.status().context("failed to start sandbox executor")
    };
    #[cfg(target_os = "macos")]
    {
        drop(command);
        job.finish()?;
        drop(job);
        native_session.finish()?;
    }
    result
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_config() -> Result<(Option<native::Config>, Option<backend::RuntimeSelection>)>
{
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("slopbox");
    let config = load_config(&root)?;
    Ok((config.macos, config.runtime))
}

fn prepare_paths(workspace: Option<&Path>) -> Result<Paths> {
    let workspace = resolve_workspace(workspace)?;

    let host_home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host_home.join(".local/share"));
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host_home.join(".config"));

    let state_root = canonicalize_allow_missing(&absolute_path(&data_home)?.join("slopbox"))?;
    let config_root = canonicalize_allow_missing(&absolute_path(&config_home)?.join("slopbox"))?;
    let project_id = blake3::hash(workspace.as_os_str().as_bytes()).to_hex();
    let box_root = state_root.join("boxes").join(project_id.as_str());
    let private_home = box_root.join("home");
    let tool_home = box_root.join("tool-home");

    Ok(Paths {
        workspace,
        box_root,
        private_home,
        tool_home,
        config_root,
        state_root,
    })
}

pub fn effective_policy(
    workspace: Option<&Path>,
    profile: Option<Profile>,
    no_host_pi_resources: bool,
) -> Result<(Profile, Policy)> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    let project_config = load_project_config(&paths.workspace)?;
    let launch_config = crate::launch::load(&paths.config_root, &paths.workspace)?;
    Ok(select_policy(
        &config,
        &project_config,
        profile,
        no_host_pi_resources,
        launch_config.as_ref(),
    ))
}

pub fn status(
    workspace: Option<&Path>,
    profile: Option<Profile>,
    no_host_pi_resources: bool,
    tool_network: ToolNetwork,
    verbose: bool,
) -> Result<String> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    let project = load_project_config(&paths.workspace)?;
    let launch_config = crate::launch::load(&paths.config_root, &paths.workspace)?;
    let (profile, policy) = select_policy(
        &config,
        &project,
        profile,
        no_host_pi_resources,
        launch_config.as_ref(),
    );
    describe_status(&paths, &config, profile, policy, tool_network, verbose)
}

pub fn doctor(
    workspace: Option<&Path>,
    profile: Option<Profile>,
    no_host_pi_resources: bool,
) -> Result<(String, bool)> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    let project = load_project_config(&paths.workspace)?;
    let launch = crate::launch::load(&paths.config_root, &paths.workspace)?;
    let (profile, policy) = select_policy(
        &config,
        &project,
        profile,
        no_host_pi_resources,
        launch.as_ref(),
    );
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let mut lines = vec![
        format!("Project {}", paths.workspace.display()),
        format!(
            "Host config {}",
            paths.config_root.join("config.toml").display()
        ),
        if config.runtime.is_some() {
            "Setup: not needed for explicit run commands; Pi default launch is unavailable".into()
        } else if launch.is_some() {
            "Setup: saved host-side policy applies".into()
        } else {
            "Setup: not initialized; run slopbox in a host terminal or use slopbox init".into()
        },
    ];
    let mut checks = vec![
        (
            "Policy",
            policy
                .ensure_implemented(profile)
                .and_then(|()| validate_runtime_selection(&config, policy))
                .map(|()| format!("{profile}; effective policy supported")),
        ),
        (
            "Pi resources",
            {
                let resources = pi::Pi::inspect(policy.harness, &home, &config.pi);
                #[cfg(target_os = "macos")]
                let resources = resources.and_then(|pi| pi.validate_native_resources());
                resources.map(|_| "resource paths checked; extension code not executed".into())
            }
            .context("check host Pi resource paths, or retry with --no-host-pi-resources"),
        ),
        (
            "Model configuration",
            check_model_configuration(&paths, policy),
        ),
        (
            "Account configuration",
            (|| {
                let routes = workspace_http_routes(&config, &paths.workspace)?;
                configured_git_rewrites(&config, &paths.workspace)?;
                for route in &routes {
                    let (HttpAuthenticationConfig::Basic { secret, .. }
                    | HttpAuthenticationConfig::Bearer { secret }
                    | HttpAuthenticationConfig::Token { secret }) = &route.authentication;
                    ensure!(
                        config.secrets.contains_key(secret),
                        "route {} refers to unknown secret {}; define it in host configuration",
                        route.name,
                        secret
                    );
                }
                Ok(format!(
                    "{} route(s); secret values and upstreams not checked",
                    routes.len()
                ))
            })(),
        ),
        (
            "Signing configuration",
            configured_git_identity(&config, &paths.workspace).map(|identity| {
                if identity.is_some() {
                    "identity configured; signing key not queried".into()
                } else {
                    "not configured (optional)".into()
                }
            }),
        ),
    ];
    native::diagnostics::check(
        &paths.workspace,
        policy,
        &mut checks,
        &mut lines,
        config.runtime.as_ref(),
    );
    let mut failures = 0;
    for (name, result) in checks {
        match result {
            Ok(message) => lines.push(format!("OK   {name}: {message}")),
            Err(error) => {
                failures += 1;
                lines.push(format!("FAIL {name}: {error:#}"));
            }
        }
    }
    lines.extend([
        String::new(),
        format!("{failures} failed check(s). No project state or approvals changed."),
        "Not checked: full Pi launch, credential validity, signing keys, upstream connectivity, or Nix evaluation/builds/daemon access.".into(),
    ]);
    Ok((
        format!(
            "{}\n",
            lines
                .iter()
                .map(|line| crate::launch::terminal_text(line))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        failures == 0,
    ))
}

fn check_model_configuration(paths: &Paths, policy: Policy) -> Result<String> {
    if policy.credentials != crate::policy::CredentialMode::Brokered {
        return Ok("disabled by policy".into());
    }
    let providers = crate::provider::configured_names(&paths.state_root);
    ensure!(
        !providers.is_empty(),
        "no model account is configured; on the host run `slopbox auth login openai-codex`, or set OPENROUTER_API_KEY, then retry"
    );
    Ok(format!(
        "{} (configured, not validated)",
        providers.join(", ")
    ))
}

fn validate_runtime_selection(config: &SlopboxConfig, policy: Policy) -> Result<()> {
    if config.runtime.is_some() {
        ensure!(
            policy.runtime == RuntimeMode::Host,
            "selected executable runtimes require runtime=host; no fallback from a project or image runtime"
        );
        ensure!(
            policy.harness == HarnessMode::None,
            "selected executable runtimes require harness=none; integrated Pi separation is not enabled implicitly"
        );
    }
    Ok(())
}

fn describe_status(
    paths: &Paths,
    config: &SlopboxConfig,
    profile: Profile,
    policy: Policy,
    tool_network: ToolNetwork,
    verbose: bool,
) -> Result<String> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let pi = pi::Pi::inspect(policy.harness, &home, &config.pi)?;
    #[cfg(target_os = "macos")]
    let native_resources = pi.validate_native_resources();
    let resources = pi.resources;
    let routes = workspace_http_routes(config, &paths.workspace)?;
    let identity = configured_git_identity(config, &paths.workspace)?;
    let rewrites = configured_git_rewrites(config, &paths.workspace)?;
    let mut lines = vec![
        format!("Project       {}", paths.workspace.display()),
        if config.runtime.is_some() {
            "Execution     Generic commands; no automatic harness/tool separation".into()
        } else {
            "Agent         Pi".into()
        },
        format!(
            "Default env   {}",
            native::diagnostics::environment_description(&paths.workspace)
        ),
        format!(
            "Changes       {}",
            match policy.workspace {
                WorkspaceMode::Live =>
                    "Immediate; host editors and build tools see edits before review",
                WorkspaceMode::Staged => "Kept separate; review and apply from the host",
                WorkspaceMode::ReadOnly => "Project is read-only; private state remains writable",
            }
        ),
        format!(
            "Host tools    {}",
            if config.runtime.is_some() {
                "Selected executables, resources and platform base; read-only"
            } else {
                native::diagnostics::runtime_description(policy.runtime)
            }
        ),
        format!(
            "Internet      {}",
            match policy.network {
                NetworkMode::None =>
                    "General egress disabled; model and account routes are separate",
                NetworkMode::Allowlist =>
                    "Only approved destinations; blocked proxy requests are logged",
                NetworkMode::Observe => "Observe-only networking requested; not implemented",
            }
        ),
        format!(
            "Tool internet {}",
            if config.runtime.is_some() {
                "No automatic separation; subprocesses inherit outer access"
            } else if policy.network == NetworkMode::None || tool_network == ToolNetwork::None {
                "General egress disabled; configured account routes remain available"
            } else {
                "Pi shell tools can use approved destinations and configured account routes"
            }
        ),
    ];
    if policy.credentials == crate::policy::CredentialMode::Brokered {
        let providers = crate::provider::configured_names(&paths.state_root);
        lines.push(format!(
            "Model         {}",
            if providers.is_empty() {
                "None configured".to_owned()
            } else {
                format!("{} (configured, not validated)", providers.join(", "))
            }
        ));
    } else {
        lines.push("Model         Disabled".into());
    }
    if routes.is_empty() {
        lines.push("Accounts      No authenticated routes".into());
    } else {
        for route in routes {
            if route.proxy {
                lines.push(format!(
                    "Account TLS   {}: opt-in mediation; explicit proxy and session trust required",
                    route.name
                ));
            }
            lines.push(format!(
                "Account route {}: {} {}{}; available to agent and commands",
                route.name,
                route
                    .methods
                    .iter()
                    .map(|method| method.to_ascii_uppercase())
                    .collect::<Vec<_>>()
                    .join(","),
                url::Url::parse(&route.upstream)?,
                if route.allow_private_addresses {
                    " (private addresses allowed)"
                } else {
                    ""
                },
            ));
        }
    }
    lines.push(format!(
        "Signing       {}",
        match identity {
            Some(identity) => format!(
                "{} <{}> ({}) — key availability not checked",
                identity.name, identity.email, identity.fingerprint
            ),
            None => "Not configured".into(),
        }
    ));
    for rewrite in rewrites {
        lines.push(format!(
            "Git URL       {} -> authenticated route {}",
            rewrite.url, rewrite.route
        ));
    }
    lines.push(format!(
        "Pi resources  {}",
        match policy.harness {
            HarnessMode::None => "No host resources".to_owned(),
            HarnessMode::Data => "Host data only; no host extension code".to_owned(),
            HarnessMode::Trusted => format!(
                "Trusted code and data; {} extension(s) with model-route access when enabled",
                resources.extensions.len()
            ),
        }
    ));
    if policy.harness != HarnessMode::None {
        for (name, description) in [
            ("settings.json", "filtered settings"),
            ("AGENTS.md", "prompt data"),
        ] {
            let source = home.join(".pi/agent").join(name);
            if source.is_file() {
                lines.push(format!(
                    "Host import   {} ({description})",
                    source.display()
                ));
            }
        }
    }
    let mut sources: Vec<_> = resources.mounts.iter().map(|(source, _)| source).collect();
    sources.sort();
    sources.dedup();
    for source in sources {
        lines.push(format!("Read-only     {}", source.display()));
    }
    for (source, target) in &resources.temporary_overlays {
        let access = if cfg!(target_os = "macos") {
            "unsupported on macOS"
        } else {
            "existing data readable, writes discarded"
        };
        lines.push(format!(
            "Temporary     {} -> {}; {access}",
            source.display(),
            target.display()
        ));
    }
    lines.push("Credentials   Broker credentials stay on the host".into());
    lines.push(format!(
        "Isolation     {}",
        match policy.backend {
            crate::policy::Backend::Native =>
                "Shared host kernel; no CPU, memory, process, or disk limits",
            crate::policy::Backend::MicroVm => "Separate guest kernel requested; not implemented",
        }
    ));
    let supported = policy
        .ensure_implemented(profile)
        .and_then(|()| backend::ensure_supported())
        .and_then(|()| validate_runtime_selection(config, policy));
    #[cfg(target_os = "macos")]
    let supported = supported
        .and_then(|()| native::validate_policy(policy))
        .and(native_resources);
    if let Err(error) = supported {
        lines.push(format!("Launch        Unsupported: {error}"));
    }
    lines.push(String::new());
    lines.push(
        "Launch policy only; runtime, credential validity, and connectivity are not checked."
            .into(),
    );
    if verbose {
        let (_, rules) = access::select(
            &config.defaults,
            &config.workspaces,
            &home,
            &paths.workspace,
        )?;
        lines.push(format!("workspace-rules: {rules:?} (broadest first)"));
        for (setting, default, rule) in [
            (
                "git_identity",
                config.defaults.git_identity.is_some(),
                rules
                    .iter()
                    .rev()
                    .find(|&&index| config.workspaces[index].git_identity.is_some()),
            ),
            (
                "accounts",
                config.defaults.accounts.is_some(),
                rules
                    .iter()
                    .rev()
                    .find(|&&index| config.workspaces[index].accounts.is_some()),
            ),
        ] {
            let source = match rule {
                Some(index) => format!("host workspaces[{index}]"),
                None if default => "host defaults".into(),
                None => "legacy exact-workspace bindings".into(),
            };
            lines.push(format!("{setting}-source: {source}"));
        }
        lines.push(String::new());
        lines.extend([
            format!("profile: {profile}"),
            format!("backend: {}", policy.backend),
            format!("workspace: {}", policy.workspace),
            format!("network: {}", policy.network),
            format!("runtime: {}", policy.runtime),
            format!("harness: {}", policy.harness),
            format!("persistence: {}", policy.persistence),
            format!("credentials: {}", policy.credentials),
            format!(
                "host-config: {}",
                paths.config_root.join("config.toml").display()
            ),
            format!("project-state: {}", paths.box_root.display()),
            format!(
                "project-launch-config: {}",
                crate::launch::config_path(&paths.config_root, &paths.workspace).display()
            ),
        ]);
        if let Some(runtime) = &config.runtime {
            lines.push("runtime-source: host runtime.executables".into());
            lines.push(
                "runtime-infrastructure: slopbox, bash, bwrap, env; ssh-keygen when signing".into(),
            );
            for executable in &runtime.executables {
                lines.push(format!("runtime-executable: {}", executable.display()));
            }
            for root in &runtime.dependency_roots {
                lines.push(format!(
                    "runtime-dependency-root: {} (discovered files only)",
                    root.display()
                ));
            }
            for bundle in &runtime.bundles {
                lines.push(format!(
                    "runtime-bundle: {} (whole tree, read-only code and data)",
                    bundle.display()
                ));
            }
        }
        for (source, target) in resources.mounts {
            lines.push(format!(
                "read-only-mount: {} -> {}",
                source.display(),
                target.display()
            ));
        }
        for extension in resources.extensions {
            lines.push(format!("trusted-extension: {}", extension.display()));
        }
    }
    Ok(format!(
        "{}\n",
        lines
            .iter()
            .map(|line| crate::launch::terminal_text(line))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

pub fn init_project(
    workspace: Option<&Path>,
    changes: Option<WorkspaceMode>,
    no_host_pi_resources: bool,
    yes: bool,
) -> Result<LaunchConfig> {
    backend::ensure_supported()?;
    ensure!(
        !yes || changes.is_some(),
        "--yes requires an explicit --changes mode"
    );
    if !yes {
        crate::launch::require_terminal()?;
    }
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    let project = load_project_config(&paths.workspace)?;
    ensure!(
        config.runtime.is_none(),
        "selected executable runtimes do not provide Pi project setup; use slopbox run -- COMMAND"
    );
    let previous = crate::launch::load(&paths.config_root, &paths.workspace)?;
    let (profile, mut policy) = select_policy(&config, &project, None, no_host_pi_resources, None);
    policy.ensure_implemented(profile)?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stderr().lock();
    writeln!(
        output,
        "Configure {} for Pi.",
        crate::launch::terminal_text(&paths.workspace.to_string_lossy())
    )?;
    let changes = match changes {
        Some(changes) => changes,
        None => crate::launch::choose_changes(
            &mut input,
            &mut output,
            policy.workspace,
            previous
                .as_ref()
                .map(|config| config.policy.workspace)
                .unwrap_or(policy.workspace),
        )?,
    };
    ensure!(
        changes <= policy.workspace,
        "requested changes={changes} exceeds the current workspace={} policy",
        policy.workspace
    );
    policy.workspace = changes;
    #[cfg(target_os = "macos")]
    native::validate_policy(policy)?;
    let launch = LaunchConfig {
        workspace: paths.workspace.clone(),
        agent: Agent::Pi,
        policy,
    };
    writeln!(
        output,
        "\n{}",
        describe_status(
            &paths,
            &config,
            profile,
            policy,
            ToolNetwork::General,
            false
        )?
    )?;
    if !yes && !crate::launch::confirm(&mut input, &mut output)? {
        bail!("setup cancelled; no configuration changed");
    }
    let path = crate::launch::save(&paths.config_root, &launch, previous.as_ref())?;
    writeln!(
        output,
        "Saved {}",
        crate::launch::terminal_text(&path.to_string_lossy())
    )?;
    Ok(launch)
}

pub fn launch(agent_arguments: Vec<OsString>, approval_view: bool) -> Result<ExitStatus> {
    backend::ensure_supported()?;
    let paths = prepare_paths(None)?;
    validate_workspace(&paths)?;
    ensure!(
        load_config(&paths.config_root)?.runtime.is_none(),
        "selected executable runtimes do not provide integrated Pi launch; use slopbox run -- COMMAND"
    );
    let (launch, initialized) = match crate::launch::load(&paths.config_root, &paths.workspace)? {
        Some(config) => (config, false),
        None => (
            init_project(Some(&paths.workspace), None, false, false)?,
            true,
        ),
    };
    let config = load_config(&paths.config_root)?;
    let project = load_project_config(&paths.workspace)?;
    let (profile, policy) = select_policy(&config, &project, None, false, Some(&launch));
    policy.ensure_implemented(profile)?;
    if !initialized {
        eprint!(
            "{}",
            describe_status(
                &paths,
                &config,
                profile,
                policy,
                ToolNetwork::General,
                false
            )?
        );
    }
    #[cfg(target_os = "linux")]
    ensure!(
        find_optional_executable("pi", &sandbox_path()?).is_some(),
        "Pi is not available in the host Nix PATH; install Pi on the host, then retry"
    );
    ensure!(
        policy.runtime != RuntimeMode::Project || paths.workspace.join("flake.nix").is_file(),
        "this policy requires a project flake development environment, but flake.nix is missing"
    );
    check_model_configuration(&paths, policy)?;
    let command = match launch.agent {
        Agent::Pi => pi::launch_command(agent_arguments),
    };
    run(RunOptions {
        workspace: Some(paths.workspace),
        profile: None,
        dev_env: DevEnvironment::Auto,
        tool_network: ToolNetwork::General,
        no_host_pi_resources: false,
        command,
        dry_run: false,
        approval_view,
        launch_config: Some(launch),
    })
}

pub fn effective_git_identity(
    workspace: Option<&Path>,
) -> Result<Option<(String, String, String)>> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    Ok(configured_git_identity(&config, &paths.workspace)?
        .map(|identity| (identity.name, identity.email, identity.fingerprint)))
}

pub fn effective_http_route_names(workspace: Option<&Path>) -> Result<Vec<String>> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    let mut names: Vec<_> = workspace_http_routes(&config, &paths.workspace)?
        .into_iter()
        .map(|route| route.name.clone())
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn configured_git_identity(
    config: &SlopboxConfig,
    workspace: &Path,
) -> Result<Option<GitSigningIdentity>> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let (selection, _) = access::select(&config.defaults, &config.workspaces, &home, workspace)?;
    if matches!(
        selection.git_identity,
        Some(access::Identity::Disabled(false))
    ) {
        return Ok(None);
    }
    let mut matching = Vec::new();
    for identity in &config.git.identities {
        let selected = match &selection.git_identity {
            Some(access::Identity::Named(id)) => identity.id.as_ref() == Some(id),
            None => identity.workspace.is_some(),
            Some(access::Identity::Disabled(_)) => unreachable!(),
        };
        if !selected || !matches_workspace(identity.workspace.as_deref(), &home, workspace)? {
            continue;
        }
        matching.push(GitSigningIdentity {
            name: identity.name.clone(),
            email: identity.email.clone(),
            fingerprint: identity.signing_key_fingerprint.clone(),
        });
    }
    ensure!(
        selection.git_identity.is_none() || !matching.is_empty(),
        "selected Git identity is unknown or outside its workspace binding"
    );
    ensure!(
        matching.len() <= 1,
        "multiple Git identities are configured for {}",
        workspace.display()
    );
    Ok(matching.pop())
}

pub fn effective_git_rewrites(workspace: Option<&Path>) -> Result<Vec<GitUrlRewrite>> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let config = load_config(&paths.config_root)?;
    configured_git_rewrites(&config, &paths.workspace)
}

fn configured_git_rewrites(config: &SlopboxConfig, workspace: &Path) -> Result<Vec<GitUrlRewrite>> {
    let mut rewrites: Vec<GitUrlRewrite> = Vec::new();
    for route in workspace_http_routes(config, workspace)? {
        for url in &route.git_urls {
            if let Some(existing) = rewrites.iter().find(|rewrite| rewrite.url == *url) {
                ensure!(
                    existing.route == route.name,
                    "Git URL is assigned to multiple authenticated routes: {} and {}",
                    existing.route,
                    route.name
                );
                continue;
            }
            rewrites.push(GitUrlRewrite::new(url.clone(), route.name.clone())?);
        }
    }
    Ok(rewrites)
}

fn configure_git(
    session_dir: &Path,
    signing: Option<&GitSigningSession>,
    rewrites: &[GitUrlRewrite],
    broker_base: &str,
    #[cfg(target_os = "macos")] executor: &Path,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    let signing_helper = PathBuf::from("/run/slopbox/git-sign");
    #[cfg(target_os = "macos")]
    let signing_helper = executor.with_file_name("git-sign");
    let gitconfig = crate::git_config::render(
        signing.map(|signing| {
            (
                signing.identity(),
                signing.public_key(),
                signing_helper.as_path(),
            )
        }),
        rewrites,
        broker_base,
    );
    let gitconfig_path = session_dir.join("gitconfig");
    write_private(&gitconfig_path, &gitconfig)?;
    fs::set_permissions(&gitconfig_path, fs::Permissions::from_mode(0o600))?;
    let Some(_signing) = signing else {
        return Ok(());
    };

    #[cfg(target_os = "linux")]
    let (wrapper_path, wrapper) = {
        let ssh_keygen = crate::command::trusted_executable("ssh-keygen")?;
        (
            session_dir.join("git-sign"),
            format!(
                "#!/bin/sh\nif [ \"${{1-}}\" = -Y ] && [ \"${{2-}}\" = sign ]; then\n  exec /run/slopbox/slopbox __git-sign \"$@\"\nfi\nexec {} \"$@\"\n",
                shell_quote(&ssh_keygen.to_string_lossy())
            ),
        )
    };
    #[cfg(target_os = "macos")]
    let (wrapper_path, wrapper) = (
        signing_helper,
        format!(
            "#!/bin/bash\nexec {} __git-sign --socket {} -- \"$@\"\n",
            shell_quote(&executor.to_string_lossy()),
            shell_quote(&_signing.socket_dir().join("gateway.sock").to_string_lossy()),
        ),
    );
    write_private(&wrapper_path, &wrapper)?;
    fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn select_policy(
    config: &SlopboxConfig,
    project_config: &ProjectConfig,
    profile: Option<Profile>,
    no_host_pi_resources: bool,
    launch_config: Option<&LaunchConfig>,
) -> (Profile, Policy) {
    let profile = profile.unwrap_or(config.profile);
    let mut policy = config.policy.apply(profile.policy());
    if let Some(launch) = launch_config {
        policy = policy.restrict(launch.policy);
    }
    policy = project_config.policy.apply(policy);
    if no_host_pi_resources {
        policy = policy.without_host_harness();
    }
    (profile, policy)
}

fn prepare_workspace_mount(
    paths: &Paths,
    mode: WorkspaceMode,
    dry_run: bool,
) -> Result<WorkspaceMount> {
    match mode {
        WorkspaceMode::Live => Ok(WorkspaceMount {
            source: paths.workspace.clone(),
            writable: true,
        }),
        WorkspaceMode::ReadOnly => Ok(WorkspaceMount {
            source: paths.workspace.clone(),
            writable: false,
        }),
        WorkspaceMode::Staged => {
            let stages = paths.box_root.join("stages");
            let id = format!(
                "{}-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("system clock is before the Unix epoch")?
                    .as_nanos(),
                std::process::id()
            );
            let stage = stages.join(id);
            let baseline = stage.join("baseline");
            let workspace = stage.join("workspace");
            if dry_run {
                return Ok(WorkspaceMount {
                    source: workspace,
                    writable: true,
                });
            }
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&stages)?;
            DirBuilder::new().mode(0o700).create(&stage)?;
            if let Err(error) = copy_workspace_tree(&paths.workspace, &baseline)
                .and_then(|()| copy_workspace_tree(&baseline, &workspace))
            {
                let _ = fs::remove_dir_all(&stage);
                return Err(error);
            }
            eprintln!(
                "slopbox: staged workspace retained at {}",
                workspace.display()
            );
            Ok(WorkspaceMount {
                source: workspace,
                writable: true,
            })
        }
    }
}

fn copy_workspace_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).with_context(|| {
        format!(
            "failed to inspect staged workspace source {}",
            source.display()
        )
    })?;
    if metadata.is_dir() {
        DirBuilder::new()
            .mode(0o700)
            .create(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        for entry in
            fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
        {
            let entry = entry?;
            copy_workspace_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
        return Ok(());
    }
    if metadata.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)
            .with_context(|| format!("failed to open {}", source.display()))?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(metadata.permissions().mode())
            .custom_flags(libc::O_NOFOLLOW)
            .open(destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        clone_or_copy_file(&mut input, &mut output)?;
        output.sync_all()?;
        fs::set_permissions(destination, metadata.permissions())?;
        return Ok(());
    }
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        std::os::unix::fs::symlink(target, destination)?;
        return Ok(());
    }
    bail!(
        "workspace contains unsupported file type at {}",
        source.display()
    )
}

pub fn list_stages(workspace: Option<&Path>) -> Result<Vec<StageInfo>> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let stages = paths.box_root.join("stages");
    let entries = match fs::read_dir(&stages) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_dir() || entry.file_type()?.is_symlink() {
            continue;
        }
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("stage ID is not valid UTF-8"))?;
        if validate_stage_id(&id).is_err() {
            continue;
        }
        let staged_workspace = entry.path().join("workspace");
        let baseline = entry.path().join("baseline");
        if staged_workspace.is_dir() && baseline.is_dir() {
            result.push(StageInfo {
                id,
                workspace: staged_workspace,
            });
        }
    }
    result.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(result)
}

pub fn diff_stage(workspace: Option<&Path>, stage: &str) -> Result<()> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let stage = resolve_stage(&paths, stage)?;
    let diff = crate::command::system_diff()?;
    diff_trees(
        &diff,
        Some(&stage.join("baseline")),
        Some(&stage.join("workspace")),
        Path::new(""),
    )
}

fn diff_trees(
    diff: &Path,
    baseline: Option<&Path>,
    workspace: Option<&Path>,
    relative: &Path,
) -> Result<()> {
    let baseline_metadata = baseline.map(fs::symlink_metadata).transpose()?;
    let workspace_metadata = workspace.map(fs::symlink_metadata).transpose()?;
    let baseline_type = baseline_metadata
        .as_ref()
        .map(|metadata| metadata.file_type());
    let workspace_type = workspace_metadata
        .as_ref()
        .map(|metadata| metadata.file_type());
    report_mode_change(
        relative,
        baseline_metadata.as_ref(),
        workspace_metadata.as_ref(),
    );
    if baseline_type.as_ref().is_some_and(|kind| kind.is_dir())
        && workspace_type.as_ref().is_some_and(|kind| kind.is_dir())
    {
        let mut names = directory_entry_names(baseline.context("missing baseline directory")?)?;
        names.extend(directory_entry_names(
            workspace.context("missing staged directory")?,
        )?);
        let mut names: Vec<_> = names.into_iter().collect();
        names.sort();
        for name in names {
            let baseline_child = existing_path(baseline.map(|path| path.join(&name)))?;
            let workspace_child = existing_path(workspace.map(|path| path.join(&name)))?;
            diff_trees(
                diff,
                baseline_child.as_deref(),
                workspace_child.as_deref(),
                &relative.join(name),
            )?;
        }
        return Ok(());
    }
    if baseline_type.as_ref().is_some_and(|kind| kind.is_file())
        && workspace_type.as_ref().is_some_and(|kind| kind.is_file())
    {
        return diff_regular_files(
            diff,
            baseline.context("missing baseline file")?,
            workspace.context("missing staged file")?,
            relative,
        );
    }
    if baseline_type.as_ref().is_some_and(|kind| kind.is_symlink())
        && workspace_type
            .as_ref()
            .is_some_and(|kind| kind.is_symlink())
    {
        let before = fs::read_link(baseline.context("missing baseline symlink")?)?;
        let after = fs::read_link(workspace.context("missing staged symlink")?)?;
        if before != after {
            println!(
                "symlink {}: {} -> {}",
                relative.display(),
                before.display(),
                after.display()
            );
        }
        return Ok(());
    }
    if baseline_type.is_some()
        && workspace_type.is_some()
        && !same_file_type(
            baseline_type.as_ref().context("missing baseline type")?,
            workspace_type.as_ref().context("missing staged type")?,
        )
    {
        diff_trees(diff, baseline, None, relative)?;
        diff_trees(diff, None, workspace, relative)?;
        return Ok(());
    }
    if baseline_type.is_none() && workspace_type.as_ref().is_some_and(|kind| kind.is_file()) {
        return diff_regular_files(
            diff,
            Path::new("/dev/null"),
            workspace.context("missing staged file")?,
            relative,
        );
    }
    if workspace_type.is_none() && baseline_type.as_ref().is_some_and(|kind| kind.is_file()) {
        return diff_regular_files(
            diff,
            baseline.context("missing baseline file")?,
            Path::new("/dev/null"),
            relative,
        );
    }
    if baseline_type.is_none() && workspace_type.as_ref().is_some_and(|kind| kind.is_dir()) {
        for name in sorted_directory_entry_names(workspace.context("missing staged directory")?)? {
            let child = workspace.map(|path| path.join(&name));
            diff_trees(diff, None, child.as_deref(), &relative.join(name))?;
        }
        return Ok(());
    }
    if workspace_type.is_none() && baseline_type.as_ref().is_some_and(|kind| kind.is_dir()) {
        for name in sorted_directory_entry_names(baseline.context("missing baseline directory")?)? {
            let child = baseline.map(|path| path.join(&name));
            diff_trees(diff, child.as_deref(), None, &relative.join(name))?;
        }
        return Ok(());
    }
    let before = tree_entry_description(baseline, baseline_type.as_ref())?;
    let after = tree_entry_description(workspace, workspace_type.as_ref())?;
    println!("{}: {before} -> {after}", relative.display());
    Ok(())
}

fn report_mode_change(
    relative: &Path,
    baseline: Option<&fs::Metadata>,
    workspace: Option<&fs::Metadata>,
) {
    let mode = |metadata: &fs::Metadata| {
        (!metadata.file_type().is_symlink()).then(|| metadata.permissions().mode() & 0o7777)
    };
    let before = baseline.and_then(mode);
    let after = workspace.and_then(mode);
    if before != after {
        let before = before.map_or_else(|| "absent".to_owned(), |mode| format!("{mode:04o}"));
        let after = after.map_or_else(|| "absent".to_owned(), |mode| format!("{mode:04o}"));
        let relative = if relative.as_os_str().is_empty() {
            Path::new(".")
        } else {
            relative
        };
        println!("mode {}: {before} -> {after}", relative.display());
    }
}

fn sorted_directory_entry_names(path: &Path) -> Result<Vec<OsString>> {
    let mut names: Vec<_> = directory_entry_names(path)?.into_iter().collect();
    names.sort();
    Ok(names)
}

fn existing_path(path: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    match fs::symlink_metadata(&path) {
        Ok(_) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn same_file_type(left: &fs::FileType, right: &fs::FileType) -> bool {
    left.is_dir() == right.is_dir()
        && left.is_file() == right.is_file()
        && left.is_symlink() == right.is_symlink()
}

fn diff_regular_files(diff: &Path, before: &Path, after: &Path, relative: &Path) -> Result<()> {
    let status = Command::new(diff)
        .arg("-u")
        .arg("-L")
        .arg(format!("a/{}", relative.display()))
        .arg("-L")
        .arg(format!("b/{}", relative.display()))
        .arg("--")
        .arg(before)
        .arg(after)
        .status()
        .context("failed to run diff")?;
    match status.code() {
        Some(0 | 1) => Ok(()),
        _ => bail!("diff failed with {status}"),
    }
}

fn tree_entry_description(path: Option<&Path>, kind: Option<&fs::FileType>) -> Result<String> {
    let Some(kind) = kind else {
        return Ok("absent".to_owned());
    };
    if kind.is_symlink() {
        return Ok(format!(
            "symlink to {}",
            fs::read_link(path.context("missing symlink")?)?.display()
        ));
    }
    Ok(if kind.is_dir() {
        "directory"
    } else if kind.is_file() {
        "file"
    } else {
        "unsupported file"
    }
    .to_owned())
}

pub fn apply_stage(workspace: Option<&Path>, stage: &str) -> Result<()> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let stage = resolve_stage(&paths, stage)?;
    let baseline = stage.join("baseline");
    let staged_workspace = stage.join("workspace");
    validate_copyable_tree(&staged_workspace)?;
    ensure!(
        trees_equal(&baseline, &paths.workspace)?,
        "host workspace changed after stage creation; refusing to apply"
    );
    sync_tree(&staged_workspace, &paths.workspace)
        .context("stage apply failed after modifying the host workspace")
}

pub fn discard_stage(workspace: Option<&Path>, stage: &str) -> Result<()> {
    let paths = prepare_paths(workspace)?;
    validate_workspace(&paths)?;
    let stage = resolve_stage(&paths, stage)?;
    fs::remove_dir_all(&stage)
        .with_context(|| format!("failed to discard stage {}", stage.display()))
}

fn validate_copyable_tree(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            validate_copyable_tree(&entry?.path())?;
        }
        return Ok(());
    }
    ensure!(
        metadata.is_file() || metadata.file_type().is_symlink(),
        "stage contains unsupported file type at {}",
        path.display()
    );
    Ok(())
}

fn trees_equal(left: &Path, right: &Path) -> Result<bool> {
    let left_metadata = match fs::symlink_metadata(left) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(right) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
                Ok(_) => Ok(false),
                Err(error) => Err(error.into()),
            };
        }
        Err(error) => return Err(error.into()),
    };
    let right_metadata = match fs::symlink_metadata(right) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let left_type = left_metadata.file_type();
    let right_type = right_metadata.file_type();
    if left_type.is_dir() != right_type.is_dir()
        || left_type.is_file() != right_type.is_file()
        || left_type.is_symlink() != right_type.is_symlink()
        || left_metadata.permissions().mode() != right_metadata.permissions().mode()
    {
        return Ok(false);
    }
    if left_type.is_symlink() {
        return Ok(fs::read_link(left)? == fs::read_link(right)?);
    }
    if left_type.is_file() {
        if left_metadata.len() != right_metadata.len() {
            return Ok(false);
        }
        let mut left = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(left)?;
        let mut right = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(right)?;
        let mut left_buffer = [0_u8; 64 * 1024];
        let mut right_buffer = [0_u8; 64 * 1024];
        loop {
            let left_read = left.read(&mut left_buffer)?;
            let right_read = right.read(&mut right_buffer)?;
            if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
                return Ok(false);
            }
            if left_read == 0 {
                return Ok(true);
            }
        }
    }
    ensure!(left_type.is_dir(), "unsupported baseline file type");
    let left_entries = directory_entry_names(left)?;
    let right_entries = directory_entry_names(right)?;
    if left_entries != right_entries {
        return Ok(false);
    }
    for name in left_entries {
        if !trees_equal(&left.join(&name), &right.join(name))? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn directory_entry_names(path: &Path) -> Result<HashSet<OsString>> {
    fs::read_dir(path)?
        .map(|entry| Ok(entry?.file_name()))
        .collect()
}

fn sync_tree(source: &Path, destination: &Path) -> Result<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    let destination_metadata = fs::symlink_metadata(destination).ok();
    if source_metadata.is_dir() {
        if destination_metadata
            .as_ref()
            .is_some_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        {
            remove_tree_entry(destination)?;
        }
        if !destination.exists() {
            DirBuilder::new().mode(0o700).create(destination)?;
        }
        let source_entries = directory_entry_names(source)?;
        for name in directory_entry_names(destination)? {
            if !source_entries.contains(&name) {
                remove_tree_entry(&destination.join(name))?;
            }
        }
        for name in source_entries {
            sync_tree(&source.join(&name), &destination.join(name))?;
        }
        fs::set_permissions(destination, source_metadata.permissions())?;
        return Ok(());
    }
    if destination_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        fs::remove_dir_all(destination)?;
    }
    let temporary = apply_temporary_path(destination)?;
    if source_metadata.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(source_metadata.permissions().mode())
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        if let Err(error) =
            clone_or_copy_file(&mut input, &mut output).and_then(|()| output.sync_all())
        {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        fs::set_permissions(&temporary, source_metadata.permissions())?;
    } else if source_metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, &temporary)?;
    } else {
        bail!(
            "stage contains unsupported file type at {}",
            source.display()
        );
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

fn remove_tree_entry(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn apply_temporary_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .context("apply destination has no parent")?;
    let id = APPLY_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = parent.join(format!(".slopbox-apply-{}-{id}", std::process::id()));
    ensure!(
        matches!(
            fs::symlink_metadata(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ),
        "temporary apply path already exists"
    );
    Ok(path)
}

fn resolve_stage(paths: &Paths, stage: &str) -> Result<PathBuf> {
    validate_stage_id(stage)?;
    let stages = paths.box_root.join("stages");
    let stages = fs::canonicalize(&stages).context("no retained stages for this workspace")?;
    let candidate = stages.join(stage);
    let metadata =
        fs::symlink_metadata(&candidate).with_context(|| format!("unknown stage {stage}"))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "invalid stage {stage}"
    );
    let candidate = fs::canonicalize(&candidate)?;
    ensure!(
        candidate.starts_with(&stages),
        "stage escapes project state"
    );
    for child in ["baseline", "workspace"] {
        let metadata = fs::symlink_metadata(candidate.join(child))?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "stage {stage} has invalid {child} directory"
        );
    }
    Ok(candidate)
}

fn validate_stage_id(stage: &str) -> Result<()> {
    ensure!(
        !stage.is_empty()
            && stage
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-')
            && !stage.starts_with('-')
            && !stage.ends_with('-'),
        "invalid stage ID"
    );
    Ok(())
}

pub fn box_root_for_workspace(workspace: Option<&Path>) -> Result<PathBuf> {
    let workspace = resolve_workspace(workspace)?;
    let host_home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| host_home.join(".local/share"));
    let state_root = canonicalize_allow_missing(&absolute_path(&data_home)?.join("slopbox"))?;
    let project_id = blake3::hash(workspace.as_os_str().as_bytes()).to_hex();
    Ok(state_root.join("boxes").join(project_id.as_str()))
}

fn resolve_workspace(workspace: Option<&Path>) -> Result<PathBuf> {
    let workspace = match workspace {
        Some(path) => path.to_path_buf(),
        None => env::current_dir().context("failed to determine the current directory")?,
    };
    let workspace = fs::canonicalize(&workspace)
        .with_context(|| format!("failed to resolve workspace {}", workspace.display()))?;
    ensure!(workspace.is_dir(), "workspace is not a directory");
    Ok(workspace)
}

fn validate_workspace(paths: &Paths) -> Result<()> {
    ensure!(
        paths.workspace != Path::new("/"),
        "refusing to expose / as a workspace"
    );

    let host_home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let host_home = fs::canonicalize(&host_home)
        .with_context(|| format!("failed to resolve host home {}", host_home.display()))?;

    ensure!(
        !host_home.starts_with(&paths.workspace),
        "refusing a workspace that contains the host home"
    );
    ensure!(
        !paths.state_root.starts_with(&paths.workspace),
        "refusing a workspace that contains Slopbox state"
    );
    ensure!(
        !paths.config_root.starts_with(&paths.workspace),
        "refusing a workspace that contains Slopbox configuration"
    );
    native::validate_workspace_target(&paths.workspace)?;

    Ok(())
}

fn create_private_home(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("failed to create private home {}", path.display()))?;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure private home {}", path.display()))?;

    for relative in [".cache", ".config", ".local/share", ".local/state"] {
        builder
            .create(path.join(relative))
            .with_context(|| format!("failed to initialize private home directory {relative}"))?;
    }

    Ok(())
}

fn load_config(config_root: &Path) -> Result<SlopboxConfig> {
    let path = config_root.join("config.toml");
    if !path.is_file() {
        return Ok(SlopboxConfig::default());
    }
    let contents = read_text(&path, "Slopbox configuration")?;
    toml::from_str(&contents)
        .with_context(|| format!("invalid Slopbox configuration {}", path.display()))
}

fn matches_workspace(binding: Option<&Path>, home: &Path, workspace: &Path) -> Result<bool> {
    match binding {
        Some(path) => Ok(canonicalize_allow_missing(&expand_host_home(path, home)?)? == workspace),
        None => Ok(true),
    }
}

fn workspace_http_routes<'a>(
    config: &'a SlopboxConfig,
    workspace: &Path,
) -> Result<Vec<&'a HttpRouteConfig>> {
    let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
    let (selection, _) = access::select(&config.defaults, &config.workspaces, &home, workspace)?;
    let mut routes = Vec::new();
    let mut names = HashSet::new();
    let mut direct: Vec<url::Url> = Vec::new();
    for route in &config.http_routes {
        let selected = match &selection.accounts {
            Some(accounts) => accounts.contains(&route.name),
            None => route.workspace.is_some(),
        };
        if !selected || !matches_workspace(route.workspace.as_deref(), &home, workspace)? {
            continue;
        }
        ensure!(
            names.insert(route.name.clone()),
            "duplicate authenticated HTTP route {}",
            route.name
        );
        let upstream =
            crate::gateway::validate_http_route(&route.name, &route.upstream, &route.methods)?;
        if route.direct || route.proxy {
            for other in &direct {
                let left = upstream.path().trim_end_matches('/');
                let right = other.path().trim_end_matches('/');
                let overlap = left == right
                    || left
                        .strip_prefix(right)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                    || right
                        .strip_prefix(left)
                        .is_some_and(|suffix| suffix.starts_with('/'));
                ensure!(
                    upstream.origin() != other.origin() || !overlap,
                    "direct account routes overlap for {}",
                    upstream.origin().ascii_serialization()
                );
            }
            direct.push(upstream);
        }
        routes.push(route);
    }
    if let Some(accounts) = &selection.accounts {
        ensure!(
            accounts.iter().all(|name| names.contains(name)),
            "selected account is unknown or outside its workspace binding"
        );
    }
    Ok(routes)
}

fn configured_http_routes(
    config: &SlopboxConfig,
    workspace: &Path,
) -> Result<Vec<AuthenticatedHttpRoute>> {
    let mut resolved_secrets: HashMap<String, Arc<str>> = HashMap::new();
    let mut routes = Vec::new();
    for route in workspace_http_routes(config, workspace)? {
        let (secret_name, authentication) = match &route.authentication {
            HttpAuthenticationConfig::Basic { username, secret } => {
                (secret, ("basic", Some(username.as_str())))
            }
            HttpAuthenticationConfig::Bearer { secret } => (secret, ("bearer", None)),
            HttpAuthenticationConfig::Token { secret } => (secret, ("token", None)),
        };
        let secret = if let Some(secret) = resolved_secrets.get(secret_name) {
            Arc::clone(secret)
        } else {
            let source = config
                .secrets
                .get(secret_name)
                .with_context(|| format!("unknown secret {secret_name}"))?;
            let value = match source {
                SecretConfig::Sops { file, key } => {
                    let file = if file.is_absolute() {
                        file.clone()
                    } else {
                        workspace.join(file)
                    };
                    let file = fs::canonicalize(&file).with_context(|| {
                        format!("failed to resolve SOPS file {}", file.display())
                    })?;
                    crate::secret::from_sops(
                        &file,
                        key,
                        #[cfg(target_os = "macos")]
                        workspace,
                    )?
                }
                SecretConfig::Environment { variable } => {
                    crate::secret::from_environment(variable)?
                }
                SecretConfig::Command { argv } => crate::secret::from_command(
                    argv,
                    #[cfg(target_os = "macos")]
                    workspace,
                )?,
            };
            let value: Arc<str> = Arc::from(value);
            resolved_secrets.insert(secret_name.clone(), Arc::clone(&value));
            value
        };
        let authentication = match authentication {
            ("basic", Some(username)) => HttpAuthentication::Basic {
                username: username.to_owned(),
                secret,
            },
            ("bearer", None) => HttpAuthentication::Bearer { secret },
            ("token", None) => HttpAuthentication::Token { secret },
            _ => unreachable!(),
        };
        routes.push(
            AuthenticatedHttpRoute::new(
                route.name.clone(),
                route.upstream.clone(),
                route.methods.clone(),
                authentication,
                route.allow_private_addresses,
                route.direct,
            )?
            .with_proxy(route.proxy),
        );
    }
    Ok(routes)
}

fn load_project_config(workspace: &Path) -> Result<ProjectConfig> {
    let path = workspace.join(".slopbox.toml");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProjectConfig::default());
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        !metadata.file_type().is_symlink() && metadata.is_file(),
        "project Slopbox configuration must be a regular file"
    );
    let contents = read_text(&path, "project Slopbox configuration")?;
    toml::from_str(&contents)
        .with_context(|| format!("invalid project Slopbox configuration {}", path.display()))
}

fn broker_environment(brokers: &BrokerConnections) -> Vec<(OsString, OsString)> {
    let mut values = Vec::new();
    if let Some(endpoint) = &brokers.general {
        let port = endpoint.port;
        let proxy = OsString::from(format!("http://127.0.0.1:{port}"));
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            values.push((OsString::from(name), proxy.clone()));
        }
        values.push((
            OsString::from("SLOPBOX_PROXY_PORT"),
            OsString::from(port.to_string()),
        ));
    }
    if let Some(endpoint) = &brokers.model {
        values.push((
            OsString::from("SLOPBOX_MODEL_PROXY_PORT"),
            OsString::from(endpoint.port.to_string()),
        ));
    }
    if let Some(endpoint) = &brokers.authenticated_http {
        values.push((
            OsString::from("SLOPBOX_AUTHENTICATED_HTTP_PROXY_PORT"),
            OsString::from(endpoint.port.to_string()),
        ));
        values.push((
            OsString::from("SLOPBOX_AUTHENTICATED_HTTP_BASE_URL"),
            OsString::from(format!("http://127.0.0.1:{}", endpoint.port)),
        ));
    }

    values
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(env::current_dir()
        .context("failed to determine the current directory")?
        .join(path))
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf> {
    let mut cursor = path;
    let mut missing = Vec::new();

    while !cursor.exists() {
        let name = cursor
            .file_name()
            .with_context(|| format!("cannot resolve path {}", path.display()))?;
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .with_context(|| format!("cannot resolve path {}", path.display()))?;
    }

    let mut resolved = fs::canonicalize(cursor)
        .with_context(|| format!("failed to resolve path {}", cursor.display()))?;
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn print_command(command: &Command) {
    eprint!("{:?}", command.get_program());
    for argument in command.get_args() {
        eprint!(" {:?}", argument);
    }
    eprintln!();
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_access_defaults_apply_to_new_workspaces_without_resolving_secrets() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let config: SlopboxConfig = toml::from_str(
            r#"
[defaults]
git_identity = "agent"
accounts = ["forge"]
[[git.identities]]
id = "agent"
name = "Agent"
email = "agent@example.test"
signing_key_fingerprint = "SHA256:fixture"
[[http_routes]]
name = "forge"
upstream = "https://forge.example.test/api"
methods = ["GET", "POST"]
authentication = { type = "bearer", secret = "unavailable" }
[secrets.unavailable]
source = "command"
argv = ["must-not-execute"]
"#,
        )
        .unwrap();
        for workspace in [&first, &second] {
            let workspace = workspace.canonicalize().unwrap();
            assert_eq!(
                configured_git_identity(&config, &workspace)
                    .unwrap()
                    .unwrap()
                    .email,
                "agent@example.test"
            );
            let routes = workspace_http_routes(&config, &workspace).unwrap();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].name, "forge");
            assert!(!workspace.join(".slopbox.toml").exists());
        }
        let mut dormant = config;
        dormant.defaults = access::Selection::default();
        assert!(configured_git_identity(&dormant, &first).unwrap().is_none());
        assert!(workspace_http_routes(&dormant, &first).unwrap().is_empty());
    }

    #[test]
    fn workspace_rules_can_remove_default_identity_and_account_authority() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let config: SlopboxConfig = toml::from_str(&format!(
            r#"
[defaults]
git_identity = "unavailable"
accounts = ["unavailable"]
[[workspaces]]
paths = ["{}"]
git_identity = false
accounts = []
"#,
            workspace.display()
        ))
        .unwrap();
        assert!(
            configured_git_identity(&config, &workspace)
                .unwrap()
                .is_none()
        );
        assert!(
            workspace_http_routes(&config, &workspace)
                .unwrap()
                .is_empty()
        );
        assert!(configured_git_identity(&config, workspace.parent().unwrap()).is_err());
        assert!(workspace_http_routes(&config, workspace.parent().unwrap()).is_err());
        for text in [
            "[defaults]\naccounts = ['forge']",
            "[[workspaces]]\npaths = ['/']\naccounts = ['forge']",
        ] {
            assert!(toml::from_str::<ProjectConfig>(text).is_err());
        }
    }

    #[test]
    fn global_selection_cannot_expand_an_existing_workspace_binding() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().canonicalize().unwrap();
        let config: SlopboxConfig = toml::from_str(&format!(
            r#"
[defaults]
accounts = ["scoped"]
[[http_routes]]
name = "scoped"
workspace = "{}"
upstream = "https://forge.example.test/repo"
methods = ["GET"]
authentication = {{ type = "bearer", secret = "missing" }}
"#,
            workspace.display()
        ))
        .unwrap();
        assert_eq!(workspace_http_routes(&config, &workspace).unwrap().len(), 1);
        assert!(workspace_http_routes(&config, workspace.parent().unwrap()).is_err());
    }

    #[test]
    fn overlapping_direct_accounts_fail_before_secret_resolution() {
        let workspace = tempfile::tempdir().unwrap();
        let workspace = workspace.path().canonicalize().unwrap();
        let text = format!(
            r#"
[secrets.github]
source = "command"
argv = ["gh", "auth", "token"]
[[http_routes]]
name = "github"
workspace = "{}"
upstream = "https://api.github.com"
methods = ["GET"]
direct = true
authentication = {{ type = "bearer", secret = "github" }}
[[http_routes]]
name = "repository"
workspace = "{}"
upstream = "https://api.github.com/repos/owner/repository"
methods = ["POST"]
direct = true
authentication = {{ type = "bearer", secret = "github" }}
"#,
            workspace.display(),
            workspace.display()
        );
        let config: SlopboxConfig = toml::from_str(&text).unwrap();
        let error = workspace_http_routes(&config, &workspace).err().unwrap();
        assert!(error.to_string().contains("direct account routes overlap"));
        let config: SlopboxConfig =
            toml::from_str(&text.replace("direct = true", "direct = false")).unwrap();
        assert_eq!(workspace_http_routes(&config, &workspace).unwrap().len(), 2);
    }

    #[test]
    fn github_example_uses_workspace_bound_routes_and_a_host_command() {
        let config: SlopboxConfig = toml::from_str(include_str!("../docs/github.toml")).unwrap();
        assert_eq!(config.git.identities.len(), 1);
        let identity = &config.git.identities[0];
        assert_eq!(identity.name, "agent");
        assert_eq!(identity.email, "agent@example.com");
        assert_eq!(
            identity.workspace.as_deref(),
            Some(Path::new("~/Projects/example"))
        );
        assert_eq!(config.http_routes.len(), 2);
        assert!(!config.http_routes[0].direct);
        assert!(config.http_routes[1].direct);
        assert!(
            config
                .http_routes
                .iter()
                .all(|route| route.workspace == identity.workspace)
        );
        assert!(matches!(
            config.secrets.get("github"),
            Some(SecretConfig::Command { argv }) if argv == &["gh", "auth", "token", "--hostname", "github.com", "--user", "ACCOUNT"]
        ));
    }

    #[test]
    fn accepts_only_generated_stage_ids() {
        assert!(validate_stage_id("123456-42").is_ok());
        for invalid in ["", "../stage", "/tmp/stage", "stage", "-1", "1-"] {
            assert!(validate_stage_id(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn copies_a_staged_workspace_without_following_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "fixture").unwrap();
        symlink("file", source.join("link")).unwrap();

        copy_workspace_tree(&source, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("file")).unwrap(),
            "fixture"
        );
        assert_eq!(
            fs::read_link(destination.join("link")).unwrap(),
            Path::new("file")
        );
    }

    #[test]
    fn git_rewrites_are_workspace_bound_and_do_not_resolve_secrets() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        let config: SlopboxConfig = toml::from_str(&format!(
            r#"
[[http_routes]]
name = "first"
workspace = "{}"
upstream = "https://forge.example/first.git"
methods = ["GET", "POST"]
authentication = {{ type = "bearer", secret = "missing" }}
git_urls = ["git@forge.example:first.git", "git@forge.example:first.git"]

[[http_routes]]
name = "second"
workspace = "{}"
upstream = "https://forge.example/second.git"
methods = ["GET", "POST"]
authentication = {{ type = "bearer", secret = "missing" }}
git_urls = ["git@forge.example:second.git"]
"#,
            first.display(),
            second.display()
        ))
        .unwrap();
        assert_eq!(
            configured_git_rewrites(&config, &first).unwrap(),
            [GitUrlRewrite {
                url: "git@forge.example:first.git".into(),
                route: "first".into(),
            }]
        );
        assert_eq!(
            configured_git_rewrites(&config, &second).unwrap(),
            [GitUrlRewrite {
                url: "git@forge.example:second.git".into(),
                route: "second".into(),
            }]
        );
        assert!(
            configured_git_rewrites(&config, root.path())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn refuses_ambiguous_git_rewrites() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let config: SlopboxConfig = toml::from_str(&format!(
            r#"
[[http_routes]]
name = "first"
workspace = "{0}"
upstream = "https://forge.example/repo.git"
methods = ["GET"]
authentication = {{ type = "bearer", secret = "missing" }}
git_urls = ["git@forge.example:repo.git"]

[[http_routes]]
name = "second"
workspace = "{0}"
upstream = "https://forge.example/repo.git"
methods = ["GET"]
authentication = {{ type = "bearer", secret = "missing" }}
git_urls = ["git@forge.example:repo.git"]
"#,
            root.display()
        ))
        .unwrap();
        let error = configured_git_rewrites(&config, &root).unwrap_err();
        assert!(error.to_string().contains("multiple authenticated routes"));
    }

    #[test]
    fn project_policy_cannot_expand_the_host_policy() {
        let host = SlopboxConfig {
            policy: PolicyRequest {
                harness: Some(HarnessMode::None),
                ..PolicyRequest::default()
            },
            ..SlopboxConfig::default()
        };
        let project = ProjectConfig {
            policy: PolicyRequest {
                harness: Some(HarnessMode::Trusted),
                ..PolicyRequest::default()
            },
        };

        let (_, policy) = select_policy(&host, &project, None, false, None);
        assert_eq!(policy.harness, HarnessMode::None);
    }

    #[test]
    fn saved_launch_policy_remains_a_ceiling_when_project_policy_changes() {
        let host = SlopboxConfig::default();
        let saved_policy = PolicyRequest {
            workspace: Some(WorkspaceMode::Staged),
            network: Some(NetworkMode::None),
            harness: Some(HarnessMode::None),
            credentials: Some(crate::policy::CredentialMode::None),
            ..PolicyRequest::default()
        }
        .apply(Profile::Developer.policy());
        let saved = LaunchConfig {
            workspace: PathBuf::from("/workspace"),
            agent: Agent::Pi,
            policy: saved_policy,
        };
        let (_, effective) =
            select_policy(&host, &ProjectConfig::default(), None, false, Some(&saved));
        assert_eq!(effective, saved_policy);
        let narrower_host = SlopboxConfig {
            policy: PolicyRequest {
                workspace: Some(WorkspaceMode::ReadOnly),
                ..PolicyRequest::default()
            },
            ..SlopboxConfig::default()
        };
        let (_, effective) = select_policy(
            &narrower_host,
            &ProjectConfig::default(),
            None,
            false,
            Some(&saved),
        );
        assert_eq!(effective.workspace, WorkspaceMode::ReadOnly);
        assert_eq!(effective.network, NetworkMode::None);
        let (_, reset) = select_policy(&host, &ProjectConfig::default(), None, false, None);
        assert_eq!(reset, Profile::Developer.policy());
    }

    #[test]
    fn refuses_symlinked_project_configuration() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(
            root.path().join("policy.toml"),
            "[policy]\nharness = \"none\"\n",
        )
        .unwrap();
        symlink(
            root.path().join("policy.toml"),
            workspace.join(".slopbox.toml"),
        )
        .unwrap();

        assert!(load_project_config(&workspace).is_err());
    }
}
