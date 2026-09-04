//! HTTP server and client behaviour (SPEC §9).
//!
//! The YAML suite covers this from outside, but it spawns a separate process,
//! so none of it shows up in coverage and none of it is debuggable in-process.
//! These drive the real server on a background thread over a real loopback
//! socket.

mod support;

use snap::http;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;

const SNAPSHOT: &str = "{\n  \"format\": 1,\n  \"frontier\": [],\n  \"patches\": []\n}\n";

/// Start a server on an OS-chosen port and return its base URL.
fn start_server() -> String {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // The server writes its startup URL to this sink and then flushes,
        // before the accept loop begins. `writeln!` emits the line in several
        // `write` calls, so the sink accumulates and forwards on flush.
        struct Notify {
            buffer: String,
            tx: mpsc::Sender<String>,
        }
        impl Write for Notify {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.buffer.push_str(&String::from_utf8_lossy(buf));
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                let _ = self.tx.send(std::mem::take(&mut self.buffer));
                Ok(())
            }
        }
        let mut sink = Notify {
            buffer: String::new(),
            tx,
        };
        let _ = http::serve(0, SNAPSHOT, &mut sink);
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("server announced its URL")
        .trim_end()
        .to_string()
}

/// Issue a raw request and return (status line, headers, body).
fn request(url: &str, method: &str, target: &str) -> (String, Vec<String>, String) {
    let parsed = http::parse_url(url).expect("valid URL");
    let address = format!("{}:{}", parsed.host, parsed.port);
    let mut stream = TcpStream::connect(&address).expect("connect");
    write!(
        stream,
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .expect("send");
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status).expect("status line");
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("header") == 0 || line.trim_end().is_empty() {
            break;
        }
        headers.push(line.trim_end().to_string());
    }
    let mut body = String::new();
    let _ = reader.read_to_string(&mut body);
    (status.trim_end().to_string(), headers, body)
}

fn header<'a>(headers: &'a [String], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| {
            h.to_ascii_lowercase()
                .starts_with(&format!("{}:", name.to_ascii_lowercase()))
        })
        .map(|h| h.split_once(':').expect("header").1.trim())
}

#[test]
fn get_returns_the_startup_snapshot() {
    let url = start_server();
    assert!(
        url.ends_with("/repository.json"),
        "startup URL is plain and complete: {url}"
    );
    let (status, headers, body) = request(&url, "GET", http::RESOURCE);
    assert!(status.contains("200"), "{status}");
    assert_eq!(header(&headers, "content-type"), Some(http::CONTENT_TYPE));
    assert_eq!(body, SNAPSHOT);
}

#[test]
fn head_returns_the_same_headers_without_a_body() {
    let url = start_server();
    let (get_status, get_headers, _) = request(&url, "GET", http::RESOURCE);
    let (head_status, head_headers, head_body) = request(&url, "HEAD", http::RESOURCE);
    assert_eq!(head_status, get_status);
    assert_eq!(
        header(&head_headers, "content-length"),
        header(&get_headers, "content-length"),
        "HEAD reports the same length it would have sent"
    );
    assert!(head_body.is_empty(), "HEAD carries no body");
}

#[test]
fn other_paths_are_404_and_other_methods_are_405_with_allow() {
    let url = start_server();
    let (status, _, _) = request(&url, "GET", "/elsewhere");
    assert!(status.contains("404"), "{status}");

    let (status, headers, _) = request(&url, "POST", http::RESOURCE);
    assert!(status.contains("405"), "{status}");
    assert_eq!(header(&headers, "allow"), Some("GET, HEAD"));
}

#[test]
fn the_client_fetches_the_snapshot() {
    let url = start_server();
    assert_eq!(http::fetch(&url).expect("fetch"), SNAPSHOT);
}

#[test]
fn the_client_rejects_a_non_200_response() {
    let url = start_server();
    let base = url.trim_end_matches(http::RESOURCE);
    let err = http::fetch(&format!("{base}/missing")).expect_err("404 must fail");
    assert!(err.detail().contains("HTTP 404"), "{}", err.detail());
}

#[test]
fn the_client_refuses_an_unreachable_host() {
    // Port 1 on loopback is reserved and never listening.
    let err = http::fetch("http://127.0.0.1:1/repository.json").expect_err("must fail");
    assert!(err.detail().contains("cannot connect"), "{}", err.detail());
}

#[test]
fn the_snapshot_is_immutable_once_serving() {
    // SPEC §7.9: the server validates and snapshots at startup, then serves
    // that snapshot. Two reads must agree even though nothing prevents the
    // underlying repository changing on disk.
    let url = start_server();
    let first = http::fetch(&url).expect("first");
    let second = http::fetch(&url).expect("second");
    assert_eq!(first, second);
}
