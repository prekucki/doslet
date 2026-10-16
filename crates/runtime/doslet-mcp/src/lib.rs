//! Optional host MCP transport for observing and controlling a desktop guest.
mod keys;
mod tools;
pub use keys::{Stroke, key_stroke, text_strokes};
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
    Screenshot,
    Status,
    CpuMhz {
        mhz: u16,
    },
    Move {
        x: u32,
        y: u32,
    },
    Click {
        x: u32,
        y: u32,
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
}
#[derive(Debug)]
pub struct Screen {
    pub status: Status,
    pub rgba: Vec<u8>,
}
#[derive(Debug)]
pub enum Response {
    Screen(Screen),
    Status(Status),
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
                    tokio::select! {
                        result = axum::serve(listener, router) => {
                            if let Err(e) = result { eprintln!("[ERROR doslet] MCP server: {e}"); }
                        }
                        _ = stopped => cancellation.cancel(),
                    }
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
