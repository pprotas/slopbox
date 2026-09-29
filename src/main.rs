mod approval;
mod backend;
mod command;
mod fs_util;
mod gateway;
mod git_config;
mod git_signing;
mod github;
mod guest_environment;
mod http;
mod launch;
mod network;
mod policy;
mod provider;
mod secret;
mod session;
mod terminal;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use policy::Profile;

#[derive(Debug, Parser)]
#[command(
    name = "slopbox",
    version,
    about,
    after_help = "Run a command with slopbox run -- COMMAND. Configure default_command for bare slopbox; pass additional arguments after --.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Enable the experimental Ctrl-] host approval view (requires a host TTY).
    #[arg(long)]
    approval_view: bool,

    /// Arguments appended to the configured default command (after --).
    #[arg(last = true, allow_hyphen_values = true, value_name = "ARGUMENTS")]
    agent_arguments: Vec<OsString>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum DevEnvironment {
    /// Use the default flake dev shell when flake.nix exists.
    #[default]
    Auto,
    /// Require and use the default flake dev shell.
    Flake,
    /// Do not activate a development environment.
    None,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum ToolNetwork {
    /// Disable general egress; configured authenticated account routes remain available.
    #[default]
    None,
    /// Expose approved general egress, but not authenticated model routes.
    General,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage host-side provider credentials.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },

    /// Run a command with the configured workspace, runtime, and host resources.
    Run {
        /// Workspace to expose. Defaults to the current directory.
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Security policy preset. Developer and contained have native implementations.
        #[arg(long, value_enum)]
        profile: Option<Profile>,

        /// Development environment to activate.
        #[arg(long, value_enum, default_value_t)]
        dev_env: DevEnvironment,

        /// Enable experimental Ctrl-] host approvals (requires a host TTY).
        #[arg(long)]
        approval_view: bool,

        /// Print the selected backend invocation without running it.
        #[arg(long)]
        dry_run: bool,

        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Run a command in an inner sandbox without model-provider authority.
    ToolRun {
        /// Network capability available to the tool.
        #[arg(long, value_enum, default_value_t)]
        network: ToolNetwork,

        /// Command and arguments to run.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    /// Show the effective sandbox policy without starting a sandbox.
    Policy {
        /// Workspace whose policy should be shown. Defaults to the current directory.
        #[arg(long)]
        workspace: Option<PathBuf>,

        /// Override the configured policy preset.
        #[arg(long, value_enum)]
        profile: Option<Profile>,
    },

    /// Explain launch access without resolving credentials or starting a sandbox.
    Status {
        #[arg(long)]
        workspace: Option<PathBuf>,

        #[arg(long, value_enum)]
        profile: Option<Profile>,

        /// Include policy axes, mount targets, and selected runtime paths.
        #[arg(long)]
        verbose: bool,
    },

    /// Diagnose launch prerequisites without executing project code.
    Doctor {
        #[arg(long)]
        workspace: Option<PathBuf>,

        #[arg(long, value_enum)]
        profile: Option<Profile>,
    },

    /// Inspect and manage retained staged workspaces.
    Stage {
        #[command(subcommand)]
        command: StageCommand,
    },

    /// Inspect denied destinations and manage revocable network approvals on the host.
    Network {
        #[arg(long, global = true)]
        workspace: Option<PathBuf>,

        #[command(subcommand)]
        command: NetworkCommand,
    },

    /// Show network requests denied in this sandbox.
    Denials,

    #[cfg(target_os = "macos")]
    #[command(name = "__macos-command-worker", hide = true)]
    MacosCommandWorker {
        socket: PathBuf,
        profile: String,
        workspace: PathBuf,
        home: PathBuf,
        environment: PathBuf,
    },

    #[cfg(target_os = "macos")]
    #[command(name = "__macos-stdio", hide = true)]
    MacosStdio {
        descriptor: i32,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },

    #[cfg(target_os = "macos")]
    #[command(name = "__macos-relay-worker", hide = true)]
    MacosRelayWorker {
        socket: PathBuf,
        general: String,
        model: String,
        account: String,
    },

    #[cfg(target_os = "macos")]
    #[command(name = "__macos-recover", hide = true)]
    MacosRecover { directory: PathBuf },

    #[command(name = "__sandbox-init", hide = true)]
    SandboxInit {
        #[arg(long, requires = "general_proxy_port")]
        general_gateway_socket: Option<PathBuf>,

        #[arg(long, requires = "general_gateway_socket")]
        general_proxy_port: Option<u16>,

        #[arg(long, requires = "model_proxy_port")]
        model_gateway_socket: Option<PathBuf>,

        #[arg(long, requires = "model_gateway_socket")]
        model_proxy_port: Option<u16>,

        #[arg(long, requires = "authenticated_http_proxy_port")]
        authenticated_http_gateway_socket: Option<PathBuf>,

        #[arg(long, requires = "authenticated_http_gateway_socket")]
        authenticated_http_proxy_port: Option<u16>,

        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },

    #[command(name = "__git-sign", hide = true)]
    GitSign {
        #[cfg(target_os = "macos")]
        #[arg(long)]
        socket: PathBuf,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
    },

    #[command(name = "__tool-init", hide = true)]
    ToolInit {
        #[arg(long, requires = "general_proxy_port")]
        general_gateway_socket: Option<PathBuf>,

        #[arg(long, requires = "general_gateway_socket")]
        general_proxy_port: Option<u16>,

        #[arg(long, requires = "authenticated_http_proxy_port")]
        authenticated_http_gateway_socket: Option<PathBuf>,

        #[arg(long, requires = "authenticated_http_gateway_socket")]
        authenticated_http_proxy_port: Option<u16>,

        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<OsString>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthProvider {
    #[value(name = "openai-codex")]
    OpenAiCodex,
}

impl From<AuthProvider> for provider::AuthProvider {
    fn from(provider: AuthProvider) -> Self {
        match provider {
            AuthProvider::OpenAiCodex => Self::OpenAiCodex,
        }
    }
}

#[derive(Debug, Subcommand)]
enum NetworkCommand {
    /// Show retained denials with project and session context.
    Events {
        /// Poll for new request IDs until interrupted. Nothing is retried automatically.
        #[arg(long)]
        follow: bool,
        /// Emit one JSON object per event.
        #[arg(long)]
        json: bool,
    },
    /// List approvals, including revoked and expired session rules.
    Approvals {
        #[arg(long)]
        json: bool,
    },
    /// Approve an exact destination; defaults to the request's live session.
    Approve {
        request_id: String,
        #[arg(long, conflicts_with = "project")]
        session: bool,
        /// Persist across this project's sessions.
        #[arg(long)]
        project: bool,
    },
    /// Revoke a rule for subsequent authorization checks; existing tunnels remain open.
    Revoke { rule_id: String },
}

#[derive(Debug, Subcommand)]
enum StageCommand {
    /// List retained stages for a workspace.
    List {
        #[arg(long)]
        workspace: Option<PathBuf>,
    },

    /// Show changes made inside a retained stage.
    Diff {
        stage: String,

        #[arg(long)]
        workspace: Option<PathBuf>,
    },

    /// Apply all staged changes when the host workspace still matches the baseline.
    Apply {
        stage: String,

        #[arg(long)]
        workspace: Option<PathBuf>,
    },

    /// Permanently remove a retained stage.
    Discard {
        stage: String,

        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Authenticate a provider using a host-side OAuth flow.
    Login { provider: AuthProvider },

    /// List configured providers without exposing credentials.
    List,

    /// Show whether a provider is configured.
    Status { provider: AuthProvider },

    /// Remove a stored provider credential.
    Logout { provider: AuthProvider },
}

fn main() {
    if let Err(error) = try_main() {
        eprintln!("slopbox: {error:#}");
        process::exit(1);
    }
}

fn try_main() -> Result<()> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> Result<()> {
    let command = match cli.command {
        Some(command) => command,
        None => {
            exit_with_status(session::launch(cli.agent_arguments, cli.approval_view)?);
            return Ok(());
        }
    };
    match command {
        Command::Auth { command } => match command {
            AuthCommand::Login { provider } => {
                provider::AuthProvider::from(provider).login()?;
            }
            AuthCommand::List => {
                for provider in provider::AuthProvider::configured()? {
                    println!("{}", provider.id());
                }
            }
            AuthCommand::Status { provider } => {
                let provider = provider::AuthProvider::from(provider);
                let configured = provider.status()?;
                println!(
                    "{}: {}",
                    provider.id(),
                    if configured {
                        "configured"
                    } else {
                        "not configured"
                    }
                );
            }
            AuthCommand::Logout { provider } => {
                let provider = provider::AuthProvider::from(provider);
                let removed = provider.logout()?;
                println!(
                    "{}: {}",
                    provider.id(),
                    if removed {
                        "logged out"
                    } else {
                        "not configured"
                    }
                );
            }
        },
        Command::Run {
            workspace,
            profile,
            dev_env,
            approval_view,
            dry_run,
            command,
        } => {
            let status = session::run(session::RunOptions {
                workspace,
                profile,
                dev_env,
                command,
                dry_run,
                approval_view,
                launch_config: None,
            })?;

            exit_with_status(status);
        }
        Command::ToolRun { network, command } => {
            exit_with_status(backend::native::tool::run(network, &command)?);
        }
        Command::Policy { workspace, profile } => {
            let (profile, policy) = session::effective_policy(workspace.as_deref(), profile)?;
            let supported = policy
                .ensure_implemented(profile)
                .and_then(|()| backend::ensure_supported());
            #[cfg(target_os = "macos")]
            let supported = supported.and_then(|()| backend::native::validate_policy(policy));
            println!("profile: {profile}");
            println!(
                "status: {}",
                if supported.is_ok() {
                    "implemented"
                } else {
                    "planned"
                }
            );
            println!("backend: {}", policy.backend);
            println!("workspace: {}", policy.workspace);
            println!("network: {}", policy.network);
            println!("runtime: {}", policy.runtime);
            println!("harness: {}", policy.harness);
            println!("persistence: {}", policy.persistence);
            println!("credentials: {}", policy.credentials);
            for route in session::effective_http_route_names(workspace.as_deref())? {
                println!("authenticated-http-route: {route}");
            }
            for rewrite in session::effective_git_rewrites(workspace.as_deref())? {
                println!("git-url: {} -> {}", rewrite.url, rewrite.route);
            }
            if let Some((name, email, fingerprint)) =
                session::effective_git_identity(workspace.as_deref())?
            {
                println!("git-identity: {name} <{email}>");
                println!("git-signing-key: {fingerprint}");
            }
        }
        Command::Doctor { workspace, profile } => {
            let (report, passed) = session::doctor(workspace.as_deref(), profile)?;
            print!("{report}");
            anyhow::ensure!(
                passed,
                "doctor found launch blockers; fix the reported failures and retry"
            );
        }
        Command::Status {
            workspace,
            profile,
            verbose,
        } => {
            print!(
                "{}",
                session::status(workspace.as_deref(), profile, verbose)?
            );
        }
        Command::Stage { command } => match command {
            StageCommand::List { workspace } => {
                for stage in session::list_stages(workspace.as_deref())? {
                    println!("{}\t{}", stage.id, stage.workspace.display());
                }
            }
            StageCommand::Diff { stage, workspace } => {
                session::diff_stage(workspace.as_deref(), &stage)?;
            }
            StageCommand::Apply { stage, workspace } => {
                session::apply_stage(workspace.as_deref(), &stage)?;
                println!("applied {stage}");
            }
            StageCommand::Discard { stage, workspace } => {
                session::discard_stage(workspace.as_deref(), &stage)?;
                println!("discarded {stage}");
            }
        },
        Command::Network { workspace, command } => {
            let workspace = std::fs::canonicalize(match workspace {
                Some(path) => path,
                None => std::env::current_dir()?,
            })?;
            let box_root = session::box_root_for_workspace(Some(&workspace))?;
            match command {
                NetworkCommand::Events { follow, json } => {
                    network::print_events(&box_root, &workspace, follow, json)?;
                }
                NetworkCommand::Approvals { json } => {
                    let rules = network::list_rules(&box_root)?;
                    if json {
                        let mut rows = Vec::new();
                        for rule in rules {
                            rows.push(serde_json::json!({"project": workspace, "state": rule.state(&box_root)?, "rule": rule}));
                        }
                        println!("{}", serde_json::to_string(&rows)?);
                    } else {
                        for rule in rules {
                            println!(
                                "{} {}",
                                rule.state(&box_root)?,
                                network::describe_rule(&rule)
                            );
                        }
                    }
                }
                NetworkCommand::Approve {
                    request_id,
                    project,
                    session: _,
                } => {
                    let scope = if project {
                        gateway::ApprovalScope::Project
                    } else {
                        gateway::ApprovalScope::Session
                    };
                    let rule = gateway::approve(&box_root, &request_id, scope)?;
                    println!(
                        "approved {}; retry the operation",
                        network::describe_rule(&rule)
                    );
                }
                NetworkCommand::Revoke { rule_id } => {
                    let rule = network::revoke(&box_root, &rule_id)?;
                    println!("revoked {}", network::describe_rule(&rule));
                    println!(
                        "Existing tunnels and in-flight requests are not cancelled. Other matching rules may still permit this destination."
                    );
                }
            }
        }
        Command::Denials => {
            let port = std::env::var("SLOPBOX_PROXY_PORT")
                .context("not running inside a network-enabled Slopbox sandbox")?
                .parse()?;
            backend::native::init::print_denials(port)?;
        }
        #[cfg(target_os = "macos")]
        Command::MacosCommandWorker {
            socket,
            profile,
            workspace,
            home,
            environment,
        } => {
            backend::native::engine::stdio::worker(
                &socket,
                &profile,
                &workspace,
                &home,
                &environment,
            )?;
        }
        #[cfg(target_os = "macos")]
        Command::MacosStdio {
            descriptor,
            arguments,
        } => {
            process::exit(backend::native::engine::stdio::bridge(
                descriptor, &arguments,
            )?);
        }
        #[cfg(target_os = "macos")]
        Command::MacosRelayWorker {
            socket,
            general,
            model,
            account,
        } => {
            backend::native::engine::relay::worker(
                &socket,
                [general, model, account]
                    .map(|path| (!path.is_empty()).then(|| PathBuf::from(path))),
            )?;
        }
        #[cfg(target_os = "macos")]
        Command::MacosRecover { directory } => {
            backend::native::engine::session::Session::recover(&directory)?;
        }
        Command::SandboxInit {
            general_gateway_socket,
            general_proxy_port,
            model_gateway_socket,
            model_proxy_port,
            authenticated_http_gateway_socket,
            authenticated_http_proxy_port,
            command,
        } => {
            exit_with_status(backend::native::init::run(
                general_gateway_socket.as_deref(),
                general_proxy_port,
                model_gateway_socket.as_deref(),
                model_proxy_port,
                authenticated_http_gateway_socket.as_deref(),
                authenticated_http_proxy_port,
                &command,
            )?);
        }
        Command::GitSign {
            arguments,
            #[cfg(target_os = "macos")]
            socket,
        } => {
            #[cfg(target_os = "linux")]
            let socket = PathBuf::from("/run/slopbox-host/git-signing/gateway.sock");
            git_signing::run_helper(&arguments, &socket)?;
        }
        Command::ToolInit {
            general_gateway_socket,
            general_proxy_port,
            authenticated_http_gateway_socket,
            authenticated_http_proxy_port,
            command,
        } => {
            exit_with_status(backend::native::init::run_tool(
                general_gateway_socket.as_deref(),
                general_proxy_port,
                authenticated_http_gateway_socket.as_deref(),
                authenticated_http_proxy_port,
                &command,
            )?);
        }
    }

    Ok(())
}

fn exit_with_status(status: std::process::ExitStatus) {
    if status.success() {
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)),
        );
    }

    #[cfg(not(unix))]
    process::exit(status.code().unwrap_or(1));
}

#[cfg(all(test, target_os = "macos"))]
#[path = "../tests/native/cli.rs"]
mod native_cli_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_launch_accepts_extra_arguments_only_after_separator() {
        let cli = Cli::try_parse_from(["slopbox"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.agent_arguments.is_empty());
        let cli = Cli::try_parse_from(["slopbox", "--", "--mode", "rpc", "--no-session"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(
            cli.agent_arguments,
            ["--mode", "rpc", "--no-session"].map(OsString::from)
        );
        assert!(Cli::try_parse_from(["slopbox", "--mode", "rpc"]).is_err());
        assert!(Cli::try_parse_from(["slopbox", "status", "--", "--mode", "rpc"]).is_err());
    }

    #[test]
    fn network_commands_default_to_session_scope_and_accept_workspace_after_subcommand() {
        let cli = Cli::try_parse_from([
            "slopbox",
            "network",
            "approve",
            "req-session-1",
            "--workspace",
            "/project",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Network {
                workspace: Some(_),
                command: NetworkCommand::Approve {
                    project: false,
                    session: false,
                    ..
                }
            })
        ));
        assert!(
            Cli::try_parse_from([
                "slopbox",
                "network",
                "approve",
                "req-session-1",
                "--project",
                "--session"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from(["slopbox", "network", "events", "--json", "--follow"]).is_ok()
        );
    }

    #[test]
    fn removed_pi_setup_options_are_rejected() {
        assert!(Cli::try_parse_from(["slopbox", "init", "--yes"]).is_err());
        assert!(Cli::try_parse_from(["slopbox", "status", "--no-host-pi-resources"]).is_err());
    }
}
