pub(crate) mod nix;

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;

#[cfg(target_os = "linux")]
pub(crate) use linux as native;
#[cfg(target_os = "macos")]
pub(crate) use macos as native;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::harness::PreparedHarness;

pub(crate) fn ensure_supported() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        macos::ensure_supported()
    }
}

pub(crate) struct Workspace<'a> {
    pub source: &'a Path,
    pub target: &'a Path,
    pub writable: bool,
}

pub(crate) struct ExecutionPlan<'a> {
    pub workspace: Workspace<'a>,
    pub private_home: &'a Path,
    pub tool_home: &'a Path,
    #[cfg(target_os = "macos")]
    pub tool_cache: &'a Path,
    pub session_dir: &'a Path,
    pub dev_environment: Option<&'a PreparedDevEnvironment>,
    pub runtime: &'a RuntimePlan,
    pub harness: &'a PreparedHarness,
    pub brokers: &'a BrokerConnections,
    pub environment: &'a [(OsString, OsString)],
    pub private_terminal: bool,
    pub clipboard: bool,
}

pub(crate) struct PreparedDevEnvironment {
    pub script: PathBuf,
    pub profile: PathBuf,
    #[cfg(target_os = "macos")]
    pub bash: PathBuf,
    #[cfg(target_os = "macos")]
    pub store_paths: Vec<PathBuf>,
}

#[derive(Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeSelection {
    #[serde(default)]
    pub executables: Vec<PathBuf>,
    #[serde(default)]
    pub dependency_roots: Vec<PathBuf>,
}

pub(crate) struct RuntimePlan {
    #[cfg(target_os = "macos")]
    pub native: macos::runtime::NativeRuntime,
    #[cfg(target_os = "linux")]
    pub read_only_paths: Vec<PathBuf>,
    #[cfg(target_os = "linux")]
    pub system_links: Vec<(PathBuf, PathBuf)>,
    pub path: OsString,
}

pub(crate) struct ProxyEndpoint {
    pub socket_dir: PathBuf,
    pub port: u16,
}

#[derive(Default)]
pub(crate) struct BrokerConnections {
    pub general: Option<ProxyEndpoint>,
    pub model: Option<ProxyEndpoint>,
    pub authenticated_http: Option<ProxyEndpoint>,
    pub git_signing: Option<PathBuf>,
}
