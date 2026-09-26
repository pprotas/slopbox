use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::os::unix::net::UnixStream;

use super::transport::{self, Target};
use crate::http::{BufferedRequest, send_simple_response};

pub(super) const PROVIDER: &str = "openai-codex";
pub(super) const ROUTE: &str = "/openai-codex/codex/responses";

pub(super) fn configured(state_root: &Path) -> bool {
    state_root.join("credentials/openai-codex.json").is_file()
}

fn forward(
    client: &mut UnixStream,
    request: BufferedRequest,
    access: &Access,
    target: &Target,
) -> Result<()> {
    transport::forward(
        client,
        request,
        &access.access,
        target,
        "OpenAI Codex",
        &[
            ("chatgpt-account-id", &access.account_id),
            ("originator", "pi"),
        ],
        &[access.account_id.as_bytes()],
    )
}
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const REFRESH_MARGIN_MS: u64 = 5 * 60 * 1000;
const DEVICE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub struct CredentialManager {
    credential_path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Clone)]
pub struct Access {
    pub access: String,
    pub account_id: String,
}

#[derive(Deserialize, Serialize)]
struct StoredCodexCredential {
    provider: String,
    access: String,
    refresh: String,
    expires: u64,
    account_id: String,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: Value,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

impl CredentialManager {
    pub fn handle(&self, client: &mut UnixStream, request: BufferedRequest) -> Result<()> {
        let access = match self.access() {
            Ok(access) => access,
            Err(error) => {
                eprintln!("slopbox model gateway: OpenAI Codex credential unavailable: {error:#}");
                return send_simple_response(
                    client,
                    503,
                    "Service Unavailable",
                    "OpenAI Codex credential is unavailable\n",
                );
            }
        };
        let target = match Target::new(
            "https://chatgpt.com/backend-api/codex/responses",
            "chatgpt.com",
        ) {
            Ok(target) => target,
            Err(_) => {
                return send_simple_response(
                    client,
                    502,
                    "Bad Gateway",
                    "OpenAI Codex upstream request failed\n",
                );
            }
        };
        forward(client, request, &access, &target)
    }

    pub fn discover() -> Result<Option<Self>> {
        let manager = Self::new()?;
        if manager.credential_path.is_file() {
            Ok(Some(manager))
        } else {
            Ok(None)
        }
    }

    pub fn new() -> Result<Self> {
        let directory = credential_directory()?;
        Ok(Self {
            credential_path: directory.join(format!("{PROVIDER}.json")),
            lock_path: directory.join(format!("{PROVIDER}.lock")),
        })
    }

    pub fn login(&self) -> Result<()> {
        let client = oauth_client()?;
        let response = client
            .post(DEVICE_USER_CODE_URL)
            .json(&serde_json::json!({ "client_id": CLIENT_ID }))
            .send()
            .context("failed to start OpenAI Codex device login")?;
        ensure!(
            response.status().is_success(),
            "OpenAI Codex device login failed with status {}",
            response.status()
        );
        let device: DeviceCodeResponse = response
            .json()
            .context("OpenAI Codex returned an invalid device login response")?;
        ensure!(
            !device.device_auth_id.is_empty() && !device.user_code.is_empty(),
            "OpenAI Codex returned an incomplete device login response"
        );
        let mut interval = parse_interval(&device.interval)?.max(1);

        println!("Open {DEVICE_VERIFICATION_URI}");
        println!("Enter code: {}", device.user_code);
        println!("Waiting for authentication...");

        let started = std::time::Instant::now();
        let authorization = loop {
            ensure!(
                started.elapsed() < DEVICE_TIMEOUT,
                "OpenAI Codex device login timed out"
            );
            thread::sleep(Duration::from_secs(interval));
            let response = client
                .post(DEVICE_TOKEN_URL)
                .json(&serde_json::json!({
                    "device_auth_id": device.device_auth_id,
                    "user_code": device.user_code,
                }))
                .send()
                .context("failed while waiting for OpenAI Codex device login")?;
            if response.status().is_success() {
                break response
                    .json::<DeviceTokenResponse>()
                    .context("OpenAI Codex returned an invalid device authorization response")?;
            }
            let status = response.status();
            if matches!(status.as_u16(), 403 | 404) {
                continue;
            }
            let error_code =
                response
                    .json::<Value>()
                    .ok()
                    .and_then(|body| match body.get("error") {
                        Some(Value::String(code)) => Some(code.clone()),
                        Some(Value::Object(error)) => {
                            error.get("code").and_then(Value::as_str).map(str::to_owned)
                        }
                        _ => None,
                    });
            match error_code.as_deref() {
                Some("deviceauth_authorization_pending") => continue,
                Some("slow_down") => {
                    interval = interval.saturating_add(5);
                    continue;
                }
                _ => bail!("OpenAI Codex device authorization failed with status {status}"),
            }
        };

        let token = exchange_authorization_code(&client, &authorization)?;
        let credential = credential_from_token(token)?;
        let lock = open_lock(&self.lock_path)?;
        lock_exclusive(&lock)?;
        write_credential(&self.credential_path, &credential)?;
        println!("Authenticated {PROVIDER}.");
        Ok(())
    }

