use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::gateway::{ApprovalScope, Event, session_is_active};

const STORE: &str = "network-rules.json";
const MAX_STORE_SIZE: u64 = 16 * 1024 * 1024;
static RULE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub scope: ApprovalScope,
    pub session_id: Option<String>,
    pub created_at_ms: u64,
    pub revoked_at_ms: Option<u64>,
    pub request_id: String,
}

impl Rule {
    pub fn state(&self, box_root: &Path) -> Result<&'static str> {
        if self.revoked_at_ms.is_some() {
            Ok("revoked")
        } else if let Some(session) = &self.session_id {
            if session_is_active(&box_root.join("sessions").join(session))? {
                Ok("active")
            } else {
                Ok("expired")
            }
        } else {
            Ok("active")
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    rules: Vec<Rule>,
}

pub fn destination(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub fn describe_rule(rule: &Rule) -> String {
    let scope = match rule.scope {
        ApprovalScope::Session => "session",
        ApprovalScope::Project => "project",
    };
    crate::launch::terminal_text(&format!(
        "{} {scope} {} session={} created_ms={} request={} revoked_ms={}",
        rule.id,
        destination(&rule.host, rule.port),
        rule.session_id.as_deref().unwrap_or("-"),
        rule.created_at_ms,
        rule.request_id,
        rule.revoked_at_ms
            .map(|time| time.to_string())
            .unwrap_or_else(|| "-".into())
    ))
}

pub fn print_events(box_root: &Path, workspace: &Path, follow: bool, json: bool) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    let mut output = std::io::stdout().lock();
    loop {
        for event in crate::gateway::all_events(box_root)? {
            if !seen.insert(event.id.clone()) {
                continue;
            }
            let session = crate::gateway::request_session(&event.id)?;
            let active = session_is_active(&box_root.join("sessions").join(session))?;
            let line = if json {
                serde_json::to_string(&serde_json::json!({
                    "project": workspace, "session_id": session,
                    "session_active": active, "event": event
                }))?
            } else {
                format!(
                    "{event} [project={} session={session} active={active} created_ms={}]",
                    crate::launch::terminal_text(&workspace.to_string_lossy()),
                    event.created_at_ms
                )
            };
            if let Err(error) = writeln!(output, "{line}").and_then(|()| output.flush()) {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(());
                }
                return Err(error.into());
            }
        }
        if !follow {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn normalize_destination(host: &str) -> Result<String> {
    let host = host.trim().trim_end_matches('.');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip.to_string());
    }
    let parsed = url::Host::parse(host).context("invalid destination hostname")?;
    if let url::Host::Domain(domain) = &parsed {
        ensure!(
            domain.len() <= 253
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                }),
            "destination must be an exact hostname or IP address, not a pattern"
        );
    }
    Ok(match parsed {
        url::Host::Ipv6(ip) => ip.to_string(),
        other => other.to_string(),
    })
}

pub fn list_rules(box_root: &Path) -> Result<Vec<Rule>> {
    Ok(load(box_root)?.rules)
}

