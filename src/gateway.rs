use std::collections::{HashMap, HashSet};
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::http::{
    BufferedRequest, contains_bytes, copy_redacting_many, parse_request_line, read_request_header,
    read_retry, resolve_public, send_simple_response,
};
use crate::provider::{Kind, Providers};
mod tls;

const GENERAL_PROXY_PORT: u16 = 39_080;
const MODEL_PROXY_PORT: u16 = 39_081;
const AUTHENTICATED_HTTP_PROXY_PORT: u16 = 39_082;

pub struct GatewaySession {
    session_id: String,
    _active_lock: File,
    runtime_dir: PathBuf,
    general_socket_path: PathBuf,
    model_socket_path: PathBuf,
    authenticated_http_socket_path: Option<PathBuf>,
    direct_http_socket_path: Option<PathBuf>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    providers: Vec<Kind>,
    account_ca: Option<String>,
}

#[derive(Clone)]
pub enum HttpAuthentication {
    Basic { username: String, secret: Arc<str> },
    Bearer { secret: Arc<str> },
    Token { secret: Arc<str> },
}

#[derive(Clone)]
pub struct AuthenticatedHttpRoute {
    name: String,
    upstream_base: Url,
    origin: Url,
    direct_base: Option<Url>,
    proxy: bool,
    tls: Option<Arc<rustls::ServerConfig>>,
    methods: HashSet<String>,
    authorization: Arc<str>,
    secret: Arc<str>,
    resolved: Option<SocketAddr>,
    connect_timeout: Duration,
    load_system_roots: bool,
    #[cfg(test)]
    test_root: Option<reqwest::Certificate>,
    allow_private_addresses: bool,
}

impl AuthenticatedHttpRoute {
    pub fn new(
        name: String,
        upstream_base: String,
        methods: Vec<String>,
        authentication: HttpAuthentication,
        allow_private_addresses: bool,
        direct: bool,
    ) -> Result<Self> {
        let upstream_base = validate_http_route(&name, &upstream_base, &methods)?;
        let methods = methods
            .into_iter()
            .map(|method| method.to_ascii_uppercase())
            .collect();
        let (authorization, secret) = match authentication {
            HttpAuthentication::Basic { username, secret } => {
                ensure!(
                    !username.is_empty() && !username.contains(['\r', '\n', ':']),
                    "authenticated HTTP basic username is invalid"
                );
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{secret}"));
                (Arc::from(format!("Basic {encoded}")), secret)
            }
            HttpAuthentication::Bearer { secret } => {
                (Arc::from(format!("Bearer {secret}")), secret)
            }
            HttpAuthentication::Token { secret } => (Arc::from(format!("token {secret}")), secret),
        };
        let route = Self {
            name,
            direct_base: direct.then(|| upstream_base.clone()),
            origin: upstream_base.clone(),
            upstream_base,
            proxy: false,
            tls: None,
            methods,
            authorization,
            secret,
            resolved: None,
            connect_timeout: Duration::from_secs(15),
            load_system_roots: true,
            #[cfg(test)]
            test_root: None,
            allow_private_addresses,
        };
        #[cfg(all(test, target_os = "macos"))]
        let route = {
            let mut route = route;
            if matches!(
                route.upstream_base.host_str(),
                Some("forgejo.native.invalid" | "github.native.invalid")
            ) && let Ok(address) = std::env::var("SLOPBOX_TEST_ACCOUNT")
            {
                let address: SocketAddr = address.parse()?;
                ensure!(
                    address.ip().is_loopback() && allow_private_addresses,
                    "test account upstream requires explicit loopback access"
                );
                route.upstream_base =
                    Url::parse(&format!("http://{address}{}", route.upstream_base.path()))?;
                route.load_system_roots = false;
                route.connect_timeout = Duration::from_secs(2);
            }
            route
        };
        Ok(route)
    }

