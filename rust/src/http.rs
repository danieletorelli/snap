//! The read-only HTTP repository (SPEC §9).
//!
//! One fixed resource, served over a hand-written HTTP/1.1 subset. A full
//! server or client library would bring an async runtime for a surface that is
//! two methods and one path, and would take exact control of the response
//! bytes away from us.

use crate::error::{self, Result};
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

pub const RESOURCE: &str = "/repository.json";
pub const CONTENT_TYPE: &str = "application/json; charset=utf-8";
const DEFAULT_PORT: u16 = 8765;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

pub fn parse_port(raw: Option<&str>) -> Result<u16> {
    match raw {
        None => Ok(DEFAULT_PORT),
        Some(text) => {
            // Reject anything `u16` would accept loosely, and anything the
            // spec does not describe as a port.
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(error::invalid_port(text));
            }
            text.parse::<u16>().map_err(|_| error::invalid_port(text))
        }
    }
}

/// Install handlers so SIGINT and SIGTERM exit 0 (SPEC §7.9).
///
/// The handler calls `_exit`, which is async-signal-safe; the server holds no
/// lock and owns nothing that needs flushing, because the snapshot was written
/// to stdout before the accept loop began. Doing anything richer inside a
/// signal handler — allocating, locking, running destructors — would be
/// undefined behaviour.
fn install_signal_handlers() {
    extern "C" fn handler(_signal: libc::c_int) {
        // SAFETY: `_exit` is async-signal-safe and this is the only statement.
        #[allow(unsafe_code)]
        unsafe {
            libc::_exit(0)
        }
    }
    // SAFETY: registering an async-signal-safe handler for two standard
    // signals. `signal` is the portable POSIX call and the handler above does
    // nothing that requires reentrancy guarantees beyond `_exit`.
    #[allow(unsafe_code)]
    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
    }
}

/// Serve `snapshot` until a signal arrives (SPEC §7.9).
///
/// Binds only to loopback. The startup URL is written to `out` and flushed
/// before the accept loop so a client can rely on it, and it is always plain
/// text even in terminal mode so it can be copied or piped.
pub fn serve(port: u16, snapshot: &str, out: &mut dyn Write) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .map_err(|e| error::Error::new(format!("cannot bind port {port}: {e}")))?;
    let actual = listener
        .local_addr()
        .map_err(|e| error::Error::new(format!("cannot read local address: {e}")))?
        .port();

    install_signal_handlers();

    writeln!(out, "http://127.0.0.1:{actual}{RESOURCE}")
        .and_then(|()| out.flush())
        .map_err(|e| error::Error::new(format!("cannot write startup URL: {e}")))?;

    // `flatten` drops failed accepts: one bad connection must not take the
    // server down, and SPEC §7.9 has it serve until a signal arrives.
    for stream in listener.incoming().flatten() {
        let _ = handle(stream, snapshot);
    }
    Ok(())
}

