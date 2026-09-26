use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context as _, Result, bail, ensure};

use crate::gateway::{self, ApprovalScope, Event};
use crate::launch::terminal_text;
use crate::network::{self, Rule};

pub struct Context<'a> {
    pub workspace: &'a Path,
    pub box_root: &'a Path,
    pub session: Option<&'a str>,
    pub general_network: bool,
}

enum Entry {
    Request(Event),
    Rule(Rule),
}

enum Action {
    Approve(Event, ApprovalScope),
    Revoke(Rule),
}

struct Confirmation {
    action: Action,
    code: String,
}

pub struct View {
    entries: Vec<Entry>,
    page: usize,
    confirmation: Option<Confirmation>,
    message: String,
}

impl View {
    pub fn failed(error: anyhow::Error) -> Self {
        Self {
            entries: Vec::new(),
            page: 0,
            confirmation: None,
            message: format!("Cannot inspect network state: {error:#}"),
        }
    }

    pub fn open(context: &Context<'_>) -> Result<Self> {
        let mut events = context
            .session
            .map(|session| gateway::session_events(context.box_root, session))
            .transpose()?
            .unwrap_or_default();
        events.sort_by(|a, b| {
            (&a.host, a.port, &a.method, &a.id).cmp(&(&b.host, b.port, &b.method, &b.id))
        });
        let mut entries: Vec<_> = events.into_iter().map(Entry::Request).collect();
        for rule in network::list_rules(context.box_root)? {
            if (rule.session_id.is_none() || rule.session_id.as_deref() == context.session)
                && rule.state(context.box_root)? == "active"
            {
                entries.push(Entry::Rule(rule));
            }
        }
        Ok(Self {
            entries,
            page: 0,
            confirmation: None,
            message: String::new(),
        })
    }

