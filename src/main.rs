//! DroidCam Relay: pull a DroidCam MJPEG stream and republish it to a
//! v4l2loopback virtual webcam.
//!
//! phone (MJPEG/HTTP) -> JPEG frames -> ffmpeg (decode + yuv420p) -> /dev/videoN
//!
//! Config via env: PHONE_URL or PHONE_IP (else LAN discovery); DEVICE (else the
//! loopback is auto-detected).

mod discovery;
mod stream;

use std::io::{Error, ErrorKind, Write};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn main() {
    let configured_url = std::env::var("PHONE_URL").ok().or_else(|| {
        std::env::var("PHONE_IP")
            .ok()
            .map(|ip| format!("http://{ip}:4747/video"))
    });
    let configured_device = std::env::var("DEVICE").ok();

    let min_backoff = Duration::from_secs(1);
    let max_backoff = Duration::from_secs(15);
    let mut backoff = min_backoff;

    log("starting (Ctrl-C to stop)");
    loop {
        let started = Instant::now();
        match run_once(&configured_url, &configured_device) {
            Ok(()) => log("stream ended"),
            Err(e) => log(&format!("error: {e}")),
        }

        // Reset backoff after a healthy session; otherwise grow it.
        backoff = if started.elapsed() >= Duration::from_secs(15) {
            min_backoff
        } else {
            (backoff * 2).min(max_backoff)
        };
        log(&format!("reconnecting in {}s", backoff.as_secs()));
        sleep(backoff);
    }
}

fn run_once(
    configured_url: &Option<String>,
    configured_device: &Option<String>,
) -> std::io::Result<()> {
    let url = resolve_url(configured_url)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no DroidCam found on the LAN"))?;
    let device = resolve_device(configured_device)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "no v4l2loopback device found"))?;

    log(&format!("{url} -> {device}"));
    let mut ffmpeg = spawn_ffmpeg(&device)?;
    let mut stdin = ffmpeg.stdin.take().expect("ffmpeg stdin");

    let result = stream::stream_jpegs(&url, &mut |frame| stdin.write_all(frame));

    drop(stdin);
    let _ = ffmpeg.kill();
    let _ = ffmpeg.wait();
    result
}

fn resolve_url(configured: &Option<String>) -> Option<String> {
    match configured {
        Some(url) if reachable(url) => Some(url.clone()),
        _ => discovery::find_phone(),
    }
}

fn reachable(url: &str) -> bool {
    url.strip_prefix("http://")
        .and_then(|r| r.split('/').next())
        .map(|authority| authority.split(':').next().unwrap_or(authority))
        .map(discovery::reachable)
        .unwrap_or(false)
}

fn resolve_device(configured: &Option<String>) -> Option<String> {
    match configured {
        Some(dev) if discovery::is_loopback(dev) => Some(dev.clone()),
        _ => discovery::find_loopback(),
    }
}

fn spawn_ffmpeg(device: &str) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args([
            "-loglevel",
            "warning",
            "-fflags",
            "nobuffer",
            "-flags",
            "low_delay",
            // image2pipe reads the concatenated JPEGs and assigns monotonic
            // timestamps at this rate (the raw mjpeg demuxer leaves DTS at 0).
            "-f",
            "image2pipe",
            "-framerate",
            "30",
            "-i",
            "pipe:0",
            "-vf",
            "format=yuv420p",
            "-fps_mode",
            "passthrough",
            "-f",
            "v4l2",
            device,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
}

fn log(msg: &str) {
    eprintln!("[droidcam-relay] {msg}");
}
