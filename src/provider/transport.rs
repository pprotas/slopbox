use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Result, ensure};

use crate::http::{
    BufferedRequest, contains_bytes, copy_redacting_many, read_retry, resolve_public,
    send_simple_response,
};

pub(super) struct Target {
    pub url: String,
    pub resolved: Option<(&'static str, SocketAddr)>,
    pub connect_timeout: Duration,
    pub load_system_roots: bool,
}

impl Target {
    pub fn new(url: &str, host: &'static str) -> Result<Self> {
        #[cfg(test)]
        if host == "openrouter.ai"
            && let Ok(address) = std::env::var("SLOPBOX_TEST_OPENROUTER")
        {
            let address: SocketAddr = address.parse()?;
            ensure!(address.ip().is_loopback(), "test upstream must be loopback");
            return Ok(Self {
                url: format!("http://{address}/api/v1/chat/completions"),
                resolved: None,
                connect_timeout: Duration::from_secs(2),
                load_system_roots: false,
            });
        }
        Ok(Self {
            url: url.to_owned(),
            resolved: Some((host, resolve_public(host, 443)?)),
            connect_timeout: Duration::from_secs(15),
            load_system_roots: true,
        })
    }
}

pub(super) fn forward(
    client: &mut UnixStream,
    request: BufferedRequest,
    key: &str,
    target: &Target,
    provider_name: &str,
    injected_headers: &[(&str, &str)],
    additional_response_secrets: &[&[u8]],
) -> Result<()> {
    const MAX_BODY_SIZE: usize = 64 * 1024 * 1024;

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    ensure!(
        parsed.parse(&request.header)?.is_complete(),
        "incomplete provider request"
    );

    let content_length = parsed
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("content-length"))
        .map(|header| std::str::from_utf8(header.value))
        .transpose()?
        .map(str::parse::<usize>)
        .transpose()?
        .unwrap_or(0);
    ensure!(
        content_length <= MAX_BODY_SIZE,
        "provider request body is too large"
    );
    ensure!(
        !parsed.headers.iter().any(|header| {
            header.name.eq_ignore_ascii_case("transfer-encoding")
                && !header.value.eq_ignore_ascii_case(b"identity")
        }),
        "chunked provider requests are not supported"
    );

    let mut body = request.remainder;
    ensure!(
        body.len() <= content_length,
        "provider request contains excess body data"
    );
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let mut buffer = vec![0_u8; remaining.min(16 * 1024)];
        let count = read_retry(client, &mut buffer)?;
        ensure!(
            count != 0,
            "provider client closed before sending its request body"
        );
        body.extend_from_slice(&buffer[..count]);
    }

    let mut client_builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(target.connect_timeout);
    if !target.load_system_roots {
        client_builder = client_builder.tls_certs_only(std::iter::empty::<reqwest::Certificate>());
    }
    if let Some((host, address)) = target.resolved {
        client_builder = client_builder.resolve(host, address);
    }
    let client_builder = client_builder.build()?;
    let mut upstream = client_builder
        .post(&target.url)
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT_ENCODING, "identity");

    for header in parsed.headers.iter() {
        if header.name.eq_ignore_ascii_case("authorization")
            || injected_headers
                .iter()
                .any(|(name, _)| header.name.eq_ignore_ascii_case(name))
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
        upstream = upstream.header(name, value);
    }
    for (name, value) in injected_headers {
        upstream = upstream.header(*name, *value);
    }

    let mut response = match upstream.body(body).send() {
        Ok(response) => response,
        Err(_) => {
            return send_simple_response(
                client,
                502,
                "Bad Gateway",
                &format!("{provider_name} upstream request failed\n"),
            );
        }
    };
    if response
        .headers()
        .get(reqwest::header::CONTENT_ENCODING)
        .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return send_simple_response(
            client,
            502,
            "Bad Gateway",
            &format!("encoded {provider_name} responses are not supported\n"),
        );
    }

    let response_secrets: Vec<&[u8]> = std::iter::once(key.as_bytes())
        .chain(additional_response_secrets.iter().copied())
        .collect();
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
            || response_secrets
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
    copy_redacting_many(&mut response, client, &response_secrets)?;
    Ok(())
}
