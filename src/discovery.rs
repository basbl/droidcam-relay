//! Finding the phone on the LAN and the v4l2loopback device. Std-only.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const PORT: u16 = 4747;

/// True if `host:4747` currently accepts a TCP connection.
pub fn reachable(host: &str) -> bool {
    resolve(host, PORT)
        .and_then(|addr| TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok())
        .is_some()
}

fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs;
    (host, port).to_socket_addrs().ok()?.next()
}

/// Scan the local /24 for a DroidCam (port 4747 redirecting to /remote) and
/// return its video URL, or None.
pub fn find_phone() -> Option<String> {
    let base = local_subnet_base()?;
    let (tx, rx) = mpsc::channel();

    for i in 2..=254u8 {
        let host = format!("{base}{i}");
        let tx = tx.clone();
        thread::spawn(move || {
            if is_droidcam(&host) {
                let _ = tx.send(host);
            }
        });
    }
    drop(tx);

    // First responder wins; threads with a dead host just time out and never send.
    rx.recv_timeout(Duration::from_millis(1500))
        .ok()
        .map(|host| format!("http://{host}:{PORT}/video"))
}

fn is_droidcam(host: &str) -> bool {
    let Some(addr) = resolve(host, PORT) else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(600)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(600)));
    let req = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).unwrap_or(0);
    droidcam_redirect(&String::from_utf8_lossy(&buf[..n]))
}

/// Whether an HTTP response head is DroidCam's redirect to `/remote`.
fn droidcam_redirect(head: &str) -> bool {
    let head = head.to_ascii_lowercase();
    head.contains("location: /remote") || head.contains("location:/remote")
}

fn local_subnet_base() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(subnet_base(v4.octets())),
        _ => None,
    }
}

/// The "a.b.c." prefix of an IPv4 /24, for appending host octets.
fn subnet_base(octets: [u8; 4]) -> String {
    format!("{}.{}.{}.", octets[0], octets[1], octets[2])
}

/// Whether a video4linux sysfs link target is a virtual node (v4l2loopback),
/// as opposed to a real camera under the PCI/USB tree.
fn virtual_target(target: &str) -> bool {
    target.contains("/virtual/")
}

/// The v4l2loopback output device: a virtual video4linux node (its sysfs path is
/// under /devices/virtual/), unlike real cameras. Returns "/dev/videoN" or None.
pub fn find_loopback() -> Option<String> {
    let mut nodes: Vec<(u32, String)> = std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let idx: u32 = name.strip_prefix("video")?.parse().ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            virtual_target(&target.to_string_lossy()).then_some((idx, format!("/dev/{name}")))
        })
        .collect();

    nodes.sort_by_key(|(idx, _)| *idx);
    nodes.into_iter().next().map(|(_, dev)| dev)
}

/// Whether `device` is a present v4l2loopback (virtual) node.
pub fn is_loopback(device: &str) -> bool {
    let Some(name) = device.strip_prefix("/dev/") else {
        return false;
    };
    if !std::path::Path::new(device).exists() {
        return false;
    }
    std::fs::read_link(format!("/sys/class/video4linux/{name}"))
        .map(|t| virtual_target(&t.to_string_lossy()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_droidcam_redirect() {
        assert!(droidcam_redirect(
            "HTTP/1.1 302 Found\r\nLocation: /remote\r\n"
        ));
        assert!(droidcam_redirect(
            "HTTP/1.1 302 Found\r\nlocation:/remote\r\n"
        ));
    }

    #[test]
    fn ignores_non_droidcam_responses() {
        assert!(!droidcam_redirect(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n"
        ));
        assert!(!droidcam_redirect(
            "HTTP/1.1 302 Found\r\nLocation: /login\r\n"
        ));
        assert!(!droidcam_redirect(""));
    }

    #[test]
    fn builds_subnet_base() {
        assert_eq!(subnet_base([192, 168, 1, 35]), "192.168.1.");
        assert_eq!(subnet_base([10, 0, 0, 7]), "10.0.0.");
    }

    #[test]
    fn identifies_virtual_nodes() {
        assert!(virtual_target("../../devices/virtual/video4linux/video10"));
        assert!(!virtual_target(
            "../../devices/pci0000:00/0000:00:14.0/usb3/3-11/video4linux/video0"
        ));
    }
}
