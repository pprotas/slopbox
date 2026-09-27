use super::*;
use crate::gateway::{HttpAuthentication, handle_account_stream};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

fn route() -> AuthenticatedHttpRoute {
    AuthenticatedHttpRoute::new(
        "account".into(),
        "https://forge.example.test/api".into(),
        vec!["GET".into()],
        HttpAuthentication::Bearer {
            secret: Arc::from("disposable-upstream-secret"),
        },
        false,
        false,
    )
    .unwrap()
    .with_proxy(true)
}

fn accept(listener: TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "fixture accept timed out");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

fn connect(
    routes: Vec<AuthenticatedHttpRoute>,
    pem: &str,
) -> (
    StreamOwned<ClientConnection, UnixStream>,
    JoinHandle<Result<()>>,
) {
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let handler = thread::spawn(move || handle_account_stream(server, &routes, false));
    client
        .write_all(
            b"CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test:443\r\n\r\n",
        )
        .unwrap();
    assert!(
        read_request_header(&mut client)
            .unwrap()
            .header
            .starts_with(b"HTTP/1.1 200")
    );
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(pem.as_bytes()).unwrap())
        .unwrap();
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection = ClientConnection::new(
        Arc::new(config),
        ServerName::try_from("forge.example.test").unwrap(),
    )
    .unwrap();
    (StreamOwned::new(connection, client), handler)
}

fn upstream(response: String) -> (AuthenticatedHttpRoute, JoinHandle<Result<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let certificate =
        rcgen::generate_simple_self_signed(vec!["forge.example.test".into()]).unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
        )
        .unwrap();
    let mut route = route();
    route.resolved = Some(address);
    route.load_system_roots = false;
    route.test_root = Some(reqwest::Certificate::from_der(certificate.cert.der()).unwrap());
    let handler = thread::spawn(move || {
        let mut stream =
            StreamOwned::new(ServerConnection::new(Arc::new(config))?, accept(listener));
        let request = read_request_header(&mut stream)?;
        stream.write_all(response.as_bytes())?;
        stream.conn.send_close_notify();
        stream.flush()?;
        Ok(String::from_utf8(request.header)?)
    });
    (route, handler)
}

#[test]
fn decrypted_requests_cannot_change_authority_path_or_framing() {
    for request in [
        "GET /api HTTP/1.1\r\nHost: other.example.test\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test:444\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test\r\nHost: forge.example.test\r\n\r\n",
        "GET /api HTTP/1.1\r\n\r\n",
        "GET https://forge.example.test/api HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "GET /api-other HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "GET /api/../other HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "GET /api/%2e%2e/other HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "GET /api/%252e%252e/other HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test\r\nTransfer-Encoding: identity\r\nContent-Length: 0\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
        "GET /api HTTP/1.1\r\nHost: forge.example.test\r\nContent-Length: 0\r\n\r\nexcess",
    ] {
        let mut routes = vec![route()];
        let pem = prepare(&mut routes).unwrap().unwrap();
        let (mut client, handler) = connect(routes, &pem);
        client.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = client.read_to_string(&mut response);
        assert!(response.is_empty(), "{request}: {response}");
        drop(client);
        assert!(handler.join().unwrap().is_err(), "{request}");
    }
}

#[test]
fn mediated_upstream_authentication_is_verified_and_reflections_are_redacted() {
    let (route, upstream) = upstream("HTTP/1.1 200 OK\r\nX-Reflected: disposable-upstream-secret\r\nConnection: close\r\n\r\nbefore disposable-upstream-secret after".into());
    let mut routes = vec![route];
    let pem = prepare(&mut routes).unwrap().unwrap();
    let (mut client, handler) = connect(routes, &pem);
    client.write_all(b"GET /api/repository?query=1 HTTP/1.1\r\nHost: forge.example.test\r\nAuthorization: Bearer guest\r\nProxy-Authorization: Basic guest\r\nAccept-Encoding: gzip\r\n\r\n").unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    handler.join().unwrap().unwrap();
    let request = upstream.join().unwrap().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("get /api/repository?query=1 http/1.1\r\n"));
    assert!(request.contains("authorization: bearer disposable-upstream-secret\r\n"));
    assert_eq!(request.matches("authorization:").count(), 1);
    assert!(request.contains("accept-encoding: identity\r\n"));
    assert!(!request.contains("guest"));
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.ends_with("before [REDACTED] after"));
    assert!(!response.contains("disposable-upstream-secret"));
    assert!(!response.to_ascii_lowercase().contains("x-reflected:"));
}

