//! Small localhost-only preview server. Deployment remains a static directory.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    time::Duration,
};

fn resource(root: &Path, target: &str) -> Option<PathBuf> {
    let target = target.split('?').next()?.strip_prefix('/')?;
    let mut decoded = Vec::new();
    let mut bytes = target.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let a = char::from(bytes.next()?).to_digit(16)?;
            let b = char::from(bytes.next()?).to_digit(16)?;
            (a * 16 + b) as u8
        } else {
            byte
        });
    }
    let decoded = String::from_utf8(decoded).ok()?;
    if decoded.contains(['\\', '\0']) {
        return None;
    }
    let path = Path::new(&decoded);
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return None;
    }
    let path = root.join(if decoded.is_empty() {
        "index.html"
    } else {
        &decoded
    });
    let path = path.canonicalize().ok()?;
    (path.starts_with(root) && path.is_file()).then_some(path)
}
fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
fn respond(stream: &mut TcpStream, root: &Path) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(&mut *stream);
    let mut request = Vec::new();
    // Limit headers before allocating their body. This server never reads or
    // accepts a request body, and closes every connection after its response.
    for _ in 0..16 * 1024 {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        let byte = buffer[0];
        request.push(byte);
        reader.consume(1);
        if request.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    drop(reader);
    let text = std::str::from_utf8(&request).unwrap_or("");
    let mut words = text.lines().next().unwrap_or("").split_whitespace();
    let method = words.next().unwrap_or("");
    let target = words.next().unwrap_or("");
    if !request.ends_with(b"\r\n\r\n") || !matches!(method, "GET" | "HEAD") {
        return stream.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    }
    let Some(path) = resource(root, target) else {
        return stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    let mut file = fs::File::open(&path)?;
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        mime(&path),
        file.metadata()?.len()
    )?;
    if method == "GET" {
        std::io::copy(&mut file, stream)?;
    }
    Ok(())
}
pub fn serve(root: &Path, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let root = root.canonicalize()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))?;
    println!(
        "Preview: http://localhost:{}/ (Ctrl+C to stop)",
        listener.local_addr()?.port()
    );
    for stream in listener.incoming() {
        if let Err(error) = respond(&mut stream?, &root) {
            eprintln!("preview: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_mime_types_match_browser_requirements() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("index.html"), "<body></body>").unwrap();
        fs::write(tmp.path().join("a b.wasm"), b"\0asm").unwrap();
        let root = tmp.path().canonicalize().unwrap();
        assert_eq!(resource(&root, "/?cache=1"), Some(root.join("index.html")));
        assert_eq!(resource(&root, "/a%20b.wasm"), Some(root.join("a b.wasm")));
        for target in [
            "/../secret",
            "/%2e%2e/secret",
            "/%2fetc/passwd",
            "/x%5csecret",
            "/x%00",
            "/%GG",
            "/%",
            "http://elsewhere/",
        ] {
            assert_eq!(resource(&root, target), None, "{target}");
        }
        assert_eq!(mime(Path::new("runtime.wasm")), "application/wasm");
        assert_eq!(
            mime(Path::new("worker.js")),
            "text/javascript; charset=utf-8"
        );
    }
}
