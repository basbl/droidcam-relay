# DroidCam Relay

[![CI](https://github.com/basbl/droidcam-relay/actions/workflows/ci.yml/badge.svg)](https://github.com/basbl/droidcam-relay/actions/workflows/ci.yml)

Relays a DroidCam MJPEG stream from a phone to a v4l2loopback virtual webcam.

```
phone (MJPEG/HTTP :4747) ──▶ JPEG frames ──▶ ffmpeg (scale + yuv420p) ──▶ /dev/videoN
```

It uses only the Rust standard library.
The one runtime dependency is **ffmpeg**, and the virtual camera needs the **v4l2loopback** kernel module.

## Features

- **Phone discovery** — scans the local /24 for a DroidCam (port 4747 redirecting to `/remote`) when no address is configured or the configured one is unreachable.
- **Loopback auto-detect** — finds the virtual `/dev/videoN` by its sysfs node under `/devices/virtual/`, so a shifted index after a reboot still works.
- **Resilient reconnect** — re-resolves the phone and device on every attempt, so DHCP changes, phone sleep and an unplugged loopback all self-heal.
- **No dropped camera** — one ffmpeg feeds the device for the whole run; only the frame source switches. While the phone is gone it shows a "broadcast interrupted" placeholder (`assets/interrupted.jpg`) instead of stopping, so apps like Google Meet keep the camera and recover on their own when the phone returns.

## Setting up the virtual camera

The output goes to a [v4l2loopback](https://github.com/umlaeute/v4l2loopback) device, which apps see as a normal webcam.
You also need **ffmpeg** on `PATH`.

On Fedora:

```sh
sudo dnf install -y akmod-v4l2loopback v4l2loopback-utils ffmpeg
sudo modprobe v4l2loopback card_label="DroidCam Relay" exclusive_caps=1
```

`exclusive_caps=1` is required for Chrome-based apps (Google Meet) to accept the device.
The loopback is found automatically, so its device index doesn't matter.

To load it on every boot:

```sh
echo v4l2loopback | sudo tee /etc/modules-load.d/v4l2loopback.conf
printf 'options v4l2loopback card_label="DroidCam Relay" exclusive_caps=1\n' | sudo tee /etc/modprobe.d/v4l2loopback.conf
```

## Build and run

```sh
cargo build --release

# Autodiscover phone and loopback:
./target/release/droidcam-relay

# Or pin them:
PHONE_IP=192.168.1.35 DEVICE=/dev/video10 ./target/release/droidcam-relay
```

Then pick the loopback as the camera in Meet, Zoom or OBS.

## Running as a service

To keep it running and restart on failure, install the binary and the sample
[systemd user unit](contrib/droidcam-relay.service):

```sh
cargo build --release
install -Dm755 target/release/droidcam-relay ~/.local/bin/droidcam-relay
install -Dm644 contrib/droidcam-relay.service ~/.config/systemd/user/droidcam-relay.service
systemctl --user enable --now droidcam-relay
```

`systemctl --user status droidcam-relay` shows its state; logs go to `journalctl --user -u droidcam-relay`.

## Configuration

Everything is optional and set via environment variables.

| Variable | Default |
|----------|---------|
| `PHONE_URL` | — (else `PHONE_IP`, else LAN discovery) |
| `PHONE_IP`  | — |
| `DEVICE`    | auto-detected v4l2loopback node |
| `OUTPUT_SIZE` | `1280x720` — everything is scaled to this, so the size never changes |

## Development

```sh
cargo test        # unit tests for frame extraction, URL parsing and discovery predicates
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

CI runs the same checks on every push and pull request (`.github/workflows/ci.yml`).
Pushing a `v*` tag builds a release binary and attaches it to a GitHub release.

Enable the pre-commit hook (runs fmt, clippy and tests before each commit) once per clone:

```sh
git config core.hooksPath .githooks
```
