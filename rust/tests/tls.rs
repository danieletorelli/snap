//! `https://` repository operands (SPEC §9).
//!
//! No YAML case exercises HTTPS — the acceptance suite has no way to stand up
//! a TLS server — so this is the only thing keeping a specified MUST honest.
//! It drives a real rustls handshake against a self-signed certificate.

#![cfg(feature = "tls")]

use snap::http::{self, Scheme, Url};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::Arc;

const SNAPSHOT: &str = "{\n  \"format\": 1,\n  \"frontier\": [],\n  \"patches\": []\n}\n";

/// Start a minimal TLS server on an OS-chosen port.
///
/// Returns the port and the DER certificate the client must trust.
fn start_tls_server(status_line: &'static str, body: &'static str) -> (u16, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("generate self-signed certificate");
    let cert_der = certified.cert.der().to_vec();
    let key_der = certified.key_pair.serialize_der();

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert_der.clone())],
            rustls::pki_types::PrivateKeyDer::try_from(key_der).expect("private key"),
        )
        .expect("server config");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let (ready_tx, ready_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let config = Arc::new(config);
        ready_tx.send(()).expect("signal ready");
        // One connection per test is enough.
        if let Ok((mut socket, _)) = listener.accept() {
            let mut connection = rustls::ServerConnection::new(config).expect("connection");
            let mut tls = rustls::Stream::new(&mut connection, &mut socket);
            let mut buffer = [0u8; 2048];
            let _ = tls.read(&mut buffer);
            let response = format!(
                "{status_line}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tls.write_all(response.as_bytes());
            let _ = tls.flush();
        }
    });
    ready_rx.recv().expect("server started");
    (port, cert_der)
}

fn trust(cert_der: Vec<u8>) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(cert_der))
        .expect("trust the test certificate");
    roots
}

fn url(port: u16) -> Url {
    Url {
        scheme: Scheme::Https,
        host: "localhost".to_string(),
        port,
        path: "/repository.json".to_string(),
    }
}

#[test]
fn https_urls_parse_with_the_right_default_port() {
    let parsed = http::parse_url("https://example.com/repository.json").expect("valid");
    assert_eq!(parsed.scheme, Scheme::Https);
    assert_eq!(parsed.port, 443);
    assert_eq!(parsed.host, "example.com");
    assert_eq!(
        http::parse_url("https://example.com:8443/x").unwrap().port,
        8443
    );
}

#[test]
fn fetches_a_repository_over_tls() {
    let (port, cert) = start_tls_server("HTTP/1.1 200 OK", SNAPSHOT);
    let raw = http::fetch_tls_with(&url(port), trust(cert)).expect("TLS fetch");
    let text = String::from_utf8(raw).expect("UTF-8");
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(
        text.ends_with(SNAPSHOT),
        "body must survive the transport: {text}"
    );
}

#[test]
fn an_untrusted_certificate_is_rejected() {
    // The server's certificate is self-signed and deliberately not trusted:
    // SPEC §9 gives no way to opt out of verification.
    let (port, _cert) = start_tls_server("HTTP/1.1 200 OK", SNAPSHOT);
    let empty_roots = rustls::RootCertStore::empty();
    assert!(
        http::fetch_tls_with(&url(port), empty_roots).is_err(),
        "an unverifiable certificate must not be accepted"
    );
}

#[test]
fn a_redirect_over_tls_is_reported_rather_than_followed() {
    let (port, cert) = start_tls_server("HTTP/1.1 302 Found", "");
    let raw = http::fetch_tls_with(&url(port), trust(cert)).expect("transport succeeds");
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.starts_with("HTTP/1.1 302"),
        "the status is surfaced, not chased: {text}"
    );
}