    pub fn with_proxy(mut self, proxy: bool) -> Self {
        self.proxy = proxy;
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn is_direct(&self) -> bool {
        self.direct_base.is_some()
    }
}

pub fn validate_http_route(name: &str, upstream: &str, methods: &[String]) -> Result<Url> {
    ensure!(
        !name.is_empty()
            && name.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_')),
        "authenticated HTTP route name is invalid"
    );
    let upstream =
        Url::parse(upstream).context("authenticated HTTP route has an invalid upstream URL")?;
    ensure!(
        upstream.scheme() == "https"
            && upstream.username().is_empty()
            && upstream.password().is_none()
            && upstream.query().is_none()
            && upstream.fragment().is_none()
            && upstream.host_str().is_some(),
        "authenticated HTTP route requires a fixed HTTPS upstream"
    );
    ensure!(
        !methods.is_empty()
            && methods.iter().all(|method| !method.is_empty()
                && method
                    .bytes()
                    .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')),
        "authenticated HTTP route has invalid methods"
    );
    Ok(upstream)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    pub method: String,
    pub host: String,
    pub port: u16,
    pub reason: String,
    pub created_at_ms: u64,
}

#[derive(Hash, Eq, PartialEq)]
struct DenialKey {
    method: String,
    host: String,
    port: u16,
    reason: String,
}

struct GatewayLoopConfig {
    general_listener: UnixListener,
    model_listener: UnixListener,
    authenticated_http_listener: Option<UnixListener>,
    direct_http_listener: Option<UnixListener>,
    authenticated_http_routes: Arc<Vec<AuthenticatedHttpRoute>>,
    box_root: PathBuf,
    session_id: String,
    events_path: PathBuf,
    providers: Arc<Providers>,
}

struct EventRecorder {
    session_id: String,
    events_path: PathBuf,
    counter: AtomicU64,
    seen: Mutex<HashMap<DenialKey, Event>>,
}

impl EventRecorder {
    fn new(session_id: &str, events_path: &Path) -> Self {
        Self {
            session_id: session_id.to_owned(),
            events_path: events_path.to_path_buf(),
            counter: AtomicU64::new(1),
            seen: Mutex::new(HashMap::new()),
        }
    }

    fn record(&self, method: &str, host: &str, port: u16, reason: &str) -> Result<Event> {
        let key = DenialKey {
            method: sanitize(method),
            host: sanitize(
                &crate::network::normalize_destination(host)
                    .unwrap_or_else(|_| normalize_host(host)),
            ),
            port,
            reason: sanitize(reason),
        };
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| anyhow::anyhow!("denial event lock is poisoned"))?;
        if let Some(event) = seen.get(&key) {
            return Ok(event.clone());
        }

        let event = Event {
            id: format!(
                "req-{}-{}",
                self.session_id,
                self.counter.fetch_add(1, Ordering::Relaxed)
            ),
            method: key.method.clone(),
            host: key.host.clone(),
            port: key.port,
            reason: key.reason.clone(),
            created_at_ms: crate::network::now_ms(),
        };
        append_private(
            &self.events_path,
            &format!("{}\n", serde_json::to_string(&event)?),
        )?;
        seen.insert(key, event.clone());
        Ok(event)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalScope {
    Session,
    Project,
}

impl GatewaySession {
    pub fn start(
        box_root: &Path,
        mut authenticated_http_routes: Vec<AuthenticatedHttpRoute>,
    ) -> Result<Self> {
        let account_ca = tls::prepare(&mut authenticated_http_routes)?;
        secure_dir(box_root)?;
        crate::network::list_rules(box_root)?;
        let session_id = session_id();
        let session_dir = box_root.join("sessions").join(&session_id);
        secure_dir(&session_dir)?;

        #[cfg(target_os = "linux")]
        let runtime_base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .context("XDG_RUNTIME_DIR is required for the network gateway")?
            .join("slopbox");
        #[cfg(target_os = "macos")]
        let runtime_base = crate::backend::macos::runtime_root()?.join("gateway");
        secure_dir(&runtime_base)?;
        let runtime_dir = runtime_base.join(&session_id);
        secure_dir(&runtime_dir)?;
        let general_socket_dir = runtime_dir.join("general");
        let model_socket_dir = runtime_dir.join("model");
        let authenticated_http_socket_dir = runtime_dir.join("authenticated-http");
        secure_dir(&general_socket_dir)?;
        secure_dir(&model_socket_dir)?;
        let general_socket_path = general_socket_dir.join("gateway.sock");
        let model_socket_path = model_socket_dir.join("gateway.sock");
        let general_listener = bind_listener(&general_socket_path)?;
        let model_listener = bind_listener(&model_socket_path)?;
        let (authenticated_http_listener, authenticated_http_socket_path) =
            if authenticated_http_routes.is_empty() {
                (None, None)
            } else {
                secure_dir(&authenticated_http_socket_dir)?;
                let path = authenticated_http_socket_dir.join("gateway.sock");
                (Some(bind_listener(&path)?), Some(path))
            };

        let (direct_http_listener, direct_http_socket_path) = if authenticated_http_routes
            .iter()
            .any(AuthenticatedHttpRoute::is_direct)
        {
            let path = authenticated_http_socket_dir.join("direct.sock");
            (Some(bind_listener(&path)?), Some(path))
        } else {
            (None, None)
        };

        let events_path = session_dir.join("events.jsonl");
        create_private_file(&events_path)?;
        let active_lock = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(session_dir.join("active.lock"))?;
        active_lock.lock()?;

        let providers = Arc::new(Providers::discover()?);
        let available_providers = providers.available();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let loop_config = GatewayLoopConfig {
            general_listener,
            model_listener,
            authenticated_http_listener,
            direct_http_listener,
            authenticated_http_routes: Arc::new(authenticated_http_routes),
            box_root: box_root.to_path_buf(),
            session_id: session_id.clone(),
            events_path,
            providers,
        };
        let thread = thread::Builder::new()
            .name("slopbox-gateway".into())
            .spawn(move || gateway_loop(loop_config, thread_stop))
            .context("failed to start network gateway")?;

        Ok(Self {
            session_id,
            _active_lock: active_lock,
            runtime_dir,
            general_socket_path,
            model_socket_path,
            authenticated_http_socket_path,
            direct_http_socket_path,
            stop,
            thread: Some(thread),
            providers: available_providers,
            account_ca,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn general_socket_dir(&self) -> PathBuf {
        self.runtime_dir.join("general")
    }

    pub fn model_socket_dir(&self) -> PathBuf {
        self.runtime_dir.join("model")
    }

    pub fn authenticated_http_socket_dir(&self) -> PathBuf {
        self.runtime_dir.join("authenticated-http")
    }

    pub fn general_proxy_port(&self) -> u16 {
        GENERAL_PROXY_PORT
    }

    pub fn model_proxy_port(&self) -> u16 {
        MODEL_PROXY_PORT
    }

    pub fn authenticated_http_proxy_port(&self) -> u16 {
        AUTHENTICATED_HTTP_PROXY_PORT
    }

    pub fn providers(&self) -> &[Kind] {
        &self.providers
    }

    pub fn account_ca(&self) -> Option<&str> {
        self.account_ca.as_deref()
    }
}

impl Drop for GatewaySession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = UnixStream::connect(&self.general_socket_path);
        let _ = UnixStream::connect(&self.model_socket_path);
        for path in [
            &self.authenticated_http_socket_path,
            &self.direct_http_socket_path,
        ]
        .into_iter()
        .flatten()
        {
            let _ = UnixStream::connect(path);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.runtime_dir);
    }
}

pub fn all_events(box_root: &Path) -> Result<Vec<Event>> {
    let mut sessions = Vec::new();
    match fs::read_dir(box_root.join("sessions")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    sessions.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    sessions.sort();
    let mut events = Vec::new();
    for session in sessions {
        events.extend(read_events(&session.join("events.jsonl"))?);
    }
    Ok(events)
}

pub fn session_events(box_root: &Path, session: &str) -> Result<Vec<Event>> {
    validate_session_id(session)?;
    let events = read_events(&box_root.join("sessions").join(session).join("events.jsonl"))?;
    for event in &events {
        ensure!(
            request_session(&event.id)? == session,
            "event belongs to another session"
        );
    }
    Ok(events)
}

pub fn approve(
    box_root: &Path,
    request_id: &str,
    scope: ApprovalScope,
) -> Result<crate::network::Rule> {
    let session_id = request_session(request_id)?;
    let event = session_events(box_root, session_id)?
        .into_iter()
        .find(|event| event.id == request_id)
        .with_context(|| format!("unknown request ID {request_id}"))?;

    crate::network::grant(box_root, &event, scope)
}

fn bind_listener(path: &Path) -> Result<UnixListener> {
    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind gateway socket {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn accept_connection(listener: &UnixListener) -> std::io::Result<UnixStream> {
    let (stream, _) = listener.accept()?;
    // Darwin inherits O_NONBLOCK from the listener; request handlers use blocking I/O.
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn gateway_loop(config: GatewayLoopConfig, stop: Arc<AtomicBool>) {
    let recorder = Arc::new(EventRecorder::new(&config.session_id, &config.events_path));
    while !stop.load(Ordering::Relaxed) {
        let mut accepted = false;

        match accept_connection(&config.general_listener) {
            Ok(stream) => {
                accepted = true;
                let box_root = config.box_root.clone();
                let session_id = config.session_id.clone();
                let recorder = Arc::clone(&recorder);
                thread::spawn(move || {
                    if let Err(error) =
                        handle_general_connection(stream, &box_root, &session_id, &recorder)
                    {
                        eprintln!("slopbox general gateway: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => eprintln!("slopbox general gateway: accept failed: {error}"),
        }

        match accept_connection(&config.model_listener) {
            Ok(stream) => {
                accepted = true;
                let providers = Arc::clone(&config.providers);
                thread::spawn(move || {
                    if let Err(error) = providers.handle(stream) {
                        eprintln!("slopbox model gateway: {error:#}");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => eprintln!("slopbox model gateway: accept failed: {error}"),
        }

        for (listener, direct) in [
            (config.authenticated_http_listener.as_ref(), false),
            (config.direct_http_listener.as_ref(), true),
        ]
        .into_iter()
        .filter_map(|(listener, direct)| listener.map(|listener| (listener, direct)))
        {
            match accept_connection(listener) {
                Ok(stream) => {
                    accepted = true;
                    let routes = Arc::clone(&config.authenticated_http_routes);
                    thread::spawn(move || {
                        if let Err(error) =
                            handle_authenticated_http_connection(stream, &routes, direct)
                        {
                            eprintln!("slopbox authenticated HTTP gateway: {error:#}");
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    eprintln!("slopbox authenticated HTTP gateway: accept failed: {error}")
                }
            }
        }

        if !accepted {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn handle_general_connection(
    mut client: UnixStream,
    box_root: &Path,
    session_id: &str,
    recorder: &EventRecorder,
) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(30)))?;
    let request = read_request_header(&mut client)?;
    let (method, target) = parse_request_line(&request.header)?;

    if method == "GET" && target == "/denials" {
        return send_denials(&mut client, &recorder.events_path);
    }
    if target.starts_with('/') {
        return send_simple_response(&mut client, 404, "Not Found", "not found\n");
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = parse_authority(&target, 443)?;
        return handle_connect(
            client,
            request.remainder,
            box_root,
            session_id,
            recorder,
            &method,
            &host,
            port,
        );
    }

    handle_plain_http(
        client, request, box_root, session_id, recorder, &method, &target,
    )
}

fn handle_authenticated_http_connection(
    client: UnixStream,
    routes: &[AuthenticatedHttpRoute],
    direct: bool,
) -> Result<()> {
    client.set_read_timeout(Some(Duration::from_secs(60)))?;
    client.set_write_timeout(Some(Duration::from_secs(60)))?;
    handle_account_stream(client, routes, direct)
}

fn handle_account_stream(
    mut client: impl Read + Write,
    routes: &[AuthenticatedHttpRoute],
    direct: bool,
) -> Result<()> {
    let request = read_request_header(&mut client)?;
    let (method, target) = parse_request_line(&request.header)?;
    if method.eq_ignore_ascii_case("CONNECT") {
        ensure!(
            !direct,
            "CONNECT is not supported on the origin-form socket"
        );
        return tls::handle(client, request, &target, routes);
    }
    forward_authenticated_request(&mut client, request, routes, direct)
}

fn forward_authenticated_request(
    mut client: &mut (impl Read + Write),
    request: BufferedRequest,
    routes: &[AuthenticatedHttpRoute],
    direct: bool,
) -> Result<()> {
    const MAX_BODY_SIZE: usize = 512 * 1024 * 1024;
    let (method, target) = parse_request_line(&request.header)?;
    let mut headers = [httparse::EMPTY_HEADER; 96];
    let mut parsed = httparse::Request::new(&mut headers);
    ensure!(
        parsed.parse(&request.header)?.is_complete(),
        "incomplete authenticated HTTP request"
    );
    let (route, suffix) = if direct {
        let mut hosts = parsed
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("host"));
        let host = hosts
            .next()
            .context("direct account request requires Host")?;
        ensure!(
            hosts.next().is_none(),
            "direct account request has duplicate Host headers"
        );
        match_direct_http_route(routes, &target, std::str::from_utf8(host.value)?)?
    } else {
        match_authenticated_http_route(routes, &target)?
    };
    if !route.methods.contains(&method.to_ascii_uppercase()) {
        return send_simple_response(
            &mut client,
            405,
            "Method Not Allowed",
            "method not allowed\n",
        );
    }
    ensure!(
        !parsed
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("transfer-encoding")),
        "Transfer-Encoding is not supported for authenticated HTTP requests"
    );
    let mut lengths = parsed
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("content-length"));
    let length = lengths.next();
    ensure!(
        lengths.next().is_none(),
        "duplicate authenticated HTTP Content-Length"
    );
    let content_length = length
        .map(|header| std::str::from_utf8(header.value))
        .transpose()?
        .map(str::parse::<usize>)
        .transpose()?
        .unwrap_or(0);
    ensure!(
        content_length <= MAX_BODY_SIZE,
        "authenticated HTTP request is too large"
    );
    let mut body = request.remainder;
    ensure!(
        body.len() <= content_length,
        "authenticated HTTP request contains excess body data"
    );
    while body.len() < content_length {
        let mut buffer = vec![0_u8; (content_length - body.len()).min(64 * 1024)];
        let count = read_retry(&mut client, &mut buffer)?;
        ensure!(
            count != 0,
            "authenticated HTTP client closed during request body"
        );
        body.extend_from_slice(&buffer[..count]);
    }

    let base = route.upstream_base.as_str().trim_end_matches('/');
    let upstream_url = format!("{base}{suffix}");
    let upstream = Url::parse(&upstream_url)?;
    ensure!(
        upstream.scheme() == route.upstream_base.scheme()
            && upstream.host_str() == route.upstream_base.host_str()
            && upstream.path().starts_with(route.upstream_base.path()),
        "authenticated HTTP request escaped its configured upstream"
    );
    let host = upstream
        .host_str()
        .context("authenticated HTTP upstream has no host")?;
    let port = upstream
        .port_or_known_default()
        .context("authenticated HTTP upstream has no port")?;
    let address = match route.resolved {
        Some(address) => address,
        None => resolve_http_route(host, port, route.allow_private_addresses)?,
    };
    let reqwest_method = reqwest::Method::from_bytes(method.as_bytes())?;
    let mut http = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(route.connect_timeout);
    if !route.load_system_roots {
        http = http.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
    }
    #[cfg(test)]
    if let Some(root) = &route.test_root {
        http = http.tls_certs_only([root.clone()]);
    }
    let http = http.resolve(host, address).build()?;
    let mut upstream_request = http
        .request(reqwest_method, upstream)
        .header(reqwest::header::AUTHORIZATION, route.authorization.as_ref())
        .header(reqwest::header::ACCEPT_ENCODING, "identity");
    for header in parsed.headers.iter() {
        if header.name.eq_ignore_ascii_case("authorization")
            || header.name.eq_ignore_ascii_case("host")
            || header.name.eq_ignore_ascii_case("connection")
            || header.name.eq_ignore_ascii_case("proxy-connection")
            || header.name.eq_ignore_ascii_case("proxy-authorization")
            || header.name.eq_ignore_ascii_case("content-length")
            || header.name.eq_ignore_ascii_case("transfer-encoding")
            || header.name.eq_ignore_ascii_case("accept-encoding")
        {
            continue;
        }
        let name = reqwest::header::HeaderName::from_bytes(header.name.as_bytes())?;
        let value = reqwest::header::HeaderValue::from_bytes(header.value)?;
        upstream_request = upstream_request.header(name, value);
    }
    let mut response = match upstream_request.body(body).send() {
        Ok(response) => response,
        Err(_) => {
            return send_simple_response(
                &mut client,
                502,
                "Bad Gateway",
                "authenticated HTTP upstream request failed\n",
            );
        }
    };
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return send_simple_response(
            &mut client,
            502,
            "Bad Gateway",
            "encoded authenticated HTTP responses are not supported\n",
        );
    }

    let secrets = [route.secret.as_bytes(), route.authorization.as_bytes()];
    let status = response.status();
    write!(
        client,
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Upstream Response")
    )?;
    for (name, value) in response.headers() {
        if name == reqwest::header::CONNECTION
            || name == reqwest::header::CONTENT_LENGTH
            || name == reqwest::header::TRANSFER_ENCODING
            || secrets
                .iter()
                .any(|secret| contains_bytes(value.as_bytes(), secret))
        {
            continue;
        }
        client.write_all(name.as_str().as_bytes())?;
        client.write_all(b": ")?;
        client.write_all(value.as_bytes())?;
        client.write_all(b"\r\n")?;
    }
    client.write_all(b"Connection: close\r\n\r\n")?;
    copy_redacting_many(&mut response, &mut client, &secrets)
}

fn validate_account_target(target: &str) -> Result<()> {
    ensure!(
        target.starts_with('/'),
        "authenticated HTTP requests require origin-form targets"
    );
    let path = target.split('?').next().unwrap_or(target);
    let lowered = path.to_ascii_lowercase();
    let decoded_separators = lowered.replace("%2f", "/");
    ensure!(
        !path.contains('\\')
            && !lowered.contains("%5c")
            && !lowered.contains("%2e")
            && !lowered.contains("%25")
            && !decoded_separators.split('/').any(|segment| segment == ".."),
        "authenticated HTTP request contains an unsafe path"
    );
    Ok(())
}

fn account_suffix<'a>(base: &str, target: &'a str) -> Option<&'a str> {
    let base = base.trim_end_matches('/');
    let path = target.split('?').next().unwrap_or(target);
    (base.is_empty()
        || path == base
        || path
            .strip_prefix(base)
            .is_some_and(|suffix| suffix.starts_with('/')))
    .then(|| &target[base.len()..])
}

fn account_origin(host: &str) -> Result<Url> {
    ensure!(
        !host.chars().any(char::is_whitespace) && !host.contains(['/', '\\', '@', '?', '#', '%']),
        "invalid direct account Host"
    );
    let origin = Url::parse(&format!("https://{host}/")).context("invalid direct account Host")?;
    ensure!(
        origin.username().is_empty()
            && origin.password().is_none()
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none(),
        "invalid direct account Host"
    );
    Ok(origin)
}

fn match_direct_http_route<'r, 't>(
    routes: &'r [AuthenticatedHttpRoute],
    target: &'t str,
    host: &str,
) -> Result<(&'r AuthenticatedHttpRoute, &'t str)> {
    validate_account_target(target)?;
    let origin = account_origin(host)?;
    let mut matched = None;
    for route in routes {
        let Some(base) = &route.direct_base else {
            continue;
        };
        if base.origin() == origin.origin()
            && let Some(suffix) = account_suffix(base.path(), target)
        {
            ensure!(matched.is_none(), "ambiguous direct account route");
            matched = Some((route, suffix));
        }
    }
    matched.context("no configured direct account route for this host and path")
}

fn match_authenticated_http_route<'r, 't>(
    routes: &'r [AuthenticatedHttpRoute],
    target: &'t str,
) -> Result<(&'r AuthenticatedHttpRoute, &'t str)> {
    validate_account_target(target)?;
    let path = target.split('?').next().unwrap_or(target);
    for route in routes {
        let prefix = format!("/{}", route.name);
        if path == prefix || path.starts_with(&format!("{prefix}/")) {
            return Ok((route, &target[prefix.len()..]));
        }
    }
    bail!("unknown authenticated HTTP route")
}

#[allow(clippy::too_many_arguments)]
fn handle_connect(
    mut client: UnixStream,
    remainder: Vec<u8>,
    box_root: &Path,
    session_id: &str,
    recorder: &EventRecorder,
    method: &str,
    host: &str,
    port: u16,
) -> Result<()> {
    let address = match authorize_and_resolve(box_root, session_id, host, port) {
        Ok(address) => address,
        Err(reason) => {
            return deny(
                &mut client,
                recorder,
                method,
                host,
                port,
                &reason.to_string(),
            );
        }
    };

    let mut upstream = TcpStream::connect_timeout(&address, Duration::from_secs(15))
        .with_context(|| format!("failed to connect to approved destination {host}:{port}"))?;
    upstream.set_nodelay(true)?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    if !remainder.is_empty() {
        upstream.write_all(&remainder)?;
    }
    relay(client, upstream)
}

#[allow(clippy::too_many_arguments)]
fn handle_plain_http(
    mut client: UnixStream,
    request: BufferedRequest,
    box_root: &Path,
    session_id: &str,
    recorder: &EventRecorder,
    method: &str,
    target: &str,
) -> Result<()> {
    let url = Url::parse(target).context("plain HTTP proxy requests must use an absolute URL")?;
    ensure!(url.scheme() == "http", "unsupported proxy URL scheme");
    let host = normalize_host(url.host_str().context("proxy URL has no host")?);
    let port = url.port_or_known_default().unwrap_or(80);

    let address = match authorize_and_resolve(box_root, session_id, &host, port) {
        Ok(address) => address,
        Err(reason) => {
            return deny(
                &mut client,
                recorder,
                method,
                &host,
                port,
                &reason.to_string(),
            );
        }
    };

    let mut upstream = TcpStream::connect_timeout(&address, Duration::from_secs(15))?;
    let path = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    ensure!(
        parsed.parse(&request.header)?.is_complete(),
        "incomplete proxy request"
    );

    write!(upstream, "{method} {path} HTTP/1.1\r\n")?;
    for header in parsed.headers.iter() {
        if header.name.eq_ignore_ascii_case("host")
            || header.name.eq_ignore_ascii_case("connection")
            || header.name.eq_ignore_ascii_case("proxy-connection")
        {
            continue;
        }
        upstream.write_all(header.name.as_bytes())?;
        upstream.write_all(b": ")?;
        upstream.write_all(header.value)?;
        upstream.write_all(b"\r\n")?;
    }
    let host_header = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    write!(upstream, "Host: {host_header}\r\nConnection: close\r\n\r\n")?;
    upstream.write_all(&request.remainder)?;
    relay(client, upstream)
}

fn authorize_and_resolve(
    box_root: &Path,
    session_id: &str,
    host: &str,
    port: u16,
) -> Result<SocketAddr> {
    let host = crate::network::normalize_destination(host)?;
    ensure!(
        is_allowed(box_root, session_id, &host, port)?,
        "no matching allow rule"
    );

    resolve_public(&host, port)
}

fn resolve_http_route(host: &str, port: u16, allow_private: bool) -> Result<SocketAddr> {
    if !allow_private {
        return resolve_public(host, port);
    }
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve {host}"))?
        .collect();
    ensure!(!addresses.is_empty(), "{host} did not resolve");
    addresses
        .into_iter()
        .find(|address| !address.ip().is_unspecified() && !address.ip().is_multicast())
        .with_context(|| format!("{host} did not resolve to a usable address"))
}

fn is_allowed(box_root: &Path, session_id: &str, host: &str, port: u16) -> Result<bool> {
    crate::network::is_allowed(box_root, session_id, host, port)
}

#[allow(clippy::too_many_arguments)]
fn deny(
    client: &mut UnixStream,
    recorder: &EventRecorder,
    method: &str,
    host: &str,
    port: u16,
    reason: &str,
) -> Result<()> {
    let event = recorder.record(method, host, port, reason)?;

    let body = format!(
        "SLOPBOX_EGRESS_DENIED\nrequest: {}\ndestination: {}\nreason: {}\n",
        event.id,
        crate::network::destination(&event.host, event.port),
        event.reason
    );
    write!(
        client,
        "HTTP/1.1 403 Forbidden\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nX-Slopbox-Request-Id: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        event.id,
        body
    )?;
    Ok(())
}

fn send_denials(client: &mut UnixStream, events_path: &Path) -> Result<()> {
    let mut body = read_events(events_path)?
        .into_iter()
        .map(|event| event.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    write!(
        client,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    Ok(())
}

fn relay(mut client: UnixStream, mut upstream: TcpStream) -> Result<()> {
    let mut client_read = client.try_clone()?;
    let mut upstream_write = upstream.try_clone()?;
    let upload = thread::spawn(move || {
        let result = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
        result
    });

    std::io::copy(&mut upstream, &mut client)?;
    let _ = client.shutdown(std::net::Shutdown::Write);
    upload
        .join()
        .map_err(|_| anyhow::anyhow!("proxy upload thread panicked"))??;
    Ok(())
}

fn parse_authority(value: &str, default_port: u16) -> Result<(String, u16)> {
    if let Ok(address) = SocketAddr::from_str(value) {
        return Ok((normalize_host(&address.ip().to_string()), address.port()));
    }
    if let Some(host) = value
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return Ok((normalize_host(host), default_port));
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        return Ok((normalize_host(host), port.parse()?));
    }
    Ok((normalize_host(value), default_port))
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

fn read_events(path: &Path) -> Result<Vec<Event>> {
    let mut bytes = crate::network::read_optional_bytes(path)?.unwrap_or_default();
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    bytes.truncate(complete);
    String::from_utf8(bytes)
        .context("invalid gateway event encoding")?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let event: Event = serde_json::from_str(line).context("invalid gateway event")?;
            request_session(&event.id)?;
            Ok(event)
        })
        .collect()
}

impl std::fmt::Display for Event {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} {} {} ({})",
            self.id,
            crate::launch::terminal_text(&self.method),
            crate::launch::terminal_text(&crate::network::destination(&self.host, self.port)),
            crate::launch::terminal_text(&self.reason)
        )
    }
}

pub(crate) fn session_is_active(session_dir: &Path) -> Result<bool> {
    let lock = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(session_dir.join("active.lock"))
    {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        lock.metadata()?.is_file(),
        "session lock must be a regular file"
    );
    match lock.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn request_session(request_id: &str) -> Result<&str> {
    let rest = request_id
        .strip_prefix("req-")
        .context("invalid request ID")?;
    let (session, counter) = rest.split_once('-').context("invalid request ID")?;
    validate_session_id(session)?;
    ensure!(
        !counter.is_empty()
            && counter.bytes().all(|byte| byte.is_ascii_digit())
            && counter.parse::<u64>()? > 0,
        "invalid request ID"
    );
    Ok(session)
}

pub(crate) fn validate_session_id(session: &str) -> Result<()> {
    ensure!(
        !session.is_empty()
            && session.len() <= 128
            && session.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        "invalid session ID"
    );
    Ok(())
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos)
}

fn secure_dir(path: &Path) -> Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn create_private_file(path: &Path) -> Result<()> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn append_private(path: &Path, value: &str) -> Result<()> {
    create_private_file(path)?;
    OpenOptions::new()
        .append(true)
        .open(path)?
        .write_all(value.as_bytes())?;
    Ok(())
}

fn sanitize(value: &str) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testing::{exchange, read_tcp_request};
    use std::io::Read;
    use std::net::Ipv4Addr;

    #[test]
    fn accepted_connections_block_while_the_listener_stays_nonblocking() {
        use std::os::fd::AsRawFd;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("broker.sock");
        let listener = bind_listener(&path).unwrap();
        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client
            .write_all(b"POST /openai-codex/codex/responses HTTP/1.1\r\n")
            .unwrap();
        let server = accept_connection(&listener).unwrap();
        let flags = unsafe { libc::fcntl(server.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );

        let client = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            client
                .write_all(b"Host: local\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).unwrap();
            response
        });
        crate::provider::Providers::default()
            .handle(server)
            .unwrap();
        assert!(
            client
                .join()
                .unwrap()
                .starts_with("HTTP/1.1 503 Service Unavailable\r\n")
        );
    }

    #[test]
    fn rules_cannot_override_reserved_address_checks() {
        let root = tempfile::tempdir().unwrap();
        let rule = crate::network::Rule {
            id: "rule-000000000000000000000001".into(),
            host: "127.0.0.1".into(),
            port: 443,
            scope: ApprovalScope::Project,
            session_id: None,
            created_at_ms: 1,
            revoked_at_ms: None,
            request_id: "req-session-1".into(),
        };
        fs::write(
            root.path().join("network-rules.json"),
            serde_json::to_vec(&serde_json::json!({"rules": [rule]})).unwrap(),
        )
        .unwrap();
        assert!(is_allowed(root.path(), "future", "127.0.0.1", 443).unwrap());
        let error = authorize_and_resolve(root.path(), "future", "127.0.0.1", 443).unwrap_err();
        assert!(error.to_string().contains("reserved address"));
    }

    #[test]
    fn event_readers_ignore_incomplete_appends_and_escape_terminal_controls() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("events.jsonl");
        EventRecorder::new("session", &path)
            .record("CONNECT", "example.com", 443, "no matching allow rule")
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"partial \xf0\x9f")
            .unwrap();
        let events = read_events(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].created_at_ms > 0);
        assert_eq!(request_session(&events[0].id).unwrap(), "session");
        let mut event = events[0].clone();
        event.reason = "unsafe\u{1b}[2J\ntext".into();
        assert!(!event.to_string().contains(['\u{1b}', '\n']));
    }

    #[test]
    fn request_ids_cannot_select_paths_outside_the_session_directory() {
        for id in [
            "req-../outside-1",
            "req-..-1",
            "req--1",
            "req-session-0",
            "req-session-1/../x",
            "req-session-extra-1",
        ] {
            assert!(request_session(id).is_err(), "{id}");
        }
        assert_eq!(request_session("req-abc123-42").unwrap(), "abc123");
    }

    #[test]
    fn parses_authorities() {
        assert_eq!(
            parse_authority("Example.COM:8443", 443).unwrap(),
            ("example.com".into(), 8443)
        );
        assert_eq!(
            parse_authority("[2001:db8::1]:443", 80).unwrap(),
            ("2001:db8::1".into(), 443)
        );
    }

    #[test]
    fn deduplicates_identical_denials() {
        let directory = tempfile::tempdir().unwrap();
        let events = directory.path().join("events.jsonl");
        let recorder = EventRecorder::new("session", &events);

        let first = recorder
            .record("CONNECT", "Example.COM", 443, "no matching allow rule")
            .unwrap();
        let second = recorder
            .record("CONNECT", "example.com", 443, "no matching allow rule")
            .unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(read_events(&events).unwrap().len(), 1);
    }

    #[test]
    fn concurrent_sessions_have_independent_approvals_and_events() {
        let root = tempfile::tempdir().unwrap();
        let first_dir = root.path().join("sessions/first");
        let second_dir = root.path().join("sessions/second");
        fs::create_dir_all(&first_dir).unwrap();
        fs::create_dir_all(&second_dir).unwrap();
        let first_lock = File::create(first_dir.join("active.lock")).unwrap();
        first_lock.lock().unwrap();
        let second_lock = File::create(second_dir.join("active.lock")).unwrap();
        second_lock.lock().unwrap();
        let first = EventRecorder::new("first", &first_dir.join("events.jsonl"))
            .record("CONNECT", "first.example", 443, "no matching allow rule")
            .unwrap();
        let second = EventRecorder::new("second", &second_dir.join("events.jsonl"))
            .record("CONNECT", "second.example", 443, "no matching allow rule")
            .unwrap();

        let events = all_events(root.path()).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.id == first.id));
        assert!(events.iter().any(|event| event.id == second.id));
        approve(root.path(), &first.id, ApprovalScope::Session).unwrap();
        approve(root.path(), &second.id, ApprovalScope::Session).unwrap();
        assert!(is_allowed(root.path(), "first", "first.example", 443).unwrap());
        assert!(!is_allowed(root.path(), "second", "first.example", 443).unwrap());
        assert!(is_allowed(root.path(), "second", "second.example", 443).unwrap());
        assert!(!is_allowed(root.path(), "first", "second.example", 443).unwrap());

        drop(second_lock);
        let observer = File::open(second_dir.join("active.lock")).unwrap();
        observer.lock_shared().unwrap();
        assert!(!session_is_active(&second_dir).unwrap());
        assert!(approve(root.path(), &second.id, ApprovalScope::Session).is_err());
        assert_eq!(all_events(root.path()).unwrap().len(), 2);
        approve(root.path(), &first.id, ApprovalScope::Session).unwrap();

        drop(first_lock);
        // Another test's fork may briefly retain the descriptor until exec.
        let observer = File::open(first_dir.join("active.lock")).unwrap();
        observer.lock_shared().unwrap();
        assert!(!session_is_active(&first_dir).unwrap());
        assert!(approve(root.path(), &first.id, ApprovalScope::Session).is_err());
        assert_eq!(all_events(root.path()).unwrap().len(), 2);
        approve(root.path(), &first.id, ApprovalScope::Project).unwrap();
        assert!(is_allowed(root.path(), "future", "first.example", 443).unwrap());
    }

    #[test]
    fn separates_general_and_model_routes() {
        let directory = tempfile::tempdir().unwrap();
        let events = directory.path().join("events.jsonl");
        let recorder = EventRecorder::new("session", &events);

        let general_response = exchange(
            b"POST /openrouter/api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            |stream| handle_general_connection(stream, directory.path(), "session", &recorder),
        );
        assert!(general_response.starts_with("HTTP/1.1 404 Not Found\r\n"));

        let model_response = exchange(
            b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n",
            |stream| Providers::default().handle(stream),
        );
        assert!(model_response.starts_with("HTTP/1.1 404 Not Found\r\n"));

        let unavailable_response = exchange(
            b"POST /openrouter/api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            |stream| Providers::default().handle(stream),
        );
        assert!(unavailable_response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));

        let codex_response = exchange(
            b"POST /openai-codex/codex/responses HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            |stream| Providers::default().handle(stream),
        );
        assert!(codex_response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    }

    #[test]
    fn direct_accounts_require_an_opted_in_origin_and_path() {
        let route = AuthenticatedHttpRoute::new(
            "github".into(),
            "https://api.github.com/repos/owner/repository".into(),
            vec!["GET".into()],
            HttpAuthentication::Bearer {
                secret: Arc::from("host-secret"),
            },
            false,
            true,
        )
        .unwrap();
        let routes = std::slice::from_ref(&route);
        let (matched, suffix) = match_direct_http_route(
            routes,
            "/repos/owner/repository/issues?state=open",
            "API.GITHUB.COM:443",
        )
        .unwrap();
        assert_eq!(matched.name(), "github");
        assert_eq!(suffix, "/issues?state=open");
        for (host, path) in [
            ("other.example", "/repos/owner/repository/issues"),
            ("api.github.com:444", "/repos/owner/repository/issues"),
            (
                "api.github.com@other.example",
                "/repos/owner/repository/issues",
            ),
            ("api.github.com/path", "/repos/owner/repository/issues"),
            ("api.github.com", "/repos/owner/repository-other"),
            ("api.github.com", "/repos/other/repository"),
            ("api.github.com", "/repos/owner/repository/../other"),
            ("api.github.com", "/repos/owner/repository/%2e%2e/other"),
            ("api.github.com", "/github/issues"),
        ] {
            assert!(
                match_direct_http_route(routes, path, host).is_err(),
                "{host} {path}"
            );
        }
        assert!(
            match_direct_http_route(
                &[route.clone(), route.clone()],
                "/repos/owner/repository",
                "api.github.com"
            )
            .is_err()
        );
        let mut disabled = route;
        disabled.direct_base = None;
        assert!(
            match_direct_http_route(&[disabled], "/repos/owner/repository", "api.github.com")
                .is_err()
        );
    }

    #[test]
    fn direct_accounts_reject_disallowed_methods_and_duplicate_host_headers() {
        let route = AuthenticatedHttpRoute::new(
            "github".into(),
            "https://api.github.com".into(),
            vec!["GET".into()],
            HttpAuthentication::Bearer {
                secret: Arc::from("host-secret"),
            },
            false,
            true,
        )
        .unwrap();
        let response = exchange(
            b"DELETE /repos/owner/repository HTTP/1.1\r\nHost: api.github.com\r\n\r\n",
            |stream| {
                handle_authenticated_http_connection(stream, std::slice::from_ref(&route), true)
            },
        );
        assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        let response = exchange(
            b"GET /user HTTP/1.1\r\nHost: api.github.com\r\nHost: other.example\r\n\r\n",
            |stream| {
                assert!(
                    handle_authenticated_http_connection(
                        stream,
                        std::slice::from_ref(&route),
                        true
                    )
                    .is_err()
                );
                Ok(())
            },
        );
        assert!(response.is_empty());
    }

    #[test]
    fn authenticated_http_routes_confine_credentials_and_paths() {
        use std::sync::mpsc;

        let token = "host-route-token";
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            request_tx.send(read_tcp_request(&mut stream)).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nX-Reflected: {token}\r\nConnection: close\r\n\r\nbefore-{token}-after"
            )
            .unwrap();
        });
        let route = AuthenticatedHttpRoute {
            name: "git".to_owned(),
            upstream_base: Url::parse("http://route.test/repository").unwrap(),
            origin: Url::parse("https://route.test/repository").unwrap(),
            direct_base: None,
            proxy: false,
            tls: None,
            methods: ["POST".to_owned()].into(),
            authorization: Arc::from("Basic cGk6aG9zdC1yb3V0ZS10b2tlbg=="),
            secret: Arc::from(token),
            resolved: Some(address),
            connect_timeout: Duration::from_secs(1),
            load_system_roots: false,
            test_root: None,
            allow_private_addresses: true,
        };
        let body = b"git-payload";
        let response = exchange(
            format!(
                "POST /git/git-receive-pack HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer guest\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            )
            .as_bytes(),
            |stream| handle_authenticated_http_connection(stream, &[route], false),
        );
        let request = String::from_utf8(request_rx.recv().unwrap()).unwrap();
        assert!(request.starts_with("POST /repository/git-receive-pack HTTP/1.1\r\n"));
        assert!(request.contains("authorization: Basic cGk6aG9zdC1yb3V0ZS10b2tlbg==\r\n"));
        assert!(!request.contains("Bearer guest"));
        assert!(request.ends_with("git-payload"));
        assert!(!response.contains(token));
        assert!(response.contains("before-[REDACTED]-after"));
        upstream.join().unwrap();

        let route = AuthenticatedHttpRoute::new(
            "safe".to_owned(),
            "https://example.com/repository".to_owned(),
            vec!["GET".to_owned()],
            HttpAuthentication::Bearer {
                secret: Arc::from("secret"),
            },
            false,
            false,
        )
        .unwrap();
        assert!(
            match_authenticated_http_route(std::slice::from_ref(&route), "/safe/info/refs").is_ok()
        );
        assert!(
            match_authenticated_http_route(std::slice::from_ref(&route), "/safeish/info").is_err()
        );
        assert!(
            match_authenticated_http_route(std::slice::from_ref(&route), "/safe/%2e%2e/admin")
                .is_err()
        );
        assert!(
            match_authenticated_http_route(std::slice::from_ref(&route), "/safe/raw/file%2Fname")
                .is_ok()
        );
        assert!(
            match_authenticated_http_route(std::slice::from_ref(&route), "/safe/raw/..%2Fadmin")
                .is_err()
        );
    }
}
