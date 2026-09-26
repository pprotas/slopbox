use std::path::Path;

use anyhow::{Context, Result, ensure};
use url::Url;

use crate::git_signing::GitSigningIdentity;

#[derive(Debug, Eq, PartialEq)]
pub struct GitUrlRewrite {
    pub url: String,
    pub route: String,
}

impl GitUrlRewrite {
    pub fn new(url: String, route: String) -> Result<Self> {
        ensure!(
            !url.is_empty() && !url.chars().any(char::is_whitespace),
            "Git rewrite URL must be a complete repository URL without whitespace"
        );
        let parsed = if url.contains("://") {
            Url::parse(&url)
        } else {
            let separator = url
                .find("]:")
                .map(|index| index + 1)
                .or_else(|| url.find(':'));
            let separator =
                separator.context("Git rewrite URL must use HTTP, HTTPS, SSH, or Git transport")?;
            let (authority, path) = url.split_at(separator);
            ensure!(
                !authority.contains('/') && !path[1..].starts_with(':'),
                "invalid Git SSH repository URL"
            );
            Url::parse(&format!("ssh://{authority}/{}", &path[1..]))
        }
        .context("invalid Git rewrite URL")?;
        ensure!(
            matches!(parsed.scheme(), "http" | "https" | "ssh" | "git")
                && parsed.host_str().is_some()
                && parsed.password().is_none()
                && (parsed.scheme() == "ssh" || parsed.username().is_empty())
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && !parsed.path().trim_matches('/').is_empty(),
            "Git rewrite URL must name a repository, without passwords, queries, or fragments"
        );
        ensure!(
            !url.chars().any(char::is_control),
            "Git rewrite URL contains a control character"
        );
        Ok(Self { url, route })
    }
}

pub fn render(
    signing: Option<(&GitSigningIdentity, &str, &Path)>,
    rewrites: &[GitUrlRewrite],
    broker_base: &str,
) -> String {
    let mut config = String::new();
    if let Some((identity, public_key, helper)) = signing {
        config.push_str(&format!(
            "[user]\n\tname = {}\n\temail = {}\n\tuseConfigOnly = true\n\tsigningKey = {}\n[gpg]\n\tformat = ssh\n[gpg \"ssh\"]\n\tprogram = {}\n[commit]\n\tgpgSign = true\n",
            quote(&identity.name),
            quote(&identity.email),
            quote(&format!("key::{public_key}")),
            quote(&helper.to_string_lossy()),
        ));
    }
    for rewrite in rewrites {
        let destination = format!(
            "{}/{}{}",
            broker_base.trim_end_matches('/'),
            rewrite.route,
            if rewrite.url.ends_with('/') { "/" } else { "" }
        );
        config.push_str(&format!(
            "[url {}]\n\tinsteadOf = {}\n",
            quote(&destination),
            quote(&rewrite.url),
        ));
    }
    config
}

