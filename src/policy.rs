use std::fmt;

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    #[default]
    Developer,
    Contained,
    Adversarial,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceMode {
    ReadOnly,
    Staged,
    Live,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    None,
    Allowlist,
    Observe,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeMode {
    Image,
    Project,
    Host,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessMode {
    None,
    Data,
    Trusted,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum PersistenceMode {
    Ephemeral,
    Project,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialMode {
    None,
    Brokered,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Native,
    MicroVm,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    pub workspace: Option<WorkspaceMode>,
    pub network: Option<NetworkMode>,
    pub runtime: Option<RuntimeMode>,
    pub harness: Option<HarnessMode>,
    pub persistence: Option<PersistenceMode>,
    pub credentials: Option<CredentialMode>,
    pub backend: Option<Backend>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub workspace: WorkspaceMode,
    pub network: NetworkMode,
    pub runtime: RuntimeMode,
    pub harness: HarnessMode,
    pub persistence: PersistenceMode,
    pub credentials: CredentialMode,
    pub backend: Backend,
}

macro_rules! display_enum {
    ($type:ty, { $($variant:path => $name:literal),+ $(,)? }) => {
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(match self {
                    $($variant => $name),+
                })
            }
        }
    };
}

display_enum!(WorkspaceMode, {
    WorkspaceMode::ReadOnly => "read-only",
    WorkspaceMode::Staged => "staged",
    WorkspaceMode::Live => "live",
});
display_enum!(NetworkMode, {
    NetworkMode::None => "none",
    NetworkMode::Allowlist => "allowlist",
    NetworkMode::Observe => "observe",
});
display_enum!(RuntimeMode, {
    RuntimeMode::Image => "image",
    RuntimeMode::Project => "project",
    RuntimeMode::Host => "host",
});
display_enum!(HarnessMode, {
    HarnessMode::None => "none",
    HarnessMode::Data => "data",
    HarnessMode::Trusted => "trusted",
});
display_enum!(PersistenceMode, {
    PersistenceMode::Ephemeral => "ephemeral",
    PersistenceMode::Project => "project",
});
display_enum!(CredentialMode, {
    CredentialMode::None => "none",
    CredentialMode::Brokered => "brokered",
});
display_enum!(Backend, {
    Backend::Native => "native",
    Backend::MicroVm => "micro-vm",
});

impl fmt::Display for Profile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Developer => "developer",
            Self::Contained => "contained",
            Self::Adversarial => "adversarial",
        })
    }
}

impl Profile {
    pub fn policy(self) -> Policy {
        match self {
            Self::Developer => Policy {
                workspace: WorkspaceMode::Live,
                network: NetworkMode::Allowlist,
                runtime: RuntimeMode::Host,
                harness: HarnessMode::None,
                persistence: PersistenceMode::Project,
                credentials: CredentialMode::Brokered,
                backend: Backend::Native,
            },
            Self::Contained => Policy {
                workspace: WorkspaceMode::Staged,
                network: NetworkMode::Allowlist,
                runtime: RuntimeMode::Project,
                harness: HarnessMode::None,
                persistence: PersistenceMode::Project,
                credentials: CredentialMode::Brokered,
                backend: Backend::Native,
            },
            Self::Adversarial => Policy {
                workspace: WorkspaceMode::Staged,
                network: NetworkMode::Allowlist,
                runtime: RuntimeMode::Image,
                harness: HarnessMode::None,
                persistence: PersistenceMode::Ephemeral,
                credentials: CredentialMode::Brokered,
                backend: Backend::MicroVm,
            },
        }
    }
}

impl PolicyRequest {
    pub fn apply(self, base: Policy) -> Policy {
        base.restrict(Policy {
            workspace: self.workspace.unwrap_or(base.workspace),
            network: self.network.unwrap_or(base.network),
            runtime: self.runtime.unwrap_or(base.runtime),
            harness: self.harness.unwrap_or(base.harness),
            persistence: self.persistence.unwrap_or(base.persistence),
            credentials: self.credentials.unwrap_or(base.credentials),
            backend: self.backend.unwrap_or(base.backend),
        })
    }
}

