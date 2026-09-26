use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use super::{Boundary, DeveloperTools, Host, executable};
use crate::backend::{PreparedDevEnvironment, nix};
use crate::command::shell_quote;

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
struct Environment {
    variables: BTreeMap<String, Variable>,
    #[serde(rename = "bashFunctions")]
    functions: BTreeMap<String, String>,
    #[serde(default, rename = "structuredAttrs")]
    structured: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "value")]
enum Variable {
    #[serde(rename = "exported")]
    Exported(String),
    #[serde(rename = "var")]
    Local(String),
    #[serde(rename = "array")]
    Array(Vec<String>),
    #[serde(rename = "associative")]
    Associative(BTreeMap<String, String>),
}

impl Environment {
    fn string(&self, name: &str) -> Result<&str> {
        match self.variables.get(name) {
            Some(Variable::Exported(value) | Variable::Local(value)) => Ok(value),
            _ => anyhow::bail!("Nix environment requires string variable {name}"),
        }
    }

    fn render(&self, path: &str, bash: &Path) -> Result<String> {
        ensure!(
            self.structured.is_none(),
            "native Nix environments with structured attributes are not supported"
        );
        let mut script = String::new();
        for (name, variable) in &self.variables {
            ensure!(identifier(name), "invalid Nix environment variable name");
            if protected(name) || name == "PATH" {
                continue;
            }
            match variable {
                Variable::Exported(value) | Variable::Local(value) => {
                    script.push_str(&format!("{name}={}\n", shell_quote(value)));
                    if matches!(variable, Variable::Exported(_)) {
                        script.push_str(&format!("export {name}\n"));
                    }
                }
                Variable::Array(values) => {
                    script.push_str(&format!(
                        "declare -a {name}=({})\n",
                        values
                            .iter()
                            .map(|value| shell_quote(value))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
                Variable::Associative(values) => {
                    script.push_str(&format!(
                        "declare -A {name}=({})\n",
                        values
                            .iter()
                            .map(|(key, value)| format!(
                                "[{}]={}",
                                shell_quote(key),
                                shell_quote(value)
                            ))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
            }
        }
        for (name, body) in &self.functions {
            ensure!(identifier(name), "invalid Nix environment function name");
            script.push_str(&format!("{name} () {{\n{body}\n}}\n"));
        }
        script.push_str(&format!(
            "export PATH={}${{PATH:+:$PATH}}\nexport SHELL={}\nexport IN_NIX_SHELL=impure\nexport NIX_BUILD_TOP=\"$TMPDIR\" TMP=\"$TMPDIR\" TEMP=\"$TMPDIR\" TEMPDIR=\"$TMPDIR\"\neval \"${{shellHook:-}}\"\nslopbox_hook_status=$?\nif [ \"$slopbox_hook_status\" -ne 0 ]; then exit \"$slopbox_hook_status\"; fi\nexec \"$@\"\n",
            shell_quote(path), shell_quote(bash.to_str().context("invalid Nix Bash path")?)
        ));
        ensure!(!script.contains('\0'), "Nix environment contains NUL bytes");
        Ok(script)
    }
}

fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn protected(name: &str) -> bool {
    [
        "BASH", "DYLD_", "LD_", "SLOPBOX_", "GIT_", "GH_", "SSH_", "XDG_",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || matches!(
            name,
            "HOME"
                | "USER"
                | "LOGNAME"
                | "PWD"
                | "OLDPWD"
                | "SHLVL"
                | "PPID"
                | "UID"
                | "EUID"
                | "_"
                | "IFS"
                | "ENV"
                | "SHELLOPTS"
                | "SHELL"
                | "TERM"
                | "COLORTERM"
                | "TZ"
                | "TMP"
                | "TMPDIR"
                | "TEMP"
                | "TEMPDIR"
                | "CARGO_HOME"
                | "RUSTUP_HOME"
                | "NIX_BUILD_TOP"
                | "NIX_ENFORCE_PURITY"
                | "NIX_LOG_FD"
                | "NIX_REMOTE"
                | "NIX_CONFIG"
                | "NIX_USER_CONF_FILES"
                | "NIX_PATH"
                | "NIX_PROFILES"
                | "SSL_CERT_FILE"
                | "NIX_SSL_CERT_FILE"
                | "GITHUB_TOKEN"
                | "GITHUB_ENTERPRISE_TOKEN"
                | "HTTP_PROXY"
                | "HTTPS_PROXY"
                | "ALL_PROXY"
                | "NO_PROXY"
                | "http_proxy"
                | "https_proxy"
                | "all_proxy"
                | "no_proxy"
        )
}

fn selected_path(path: &Path, store_paths: &[PathBuf]) -> Result<PathBuf> {
    ensure!(
        store_paths.contains(&nix::store_root(path)?),
        "Nix tool path is outside the selected closure: {}",
        path.display()
    );
    let canonical = path
        .canonicalize()
        .with_context(|| format!("resolve Nix tool path {}", path.display()))?;
    ensure!(
        store_paths.contains(&nix::store_root(&canonical)?),
        "Nix tool path is outside the selected closure: {}",
        path.display()
    );
    Ok(canonical)
}

fn tool_path(value: &str, store_paths: &[PathBuf]) -> Result<String> {
    let mut paths = Vec::new();
    for path in std::env::split_paths(value) {
        // Do not inherit builder placeholders, workspace paths or host profiles.
        if nix::store_root(&path).is_err() {
            continue;
        }
        let canonical = selected_path(&path, store_paths)?;
        ensure!(canonical.is_dir(), "Nix PATH entry is not a directory");
        if !paths.contains(&canonical) {
            paths.push(canonical);
        }
    }
    ensure!(
        !paths.is_empty(),
        "Nix environment has no selected tool PATH"
    );
    std::env::join_paths(paths)?
        .into_string()
        .map_err(|_| anyhow::anyhow!("non-UTF-8 Nix PATH"))
}

pub(super) fn prepare(session: &Path, workspace: &Path) -> Result<PreparedDevEnvironment> {
    let nix_executable = crate::command::trusted_executable("nix", workspace)?;
    let json = session.join("dev-env.json");
    let profile = nix::realize(
        &mut nix::command(&nix_executable)?,
        workspace,
        &session.join("dev-profile"),
        &json,
        true,
    )?;
    let output = nix::command(&nix_executable)?
        .args(["path-info", "--recursive"])
        .arg(&profile)
        .output()
        .context("query native project closure")?;
    ensure!(
        output.status.success(),
        "Nix closure query failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let store_paths = nix::closure_paths(&output.stdout)?;
    ensure!(
        store_paths.contains(&nix::store_root(&profile)?),
        "Nix closure is missing the development profile"
    );
    let mut text = String::new();
    fs::File::open(&json)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= 4 * 1024 * 1024,
        "Nix development environment exceeds 4 MiB"
    );
    let environment: Environment =
        serde_json::from_str(&text).context("parse Nix development environment")?;
    let bash = selected_path(Path::new(environment.string("BASH")?), &store_paths)?;
    ensure!(executable(&bash), "selected Nix Bash is not executable");
    let path = tool_path(environment.string("PATH")?, &store_paths)?;
    let script = session.join("dev-env.sh");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&script)?
        .write_all(environment.render(&path, &bash)?.as_bytes())?;
    Ok(PreparedDevEnvironment {
        script,
        profile,
        bash,
        store_paths,
    })
}

pub(super) fn tools(
    environment: &PreparedDevEnvironment,
    workspace: &Path,
) -> Result<DeveloperTools> {
    let boundary = Boundary::new(&Host::read(workspace)?)?;
    for path in &environment.store_paths {
        boundary.check(path)?;
    }
    ensure!(
        environment
            .store_paths
            .contains(&nix::store_root(&environment.profile)?),
        "Nix profile is outside the tool closure"
    );
    Ok(DeveloperTools {
        read_roots: environment.store_paths.clone(),
        path: vec!["/usr/bin".into(), "/bin".into()],
        ..Default::default()
    })
}