fn quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, cwd: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"))
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .current_dir(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn accepts_complete_repository_urls() {
        for url in [
            "https://forge.example/org/repo.git",
            "http://forge.example:8080/org/repo",
            "git://forge.example/org/repo.git",
            "ssh://bot@alias:2222/org/repo.git",
            "ssh://git@[2001:db8::1]/org/repo.git",
            "git@forge.example:org/repo.git",
            "alias:org/repo.git",
            "git@[2001:db8::1]:org/repo.git",
            "https://forge.example/org/repo.git/",
        ] {
            assert!(
                GitUrlRewrite::new(url.into(), "repo".into()).is_ok(),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_broad_local_and_credential_bearing_urls() {
        for url in [
            "",
            "https://forge.example/",
            "git@forge.example:",
            "ssh://git@forge.example/",
            "https://user:password@forge.example/repo",
            "ssh://git:password@forge.example/repo",
            "file:///tmp/repo",
            "/tmp/repo",
            "../repo",
            "ext::command",
            "https://forge.example/repo?query",
            "https://forge.example/repo#fragment",
            "https://forge.example/repo\ninsteadOf=anything",
            "https://forge.example/repo\0",
        ] {
            assert!(
                GitUrlRewrite::new(url.into(), "repo".into()).is_err(),
                "{url:?}"
            );
        }
    }

    #[test]
    fn git_parses_quoted_signing_identity_and_rewrites_together() {
        let root = tempfile::tempdir().unwrap();
        let identity = GitSigningIdentity {
            name: "Bot \"quoted\" # ; \\".into(),
            email: "bot@example.com".into(),
            fingerprint: "SHA256:test".into(),
        };
        let url = "ssh://git@forge.example/org/repo.git";
        let helper = Path::new("/private/var/tmp/slopbox-501/native/run-example/git-sign");
        let config = render(
            Some((&identity, "ssh-ed25519 public-key", helper)),
            &[GitUrlRewrite::new(url.into(), "repo".into()).unwrap()],
            "http://127.0.0.1:49123",
        );
        fs::write(root.path().join("gitconfig"), config).unwrap();
        assert_eq!(
            git(root.path(), root.path(), &["config", "--get", "user.name"]),
            identity.name
        );
        assert_eq!(
            git(
                root.path(),
                root.path(),
                &["config", "--get", "user.signingKey"]
            ),
            "key::ssh-ed25519 public-key"
        );
        assert_eq!(
            git(
                root.path(),
                root.path(),
                &["config", "--get", "gpg.ssh.program"]
            ),
            helper.to_str().unwrap()
        );
        assert_eq!(
            git(root.path(), root.path(), &["ls-remote", "--get-url", url]),
            "http://127.0.0.1:49123/repo"
        );
    }

    #[test]
    fn ordinary_git_commands_use_rewrites_without_changing_repository_config() {
        let root = tempfile::tempdir().unwrap();
        let source = "git@forge.example:org/repo.git";
        let push_url = "https://forge.example/org/repo.git";
        let base = Url::from_directory_path(root.path()).unwrap();
        let rewrites = [
            GitUrlRewrite::new(source.into(), "repository".into()).unwrap(),
            GitUrlRewrite::new(push_url.into(), "repository".into()).unwrap(),
        ];
        fs::write(
            root.path().join("gitconfig"),
            render(None, &rewrites, base.as_str()),
        )
        .unwrap();
        git(
            root.path(),
            root.path(),
            &["init", "--bare", "--initial-branch=main", "repository"],
        );
        git(
            root.path(),
            root.path(),
            &["init", "--initial-branch=main", "work"],
        );
        let work = root.path().join("work");
        git(root.path(), &work, &["remote", "add", "origin", source]);
        git(
            root.path(),
            &work,
            &["remote", "set-url", "--push", "origin", push_url],
        );
        git(
            root.path(),
            &work,
            &["config", "branch.main.remote", "origin"],
        );
        git(
            root.path(),
            &work,
            &["config", "branch.main.merge", "refs/heads/main"],
        );
        let before = fs::read(work.join(".git/config")).unwrap();
        git(
            root.path(),
            &work,
            &["commit", "--allow-empty", "-m", "initial"],
        );
        git(root.path(), &work, &["push"]);

        git(root.path(), root.path(), &["clone", "repository", "other"]);
        let other = root.path().join("other");
        git(
            root.path(),
            &other,
            &["commit", "--allow-empty", "-m", "upstream"],
        );
        git(root.path(), &other, &["push"]);
        let upstream = git(root.path(), &other, &["rev-parse", "HEAD"]);
        git(root.path(), &work, &["fetch", "origin"]);
        git(root.path(), &work, &["pull", "--ff-only"]);
        assert_eq!(git(root.path(), &work, &["rev-parse", "HEAD"]), upstream);
        git(
            root.path(),
            &work,
            &["commit", "--allow-empty", "-m", "local"],
        );
        git(root.path(), &work, &["push"]);
        assert_eq!(
            git(
                root.path(),
                &root.path().join("repository"),
                &["rev-parse", "main"]
            ),
            git(root.path(), &work, &["rev-parse", "HEAD"]),
        );
        assert_eq!(fs::read(work.join(".git/config")).unwrap(), before);
        assert_eq!(
            git(
                root.path(),
                &work,
                &["config", "--local", "--get", "remote.origin.url"]
            ),
            source
        );
        let unrelated = "ssh://git@forge.example/org/other.git";
        assert_eq!(
            git(root.path(), &work, &["ls-remote", "--get-url", unrelated]),
            unrelated
        );
    }
}