    pub fn status(&self) -> Result<bool> {
        if !self.credential_path.is_file() {
            return Ok(false);
        }
        let lock = open_lock(&self.lock_path)?;
        lock_exclusive(&lock)?;
        let credential = read_credential(&self.credential_path)?;
        ensure!(
            credential.provider == PROVIDER,
            "stored credential has the wrong provider"
        );
        Ok(true)
    }

    pub fn logout(&self) -> Result<bool> {
        let lock = open_lock(&self.lock_path)?;
        lock_exclusive(&lock)?;
        match fs::remove_file(&self.credential_path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to remove credential {}",
                    self.credential_path.display()
                )
            }),
        }
    }

    pub fn access(&self) -> Result<Access> {
        let lock = open_lock(&self.lock_path)?;
        lock_exclusive(&lock)?;
        let mut credential = read_credential(&self.credential_path)
            .context("OpenAI Codex is not authenticated; run `slopbox auth login openai-codex`")?;
        ensure!(
            credential.provider == PROVIDER,
            "stored credential has the wrong provider"
        );

        if credential.expires <= now_ms().saturating_add(REFRESH_MARGIN_MS) {
            credential = refresh_token(&oauth_client()?, &credential.refresh)?;
            write_credential(&self.credential_path, &credential)?;
        }

        Ok(Access {
            access: credential.access,
            account_id: credential.account_id,
        })
    }
}

fn credential_directory() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")?;
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let directory = data_home.join("slopbox/credentials");
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

fn oauth_client() -> Result<Client> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .context("failed to create OAuth client")
}

fn parse_interval(value: &Value) -> Result<u64> {
    match value {
        Value::Number(number) => number
            .as_u64()
            .context("OpenAI Codex returned an invalid polling interval"),
        Value::String(value) => value
            .parse()
            .context("OpenAI Codex returned an invalid polling interval"),
        _ => bail!("OpenAI Codex returned an invalid polling interval"),
    }
}

fn exchange_authorization_code(
    client: &Client,
    authorization: &DeviceTokenResponse,
) -> Result<TokenResponse> {
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", authorization.authorization_code.as_str()),
            ("code_verifier", authorization.code_verifier.as_str()),
            ("redirect_uri", DEVICE_REDIRECT_URI),
        ])
        .send()
        .context("failed to exchange the OpenAI Codex authorization code")?;
    ensure!(
        response.status().is_success(),
        "OpenAI Codex token exchange failed with status {}",
        response.status()
    );
    response
        .json()
        .context("OpenAI Codex returned an invalid token response")
}

fn refresh_token(client: &Client, refresh: &str) -> Result<StoredCodexCredential> {
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .context("failed to refresh the OpenAI Codex credential")?;
    ensure!(
        response.status().is_success(),
        "OpenAI Codex token refresh failed with status {}",
        response.status()
    );
    let token = response
        .json()
        .context("OpenAI Codex returned an invalid refresh response")?;
    credential_from_token(token)
}

fn credential_from_token(token: TokenResponse) -> Result<StoredCodexCredential> {
    ensure!(
        !token.access_token.is_empty() && !token.refresh_token.is_empty(),
        "OpenAI Codex returned an incomplete token response"
    );
    let account_id = account_id_from_jwt(&token.access_token)?;
    Ok(StoredCodexCredential {
        provider: PROVIDER.to_owned(),
        access: token.access_token,
        refresh: token.refresh_token,
        expires: now_ms().saturating_add(token.expires_in.saturating_mul(1000)),
        account_id,
    })
}

fn account_id_from_jwt(token: &str) -> Result<String> {
    let payload = token
        .split('.')
        .nth(1)
        .context("OpenAI Codex returned an invalid access token")?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .context("OpenAI Codex returned an invalid access token")?;
    let payload: Value = serde_json::from_slice(&decoded)
        .context("OpenAI Codex returned an invalid access token")?;
    payload
        .get(JWT_CLAIM_PATH)
        .and_then(|claim| claim.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .context("OpenAI Codex access token contains no account ID")
}

fn read_credential(path: &Path) -> Result<StoredCodexCredential> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to read credential {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "credential path is not a regular file");
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "credential is not owned by the current user"
    );
    ensure!(
        metadata.mode() & 0o077 == 0,
        "credential is accessible by another user"
    );
    serde_json::from_reader(file).context("stored OpenAI Codex credential is invalid")
}

