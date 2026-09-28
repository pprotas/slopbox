use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use super::AuthenticatedHttpRoute;
use crate::http::{BufferedRequest, read_request_header, send_simple_response};

#[cfg(test)]
mod tests;

pub(super) fn prepare(routes: &mut [AuthenticatedHttpRoute]) -> Result<Option<String>> {
    if !routes.iter().any(|route| route.proxy) {
        return Ok(None);
    }
    let mut params = parameters(Vec::new())?;
    params
        .distinguished_name
        .push(DnType::CommonName, "Slopbox session authority");
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate()?)?;
    let mut servers = HashMap::new();
    for route in routes.iter_mut().filter(|route| route.proxy) {
        let origin = route.origin.origin().ascii_serialization();
        if !servers.contains_key(&origin) {
            let host = match route.origin.host().context("account origin has no host")? {
                url::Host::Domain(host) => host.to_owned(),
                url::Host::Ipv4(address) => address.to_string(),
                url::Host::Ipv6(address) => address.to_string(),
            };
            let mut params = parameters(vec![host])?;
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
            let key = KeyPair::generate()?;
            let certificate = params.signed_by(&key, &issuer)?;
            let mut config = ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![certificate.der().clone()],
                    PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
                )?;
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            servers.insert(origin.clone(), Arc::new(config));
        }
        route.tls = servers.get(&origin).cloned();
    }
    // No signing service survives setup; retain only the preissued origin certificates.
    Ok(Some(issuer.pem()))
}

fn parameters(names: Vec<String>) -> Result<CertificateParams> {
    let mut params = CertificateParams::new(names)?;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::minutes(5);
    params.not_after = now + time::Duration::days(1);
    Ok(params)
}

pub(super) fn handle<S: Read + Write>(
    mut client: S,
    request: BufferedRequest,
    authority: &str,
    routes: &[AuthenticatedHttpRoute],
) -> Result<()> {
    let origin = super::account_origin(authority)?;
    let mut headers = [httparse::EMPTY_HEADER; 96];
    let mut parsed = httparse::Request::new(&mut headers);
    ensure!(
        parsed.parse(&request.header)?.is_complete(),
        "incomplete CONNECT request"
    );
    let hosts: Vec<_> = parsed
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("host"))
        .collect();
    ensure!(
        hosts.len() == 1
            && super::account_origin(std::str::from_utf8(hosts[0].value)?)?.origin()
                == origin.origin(),
        "CONNECT Host does not match its authority"
    );
    ensure!(
        !parsed
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("content-length")
                || header.name.eq_ignore_ascii_case("transfer-encoding")),
        "CONNECT request must not have an HTTP body"
    );
    let selected: Vec<_> = routes
        .iter()
        .filter(|route| route.proxy && route.origin.origin() == origin.origin())
        .map(|route| {
            let mut route = route.clone();
            route.direct_base = Some(route.origin.clone());
            route
        })
        .collect();
    let Some(config) = selected.first().and_then(|route| route.tls.clone()) else {
        return send_simple_response(
            &mut client,
            403,
            "Forbidden",
            "no mediated account for this destination\n",
        );
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
    client.flush()?;
    let io = Prefixed {
        prefix: Cursor::new(request.remainder),
        io: client,
    };
    let mut stream = StreamOwned::new(ServerConnection::new(config)?, io);
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .context("account TLS handshake failed")?;
    }
    ensure!(
        stream.conn.server_name() == origin.domain(),
        "TLS server name does not match CONNECT authority"
    );
    let request = read_request_header(&mut stream)?;
    super::forward_authenticated_request(&mut stream, request, &selected, true)?;
    stream.conn.send_close_notify();
    stream.flush()?;
    Ok(())
}

struct Prefixed<S> {
    prefix: Cursor<Vec<u8>>,
    io: S,
}

impl<S: Read> Read for Prefixed<S> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix.position() < self.prefix.get_ref().len() as u64 {
            self.prefix.read(buffer)
        } else {
            self.io.read(buffer)
        }
    }
}

impl<S: Write> Write for Prefixed<S> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.io.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.io.flush()
    }
}