impl Policy {
    pub fn restrict(self, requested: Self) -> Self {
        Self {
            workspace: self.workspace.min(requested.workspace),
            network: self.network.min(requested.network),
            runtime: self.runtime.min(requested.runtime),
            harness: self.harness.min(requested.harness),
            persistence: self.persistence.min(requested.persistence),
            credentials: self.credentials.min(requested.credentials),
            backend: if self.backend == Backend::MicroVm || requested.backend == Backend::MicroVm {
                Backend::MicroVm
            } else {
                Backend::Native
            },
        }
    }

    pub fn ensure_implemented(self, profile: Profile) -> Result<()> {
        if self.backend != Backend::Native {
            bail!(
                "effective {profile} policy requires backend={}; only native is implemented",
                self.backend
            );
        }
        if self.runtime == RuntimeMode::Image {
            bail!("effective {profile} policy requires runtime=image, which is not implemented");
        }
        if self.persistence != PersistenceMode::Project {
            bail!(
                "effective {profile} policy requires persistence={}; only project is implemented",
                self.persistence
            );
        }
        if self.network == NetworkMode::Observe {
            bail!("effective {profile} policy requires network=observe, which is not implemented");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_policy_can_only_reduce_authority() {
        let developer = Profile::Developer.policy();
        let adversarial = Profile::Adversarial.policy();
        let effective = PolicyRequest {
            workspace: Some(adversarial.workspace),
            network: Some(adversarial.network),
            runtime: Some(adversarial.runtime),
            harness: Some(adversarial.harness),
            persistence: Some(adversarial.persistence),
            credentials: Some(adversarial.credentials),
            backend: Some(adversarial.backend),
        }
        .apply(developer);

        assert_eq!(effective.workspace, WorkspaceMode::Staged);
        assert_eq!(effective.network, NetworkMode::Allowlist);
        assert_eq!(effective.runtime, RuntimeMode::Image);
        assert_eq!(effective.harness, HarnessMode::None);
        assert_eq!(effective.persistence, PersistenceMode::Ephemeral);
        assert_eq!(effective.credentials, CredentialMode::Brokered);
        assert_eq!(effective.backend, Backend::MicroVm);
    }

    #[test]
    fn native_developer_and_contained_policy_modes_are_implemented() {
        assert!(
            Profile::Developer
                .policy()
                .ensure_implemented(Profile::Developer)
                .is_ok()
        );
        assert!(
            PolicyRequest {
                credentials: Some(CredentialMode::None),
                ..PolicyRequest::default()
            }
            .apply(Profile::Developer.policy())
            .ensure_implemented(Profile::Developer)
            .is_ok()
        );
        assert!(
            PolicyRequest {
                network: Some(NetworkMode::None),
                ..PolicyRequest::default()
            }
            .apply(Profile::Developer.policy())
            .ensure_implemented(Profile::Developer)
            .is_ok()
        );
        assert!(
            PolicyRequest {
                harness: Some(HarnessMode::Data),
                ..PolicyRequest::default()
            }
            .apply(Profile::Developer.policy())
            .ensure_implemented(Profile::Developer)
            .is_ok()
        );
        assert!(
            PolicyRequest {
                workspace: Some(WorkspaceMode::Staged),
                ..PolicyRequest::default()
            }
            .apply(Profile::Developer.policy())
            .ensure_implemented(Profile::Developer)
            .is_ok()
        );
        assert!(
            PolicyRequest {
                workspace: Some(WorkspaceMode::ReadOnly),
                ..PolicyRequest::default()
            }
            .apply(Profile::Developer.policy())
            .ensure_implemented(Profile::Developer)
            .is_ok()
        );
        assert!(
            Profile::Contained
                .policy()
                .ensure_implemented(Profile::Contained)
                .is_ok()
        );
        assert!(
            Profile::Adversarial
                .policy()
                .ensure_implemented(Profile::Adversarial)
                .is_err()
        );
    }
}