pub fn is_allowed(box_root: &Path, session: &str, host: &str, port: u16) -> Result<bool> {
    let host = normalize_destination(host)?;
    ensure!(port != 0, "destination port must not be zero");
    for rule in load(box_root)?.rules {
        if rule.host == host
            && rule.port == port
            && rule.revoked_at_ms.is_none()
            && (rule.session_id.is_none() || rule.session_id.as_deref() == Some(session))
            && rule.state(box_root)? == "active"
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn approvable_destination(event: &Event) -> Result<String> {
    ensure!(
        event.reason == "no matching allow rule",
        "this denial is not approvable: {}; approving cannot fix DNS or reserved-address failures",
        crate::launch::terminal_text(&event.reason)
    );
    let host = normalize_destination(&event.host)?;
    ensure!(event.port != 0, "destination port must not be zero");
    if let Ok(ip) = host.parse::<IpAddr>() {
        ensure!(
            crate::http::is_public_ip(ip),
            "reserved addresses cannot be approved"
        );
    }
    Ok(host)
}

pub fn grant(box_root: &Path, event: &Event, scope: ApprovalScope) -> Result<Rule> {
    let host = approvable_destination(event)?;
    let request_session = crate::gateway::request_session(&event.id)?;
    let session_id = match scope {
        ApprovalScope::Project => None,
        ApprovalScope::Session => Some(request_session.to_owned()),
    };
    let _lock = lock(box_root)?;
    let mut store = load(box_root)?;
    if let Some(session) = &session_id {
        ensure!(
            session_is_active(&box_root.join("sessions").join(session))?,
            "the request's session is no longer running"
        );
    }
    if let Some(rule) = store.rules.iter().find(|rule| {
        rule.host == host
            && rule.port == event.port
            && rule.scope == scope
            && rule.session_id == session_id
            && rule.revoked_at_ms.is_none()
    }) {
        return Ok(rule.clone());
    }
    let seed = format!(
        "{:?}:{}:{}:{}:{}",
        SystemTime::now(),
        std::process::id(),
        RULE_COUNTER.fetch_add(1, Ordering::Relaxed),
        event.id,
        store.rules.len()
    );
    let rule = Rule {
        id: format!("rule-{}", &blake3::hash(seed.as_bytes()).to_hex()[..24]),
        host,
        port: event.port,
        scope,
        session_id,
        created_at_ms: now_ms(),
        revoked_at_ms: None,
        request_id: event.id.clone(),
    };
    store.rules.push(rule.clone());
    save(box_root, &store)?;
    Ok(rule)
}

pub fn revoke(box_root: &Path, id: &str) -> Result<Rule> {
    validate_rule_id(id)?;
    ensure!(box_root.is_dir(), "no network approvals for this project");
    let _lock = lock(box_root)?;
    let mut store = load(box_root)?;
    let rule = store
        .rules
        .iter_mut()
        .find(|rule| rule.id == id)
        .with_context(|| format!("unknown rule ID {id}"))?;
    rule.revoked_at_ms.get_or_insert_with(now_ms);
    let revoked = rule.clone();
    save(box_root, &store)?;
    Ok(revoked)
}

fn lock(box_root: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(box_root.join("network-rules.lock"))?;
    ensure!(
        file.metadata()?.is_file(),
        "network rule lock must be a regular file"
    );
    file.lock()?;
    Ok(file)
}

fn load(box_root: &Path) -> Result<Store> {
    let Some(contents) = read_optional_bytes(&box_root.join(STORE))? else {
        return Ok(Store::default());
    };
    let store: Store = serde_json::from_slice(&contents).context("invalid network rule store")?;
    let mut ids = std::collections::HashSet::new();
    for rule in &store.rules {
        validate_rule_id(&rule.id)?;
        ensure!(ids.insert(&rule.id), "duplicate network rule ID");
        ensure!(
            rule.port != 0 && normalize_destination(&rule.host)? == rule.host,
            "invalid network rule destination"
        );
        ensure!(
            (rule.scope == ApprovalScope::Session) == rule.session_id.is_some(),
            "invalid network rule scope"
        );
        let request_session = crate::gateway::request_session(&rule.request_id)?;
        if let Some(session) = &rule.session_id {
            ensure!(
                session == request_session,
                "rule does not belong to the request's session"
            );
        }
    }
    Ok(store)
}

fn validate_rule_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 29
            && id.starts_with("rule-")
            && id[5..].bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid rule ID"
    );
    Ok(())
}

fn save(box_root: &Path, store: &Store) -> Result<()> {
    let contents = serde_json::to_vec(store)?;
    ensure!(
        contents.len() as u64 <= MAX_STORE_SIZE,
        "network rule store is too large"
    );
    let mut file = tempfile::NamedTempFile::new_in(box_root)?;
    file.write_all(&contents)?;
    file.as_file().sync_all()?;
    file.persist(box_root.join(STORE))
        .context("failed to save network rules")?;
    File::open(box_root)?.sync_all()?;
    Ok(())
}

