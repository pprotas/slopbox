pub(crate) mod pi;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::ToolNetwork;
use crate::policy::Profile;

pub(crate) struct PrepareContext<'a> {
    pub session_dir: &'a Path,
    pub private_home: &'a Path,
    pub host_path: &'a OsStr,
    pub profile: Profile,
    pub general_network: bool,
    pub tool_network: ToolNetwork,
    pub model_providers: &'a [crate::provider::Kind],
    pub dry_run: bool,
    #[cfg(target_os = "macos")]
    pub native: Option<&'a crate::backend::macos::Config>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum MountAccess {
    ReadOnly,
    ReadWrite,
    TemporaryOverlay,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Mount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub access: MountAccess,
}

#[derive(Default)]
pub(crate) struct PreparedHarness {
    pub executable: Option<PathBuf>,
    #[cfg_attr(target_os = "macos", expect(dead_code))]
    pub directories: Vec<PathBuf>,
    pub mounts: Vec<Mount>,
    pub environment: Vec<(OsString, OsString)>,
}
