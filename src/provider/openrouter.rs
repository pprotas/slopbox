use std::os::unix::net::UnixStream;
use std::sync::Arc;

use anyhow::Result;

use super::transport::{self, Target};
use crate::http::{BufferedRequest, send_simple_response};

pub(super) const ROUTE: &str = "/openrouter/api/v1/chat/completions";

pub(super) struct Provider {
    key: Arc<str>,
}

pub(super) fn configured() -> bool {
    std::env::var("OPENROUTER_API_KEY").is_ok_and(|value| !value.is_empty())
}

impl Provider {
    pub fn discover() -> Option<Self> {
        std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|value| !value.is_empty())
            .map(|key| Self {
                key: Arc::from(key),
            })
    }

    pub fn handle(&self, client: &mut UnixStream, request: BufferedRequest) -> Result<()> {
        let target = match Target::new(
            "https://openrouter.ai/api/v1/chat/completions",
            "openrouter.ai",
        ) {
            Ok(target) => target,
            Err(_) => {
                return send_simple_response(
                    client,
                    502,
                    "Bad Gateway",
                    "OpenRouter upstream request failed\n",
                );
            }
        };
        forward(client, request, &self.key, &target)
    }
}

fn forward(
    client: &mut UnixStream,
    request: BufferedRequest,
    key: &str,
    target: &Target,
) -> Result<()> {
    transport::forward(client, request, key, target, "OpenRouter", &[], &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::testing::{exchange, read_tcp_request};
    use crate::http::{contains_bytes, read_request_header};
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;
    #[test]
    fn openrouter_streams_and_confines_authentication() {
        use std::sync::mpsc;

        let key = "host-secret-cross-boundary";
        let guest_key = "guest-marker";
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_tcp_request(&mut stream);
            request_tx.send(request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nX-Upstream: yes\r\nX-Reflected: {key}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream
                .write_all(b"stream-start-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")
                .unwrap();
            stream.flush().unwrap();
            continue_rx.recv().unwrap();
            stream.write_all(b"before-host-secret-cross-").unwrap();
            stream.flush().unwrap();
            thread::sleep(Duration::from_millis(30));
            stream.write_all(b"boundary-after").unwrap();
        });

        let target = test_openrouter_target(address);
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || {
            let request = read_request_header(&mut server).unwrap();
            forward(&mut server, request, key, &target)
        });
        let body = br#"{"model":"test","messages":[]}"#;
        write!(
            client,
            "POST /openrouter/api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {guest_key}\r\nAccept-Encoding: gzip\r\nX-Test: retained\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .unwrap();
        client.write_all(body).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let upstream_request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let upstream_request = String::from_utf8(upstream_request).unwrap();
        assert!(upstream_request.starts_with("POST /api/v1/chat/completions HTTP/1.1\r\n"));
        assert!(upstream_request.contains(&format!("authorization: Bearer {key}\r\n")));
        assert!(!upstream_request.contains(guest_key));
        assert!(upstream_request.contains("accept-encoding: identity\r\n"));
        assert!(upstream_request.contains("x-test: retained\r\n"));
        assert!(upstream_request.ends_with(std::str::from_utf8(body).unwrap()));

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut response = Vec::new();
        let mut buffer = [0_u8; 256];
        while !contains_bytes(&response, b"stream-start-") {
            let count = client.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            response.extend_from_slice(&buffer[..count]);
        }
        continue_tx.send(()).unwrap();
        client.read_to_end(&mut response).unwrap();

        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("x-upstream: yes\r\n"));
        assert!(!response.contains("x-reflected:"));
        assert!(!response.contains(key));
        assert!(response.contains("before-[REDACTED]-after"));
        handler.join().unwrap().unwrap();
        upstream.join().unwrap();
    }
    #[test]
    fn openrouter_handles_redirects_encoded_responses_and_failures() {
        let (target, upstream) = mock_openrouter(
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/leak\r\nContent-Length: 9\r\nConnection: close\r\n\r\nredirect\n".to_vec(),
        );
        let response = openrouter_exchange(&target, "host-key");
        assert!(response.starts_with("HTTP/1.1 302 Found\r\n"));
        assert!(response.ends_with("redirect\n"));
        upstream.join().unwrap();

        let (target, upstream) = mock_openrouter(
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 13\r\nConnection: close\r\n\r\nrate limited\n".to_vec(),
        );
        let response = openrouter_exchange(&target, "host-key");
        assert!(response.starts_with("HTTP/1.1 429 Too Many Requests\r\n"));
        assert!(response.ends_with("rate limited\n"));
        upstream.join().unwrap();

        let (target, upstream) = mock_openrouter(
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 8\r\nConnection: close\r\n\r\nhost-key".to_vec(),
        );
        let response = openrouter_exchange(&target, "host-key");
        assert!(response.starts_with("HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(!response.contains("host-key"));
        upstream.join().unwrap();

        let unused = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = unused.local_addr().unwrap();
        drop(unused);
        let response = openrouter_exchange(&test_openrouter_target(address), "host-key");
        assert!(response.starts_with("HTTP/1.1 502 Bad Gateway\r\n"));
        assert!(!response.contains("host-key"));
    }
    #[test]
    fn openrouter_stops_streaming_after_client_disconnect() {
        use std::sync::mpsc;

        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (streaming_tx, streaming_rx) = mpsc::channel();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_tcp_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                .unwrap();
            for index in 0..100 {
                if stream
                    .write_all(&[b'x'; 1024])
                    .and_then(|()| stream.flush())
                    .is_err()
                {
                    return true;
                }
                if index == 0 {
                    streaming_tx.send(()).unwrap();
                }
                thread::sleep(Duration::from_millis(20));
            }
            false
        });

        let target = test_openrouter_target(address);
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let request = read_request_header(&mut server).unwrap();
            let result = forward(&mut server, request, "host-key", &target);
            done_tx.send(result.is_err()).unwrap();
        });
        client
            .write_all(b"POST /openrouter/api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        streaming_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut response = [0_u8; 256];
        assert_ne!(client.read(&mut response).unwrap(), 0);
        drop(client);

        assert!(done_rx.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(upstream.join().unwrap());
    }
    fn mock_openrouter(response: Vec<u8>) -> (Target, JoinHandle<()>) {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let upstream = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_tcp_request(&mut stream);
            stream.write_all(&response).unwrap();
        });
        (test_openrouter_target(address), upstream)
    }
    fn test_openrouter_target(address: SocketAddr) -> Target {
        Target {
            url: format!("http://{address}/api/v1/chat/completions"),
            resolved: None,
            connect_timeout: Duration::from_millis(250),
            load_system_roots: false,
        }
    }
    fn openrouter_exchange(target: &Target, key: &str) -> String {
        exchange(
            b"POST /openrouter/api/v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
            |mut stream| {
                let request = read_request_header(&mut stream)?;
                forward(&mut stream, request, key, target)
            },
        )
    }
}
