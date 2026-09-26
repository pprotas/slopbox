mod openai_codex;
mod openrouter;
mod transport;

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;

use crate::http::{parse_request_line, read_request_header, send_simple_response};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    OpenRouter,
    OpenAiCodex,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::OpenAiCodex => "OpenAI Codex",
        }
    }

    fn from_route(route: &str) -> Option<Self> {
        match route {
            openrouter::ROUTE => Some(Self::OpenRouter),
            openai_codex::ROUTE => Some(Self::OpenAiCodex),
            _ => None,
        }
    }
}

pub(crate) fn configured_names(state_root: &Path) -> Vec<&'static str> {
    let mut providers = Vec::new();
    if openrouter::configured() {
        providers.push(Kind::OpenRouter.name());
    }
    if openai_codex::configured(state_root) {
        providers.push(Kind::OpenAiCodex.name());
    }
    providers
}

#[derive(Default)]
pub(crate) struct Providers {
    openrouter: Option<openrouter::Provider>,
    codex: Option<openai_codex::CredentialManager>,
}

impl Providers {
    pub fn discover() -> Result<Self> {
        Ok(Self {
            openrouter: openrouter::Provider::discover(),
            codex: openai_codex::CredentialManager::discover()?,
        })
    }

    pub fn available(&self) -> Vec<Kind> {
        let mut providers = Vec::new();
        if self.openrouter.is_some() {
            providers.push(Kind::OpenRouter);
        }
        if self.codex.is_some() {
            providers.push(Kind::OpenAiCodex);
        }
        providers
    }

    pub fn handle(&self, mut client: UnixStream) -> Result<()> {
        client.set_read_timeout(Some(Duration::from_secs(30)))?;
        let request = read_request_header(&mut client)?;
        let (method, target) = parse_request_line(&request.header)?;
        let provider = Kind::from_route(&target);
        if method != "POST" {
            return if provider.is_some() {
                send_simple_response(
                    &mut client,
                    405,
                    "Method Not Allowed",
                    "method not allowed\n",
                )
            } else {
                send_simple_response(&mut client, 404, "Not Found", "not found\n")
            };
        }
        match provider {
            Some(Kind::OpenRouter) => match &self.openrouter {
                Some(provider) => provider.handle(&mut client, request),
                None => send_simple_response(
                    &mut client,
                    503,
                    "Service Unavailable",
                    "OpenRouter credential is not configured on the host\n",
                ),
            },
            Some(Kind::OpenAiCodex) => match &self.codex {
                Some(provider) => provider.handle(&mut client, request),
                None => send_simple_response(
                    &mut client,
                    503,
                    "Service Unavailable",
                    "OpenAI Codex credential is not configured on the host\n",
                ),
            },
            None => send_simple_response(&mut client, 404, "Not Found", "not found\n"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testing::exchange;

    #[test]
    fn model_routes_require_exact_paths_and_post_without_using_credentials() {
        let providers = Providers::default();
        assert!(providers.available().is_empty());
        for (method, target, status) in [
            (
                "POST",
                "/openrouter/api/v1/chat/completions",
                "503 Service Unavailable",
            ),
            (
                "POST",
                "/openai-codex/codex/responses",
                "503 Service Unavailable",
            ),
            (
                "GET",
                "/openrouter/api/v1/chat/completions",
                "405 Method Not Allowed",
            ),
            (
                "PUT",
                "/openai-codex/codex/responses",
                "405 Method Not Allowed",
            ),
            (
                "POST",
                "/openrouter/api/v1/chat/completions?upstream=other",
                "404 Not Found",
            ),
            (
                "POST",
                "/openai-codex/codex/responses/extra",
                "404 Not Found",
            ),
            (
                "POST",
                "https://openrouter.ai/api/v1/chat/completions",
                "404 Not Found",
            ),
            ("CONNECT", "example.com:443", "404 Not Found"),
            ("GET", "/denials", "404 Not Found"),
        ] {
            let response = exchange(
                format!("{method} {target} HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
                |stream| providers.handle(stream),
            );
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}\r\n")),
                "{method} {target}: {response}"
            );
        }
    }

    #[test]
    fn configuration_inspection_does_not_validate_tokens_or_create_state() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("absent-state");
        assert!(!configured_names(&state).contains(&"OpenAI Codex"));
        assert!(!state.exists());
        let credentials = state.join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let token = credentials.join("openai-codex.json");
        std::fs::write(&token, "not a credential document").unwrap();
        assert!(configured_names(&state).contains(&"OpenAI Codex"));
        assert_eq!(std::fs::read_dir(&credentials).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(&token).unwrap(),
            "not a credential document"
        );
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AuthProvider {
    OpenAiCodex,
}

impl AuthProvider {
    pub fn id(self) -> &'static str {
        match self {
            Self::OpenAiCodex => openai_codex::PROVIDER,
        }
    }

    pub fn configured() -> Result<Vec<Self>> {
        Ok(if openai_codex::CredentialManager::discover()?.is_some() {
            vec![Self::OpenAiCodex]
        } else {
            Vec::new()
        })
    }

    pub fn login(self) -> Result<()> {
        match self {
            Self::OpenAiCodex => openai_codex::CredentialManager::new()?.login(),
        }
    }

    pub fn status(self) -> Result<bool> {
        match self {
            Self::OpenAiCodex => openai_codex::CredentialManager::new()?.status(),
        }
    }

    pub fn logout(self) -> Result<bool> {
        match self {
            Self::OpenAiCodex => openai_codex::CredentialManager::new()?.logout(),
        }
    }
}
