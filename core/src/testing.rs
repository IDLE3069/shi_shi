//! Test helpers: isolated directories under the target dir, never the live data.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use crate::messages::{key, Message};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn temp_dir(label: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/test-data"));
    let dir = base.join(format!("{label}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Answer one request per connection in order; the raw requests come back.
pub fn serve(responses: Vec<(&'static str, String)>) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for (kind, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let n = stream.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen).to_string();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase().strip_prefix("content-length: ").map(|v| v.parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= length {
                        break;
                    }
                }
            }
            tx.send(String::from_utf8_lossy(&seen).to_string()).unwrap();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).unwrap();
        }
    });
    (url, rx)
}

pub fn sample(source: &str, external: &str, at: i64, text: &str) -> Message {
    Message {
        id: key(source, "c", external),
        source_id: source.into(),
        source_name: "来源".into(),
        external_id: external.into(),
        chat: "c".into(),
        sender: "张三".into(),
        time: at,
        text: text.into(),
        kind: "text".into(),
        private: true,
        ..Default::default()
    }
}