#[test]
fn session_trust_does_not_authorize_an_untrusted_upstream() {
    let (mut route, upstream) = upstream("HTTP/1.1 200 OK\r\n\r\n".into());
    route.test_root = None;
    let mut routes = vec![route];
    let pem = prepare(&mut routes).unwrap().unwrap();
    let (mut client, handler) = connect(routes, &pem);
    client
        .write_all(b"GET /api HTTP/1.1\r\nHost: forge.example.test\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 502"));
    handler.join().unwrap().unwrap();
    assert!(upstream.join().unwrap().is_err());
}

#[test]
fn redirects_are_not_followed_and_encoded_reflections_are_rejected() {
    for (upstream_response, status) in [
        (
            "HTTP/1.1 302 Found\r\nLocation: https://elsewhere.example.test/secret\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 302",
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 502",
        ),
    ] {
        let (route, upstream) = upstream(upstream_response.into());
        let mut routes = vec![route];
        let pem = prepare(&mut routes).unwrap().unwrap();
        let (mut client, handler) = connect(routes, &pem);
        client
            .write_all(b"GET /api HTTP/1.1\r\nHost: forge.example.test\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with(status), "{response}");
        handler.join().unwrap().unwrap();
        upstream.join().unwrap().unwrap();
    }
}

#[test]
#[ignore = "protocol integration: requires curl and Node.js on PATH"]
fn curl_and_node_use_the_same_explicit_proxy_and_session_trust() {
    for client in ["curl", "node"] {
        let (route, upstream) = upstream(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ndisposable-upstream-secret".into(),
        );
        let mut routes = vec![route];
        let pem = prepare(&mut routes).unwrap().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let ca = directory.path().join("ca.pem");
        std::fs::write(&ca, pem).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = format!("http://{}", listener.local_addr().unwrap());
        let handler =
            thread::spawn(move || handle_account_stream(accept(listener), &routes, false));
        let mut command = Command::new(client);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap());
        if client == "curl" {
            command
                .args([
                    "--silent",
                    "--show-error",
                    "--fail",
                    "--max-time",
                    "10",
                    "--noproxy",
                    "",
                    "--proxy",
                    &proxy,
                    "--cacert",
                ])
                .arg(&ca)
                .arg("https://forge.example.test/api/repository");
        } else {
            command
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/account-client.mjs"
                ))
                .arg(&proxy)
                .arg(&ca)
                .arg("https://forge.example.test/api/repository");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{client}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("[REDACTED]"));
        handler.join().unwrap().unwrap();
        let request = upstream.join().unwrap().unwrap();
        assert!(request.contains("authorization: Bearer disposable-upstream-secret\r\n"));
    }
}

#[test]
fn mediation_is_opt_in_and_unmatched_connects_are_denied() {
    let mut disabled = vec![route().with_proxy(false)];
    assert!(prepare(&mut disabled).unwrap().is_none());
    assert!(disabled[0].tls.is_none());
    for authority in ["elsewhere.example.test:443", "forge.example.test:444"] {
        let mut io = Cursor::new(
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").into_bytes(),
        );
        let result = handle_account_stream(&mut io, &[route()], false);
        assert!(result.is_ok());
        assert!(
            String::from_utf8(io.into_inner())
                .unwrap()
                .contains("403 Forbidden")
        );
    }
}