fn handle(mut stream: TcpStream, snapshot: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Drain headers; this resource takes no request body.
    let mut header = String::new();
    loop {
        header.clear();
        if reader.read_line(&mut header)? == 0 || header.trim_end().is_empty() {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();

    match (method, target) {
        ("GET", RESOURCE) => respond(&mut stream, 200, "OK", Some(snapshot), true),
        ("HEAD", RESOURCE) => respond(&mut stream, 200, "OK", Some(snapshot), false),
        // SPEC §9: other paths 404, other methods 405 with `Allow`.
        (_, RESOURCE) => respond(&mut stream, 405, "Method Not Allowed", None, true),
        _ => respond(&mut stream, 404, "Not Found", None, true),
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    phrase: &str,
    body: Option<&str>,
    write_body: bool,
) -> std::io::Result<()> {
    let payload = body.unwrap_or_default();
    let mut head = format!("HTTP/1.1 {status} {phrase}\r\n");
    if body.is_some() {
        let _ = write!(head, "Content-Type: {CONTENT_TYPE}\r\n");
    }
    let _ = write!(head, "Content-Length: {}\r\n", payload.len());
    if status == 405 {
        head.push_str("Allow: GET, HEAD\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    if write_body {
        stream.write_all(payload.as_bytes())?;
    }
    stream.flush()
}

/// Which transport a repository URL asks for (SPEC §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

/// A parsed repository URL. Only what SPEC §9 needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: Scheme,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(raw: &str) -> Result<Url> {
    let bad = || error::Error::new(format!("invalid repository URL: {raw}"));
    let (scheme, rest) = if let Some(rest) = raw.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else if let Some(rest) = raw.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else {
        return Err(bad());
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(bad());
    }
    let default_port = match scheme {
        Scheme::Http => 80,
        Scheme::Https => 443,
    };
    let (host, port) = if authority.starts_with('[') {
        // IPv6 literal: [::1] or [::1]:8765
        let close = authority.find(']').ok_or_else(bad)?;
        let host = &authority[..=close];
        let rest = &authority[close + 1..];
        if rest.is_empty() {
            (host, default_port)
        } else {
            let port = rest
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(bad)?;
            (host, port)
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().map_err(|_| bad())?),
            None => (authority, default_port),
        }
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok(Url {
        scheme,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

/// Perform exactly one GET of `raw` and return the body (SPEC §9).
///
/// Redirects are **not** followed: SPEC §9 says "one GET of that exact URL,
/// requires status 200", and `13-http-client` asserts that a 302 surfaces as
/// an error rather than a second request.
pub fn fetch(raw: &str) -> Result<String> {
    let url = parse_url(raw)?;
    let raw_response = match url.scheme {
        Scheme::Http => fetch_plain(&url)?,
        Scheme::Https => fetch_tls(&url)?,
    };
    parse_response(&raw_response)
}

fn request_bytes(url: &Url) -> Vec<u8> {
    // `Host` carries the port only when it is not the scheme default, which is
    // what every other client does and what virtual hosts expect.
    let default_port = match url.scheme {
        Scheme::Http => 80,
        Scheme::Https => 443,
    };
    let host_header = if url.port == default_port {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    format!(
        "GET {} HTTP/1.1\r\nHost: {host_header}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        url.path
    )
    .into_bytes()
}

fn connect(url: &Url) -> Result<TcpStream> {
    let address = format!("{}:{}", url.host, url.port);
    let stream = TcpStream::connect(&address)
        .map_err(|e| error::Error::new(format!("cannot connect to {address}: {e}")))?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT)).ok();
    stream.set_write_timeout(Some(CLIENT_TIMEOUT)).ok();
    Ok(stream)
}

fn fetch_plain(url: &Url) -> Result<Vec<u8>> {
    let mut stream = connect(url)?;
    stream
        .write_all(&request_bytes(url))
        .map_err(|e| error::Error::new(format!("cannot send request: {e}")))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| error::Error::new(format!("cannot read response: {e}")))?;
    Ok(raw)
}

#[cfg(not(feature = "tls"))]
fn fetch_tls(_url: &Url) -> Result<Vec<u8>> {
    // Only reachable in the audit-only `--no-default-features` build, which
    // does not conform to SPEC §9.
    Err(error::Error::new("https support was not compiled in"))
}

#[cfg(feature = "tls")]
fn fetch_tls(url: &Url) -> Result<Vec<u8>> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    fetch_tls_with(url, roots)
}

/// TLS fetch against a caller-supplied trust anchor set.
///
/// The root store is a parameter so tests can drive a real TLS handshake
/// against a self-signed certificate; production passes the webpki bundle.
#[cfg(feature = "tls")]
pub fn fetch_tls_with(url: &Url, roots: rustls::RootCertStore) -> Result<Vec<u8>> {
    use std::sync::Arc;

    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(url.host.clone())
        .map_err(|_| error::Error::new(format!("invalid TLS server name: {}", url.host)))?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| error::Error::new(format!("TLS setup failed: {e}")))?;
    let mut socket = connect(url)?;
    let mut tls = rustls::Stream::new(&mut connection, &mut socket);

    tls.write_all(&request_bytes(url))
        .map_err(|e| error::Error::new(format!("cannot send request: {e}")))?;
    let mut raw = Vec::new();
    match tls.read_to_end(&mut raw) {
        Ok(_) => Ok(raw),
        // Many servers close without a TLS close_notify. That is only fatal if
        // nothing was received; otherwise the response is already complete.
        Err(e) if !raw.is_empty() => {
            let _ = e;
            Ok(raw)
        }
        Err(e) => Err(error::Error::new(format!("cannot read response: {e}"))),
    }
}

/// Split an HTTP response, require status 200, and return the body.
fn parse_response(raw_response: &[u8]) -> Result<String> {
    let split = find_header_end(raw_response)
        .ok_or_else(|| error::Error::new("malformed HTTP response"))?;
    let head = String::from_utf8_lossy(&raw_response[..split.0]).to_string();
    let body = &raw_response[split.1..];

    let status_line = head.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| error::Error::new("malformed HTTP status line"))?;
    if status != 200 {
        return Err(error::http_status(status));
    }
    String::from_utf8(body.to_vec())
        .map_err(|_| error::Error::new("response body is not valid UTF-8"))
}

fn find_header_end(bytes: &[u8]) -> Option<(usize, usize)> {
    bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, i + 4))
        .or_else(|| {
            bytes
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, i + 2))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_parsing_matches_the_spec() {
        assert_eq!(parse_port(None).unwrap(), DEFAULT_PORT);
        assert_eq!(parse_port(Some("0")).unwrap(), 0, "0 asks the OS to choose");
        assert_eq!(parse_port(Some("8765")).unwrap(), 8765);
        assert_eq!(parse_port(Some("65535")).unwrap(), 65535);
    }

    #[test]
    fn rejects_out_of_range_and_non_numeric_ports() {
        for raw in ["65536", "-1", "", "abc", "80.5", "+80", " 80", "0x10"] {
            assert!(parse_port(Some(raw)).is_err(), "{raw:?} should be rejected");
        }
        assert_eq!(
            parse_port(Some("65536")).unwrap_err().detail(),
            "invalid port: 65536",
            "wording is pinned by the acceptance suite"
        );
    }

    #[test]
    fn parses_repository_urls() {
        assert_eq!(
            parse_url("http://127.0.0.1:8765/repository.json").unwrap(),
            Url {
                scheme: Scheme::Http,
                host: "127.0.0.1".into(),
                port: 8765,
                path: "/repository.json".into()
            }
        );
        assert_eq!(
            parse_url("http://example.com/x").unwrap().port,
            80,
            "default port"
        );
        assert_eq!(
            parse_url("http://example.com").unwrap().path,
            "/",
            "default path"
        );
    }

    #[test]
    fn rejects_malformed_urls() {
        for raw in [
            "",
            "ftp://x/",
            "http://",
            "http:///x",
            "http://h:notaport/",
            "https://",
        ] {
            assert!(parse_url(raw).is_err(), "{raw:?} should be rejected");
        }
    }

    #[test]
    fn finds_the_header_boundary_for_both_line_endings() {
        assert_eq!(
            find_header_end(b"HTTP/1.1 200 OK\r\n\r\nbody"),
            Some((15, 19))
        );
        assert_eq!(find_header_end(b"HTTP/1.1 200 OK\n\nbody"), Some((15, 17)));
        assert_eq!(find_header_end(b"no boundary"), None);
    }
}