    pub fn render(&self, context: &Context<'_>, rows: usize, columns: usize) -> String {
        if rows < 24 || columns < 80 {
            return "SLOPBOX HOST APPROVALS\r\nResize to at least 80x24, or type q and Enter to return.\r\n> ".into();
        }
        let mut lines = vec![
            "SLOPBOX HOST APPROVALS".to_owned(),
            format!("Project: {}", context.workspace.display()),
            format!("Session: {}", context.session.unwrap_or("none")),
        ];
        if let Some(confirmation) = &self.confirmation {
            match &confirmation.action {
                Action::Approve(event, scope) => {
                    lines.push(format!(
                        "Approve {}",
                        network::destination(&event.host, event.port)
                    ));
                    lines.push(format!(
                        "Scope: {}",
                        match scope {
                            ApprovalScope::Session => "this session only",
                            ApprovalScope::Project => "PROJECT: persists across sessions",
                        }
                    ));
                    lines.push(format!(
                        "Request: {} ({}; created_ms={})",
                        event.id, event.method, event.created_at_ms
                    ));
                    lines.push("Permits the destination, not a URL or safe use of it.".into());
                    lines.push("The failed operation will NOT be replayed.".into());
                }
                Action::Revoke(rule) => {
                    lines.push(format!(
                        "Revoke {}",
                        network::destination(&rule.host, rule.port)
                    ));
                    lines.push(format!("Rule: {}", rule.id));
                    lines.push(format!(
                        "Scope: {}",
                        if rule.scope == ApprovalScope::Project {
                            "PROJECT: affects all project sessions"
                        } else {
                            "this session"
                        }
                    ));
                    lines.push(
                        "Existing connections remain open; other rules may still allow access."
                            .into(),
                    );
                }
            }
            lines.push(format!(
                "Type {} and Enter to confirm; anything else cancels.",
                confirmation.code
            ));
        } else {
            lines.push(
                if context.general_network {
                    "Denial history and active rules (snapshot; refresh to update)."
                } else {
                    "General networking is disabled; approvals cannot enable it."
                }
                .into(),
            );
            let size = page_size(rows);
            let page = self.page.min(self.entries.len().saturating_sub(1) / size);
            for (index, entry) in self.entries.iter().enumerate().skip(page * size).take(size) {
                let line = match entry {
                    Entry::Request(event) => format!(
                        "{}: DENIAL {} {} ({})",
                        index + 1,
                        event.method,
                        network::destination(&event.host, event.port),
                        event.reason
                    ),
                    Entry::Rule(rule) => format!(
                        "{}: RULE {} {} {}",
                        index + 1,
                        network::destination(&rule.host, rule.port),
                        if rule.scope == ApprovalScope::Project {
                            "project"
                        } else {
                            "session"
                        },
                        rule.id
                    ),
                };
                lines.push(line);
            }
            if self.entries.is_empty() {
                lines.push("No denials or active rules for this session.".into());
            }
            lines.push(format!(
                "Page {}/{} | s N: session approval | p N: persistent project approval",
                page + 1,
                self.entries.len().saturating_sub(1) / size + 1
            ));
            lines.push("r N: revoke | n/b: next/back page | refresh | q: return to guest".into());
            lines.push(
                "No event? Check tool networking and whether the client uses the proxy.".into(),
            );
        }
        if !self.message.is_empty() {
            lines.push(self.message.clone());
        }
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let escaped = terminal_text(&line);
                if self.confirmation.is_some() && index == 3 {
                    escaped
                        .as_bytes()
                        .chunks(columns - 1)
                        .map(|part| {
                            std::str::from_utf8(part).expect("normalized destinations are ASCII")
                        })
                        .collect::<Vec<_>>()
                        .join("\r\n")
                } else {
                    clip(&escaped, columns - 1)
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n> "
    }

    pub fn cancel_input(&mut self) {
        self.confirmation = None;
        self.message = "Cancelled; no rule changed.".into();
    }

    pub fn submit(&mut self, context: &Context<'_>, input: &str, rows: usize) -> bool {
        match self.apply(context, input, rows) {
            Ok(close) => close,
            Err(error) => {
                self.confirmation = None;
                self.message = format!("Error: {error:#}");
                false
            }
        }
    }

    fn apply(&mut self, context: &Context<'_>, input: &str, rows: usize) -> Result<bool> {
        if let Some(confirmation) = self.confirmation.take() {
            ensure!(
                input == confirmation.code,
                "confirmation cancelled; no rule changed"
            );
            let message = match confirmation.action {
                Action::Approve(event, scope) => {
                    ensure!(context.general_network, "general networking is disabled");
                    ensure!(
                        Some(gateway::request_session(&event.id)?) == context.session,
                        "request is not from this session"
                    );
                    let rule = gateway::approve(context.box_root, &event.id, scope)?;
                    format!(
                        "Approved {}; retry the operation. Nothing was replayed.",
                        rule.id
                    )
                }
                Action::Revoke(rule) => {
                    network::revoke(context.box_root, &rule.id)?;
                    format!(
                        "Revoked {}. Existing connections and other rules are unaffected.",
                        rule.id
                    )
                }
            };
            *self = Self::open(context)?;
            self.message = message;
            return Ok(false);
        }
        self.message.clear();
        let words: Vec<_> = input.split_whitespace().collect();
        match words.as_slice() {
            ["q"] => return Ok(true),
            ["refresh"] => *self = Self::open(context)?,
            ["n"] => {
                self.page =
                    (self.page + 1).min(self.entries.len().saturating_sub(1) / page_size(rows))
            }
            ["b"] => self.page = self.page.saturating_sub(1),
            [verb @ ("s" | "p" | "r"), number] => {
                let index = number
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .context("choose an entry number")?;
                let entry = self
                    .entries
                    .get(index)
                    .context("entry number is out of range")?;
                let action = match (*verb, entry) {
                    ("s" | "p", Entry::Request(event)) => {
                        ensure!(context.general_network, "general networking is disabled");
                        let host = network::approvable_destination(event)?;
                        ensure!(
                            Some(gateway::request_session(&event.id)?) == context.session,
                            "request is not from this session"
                        );
                        let mut event = event.clone();
                        event.host = host;
                        Action::Approve(
                            event,
                            if *verb == "p" {
                                ApprovalScope::Project
                            } else {
                                ApprovalScope::Session
                            },
                        )
                    }
                    ("r", Entry::Rule(rule)) => Action::Revoke(rule.clone()),
                    _ => bail!("s/p require a denial entry; r requires a rule entry"),
                };
                let mut random = [0u8; 6];
                File::open("/dev/urandom")?.read_exact(&mut random)?;
                // A fresh, single-attempt challenge rejects queued keys and terminal replies.
                let code = random.iter().map(|byte| format!("{byte:02x}")).collect();
                self.confirmation = Some(Confirmation { action, code });
            }
            _ => bail!("use s N, p N, r N, n, b, refresh, or q"),
        }
        Ok(false)
    }
}

fn page_size(rows: usize) -> usize {
    rows.saturating_sub(11).clamp(1, 20)
}
fn clip(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        text.into()
    } else {
        text.chars()
            .take(columns.saturating_sub(3))
            .collect::<String>()
            + "..."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn confirmation_is_required_and_rules_remain_scoped_and_revocable() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions/session");
        fs::create_dir_all(&session_dir).unwrap();
        let active = File::create(session_dir.join("active.lock")).unwrap();
        active.lock().unwrap();
        let event = Event {
            id: "req-session-1".into(),
            method: "CONNECT".into(),
            host: "example.com".into(),
            port: 443,
            reason: "no matching allow rule".into(),
            created_at_ms: 1,
        };
        fs::write(
            session_dir.join("events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let context = Context {
            workspace: root.path(),
            box_root: root.path(),
            session: Some("session"),
            general_network: true,
        };
        let mut view = View::open(&context).unwrap();
        view.submit(&context, "s 1", 24);
        assert!(network::list_rules(root.path()).unwrap().is_empty());
        view.submit(&context, "yes", 24);
        assert!(network::list_rules(root.path()).unwrap().is_empty());
        view.submit(&context, "s 1", 24);
        let code = view.confirmation.as_ref().unwrap().code.clone();
        view.submit(&context, &code, 24);
        let rules = network::list_rules(root.path()).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].scope, ApprovalScope::Session);
        assert!(network::is_allowed(root.path(), "session", "example.com", 443).unwrap());
        assert!(!network::is_allowed(root.path(), "other", "example.com", 443).unwrap());
        view.submit(&context, "r 2", 24);
        let code = view.confirmation.as_ref().unwrap().code.clone();
        view.submit(&context, &code, 24);
        assert!(!network::is_allowed(root.path(), "session", "example.com", 443).unwrap());
        view.submit(&context, "p 1", 24);
        let code = view.confirmation.as_ref().unwrap().code.clone();
        assert!(
            view.render(&context, 24, 100)
                .contains("persists across sessions")
        );
        view.submit(&context, &code, 24);
        assert!(network::is_allowed(root.path(), "other", "example.com", 443).unwrap());
    }

    #[test]
    fn disabled_networking_and_unapprovable_denials_cannot_be_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions/session");
        fs::create_dir_all(&session_dir).unwrap();
        let event = Event {
            id: "req-session-1".into(),
            method: "CONNECT".into(),
            host: "example.com".into(),
            port: 443,
            reason: "no matching allow rule".into(),
            created_at_ms: 1,
        };
        fs::write(
            session_dir.join("events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let context = Context {
            workspace: root.path(),
            box_root: root.path(),
            session: Some("session"),
            general_network: true,
        };
        let mut view = View::open(&context).unwrap();
        view.submit(&context, "s 1", 24);
        let code = view.confirmation.as_ref().unwrap().code.clone();
        let disabled = Context {
            general_network: false,
            ..context
        };
        view.submit(&disabled, &code, 24);
        assert!(view.message.contains("disabled"));
        assert!(network::list_rules(root.path()).unwrap().is_empty());
        view.submit(&disabled, "p 1", 24);
        assert!(view.confirmation.is_none());
        let event = Event {
            reason: "DNS resolution failed\x1b[2J".into(),
            ..event
        };
        fs::write(
            session_dir.join("events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let mut view = View::open(&context).unwrap();
        view.submit(&context, "s 1", 24);
        assert!(view.confirmation.is_none());
        assert!(!view.render(&context, 24, 100).contains('\x1b'));
        assert!(network::list_rules(root.path()).unwrap().is_empty());
    }

    #[test]
    fn confirmation_shows_the_whole_destination_and_hides_other_sessions() {
        let root = tempfile::tempdir().unwrap();
        let session_dir = root.path().join("sessions/other");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("events.jsonl"),
            "malformed unrelated history\n",
        )
        .unwrap();
        let context = Context {
            workspace: root.path(),
            box_root: root.path(),
            session: Some("session"),
            general_network: true,
        };
        let mut view = View::open(&context).unwrap();
        assert!(view.entries.is_empty());
        let host = format!(
            "{}.{}.{}.example.com",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63)
        );
        view.entries.push(Entry::Request(Event {
            id: "req-session-1".into(),
            method: "CONNECT".into(),
            host: host.clone(),
            port: 443,
            reason: "no matching allow rule".into(),
            created_at_ms: 1,
        }));
        view.submit(&context, "p 1", 24);
        let displayed = view.render(&context, 24, 80).replace("\r\n", "");
        assert!(displayed.contains(&format!("Approve {host}:443")));
        assert!(displayed.contains("persists across sessions"));
        assert!(
            !view
                .render(&context, 10, 30)
                .contains(&view.confirmation.as_ref().unwrap().code)
        );
        view.cancel_input();
        assert!(view.confirmation.is_none());
    }

    #[test]
    fn inspection_is_read_only_and_cannot_expand_disabled_networking() {
        let root = tempfile::tempdir().unwrap();
        let context = Context {
            workspace: Path::new("project\n\x1b[2J"),
            box_root: root.path(),
            session: None,
            general_network: false,
        };
        let mut view = View::open(&context).unwrap();
        assert!(!view.render(&context, 24, 100).contains('\x1b'));
        assert!(view.render(&context, 24, 100).contains("disabled"));
        view.submit(&context, "s 1", 24);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
        assert!(view.submit(&context, "q", 24));
    }
}