pub(crate) fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    ensure!(
        file.metadata()?.is_file(),
        "network state must be a regular file: {}",
        path.display()
    );
    let mut contents = Vec::new();
    file.take(MAX_STORE_SIZE + 1).read_to_end(&mut contents)?;
    ensure!(
        contents.len() as u64 <= MAX_STORE_SIZE,
        "network state file is too large: {}",
        path.display()
    );
    Ok(Some(contents))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn denial(host: &str) -> Event {
        Event {
            id: "req-session-1".into(),
            method: "CONNECT".into(),
            host: host.into(),
            port: 443,
            reason: "no matching allow rule".into(),
            created_at_ms: 1,
        }
    }

    #[test]
    fn rules_are_exact_idempotent_revocable_and_not_reactivated_by_stale_ids() {
        let root = tempfile::tempdir().unwrap();
        assert!(list_rules(root.path()).unwrap().is_empty());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        let event = denial("EXAMPLE.COM.");
        let rule = grant(root.path(), &event, ApprovalScope::Project).unwrap();
        assert_eq!(rule.host, "example.com");
        assert_eq!(
            grant(root.path(), &event, ApprovalScope::Project)
                .unwrap()
                .id,
            rule.id
        );
        assert!(is_allowed(root.path(), "future", "example.com", 443).unwrap());
        assert!(!is_allowed(root.path(), "future", "other.example.com", 443).unwrap());
        assert!(!is_allowed(root.path(), "future", "example.com", 80).unwrap());
        let revoked = revoke(root.path(), &rule.id).unwrap();
        assert!(revoked.revoked_at_ms.is_some());
        assert_eq!(
            revoke(root.path(), &rule.id).unwrap().revoked_at_ms,
            revoked.revoked_at_ms
        );
        assert!(!is_allowed(root.path(), "future", "example.com", 443).unwrap());
        let replacement = grant(root.path(), &event, ApprovalScope::Project).unwrap();
        assert_ne!(replacement.id, rule.id);
        revoke(root.path(), &rule.id).unwrap();
        assert!(is_allowed(root.path(), "future", "example.com", 443).unwrap());
        assert_eq!(list_rules(root.path()).unwrap().len(), 2);
        assert_eq!(
            fs::metadata(root.path().join(STORE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn session_expiry_and_overlapping_rules_have_independent_authority() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("sessions/session");
        fs::create_dir_all(&session).unwrap();
        let active = File::create(session.join("active.lock")).unwrap();
        active.lock().unwrap();
        let event = denial("example.com");
        let scoped = grant(root.path(), &event, ApprovalScope::Session).unwrap();
        assert!(is_allowed(root.path(), "session", "example.com", 443).unwrap());
        assert!(!is_allowed(root.path(), "other", "example.com", 443).unwrap());
        let project = grant(root.path(), &event, ApprovalScope::Project).unwrap();
        revoke(root.path(), &project.id).unwrap();
        assert!(is_allowed(root.path(), "session", "example.com", 443).unwrap());
        assert!(!is_allowed(root.path(), "other", "example.com", 443).unwrap());
        drop(active);
        let observer = File::open(session.join("active.lock")).unwrap();
        observer.lock_shared().unwrap();
        assert_eq!(scoped.state(root.path()).unwrap(), "expired");
        assert!(!is_allowed(root.path(), "session", "example.com", 443).unwrap());
        assert!(grant(root.path(), &event, ApprovalScope::Session).is_err());
    }

    #[test]
    fn concurrent_grant_and_revoke_do_not_lose_updates() {
        let root = tempfile::tempdir().unwrap();
        let rule = grant(
            root.path(),
            &denial("first.example"),
            ApprovalScope::Project,
        )
        .unwrap();
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                revoke(root.path(), &rule.id).unwrap();
            });
            scope.spawn(|| {
                barrier.wait();
                grant(
                    root.path(),
                    &denial("second.example"),
                    ApprovalScope::Project,
                )
                .unwrap();
            });
            barrier.wait();
        });
        assert!(!is_allowed(root.path(), "future", "first.example", 443).unwrap());
        assert!(is_allowed(root.path(), "future", "second.example", 443).unwrap());
    }

    #[test]
    fn rejects_patterns_reserved_literals_and_nonapprovable_events() {
        let root = tempfile::tempdir().unwrap();
        for host in [
            "*.example.com",
            "127.0.0.1",
            "127.1",
            "[::1]",
            "::ffff:127.0.0.1",
            "10.1.2.3",
            "evil\nexample.com",
        ] {
            assert!(
                grant(root.path(), &denial(host), ApprovalScope::Project).is_err(),
                "{host}"
            );
        }
        let mut event = denial("example.com");
        event.reason = "destination resolves to a reserved address".into();
        assert!(grant(root.path(), &event, ApprovalScope::Project).is_err());
        assert!(!root.path().join(STORE).exists());
        assert_eq!(
            normalize_destination("[2001:4860:4860::8888]").unwrap(),
            "2001:4860:4860::8888"
        );
        assert_eq!(
            normalize_destination("BÜCHER.example.").unwrap(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn corrupt_or_redirected_control_files_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let rule = grant(root.path(), &denial("example.com"), ApprovalScope::Project).unwrap();
        fs::write(root.path().join(STORE), "broken JSON").unwrap();
        assert!(list_rules(root.path()).is_err());
        assert!(is_allowed(root.path(), "future", "example.com", 443).is_err());
        assert!(revoke(root.path(), &rule.id).is_err());
        fs::remove_file(root.path().join(STORE)).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "do not change").unwrap();
        symlink(&outside, root.path().join(STORE)).unwrap();
        assert!(list_rules(root.path()).is_err());
        assert!(grant(root.path(), &denial("example.com"), ApprovalScope::Project).is_err());
        fs::remove_file(root.path().join(STORE)).unwrap();
        fs::remove_file(root.path().join("network-rules.lock")).unwrap();
        symlink(&outside, root.path().join("network-rules.lock")).unwrap();
        assert!(grant(root.path(), &denial("example.com"), ApprovalScope::Project).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "do not change");
    }
}
