use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::{Context, Result, ensure};

pub(crate) fn validate(values: &BTreeMap<String, String>) -> Result<()> {
    ensure!(values.len() <= 256, "too many guest environment variables");
    ensure!(
        values
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            <= 64 * 1024,
        "guest environment exceeds 64 KiB"
    );
    for (name, value) in values {
        ensure!(
            !name.is_empty()
                && !name.starts_with(|character: char| character.is_ascii_digit())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && !value.contains('\0'),
            "invalid guest environment entry"
        );
        ensure!(
            !name.starts_with("SLOPBOX_")
                && !name.starts_with("XDG_")
                && !name.starts_with("GIT_CONFIG")
                && !matches!(
                    name.as_str(),
                    "HOME"
                        | "PATH"
                        | "TMPDIR"
                        | "USER"
                        | "LOGNAME"
                        | "SHELL"
                        | "PWD"
                        | "SSH_AUTH_SOCK"
                        | "HTTP_PROXY"
                        | "HTTPS_PROXY"
                        | "ALL_PROXY"
                        | "NO_PROXY"
                        | "http_proxy"
                        | "https_proxy"
                        | "all_proxy"
                        | "no_proxy"
                ),
            "guest environment cannot override managed variable {name}"
        );
    }
    Ok(())
}

pub(crate) fn resolve(
    values: &BTreeMap<String, String>,
    context: &BTreeMap<String, String>,
) -> Result<Vec<(OsString, OsString)>> {
    validate(values)?;
    let expanded: Vec<(OsString, OsString)> = values
        .iter()
        .map(|(name, value)| {
            let mut expanded = String::new();
            let mut remaining = value.as_str();
            while let Some(start) = remaining.find("${") {
                expanded.push_str(&remaining[..start]);
                remaining = &remaining[start + 2..];
                let end = remaining
                    .find('}')
                    .context("unterminated guest environment reference")?;
                let variable = &remaining[..end];
                expanded.push_str(context.get(variable).with_context(|| {
                    format!("guest environment reference {variable} is unavailable")
                })?);
                remaining = &remaining[end + 1..];
                ensure!(
                    expanded.len() <= 64 * 1024,
                    "expanded guest environment value exceeds 64 KiB"
                );
            }
            expanded.push_str(remaining);
            ensure!(
                expanded.len() <= 64 * 1024,
                "expanded guest environment value exceeds 64 KiB"
            );
            Ok((name.into(), expanded.into()))
        })
        .collect::<Result<_>>()?;
    ensure!(
        expanded
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            <= 64 * 1024,
        "expanded guest environment exceeds 64 KiB"
    );
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_only_supplied_guest_context_without_shell_evaluation() {
        let context = BTreeMap::from([
            ("HOME".into(), "/private-home".into()),
            (
                "SLOPBOX_AUTHENTICATED_HTTP_BASE_URL".into(),
                "http://127.0.0.1:1234".into(),
            ),
        ]);
        let values = BTreeMap::from([
            (
                "CLIENT_URL".into(),
                "${SLOPBOX_AUTHENTICATED_HTTP_BASE_URL}/account".into(),
            ),
            ("CLIENT_STATE".into(), "${HOME}/state".into()),
            ("LITERAL".into(), "$(false) $HOME".into()),
        ]);
        assert_eq!(
            resolve(&values, &context).unwrap(),
            vec![
                ("CLIENT_STATE".into(), "/private-home/state".into()),
                ("CLIENT_URL".into(), "http://127.0.0.1:1234/account".into()),
                ("LITERAL".into(), "$(false) $HOME".into()),
            ]
        );
        for value in ["${PATH}", "${UNDECLARED}", "${HOME"] {
            assert!(resolve(&BTreeMap::from([("CLIENT".into(), value.into())]), &context).is_err());
        }
    }

    #[test]
    fn rejects_managed_names_and_malformed_entries() {
        for name in [
            "HOME",
            "PATH",
            "TMPDIR",
            "SLOPBOX_MODEL_PROXY_PORT",
            "GIT_CONFIG_GLOBAL",
            "XDG_CONFIG_HOME",
            "SSH_AUTH_SOCK",
            "HTTPS_PROXY",
            "bad-name",
            "1NAME",
            "",
        ] {
            assert!(
                validate(&BTreeMap::from([(name.into(), "value".into())])).is_err(),
                "{name}"
            );
        }
        assert!(validate(&BTreeMap::from([("CLIENT".into(), "bad\0value".into())])).is_err());
    }
}
