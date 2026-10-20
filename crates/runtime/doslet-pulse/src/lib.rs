//! Linux audio through the Rust PulseAudio protocol client. No libpulse FFI.
#![cfg(target_os = "linux")]

use std::{
    future::Future,
    os::unix::net::UnixStream,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

pub use pulseaudio::{Client, PlaybackSource, PlaybackStream, protocol};

/// Bound both authentication I/O and subsequent asynchronous control requests.
pub const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn connect() -> Result<Client, String> {
    connect_named(c"Doslet")
}

pub fn connect_named(name: &std::ffi::CStr) -> Result<Client, String> {
    let path = pulseaudio::socket_path_from_env().ok_or("PulseAudio socket unavailable")?;
    let socket = UnixStream::connect(path).map_err(|e| format!("PulseAudio socket: {e}"))?;
    socket
        .set_read_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(CONTROL_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let cookie = pulseaudio::cookie_path_from_env().and_then(|path| std::fs::read(path).ok());
    Client::new_unix(name, socket, cookie).map_err(|e| format!("PulseAudio handshake: {e}"))
}

struct Notify(thread::Thread);
impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

/// Control operations only; never called from a playback callback or CPU step.
pub fn wait<T, E: std::fmt::Display>(
    future: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    let waker = Waker::from(Arc::new(Notify(thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(result) => return result.map_err(|e| format!("PulseAudio: {e}")),
            Poll::Pending => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("PulseAudio control request timed out".into());
                }
                thread::park_timeout(remaining);
            }
        }
    }
}

pub fn playback_params(
    rate: u32,
    channels: u8,
    name: &std::ffi::CStr,
) -> Result<protocol::PlaybackStreamParams, String> {
    if !(1000..=192000).contains(&rate) || !(1..=2).contains(&channels) {
        return Err("PulseAudio PCM requires 1000..192000 Hz and one or two channels".into());
    }
    let mut props = protocol::Props::new();
    // Some desktop mixers show only application.name, ignoring media.name.
    // Include the source in both so independently controlled streams differ.
    props.set(protocol::Prop::ApplicationName, name);
    props.set(protocol::Prop::ApplicationId, c"org.doslet.Doslet");
    props.set(protocol::Prop::ApplicationProcessBinary, c"doslet");
    props.set(protocol::Prop::ApplicationIconName, c"org.doslet.Doslet");
    props.set(protocol::Prop::MediaName, name);
    props.set(protocol::Prop::MediaRole, c"game");
    let frame_bytes = u32::from(channels) * 4;
    Ok(protocol::PlaybackStreamParams {
        sample_spec: protocol::SampleSpec {
            format: protocol::SampleFormat::Float32Le,
            channels,
            sample_rate: rate,
        },
        channel_map: if channels == 1 {
            protocol::ChannelMap::mono()
        } else {
            protocol::ChannelMap::stereo()
        },
        buffer_attr: protocol::stream::BufferAttr {
            max_length: rate / 10 * frame_bytes,
            target_length: (rate * 30 / 1000) * frame_bytes,
            pre_buffering: 0,
            minimum_request_length: rate / 100 * frame_bytes,
            ..Default::default()
        },
        flags: protocol::stream::StreamFlags {
            start_corked: true,
            adjust_latency: true,
            ..Default::default()
        },
        props,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_rates_keep_whole_frames_and_explicit_formats() {
        for rate in [8000, 11025, 22050, 32000, 44100, 48000, 192000] {
            for channels in [1, 2] {
                let p = playback_params(rate, channels, c"test").unwrap();
                let size = u32::from(channels) * 4;
                assert_eq!(p.sample_spec.sample_rate, rate);
                assert_eq!(p.buffer_attr.target_length % size, 0);
                assert_eq!(p.buffer_attr.minimum_request_length % size, 0);
                assert!(p.buffer_attr.target_length < p.buffer_attr.max_length);
                assert!(!p.flags.fix_rate);
            }
        }
        assert!(playback_params(u32::MAX, 2, c"test").is_err());
        assert!(playback_params(48000, 0, c"test").is_err());
    }
}
