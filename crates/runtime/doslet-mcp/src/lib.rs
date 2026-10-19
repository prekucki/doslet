//! Optional host MCP transport for observing and controlling a desktop guest.
mod keys;
mod tools;
pub use keys::{Stroke, key_stroke, text_strokes};
use rmcp::schemars;
use serde::Serialize;
use std::{
    io,
    net::{SocketAddr, TcpListener},
    sync::{Arc, mpsc},
    thread::JoinHandle,
};
use tokio::sync::oneshot;

pub const QUEUE: usize = 16;
pub type Reply = oneshot::Sender<Result<Response, String>>;

#[derive(Clone, Debug)]
pub enum Action {
    CodeTags {
        start: u32,
        end: u32,
        bits: u8,
        tier: Option<CodeTier>,
    },
    Screenshot,
    Status,
    Synchronization {
        enabled: bool,
        adaptive: bool,
    },
    CpuMhz {
        mhz: u16,
    },
    Move {
        x: u32,
        y: u32,
    },
    MoveRelative {
        dx: i16,
        dy: i16,
    },
    Click {
        position: Option<(u32, u32)>,
        button: u8,
        hold_ms: u16,
    },
    Keys {
        strokes: Vec<Stroke>,
        hold_ms: u16,
    },
    Wait {
        milliseconds: u16,
    },
    Pause,
    Resume,
    Exit,
}
#[derive(Clone, Copy, Debug, serde::Deserialize, Serialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CodeTier {
    Hot,
    Cold,
    Grey,
}
pub struct Request {
    pub action: Action,
    pub reply: Reply,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub name: String,
    pub status: String,
    pub steps: u64,
    pub emulated_nanos: u64,
    pub cs: u16,
    pub eip: u32,
    pub linear_pc: u32,
    pub width: u32,
    pub height: u32,
    pub gdb_connected: bool,
    pub cpu_mhz: u16,
    pub speed_tiers: Option<serde_json::Value>,
    /// DOS driver position; games consuming relative motion may draw elsewhere.
    pub mouse_driver_x: u16,
    pub mouse_driver_y: u16,
    pub mouse_buttons: u16,
    pub mouse_relative: bool,
    pub mouse_captured: bool,
    pub menu_idle_nanos: u64,
    pub adaptive_idle_interval_nanos: u64,
    pub adaptive_idle_head: u32,
    pub idle_observer_enabled: bool,
    pub synchronization: String,
    pub synchronization_analysis: serde_json::Value,
    pub host_audio_underruns: Option<u64>,
    pub guest_fm_started: bool,
    pub guest_fm_key_on_writes: u64,
    pub guest_fm_peak: u16,
    pub guest_fm_levels: [u8; 4],
    pub host_process_id: u32,
    /// Linux process user+system CPU ticks; None when procfs is unavailable.
    /// Compare deltas per wall-time interval; no assumed tick frequency.
    pub host_process_cpu_ticks: Option<u64>,
    pub guest_audio_service_deadline_misses: Option<u64>,
    /// Count of observed consecutive SB16 DMA block IRQ service windows.
    pub guest_audio_service_deadline_checks: u64,
    /// Scope of the optional deadline counter; null until a window completes.
    pub guest_audio_service_deadline_source: Option<&'static str>,
    pub learned_idle_enabled: bool,
    pub idle_observer_candidate: bool,
    pub idle_observer_reason: String,
    pub idle_observer_samples: u32,
    pub idle_observer_repeated: u32,
    pub idle_observer_cs: u16,
    pub idle_observer_ip: u32,
    pub idle_observer_queries: u64,
    pub idle_observer_writes: u64,
    pub idle_observer_device_accesses: u64,
    pub adaptive_idle_input_wakes: u64,
    pub adaptive_idle_work_wakes: u64,
    pub adaptive_idle_interrupt_wakes: u64,
}
#[derive(Debug)]
pub struct Screen {
    pub status: Status,
    pub rgba: Vec<u8>,
}
#[derive(Debug)]
pub enum Response {
    CodeTags(serde_json::Value),
    Screen(Screen),
    Status(Status),
    Exiting,
}

/// The server owns no machine, framebuffer cache, or per-frame observer.
pub struct Server {
    address: SocketAddr,
    requests: mpsc::Receiver<Request>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    pub fn listen(address: SocketAddr) -> io::Result<Self> {
        if !address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MCP requires a loopback address",
            ));
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (send, requests) = mpsc::sync_channel(QUEUE);
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("doslet-mcp".into())
            .spawn(move || {
                runtime.block_on(async move {
                    use rmcp::transport::streamable_http_server::{
                        StreamableHttpServerConfig, StreamableHttpService,
                        session::local::LocalSessionManager,
                    };
                    let mut config = StreamableHttpServerConfig::default();
                    config.legacy_session_mode = false;
                    config.json_response = true;
                    config.max_request_body_bytes = 16 * 1024;
                    config.allowed_hosts =
                        vec![address.to_string(), format!("localhost:{}", address.port())];
                    config.allowed_origins = vec![
                        format!("http://{address}"),
                        format!("http://localhost:{}", address.port()),
                    ];
                    let cancellation = config.cancellation_token.clone();
                    let service = StreamableHttpService::new(
                        move || Ok(tools::ComputerTools::new(send.clone())),
                        Arc::new(LocalSessionManager::default()),
                        config,
                    );
                    let router = axum::Router::new().nest_service("/mcp", service);
                    let listener = match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => listener,
                        Err(e) => {
                            eprintln!("[ERROR doslet] MCP listener: {e}");
                            return;
                        }
                    };
                    // Finish the accepted exit reply before dropping the transport.
                    // Bound draining so an idle/misbehaving client cannot block shutdown.
                    let (shutdown, draining) = oneshot::channel();
                    let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
                        let _ = stopped.await;
                        let _ = shutdown.send(());
                    });
                    tokio::select! {
                        result = serve => {
                            if let Err(e) = result { eprintln!("[ERROR doslet] MCP server: {e}"); }
                        }
                        _ = async {
                            let _ = draining.await;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        } => {}
                    }
                    cancellation.cancel();
                });
            })?;
        eprintln!("MCP listening on http://{address}/mcp");
        Ok(Self {
            address,
            requests,
            stop: Some(stop),
            thread: Some(thread),
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn try_request(&self) -> Option<Request> {
        self.requests.try_recv().ok()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
