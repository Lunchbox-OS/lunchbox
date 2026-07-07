//! Phone hand-off: a tiny LAN HTTP server so a phone — which has a real
//! keyboard — can hand a library URL to the TV, which does not.
//!
//! [`start`] binds an ephemeral port, spawns a background accept loop, and
//! returns the `http://<lan-ip>:<port>/` URL to show on the TV (as text and a
//! QR code). The phone opens that page, pastes a TOML/M3U/YouTube URL, and
//! submits; the server delivers it over a channel the UI polls, which then
//! pre-fills the add-library form. LAN-only and unauthenticated — a home TV on
//! the same Wi-Fi as the phone, the same trust model as the rest of the app.
//!
//! Deliberately dependency-light: a hand-rolled HTTP/1.1 handler (one form page,
//! one submit endpoint) rather than a server crate. Works on the host too, so
//! the desktop preview can drive it.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{Receiver, Sender};

/// A running hand-off server: the URL to show, and the channel submitted
/// library URLs arrive on. Dropping it lets the daemon accept-thread wind down
/// (its sends start failing); the socket closes with the process.
pub struct Handoff {
    /// `http://<lan-ip>:<port>/` — shown on the TV and encoded in the QR.
    pub url: String,
    /// Library URLs submitted from the phone.
    pub rx: Receiver<String>,
}

/// Start the hand-off server on an ephemeral port, bound to all interfaces.
pub fn start() -> std::io::Result<Handoff> {
    let listener = TcpListener::bind("0.0.0.0:0")?;
    let port = listener.local_addr()?.port();
    let ip = local_ip().unwrap_or_else(|| "0.0.0.0".to_string());
    let url = format!("http://{ip}:{port}/");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // One request per connection is plenty for this form; ignore errors
            // so a bad client can't take the loop down.
            let _ = handle(stream, &tx);
        }
    });
    Ok(Handoff { url, rx })
}

/// The LAN IP of the interface that would route outward. "Connecting" a UDP
/// socket sends nothing but picks the source address, which is that IP.
fn local_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}

fn handle(mut stream: TcpStream, tx: &Sender<String>) -> std::io::Result<()> {
    let Some((method, path, body)) = read_request(&mut stream) else {
        return respond(&mut stream, "400 Bad Request", "text/plain", "bad request");
    };
    if method == "POST" && path.starts_with("/submit") {
        if let Some(raw) = form_value(&body, "url") {
            let url = percent_decode(raw);
            let url = url.trim();
            if !url.is_empty() {
                let _ = tx.send(url.to_string());
            }
        }
        respond(&mut stream, "200 OK", "text/html; charset=utf-8", DONE_HTML)
    } else {
        respond(&mut stream, "200 OK", "text/html; charset=utf-8", FORM_HTML)
    }
}

/// Read a full HTTP request: the request line, then headers, then a
/// `Content-Length` body if present. Returns `(method, path, body)`.
fn read_request(stream: &mut TcpStream) -> Option<(String, String, String)> {
    let mut data = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&tmp[..n]);
        if let Some(hdr_end) = find(&data, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..hdr_end]).to_string();
            let content_len = content_length(&head);
            let body_start = hdr_end + 4;
            while data.len() < body_start + content_len {
                let n = stream.read(&mut tmp).ok()?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&tmp[..n]);
            }
            let end = (body_start + content_len).min(data.len());
            let body = String::from_utf8_lossy(&data[body_start..end]).to_string();
            let mut parts = head.lines().next()?.split_whitespace();
            let method = parts.next()?.to_string();
            let path = parts.next()?.to_string();
            return Some((method, path, body));
        }
        if data.len() > 64 * 1024 {
            return None; // runaway header; drop it
        }
    }
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The value of an `application/x-www-form-urlencoded` field, still percent- and
/// `+`-encoded (the caller decodes).
fn form_value<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Decode `application/x-www-form-urlencoded`: `+` → space, `%XX` → byte.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The QR modules for `text` as `(width, dark-flags)` in row-major order, or
/// `None` if the text won't fit a QR code.
pub fn qr_matrix(text: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Some((width, dark))
}

const FORM_HTML: &str = r#"<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Add a library</title>
<body style="font-family:system-ui,sans-serif;max-width:32rem;margin:2rem auto;padding:0 1rem;line-height:1.5">
<h2>Add a library to the TV</h2>
<p>Paste a TOML or M3U file URL, or a YouTube playlist URL:</p>
<form method="POST" action="/submit">
<input name="url" type="url" placeholder="https://…" autofocus autocapitalize="off" autocorrect="off"
 style="width:100%;box-sizing:border-box;font-size:1.2rem;padding:.6rem">
<p><button style="font-size:1.1rem;padding:.6rem 1.2rem">Send to TV</button></p>
</form>
</body>"#;

const DONE_HTML: &str = r#"<!doctype html>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sent</title>
<body style="font-family:system-ui,sans-serif;max-width:32rem;margin:2rem auto;padding:0 1rem;line-height:1.5">
<h2>Sent ✓</h2>
<p>Return to the TV to finish adding the library.</p>
</body>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_form_url() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fx%2Fa.toml"),
            "https://x/a.toml"
        );
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%zz"), "%zz"); // invalid escape kept verbatim
    }

    #[test]
    fn extracts_field() {
        assert_eq!(form_value("url=abc&x=1", "url"), Some("abc"));
        assert_eq!(form_value("x=1&url=abc", "url"), Some("abc"));
        assert_eq!(form_value("x=1", "url"), None);
    }

    #[test]
    fn parses_content_length() {
        assert_eq!(content_length("POST /submit\r\nContent-Length: 42"), 42);
        assert_eq!(content_length("content-length:7"), 7); // case-insensitive
        assert_eq!(content_length("GET /"), 0);
    }

    #[test]
    fn builds_a_qr() {
        let (w, dark) = qr_matrix("http://192.168.1.5:8099/").unwrap();
        assert!(w >= 21); // smallest QR is 21x21
        assert_eq!(dark.len(), w * w);
    }
}
