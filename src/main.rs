//! DroidCam Relay: pull a DroidCam MJPEG stream and relay it to a v4l2loopback
//! virtual webcam.
//!
//! phone (MJPEG/HTTP) -> JPEG frames -> ffmpeg (scale + yuv420p) -> /dev/videoN
//!
//! One long-lived ffmpeg keeps the v4l2 device fed for the whole run; only the
//! frame source changes. When the phone drops, a "broadcast interrupted"
//! placeholder is written until it reconnects — ffmpeg is never restarted, so
//! apps like Google Meet keep the camera instead of dropping it for good.
//!
//! Config via env: PHONE_URL or PHONE_IP (else LAN discovery); DEVICE (else the
//! loopback is auto-detected); OUTPUT_SIZE (default 1280x720).

mod discovery;
mod stream;

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// Shown on the virtual camera whenever the phone is not streaming.
const PLACEHOLDER: &[u8] = include_bytes!("../assets/interrupted.jpg");

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
const STABLE: Duration = Duration::from_secs(15);

fn main() {
    let cfg_url = std::env::var("PHONE_URL").ok().or_else(|| {
        std::env::var("PHONE_IP")
            .ok()
            .map(|ip| format!("http://{ip}:4747/video"))
    });
    let cfg_device = std::env::var("DEVICE").ok();
    let (w, h) = parse_size(std::env::var("OUTPUT_SIZE").ok().as_deref()).unwrap_or((1280, 720));

    log("starting (Ctrl-C to stop)");
    let mut sink: Option<Sink> = None;
    let mut backoff = MIN_BACKOFF;

    loop {
        // Keep one ffmpeg alive for the whole run; (re)spawn only if it's gone.
        if sink.as_mut().map_or(true, Sink::exited) {
            sink = None;
            match resolve_device(&cfg_device).and_then(|dev| Sink::spawn(&dev, w, h).ok()) {
                Some(s) => sink = Some(s),
                None => {
                    log("no v4l2loopback device yet; waiting");
                    sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let s = sink.as_mut().expect("sink present");

        // Stream the phone while it's reachable; the placeholder fills every gap.
        if let Some(url) = resolve_url(&cfg_url) {
            log(&format!("camera connected: {url}"));
            let started = Instant::now();
            let _ = stream::stream_jpegs(&url, &mut |frame| s.feed(frame));
            log("camera connection lost");
            backoff = if started.elapsed() >= STABLE {
                MIN_BACKOFF
            } else {
                backoff
            };
        } else {
            log("camera not found");
        }

        if s.exited() {
            continue; // ffmpeg died mid-stream; respawn on the next iteration
        }

        // Show "broadcast interrupted" for the backoff window, then retry.
        if feed_placeholder(s, backoff).is_err() {
            continue; // ffmpeg died; respawn
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn feed_placeholder(sink: &mut Sink, window: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + window;
    loop {
        sink.feed(PLACEHOLDER)?;
        if Instant::now() >= deadline {
            return Ok(());
        }
        sleep(Duration::from_millis(100)); // ~10 fps keeps the device alive
    }
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

/// Parse "WxH" into dimensions.
fn parse_size(spec: Option<&str>) -> Option<(u32, u32)> {
    let (w, h) = spec?.split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// A long-lived ffmpeg writing to the v4l2 device. Everything is scaled to a
/// fixed size, so the placeholder and the phone feed never resize the output.
struct Sink {
    child: Child,
    stdin: ChildStdin,
}

impl Sink {
    fn spawn(device: &str, w: u32, h: u32) -> std::io::Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-loglevel",
                "warning",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                // Don't buffer frames to estimate the rate; start producing to the
                // device immediately (matters for the low-rate placeholder).
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-f",
                "image2pipe",
                "-framerate",
                "30",
                "-i",
                "pipe:0",
                "-vf",
                &format!("scale={w}:{h},format=yuv420p"),
                "-fps_mode",
                "passthrough",
                "-f",
                "v4l2",
                device,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdin = child.stdin.take().expect("ffmpeg stdin");
        log(&format!("ffmpeg -> {device} ({w}x{h})"));
        Ok(Sink { child, stdin })
    }

    fn feed(&mut self, jpeg: &[u8]) -> std::io::Result<()> {
        self.stdin.write_all(jpeg)
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn log(msg: &str) {
    eprintln!("[droidcam-relay] {msg}");
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn parses_output_size() {
        assert_eq!(parse_size(Some("1280x720")), Some((1280, 720)));
        assert_eq!(parse_size(Some("1920X1080")), Some((1920, 1080)));
        assert_eq!(parse_size(None), None);
        assert_eq!(parse_size(Some("720p")), None);
        assert_eq!(parse_size(Some("x720")), None);
    }
}