fn write_credential(path: &Path, credential: &StoredCodexCredential) -> Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    serde_json::to_writer(&mut file, credential)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to install credential {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn open_lock(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open credential lock {}", path.display()))
}

fn lock_exclusive(file: &File) -> Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if result == -1 {
        return Err(std::io::Error::last_os_error()).context("failed to lock credential store");
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::read_request_header;
    use crate::http::testing::read_tcp_request;
    use std::io::Read;
    use std::net::Ipv4Addr;

    #[test]
    fn codex_confines_oauth_headers() {
        use std::sync::mpsc;

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request_tx.send(read_tcp_request(&mut stream)).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Token: host-codex-access-token\r\nX-Account: host-account\r\nConnection: close\r\n\r\ndata: host-codex-access-token host-account\n\n",
                )
                .unwrap();
        });

        let target = Target {
            url: format!("http://{address}/backend-api/codex/responses"),
            resolved: None,
            connect_timeout: Duration::from_millis(250),
            load_system_roots: false,
        };
        let host_token = "host-codex-access-token";
        let body = br#"{"model":"gpt-5.4","stream":true}"#;
        let (mut client, mut server) = UnixStream::pair().unwrap();
        write!(
            client,
            "POST /openai-codex/codex/responses HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer guest-token\r\nChatGPT-Account-Id: guest-account\r\nOriginator: guest\r\nContent-Encoding: zstd\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        client.write_all(body).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let request = read_request_header(&mut server).unwrap();
        forward(
            &mut server,
            request,
            &Access {
                access: host_token.into(),
                account_id: "host-account".into(),
            },
            &target,
        )
        .unwrap();

        let upstream_request = String::from_utf8(request_rx.recv().unwrap()).unwrap();
        assert!(upstream_request.starts_with("POST /backend-api/codex/responses HTTP/1.1\r\n"));
        assert!(upstream_request.contains("authorization: Bearer host-codex-access-token\r\n"));
        assert!(upstream_request.contains("chatgpt-account-id: host-account\r\n"));
        assert!(upstream_request.contains("originator: pi\r\n"));
        assert!(upstream_request.contains("content-encoding: zstd\r\n"));
        assert!(!upstream_request.contains("guest-token"));
        assert!(!upstream_request.contains("guest-account"));
        drop(server);
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("data: [REDACTED] [REDACTED]"));
        assert!(!response.contains(host_token));
        assert!(!response.contains("host-account"));
        assert!(!response.contains("x-token:"));
        assert!(!response.contains("x-account:"));
        upstream.join().unwrap();
    }

    #[test]
    fn uses_cached_access_and_rejects_unsafe_credential_files() {
        let directory = tempfile::tempdir().unwrap();
        let manager = CredentialManager {
            credential_path: directory.path().join("openai-codex.json"),
            lock_path: directory.path().join("openai-codex.lock"),
        };
        let credential = StoredCodexCredential {
            provider: PROVIDER.into(),
            access: "cached-access".into(),
            refresh: "unused-refresh".into(),
            expires: u64::MAX,
            account_id: "cached-account".into(),
        };
        write_credential(&manager.credential_path, &credential).unwrap();
        let original = fs::read(&manager.credential_path).unwrap();
        let access = manager.access().unwrap();
        assert_eq!(access.access, "cached-access");
        assert_eq!(access.account_id, "cached-account");
        assert!(manager.status().unwrap());
        assert_eq!(fs::read(&manager.credential_path).unwrap(), original);
        assert_eq!(
            fs::metadata(&manager.credential_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::set_permissions(&manager.credential_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            format!("{:#}", manager.access().err().unwrap())
                .contains("credential is accessible by another user")
        );
        fs::set_permissions(&manager.credential_path, fs::Permissions::from_mode(0o600)).unwrap();
        let redirected = directory.path().join("redirected.json");
        fs::rename(&manager.credential_path, &redirected).unwrap();
        std::os::unix::fs::symlink(&redirected, &manager.credential_path).unwrap();
        assert!(manager.access().is_err());
        assert_eq!(fs::read(redirected).unwrap(), original);
    }

    #[test]
    fn extracts_codex_account_id() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}}"#);
        let token = format!("header.{payload}.signature");
        assert_eq!(account_id_from_jwt(&token).unwrap(), "account-123");
    }

    #[test]
    fn rejects_tokens_without_an_account_id() {
        assert!(account_id_from_jwt("header.e30.signature").is_err());
    }
}