#[test]
fn verified_tls_reaches_the_existing_method_guard() {
    let mut routes = vec![route()];
    let pem = prepare(&mut routes).unwrap().unwrap();
    assert!(!pem.contains("PRIVATE KEY"));
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    server
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let handler = std::thread::spawn(move || handle_account_stream(server, &routes, false));
    client
        .write_all(
            b"CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test:443\r\n\r\n",
        )
        .unwrap();
    let connected = read_request_header(&mut client).unwrap();
    assert!(connected.header.starts_with(b"HTTP/1.1 200"));
    assert!(connected.remainder.is_empty());
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from_pem_slice(pem.as_bytes()).unwrap())
        .unwrap();
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection = ClientConnection::new(
        Arc::new(config),
        ServerName::try_from("forge.example.test").unwrap(),
    )
    .unwrap();
    let mut client = StreamOwned::new(connection, client);
    client
        .write_all(b"DELETE /api/repository HTTP/1.1\r\nHost: forge.example.test\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed"));
    handler.join().unwrap().unwrap();
}

#[test]
fn tls_trust_and_connect_authority_cannot_be_substituted() {
    for (name, trust) in [("forge.example.test", false), ("other.example.test", true)] {
        let mut routes = vec![route()];
        let pem = prepare(&mut routes).unwrap().unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handler = std::thread::spawn(move || handle_account_stream(server, &routes, false));
        client
            .write_all(
                b"CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test:443\r\n\r\n",
            )
            .unwrap();
        assert!(
            read_request_header(&mut client)
                .unwrap()
                .header
                .starts_with(b"HTTP/1.1 200")
        );
        let mut roots = RootCertStore::empty();
        if trust {
            roots
                .add(CertificateDer::from_pem_slice(pem.as_bytes()).unwrap())
                .unwrap();
        }
        let config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut client = StreamOwned::new(
            ClientConnection::new(Arc::new(config), ServerName::try_from(name).unwrap()).unwrap(),
            client,
        );
        assert!(
            client
                .write_all(b"GET /api HTTP/1.1\r\nHost: forge.example.test\r\n\r\n")
                .is_err()
        );
        drop(client);
        assert!(handler.join().unwrap().is_err());
    }
}

#[test]
fn server_rejects_missing_or_substituted_sni_even_with_valid_client_trust() {
    for substitute in [false, true] {
        let mut other = route();
        other.origin = url::Url::parse("https://other.example.test/api").unwrap();
        let mut routes = vec![route(), other];
        let pem = prepare(&mut routes).unwrap().unwrap();
        if substitute {
            // Give the client a valid certificate for its substituted SNI, so only the broker's binding rejects it.
            routes[0].tls = routes[1].tls.clone();
        }
        routes.truncate(1);
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let handler = thread::spawn(move || handle_account_stream(server, &routes, false));
        client
            .write_all(
                b"CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
            )
            .unwrap();
        assert!(
            read_request_header(&mut client)
                .unwrap()
                .header
                .starts_with(b"HTTP/1.1 200")
        );
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_slice(pem.as_bytes()).unwrap())
            .unwrap();
        let mut config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.enable_sni = substitute;
        let name = if substitute {
            "other.example.test"
        } else {
            "forge.example.test"
        };
        let mut client = StreamOwned::new(
            ClientConnection::new(Arc::new(config), ServerName::try_from(name).unwrap()).unwrap(),
            client,
        );
        let _ = client.write_all(b"GET /api HTTP/1.1\r\nHost: forge.example.test\r\n\r\n");
        let mut response = String::new();
        let _ = client.read_to_string(&mut response);
        assert!(response.is_empty());
        assert!(
            handler
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("TLS server name does not match")
        );
    }
}

#[test]
fn connect_rejects_duplicate_hosts_authority_confusion_and_request_bodies() {
    for request in [
        "CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\nHost: other.example.test\r\n\r\n",
        "CONNECT forge.example.test:443 HTTP/1.1\r\nHost: other.example.test\r\n\r\n",
        "CONNECT forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\nContent-Length: 0\r\n\r\n",
        "CONNECT user@forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "CONNECT @forge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
        "CONNECT %66orge.example.test:443 HTTP/1.1\r\nHost: forge.example.test\r\n\r\n",
    ] {
        assert!(
            handle_account_stream(Cursor::new(request.as_bytes().to_vec()), &[route()], false)
                .is_err()
        );
    }
}
