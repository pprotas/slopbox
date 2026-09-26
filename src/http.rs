use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, ensure};

const MAX_HEADER_SIZE: usize = 64 * 1024;

pub(crate) fn read_retry(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match reader.read(buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

pub(crate) fn parse_request_line(header: &[u8]) -> Result<(String, String)> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut parsed = httparse::Request::new(&mut headers);
    ensure!(
        parsed.parse(header)?.is_complete(),
        "incomplete proxy request"
    );
    Ok((
        parsed
            .method
            .context("proxy request has no method")?
            .to_owned(),
        parsed
            .path
            .context("proxy request has no target")?
            .to_owned(),
    ))
}

#[cfg(test)]
fn copy_redacting(reader: &mut impl Read, writer: &mut impl Write, secret: &[u8]) -> Result<()> {
    copy_redacting_many(reader, writer, &[secret])
}

pub(crate) fn copy_redacting_many(
    reader: &mut impl Read,
    writer: &mut impl Write,
    secrets: &[&[u8]],
) -> Result<()> {
    ensure!(
        !secrets.is_empty() && secrets.iter().all(|secret| !secret.is_empty()),
        "cannot redact an empty credential"
    );
    let mut pending = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];

    loop {
        let count = read_retry(reader, &mut buffer)?;
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..count]);
        flush_redacted(&mut pending, writer, secrets, false)?;
    }
    flush_redacted(&mut pending, writer, secrets, true)
}

fn flush_redacted(
    pending: &mut Vec<u8>,
    writer: &mut impl Write,
    secrets: &[&[u8]],
    finished: bool,
) -> Result<()> {
    const REDACTED: &[u8] = b"[REDACTED]";

    while let Some((position, secret)) = secrets
        .iter()
        .filter_map(|secret| find_bytes(pending, secret).map(|position| (position, *secret)))
        .min_by_key(|(position, secret)| (*position, std::cmp::Reverse(secret.len())))
    {
        writer.write_all(&pending[..position])?;
        writer.write_all(REDACTED)?;
        pending.drain(..position + secret.len());
    }

    let retained = if finished {
        0
    } else {
        secrets
            .iter()
            .map(|secret| secret.len())
            .max()
            .unwrap_or(1)
            .saturating_sub(1)
    };
    if pending.len() > retained {
        let count = pending.len() - retained;
        writer.write_all(&pending[..count])?;
        pending.drain(..count);
    }
    Ok(())
}

pub(crate) fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    find_bytes(haystack, needle).is_some()
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(crate) fn resolve_public(host: &str, port: u16) -> Result<SocketAddr> {
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("DNS resolution failed for {host}"))?
        .collect();
    ensure!(!addresses.is_empty(), "DNS returned no addresses");
    ensure!(
        addresses.iter().all(|address| is_public_ip(address.ip())),
        "destination resolves to a reserved address"
    );

    Ok(addresses[0])
}

pub(crate) fn send_simple_response(
    client: &mut UnixStream,
    status: u16,
    reason: &str,
    body: &str,
) -> Result<()> {
    write!(
        client,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

pub(crate) struct BufferedRequest {
    pub header: Vec<u8>,
    pub remainder: Vec<u8>,
}

pub(crate) fn read_request_header(stream: &mut impl Read) -> Result<BufferedRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = read_retry(stream, &mut buffer)?;
        ensure!(count != 0, "proxy client closed before sending a request");
        bytes.extend_from_slice(&buffer[..count]);
        ensure!(
            bytes.len() <= MAX_HEADER_SIZE,
            "proxy request headers are too large"
        );
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let end = position + 4;
            return Ok(BufferedRequest {
                header: bytes[..end].to_vec(),
                remainder: bytes[end..].to_vec(),
            });
        }
    }
}

pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_ipv4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_ipv4(mapped);
            }
            let segments = ip.segments();
            let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
            let deprecated_site_local = segments[0] & 0xffc0 == 0xfec0;
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || documentation
                || deprecated_site_local)
        }
    }
}

fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [first, second, third, _] = ip.octets();
    let shared = first == 100 && (64..=127).contains(&second);
    let protocol_assignment = first == 192 && second == 0 && third == 0;
    let benchmarking = first == 198 && (18..=19).contains(&second);
    let reserved = first >= 240 || first == 0;

    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || shared
        || protocol_assignment
        || benchmarking
        || reserved)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct ChunkedReader {
        chunks: std::collections::VecDeque<std::io::Result<Vec<u8>>>,
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let chunk = chunk?;
            buffer[..chunk.len()].copy_from_slice(&chunk);
            Ok(chunk.len())
        }
    }

    #[test]
    fn header_reads_retry_interruptions_without_losing_buffered_data() {
        let mut reader = ChunkedReader {
            chunks: [
                Err(std::io::ErrorKind::Interrupted.into()),
                Ok(b"POST /account HTTP/1.1\r\nHost: local\r\n".to_vec()),
                Err(std::io::ErrorKind::Interrupted.into()),
                Ok(b"Content-Length: 4\r\n\r\nbody".to_vec()),
            ]
            .into(),
        };
        let request = read_request_header(&mut reader).unwrap();
        assert_eq!(
            request.header,
            b"POST /account HTTP/1.1\r\nHost: local\r\nContent-Length: 4\r\n\r\n"
        );
        assert_eq!(request.remainder, b"body");
    }

    #[test]
    fn header_reads_preserve_other_errors_and_disconnects() {
        for kind in [
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::ConnectionReset,
        ] {
            let mut reader = ChunkedReader {
                chunks: [Err(kind.into()), Ok(b"not read".to_vec())].into(),
            };
            let error = read_request_header(&mut reader).err().unwrap();
            assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().kind(), kind);
            assert_eq!(reader.chunks.len(), 1);
        }
        let error = read_request_header(&mut b"GET / HTTP/1.1\r\n".as_slice())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            "proxy client closed before sending a request"
        );
    }

    #[test]
    fn redacts_secrets_split_across_reads() {
        let mut reader = ChunkedReader {
            chunks: [
                Ok(b"before-secret-".to_vec()),
                Err(std::io::ErrorKind::Interrupted.into()),
                Ok(b"value-after".to_vec()),
            ]
            .into(),
        };
        let mut output = Vec::new();
        copy_redacting(&mut reader, &mut output, b"secret-value").unwrap();
        assert_eq!(output, b"before-[REDACTED]-after");

        let mut reader = ChunkedReader {
            chunks: [Ok(b"token host-acc".to_vec()), Ok(b"ount token".to_vec())].into(),
        };
        let mut output = Vec::new();
        copy_redacting_many(&mut reader, &mut output, &[b"token", b"host-account"]).unwrap();
        assert_eq!(output, b"[REDACTED] [REDACTED] [REDACTED]");
    }
    #[test]
    fn rejects_reserved_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.100.100.200",
            "192.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::net::TcpStream;
    use std::time::Duration;
    pub(crate) fn exchange(
        request: &[u8],
        handler: impl FnOnce(UnixStream) -> Result<()>,
    ) -> String {
        let (mut client, server) = UnixStream::pair().unwrap();
        client.write_all(request).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        handler(server).unwrap();

        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    }
    pub(crate) fn read_tcp_request(stream: &mut TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let count = read_retry(stream, &mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(position) = request.windows(4).position(|value| value == b"\r\n\r\n") {
                break position + 4;
            }
        };

        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut parsed = httparse::Request::new(&mut headers);
        assert!(parsed.parse(&request[..header_end]).unwrap().is_complete());
        let content_length = parsed
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("content-length"))
            .map(|header| std::str::from_utf8(header.value).unwrap().parse().unwrap())
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let count = read_retry(stream, &mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
        }
        request
    }
}
