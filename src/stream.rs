//! Minimal HTTP MJPEG client: GET the DroidCam /video multipart stream and call
//! back with each complete JPEG frame. Std-only, no HTTP crate.

use std::io::{Error, ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];

/// Connect to `url`, read the multipart stream, and invoke `on_frame` with every
/// JPEG frame until the connection ends or `on_frame` errors (e.g. ffmpeg exited).
pub fn stream_jpegs(
    url: &str,
    on_frame: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let (host, port, path) = parse_url(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "cannot resolve host"))?;

    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: droidcam-relay\r\nConnection: keep-alive\r\n\r\n"
    );
    stream.write_all(req.as_bytes())?;

    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut read = [0u8; 64 * 1024];

    // Read past the HTTP response headers (terminated by CRLFCRLF).
    let header_end = loop {
        let n = stream.read(&mut read)?;
        if n == 0 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "closed before headers",
            ));
        }
        buf.extend_from_slice(&read[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        if buf.len() > 64 * 1024 {
            return Err(Error::new(ErrorKind::InvalidData, "headers too large"));
        }
    };
    if !buf.starts_with(b"HTTP/1.1 200") && !buf.starts_with(b"HTTP/1.0 200") {
        return Err(Error::other("non-200 response"));
    }
    buf.drain(..header_end);

    // Stream body: pull JPEG frames (SOI..EOI) out of the multipart payload.
    loop {
        emit_frames(&mut buf, on_frame)?;

        let n = stream.read(&mut read)?;
        if n == 0 {
            return Ok(()); // stream ended; caller reconnects
        }
        buf.extend_from_slice(&read[..n]);

        // Guard against unbounded growth if we somehow never see an SOI.
        if buf.len() > 32 * 1024 * 1024 {
            buf.clear();
        }
    }
}

fn emit_frames(
    buf: &mut Vec<u8>,
    on_frame: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<()> {
    loop {
        let Some(soi) = find(buf, &SOI) else {
            buf.clear(); // no start marker yet; drop junk
            return Ok(());
        };
        let Some(rel_eoi) = find(&buf[soi..], &EOI) else {
            if soi > 0 {
                buf.drain(..soi); // keep from the start marker onward
            }
            return Ok(());
        };
        let end = soi + rel_eoi + 2;
        on_frame(&buf[soi..end])?;
        buf.drain(..end);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_url(url: &str) -> std::io::Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "only http:// supported"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(4747)),
        None => (authority.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(body: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(body);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    fn collect(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        emit_frames(buf, &mut |f| {
            frames.push(f.to_vec());
            Ok(())
        })
        .unwrap();
        frames
    }

    #[test]
    fn extracts_whole_frames_and_keeps_the_partial() {
        let f1 = jpeg(b"one");
        let f2 = jpeg(b"two");
        let mut buf = Vec::new();
        buf.extend_from_slice(b"--boundary\r\nContent-Type: image/jpeg\r\n\r\n");
        buf.extend_from_slice(&f1);
        buf.extend_from_slice(b"\r\n--boundary\r\n\r\n");
        buf.extend_from_slice(&f2);
        buf.extend_from_slice(b"\r\n--boundary\r\n\r\n\xFF\xD8partial-no-end");

        let frames = collect(&mut buf);
        assert_eq!(frames, vec![f1, f2]);
        // The leftover keeps everything from the dangling SOI onward.
        assert_eq!(&buf[..2], &[0xFF, 0xD8]);
        assert!(buf.ends_with(b"partial-no-end"));
    }

    #[test]
    fn drops_bytes_before_a_start_marker() {
        let mut buf = b"junk with no jpeg here".to_vec();
        assert!(collect(&mut buf).is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn keeps_a_started_but_unfinished_frame() {
        let mut buf = b"junk\xFF\xD8still going".to_vec();
        assert!(collect(&mut buf).is_empty());
        assert_eq!(&buf[..2], &[0xFF, 0xD8]);
        assert!(buf.ends_with(b"still going"));
    }

    #[test]
    fn find_locates_and_misses() {
        assert_eq!(find(b"abcde", b"cd"), Some(2));
        assert_eq!(find(b"abcde", b"xy"), None);
        assert_eq!(find(b"a", b"ab"), None);
    }

    #[test]
    fn parses_urls() {
        assert_eq!(
            parse_url("http://192.168.1.35:4747/video").unwrap(),
            ("192.168.1.35".into(), 4747, "/video".into())
        );
        assert_eq!(
            parse_url("http://host:8080/a/b").unwrap(),
            ("host".into(), 8080, "/a/b".into())
        );
        assert_eq!(
            parse_url("http://host").unwrap(),
            ("host".into(), 80, "/".into())
        );
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(parse_url("https://host/x").is_err());
        assert!(parse_url("rtsp://host/x").is_err());
    }
}
