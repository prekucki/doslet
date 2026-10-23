//! Desktop presentation owns snapshots; the emulation thread exclusively owns Session.
use crate::app::Message;
use crate::native_effects::Effects;
use crate::presentation::{DisplayFrame, DisplaySizing, FrameRate};
use doslet_midi_routing::Mode;
use doslet_runtime::{RuntimeError, SessionStatus};
use iced::{Element, Subscription, Task, Theme};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU16, Ordering},
    mpsc,
};

use crate::wakeup;

const COMMAND_CAPACITY: usize = 256;

/// Apply the profile's frame cap to the live presentation rate. Neither
/// setting changes guest timing, input or audio.
fn publish_interval(
    rate: FrameRate,
    guest_millihertz: u32,
    frame_limit: u16,
) -> std::time::Duration {
    let requested = rate.interval(guest_millihertz);
    if frame_limit == 0 {
        requested
    } else {
        requested.max(std::time::Duration::from_secs(1) / u32::from(frame_limit))
    }
}

pub struct Registers {
    pub cs: u16,
    pub ip: u32,
    pub values: [u16; 8],
}
impl Registers {
    pub fn get(&self, register: doslet_x86::Gpr16) -> u16 {
        self.values[register as usize]
    }
}
pub struct SessionSnapshot {
    pub status: SessionStatus,
    pub steps: u64,
    pub registers: Registers,
    pub fault: Option<RuntimeError>,
    pub v86_error: Option<String>,
    pub pending_commit: bool,
    pub storage_waiting: bool,
    pub dirty: bool,
}
impl SessionSnapshot {
    pub fn status(&self) -> SessionStatus {
        self.status
    }
    pub fn steps(&self) -> u64 {
        self.steps
    }
    pub fn registers(&self) -> &Registers {
        &self.registers
    }
    pub fn fault(&self) -> Option<RuntimeError> {
        self.fault
    }
    pub fn v86_last_error(&self) -> Option<&str> {
        self.v86_error.as_deref()
    }
    pub fn pending_commit(&self) -> bool {
        self.pending_commit
    }
    pub fn storage_waiting(&self) -> bool {
        self.storage_waiting
    }
    pub fn dirty_overlay(&self) -> bool {
        self.dirty
    }
}
#[allow(dead_code)]
pub struct Snapshot {
    pub cpu_settings: crate::cpu_config::Settings,
    pub v86_host_clock: bool,
    pub v86_ram_waits: Option<(bool, u64)>,
    pub v86_vga_polling: Option<(bool, u64)>,
    pub v86_mpu_polling: Option<(bool, u64)>,
    pub v86_input_polling: Option<(bool, u64)>,
    pub idle_loops: bool,
    pub vga_waits: bool,
    pub cpu_profile: Option<crate::cpu_config::ProfileInfo>,
    pub cpu_learning_available: bool,
    pub disk_reads: (u64, u64),
    pub video_mode_label: String,
    /// Guest vertical refresh in millihertz.
    pub video_refresh_mhz: u32,
    pub audio_volume: u8,
    pub audio_use: doslet_runtime::AudioUse,
    pub memory_usage: doslet_runtime::session::MemoryUsage,
    pub(crate) cpu_activity: Option<crate::cpu_meter::Activity>,
    pub(crate) speed_tiers: Option<crate::native_speed_tiers::Stats>,
    pub analysis: doslet_runtime::session::AnalyzerStats,
    pub host_audio_underruns: Option<u64>,
    pub synchronization: doslet_runtime::session::SynchronizationStats,
    pub idle_observation: doslet_runtime::session::IdleObservation,
    pub adaptive_idle_percent: u8,
    pub learned_idle: bool,
    pub menu_idle: (bool, u64, u64),
    pub mcp: Option<std::net::SocketAddr>,
    pub gdb: Option<(std::net::SocketAddr, bool)>,
    pub session: SessionSnapshot,
    pub workspace: crate::ui::Workspace,
    pub configurator: Option<crate::configurator::ConfiguratorState>,
    pub display: DisplayFrame,
    pub name: String,
    pub notice: Option<String>,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub audio_mixer: crate::audio::Mixer,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
    pub midi_activity: crate::midi_activity::MidiActivity,
    pub midi_channels: [doslet_midi_routing::Channel; 16],
    pub spectrum: [f32; doslet_pcm::spectrum::BANDS],
    pub pointer_dimensions: (u16, u16),
    pub captured: bool,
    pub hide_cursor: bool,
    pub audio_label: String,
    pub storage_label: String,
    pub midi_label: String,
    pub guest_audio: String,
    pub lag_nanos: u64,
    #[cfg(test)]
    pub guest_nanos: u64,
    #[cfg(test)]
    pub heroes_movie: Option<(u32, u32)>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn worker(running: bool) -> (Worker, Snapshot) {
        Worker::spawn_with(move || {
            assert_eq!(std::thread::current().name(), Some("doslet-emulation"));
            let mut engine = Box::new(crate::app::Doslet::new());
            engine.session = doslet_runtime::Session::new(&[0x40, 0xEB, 0xFD])
                .unwrap()
                .into();
            if running {
                engine.session.start();
            }
            Ok(engine)
        })
        .unwrap()
    }
    fn wait_for(worker: &Worker, matches: impl Fn(&Snapshot) -> bool) -> Packet {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(packet) = worker.mailbox.take()
                && matches(&packet.snapshot)
            {
                return packet;
            }
            assert!(
                Instant::now() < deadline,
                "emulation worker did not respond"
            );
            worker.request.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn idle_switches_reach_the_worker_and_return_in_snapshots() {
        let (mut worker, initial) = worker(false);
        assert!(initial.idle_loops);
        assert!(initial.vga_waits);
        worker.send(Message::IdleLoopsToggle(false)).unwrap();
        worker.send(Message::VgaWaitsToggle(false)).unwrap();
        let changed = wait_for(&worker, |s| !s.idle_loops && !s.vga_waits);
        assert_eq!(changed.snapshot.session.status(), SessionStatus::Ready);
        worker.send(Message::IdleLoopsToggle(true)).unwrap();
        worker.send(Message::VgaWaitsToggle(true)).unwrap();
        wait_for(&worker, |s| s.idle_loops && s.vga_waits);
        drop(worker);
    }

    #[test]
    fn emulation_advances_without_ui_ticks_and_pause_step_reset_remain_ordered() {
        let (mut worker, initial) = worker(true);
        assert_ne!(
            worker.thread.as_ref().unwrap().thread().id(),
            std::thread::current().id()
        );
        assert_eq!(initial.session.steps(), 0);
        let first = wait_for(&worker, |s| s.session.steps() > 0);
        // No Tick command and no presentation requests during this interval.
        std::thread::sleep(Duration::from_millis(60));
        worker.send(Message::RunPause).unwrap();
        let paused = wait_for(&worker, |s| s.session.status() == SessionStatus::Paused);
        let steps = paused.snapshot.session.steps();
        assert!(
            steps > first.snapshot.session.steps(),
            "the core must advance while presentation is stalled"
        );
        std::thread::sleep(Duration::from_millis(30));
        let stable = wait_for(&worker, |s| s.session.status() == SessionStatus::Paused);
        assert_eq!(stable.snapshot.session.steps(), steps);
        worker.send(Message::Step).unwrap();
        let stepped = wait_for(&worker, |s| s.session.steps() == steps + 1);
        assert_eq!(stepped.snapshot.session.status(), SessionStatus::Paused);
        worker.send(Message::Reset).unwrap();
        wait_for(&worker, |s| {
            s.session.steps() == 0 && s.session.status() == SessionStatus::Ready
        });
        drop(worker); // Joins the owner and drops its resources on that thread.
    }

    #[test]
    fn frame_limit_caps_published_pictures_without_slowing_the_guest() {
        let (worker, _) = Worker::spawn_with(|| {
            let mut engine = Box::new(crate::app::Doslet::new());
            engine.session = doslet_runtime::Session::new(&[0x40, 0xEB, 0xFD])
                .unwrap()
                .into();
            engine.configurator.active.output.frame_limit = 20;
            engine.session.start();
            Ok(engine)
        })
        .unwrap();
        let start = Instant::now();
        let (mut packets, mut steps) = (0, 0);
        while start.elapsed() < Duration::from_millis(500) {
            // The UI asks for a picture as often as it can.
            worker.request.store(true, Ordering::Release);
            if let Some(packet) = worker.mailbox.take() {
                packets += 1;
                steps = packet.snapshot.session.steps();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!((1..=12).contains(&packets), "{packets} pictures in 500 ms");
        assert!(steps > 0);
    }

    #[test]
    fn profile_cap_and_live_rate_use_the_slower_presentation_interval() {
        assert_eq!(
            publish_interval(FrameRate::Limit(60), 70_000, 35),
            Duration::from_secs(1) / 35
        );
        assert_eq!(
            publish_interval(FrameRate::Limit(30), 70_000, 70),
            Duration::from_secs(1) / 30
        );
        assert_eq!(
            publish_interval(FrameRate::Guest, 70_000, 0),
            FrameRate::Guest.interval(70_000)
        );
    }

    #[test]
    fn gdb_partial_packet_keeps_owner_responsive_and_paused() {
        use std::io::Write;
        let (mut worker, initial) = Worker::spawn_with(|| {
            let mut engine = Box::new(crate::app::Doslet::new());
            engine.session = doslet_runtime::Session::new(&[0x40, 0xEB, 0xFD])
                .unwrap()
                .into();
            engine.session.start();
            engine
                .enable_gdb("127.0.0.1:0".parse().unwrap())
                .map_err(|e| e.to_string())?;
            Ok(engine)
        })
        .unwrap();
        let address = initial.gdb.unwrap().0;
        let mut client = std::net::TcpStream::connect(address).unwrap();
        client.write_all(b"$qSupported").unwrap();
        let paused = wait_for(&worker, |s| s.session.status() == SessionStatus::Paused);
        let steps = paused.snapshot.session.steps();
        assert_eq!(paused.snapshot.gdb, Some((address, true)));
        worker.send(Message::Step).unwrap();
        let refused = wait_for(&worker, |s| {
            s.notice
                .as_ref()
                .is_some_and(|n| n.contains("GDB controls"))
        });
        assert_eq!(refused.snapshot.session.steps(), steps);
        worker.send(Message::GdbToggle).unwrap();
        wait_for(&worker, |s| s.gdb.is_none() && s.session.steps() > steps);
        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[ignore = "requires GNU GDB and localhost TCP"]
    fn local_gdb_desktop_owner_steps_patches_and_detaches() {
        let (worker, initial) = Worker::spawn_with(|| {
            let mut engine = Box::new(crate::app::Doslet::new());
            engine.session = doslet_runtime::Session::new(&[0x40, 0xEB, 0xFD])
                .unwrap()
                .into();
            engine
                .enable_gdb("127.0.0.1:0".parse().unwrap())
                .map_err(|e| e.to_string())?;
            Ok(engine)
        })
        .unwrap();
        let address = initial.gdb.unwrap().0;
        let mut gdb = std::process::Command::new("gdb");
        gdb.args(["-nx", "-nh", "--batch"]);
        for command in [
            "set confirm off".to_string(),
            format!("target remote {address}"),
            "set $eax = 0x1234".into(),
            "stepi".into(),
            "set {unsigned char}0x10100 = 0x48".into(),
            "set $pc = 0x10100".into(),
            "break *0x10101".into(),
            "continue".into(),
            "stepi".into(),
            "monitor state".into(),
            "detach".into(),
        ] {
            gdb.args(["-ex", &command]);
        }
        let output = gdb.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let final_state = wait_for(&worker, |s| {
            s.session.steps() == 3 && s.gdb == Some((address, false))
        });
        assert_eq!(final_state.snapshot.session.status(), SessionStatus::Paused);
        assert_eq!(
            final_state
                .snapshot
                .session
                .registers()
                .get(doslet_x86::Gpr16::Ax),
            0x1234
        );
    }

    fn mcp_rpc(
        address: std::net::SocketAddr,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let body =
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build();
        let mut response = ureq::Agent::new_with_config(config)
            .post(&format!("http://{address}/mcp"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .send(body.as_str())
            .unwrap();
        serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap()
    }
    fn mcp_tool(
        address: std::net::SocketAddr,
        name: &str,
        arguments: serde_json::Value,
    ) -> serde_json::Value {
        let response = mcp_rpc(
            address,
            "tools/call",
            serde_json::json!({"name":name,"arguments":arguments}),
        );
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    fn mcp_screen(result: &serde_json::Value) -> (u32, u32, Vec<u8>) {
        use base64::Engine;
        assert_ne!(result["isError"], true, "{result}");
        let content = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "image")
            .unwrap();
        assert_eq!(content["mimeType"], "image/png");
        let png = base64::engine::general_purpose::STANDARD
            .decode(content["data"].as_str().unwrap())
            .unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        bytes.truncate(info.buffer_size());
        (info.width, info.height, bytes)
    }
    #[test]
    fn mcp_http_drives_guest_keyboard_mouse_and_reads_native_png() {
        let (worker, initial) = Worker::spawn_with(|| {
            assert_eq!(std::thread::current().name(), Some("doslet-emulation"));
            let mut engine = Box::new(crate::app::Doslet::new());
            // A local redistributable guest: mode 13h, poll mouse, read BIOS
            // keys, paint the last ASCII code at pixel zero and retain it in SI.
            let mut code = vec![
                0xB8, 0x13, 0, 0xCD, 0x10, 0xB8, 0, 0xA0, 0x8E, 0xC0, 0x31, 0xC0, 0xCD, 0x33,
            ];
            let top = code.len();
            code.extend([
                0xB8, 3, 0, 0xCD, 0x33, 0x26, 0x88, 0x1E, 1, 0, 0xB4, 1, 0xCD, 0x16,
            ]);
            let idle = code.len();
            code.extend([0x74, 0, 0xB4, 0, 0xCD, 0x16, 0x89, 0xC6, 0x26, 0xA2, 0, 0]);
            let back = code.len();
            code.extend([0xEB, 0]);
            code[idle + 1] = (top as isize - idle as isize - 2) as i8 as u8;
            code[back + 1] = (top as isize - back as isize - 2) as i8 as u8;
            engine.session = doslet_runtime::Session::new(&code).unwrap().into();
            engine.session.start();
            engine
                .enable_mcp("127.0.0.1:0".parse().unwrap())
                .map_err(|e| e.to_string())?;
            Ok(engine)
        })
        .unwrap();
        let address = initial.mcp.unwrap();
        let init = mcp_rpc(
            address,
            "initialize",
            serde_json::json!({"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"doslet-test","version":"1"}}),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25", "{init}");
        let tools = mcp_rpc(address, "tools/list", serde_json::json!({}));
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "screenshot")
        );
        wait_for(&worker, |s| {
            s.display.dimensions().is_some_and(|s| s.width == 320.0)
        });
        let screen = mcp_tool(address, "screenshot", serde_json::json!({}));
        let (w, h, before) = mcp_screen(&screen);
        assert_eq!((w, h), (320, 200));
        let moved = mcp_tool(address, "move_mouse", serde_json::json!({"x":319,"y":199}));
        mcp_screen(&moved);
        wait_for(&worker, |s| {
            s.session.registers().get(doslet_x86::Gpr16::Cx) == 639
                && s.session.registers().get(doslet_x86::Gpr16::Dx) == 199
        });
        let click = mcp_tool(
            address,
            "click",
            serde_json::json!({"x":160,"y":100,"hold_ms":30}),
        );
        mcp_screen(&click);
        let typed = mcp_tool(address, "type_text", serde_json::json!({"text":"Aa"}));
        let (_, _, after) = mcp_screen(&typed);
        wait_for(&worker, |s| {
            s.session.registers().get(doslet_x86::Gpr16::Si) & 0xff == u16::from(b'a')
        });
        assert_ne!(&before[..4], &after[..4]);
        let invalid = mcp_tool(address, "move_mouse", serde_json::json!({"x":320,"y":0}));
        assert_eq!(invalid["isError"], true);
        let paused = mcp_tool(address, "pause", serde_json::json!({}));
        assert_eq!(paused["structuredContent"]["status"], "Paused");
        let steps = paused["structuredContent"]["steps"].as_u64().unwrap();
        let changed = mcp_tool(address, "set_cpu_mhz", serde_json::json!({"mhz":133}));
        assert_eq!(changed["structuredContent"]["cpu_mhz"], 133);
        assert_eq!(
            changed["structuredContent"]["steps"].as_u64().unwrap(),
            steps
        );
        assert_eq!(changed["structuredContent"]["status"], "Paused");
        assert_eq!(
            changed["structuredContent"]["emulated_nanos"],
            paused["structuredContent"]["emulated_nanos"]
        );
        let blocked = mcp_tool(address, "key_press", serde_json::json!({"key":"Escape"}));
        assert_eq!(blocked["isError"], true);
        let still = mcp_tool(address, "status", serde_json::json!({}));
        assert_eq!(still["structuredContent"]["steps"].as_u64().unwrap(), steps);
        let resumed = mcp_tool(address, "resume", serde_json::json!({}));
        assert_eq!(resumed["structuredContent"]["status"], "Running");
        mcp_screen(&mcp_tool(
            address,
            "wait",
            serde_json::json!({"milliseconds":20}),
        ));
        drop(worker);
    }

    #[test]
    fn mcp_exit_acknowledges_and_requests_shutdown_even_when_paused() {
        for running in [true, false] {
            let (worker, initial) = Worker::spawn_with(move || {
                let mut engine = Box::new(crate::app::Doslet::new());
                engine.session = doslet_runtime::Session::new(&[0x40, 0xEB, 0xFD])
                    .unwrap()
                    .into();
                engine.session.start();
                if !running {
                    engine.session.pause();
                }
                engine
                    .enable_mcp("127.0.0.1:0".parse().unwrap())
                    .map_err(|e| e.to_string())?;
                Ok(engine)
            })
            .unwrap();
            let address = initial.mcp.unwrap();
            let tools = mcp_rpc(address, "tools/list", serde_json::json!({}));
            assert!(
                tools["result"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|t| {
                        t["name"] == "exit" && t["annotations"]["destructiveHint"] == true
                    })
            );
            let exiting = mcp_tool(address, "exit", serde_json::json!({}));
            assert_ne!(exiting["isError"], true, "{exiting}");
            assert_eq!(exiting["structuredContent"]["status"], "Exiting");
            let packet = wait_for(&worker, |s| s.session.status() == SessionStatus::Paused);
            use iced::futures::{StreamExt, executor::block_on};
            let mut actions = iced_runtime::task::into_stream(packet.effects.into_task()).unwrap();
            assert!(
                block_on(async {
                    while let Some(action) = actions.next().await {
                        if matches!(action, iced_runtime::Action::Exit) {
                            return true;
                        }
                    }
                    false
                }),
                "shutdown must reach the UI"
            );
            let rejected = mcp_tool(address, "resume", serde_json::json!({}));
            assert_eq!(rejected["isError"], true);
            let began = Instant::now();
            drop(worker);
            assert!(began.elapsed() < Duration::from_secs(5));
            assert!(std::net::TcpStream::connect(address).is_err());
        }
    }

    #[test]
    fn bounded_input_overflow_pauses_the_machine() {
        let (worker, _) = worker(true);
        worker.overflow.store(true, Ordering::Release);
        let paused = wait_for(&worker, |s| s.session.status() == SessionStatus::Paused);
        assert!(
            paused
                .snapshot
                .notice
                .unwrap()
                .contains("held input released")
        );
    }

    #[test]
    fn input_queue_is_bounded_and_preparation_completion_has_reserved_capacity() {
        let (sender, _receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let mut worker = Worker {
            pointer: Arc::new(Mutex::new(PendingPointer::default())),
            sender,
            completion_sender,
            sequence: 0,
            mailbox: Arc::new(Mailbox::default()),
            request: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
            frame_rate: Arc::new(AtomicU16::new(0)),
            signal: wakeup::Signal::default(),
            thread: None,
        };
        for _ in 0..COMMAND_CAPACITY {
            worker.send(Message::PointerButton(0, true)).unwrap();
        }
        assert!(worker.send(Message::PointerButton(0, false)).is_err());
        assert!(worker.overflow.load(Ordering::Acquire));
        worker
            .send(Message::MachinePrepared(Err("preparation failed".into())))
            .unwrap();
        assert!(matches!(
            completion_receiver.try_recv().unwrap().message,
            Message::MachinePrepared(Err(_))
        ));
    }

    #[test]
    fn pointer_floods_are_coalesced_instead_of_overflowing_the_queue() {
        let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (completion_sender, _completion_receiver) = mpsc::sync_channel(1);
        let mut worker = Worker {
            pointer: Arc::new(Mutex::new(PendingPointer::default())),
            sender,
            completion_sender,
            sequence: 0,
            mailbox: Arc::new(Mailbox::default()),
            request: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
            frame_rate: Arc::new(AtomicU16::new(0)),
            signal: wakeup::Signal::default(),
            thread: None,
        };
        let id = iced::window::Id::unique();
        let motion = |dx, dy| {
            Message::MouseCapture(id, 7, iced::window::MouseCaptureEvent::Motion { dx, dy })
        };
        // A 1000 Hz mouse during a long worker stall.
        for _ in 0..(COMMAND_CAPACITY * 4) {
            worker.send(motion(1.0, -0.5)).unwrap();
            worker.send(Message::PointerMoved(3, 4)).unwrap();
        }
        assert!(!worker.overflow.load(Ordering::Acquire));
        // One wake-up for the whole batch.
        assert!(matches!(
            receiver.try_recv().unwrap().message,
            Message::Tick
        ));
        assert!(receiver.try_recv().is_err());
        let replay: Vec<_> = worker.pointer.lock().unwrap().take_messages().collect();
        let total = (COMMAND_CAPACITY * 4) as f64;
        assert!(matches!(
            replay.as_slice(),
            [
                Message::MouseCapture(_, 7, iced::window::MouseCaptureEvent::Motion { dx, dy }),
                Message::PointerMoved(3, 4),
            ] if *dx == total && *dy == -total / 2.0
        ));
        // Leaving drops older positions; re-entering replays after the leave.
        worker.send(Message::PointerMoved(1, 1)).unwrap();
        worker.send(Message::PointerLeft).unwrap();
        let replay: Vec<_> = worker.pointer.lock().unwrap().take_messages().collect();
        assert!(matches!(replay.as_slice(), [Message::PointerLeft]));
        worker.send(Message::PointerLeft).unwrap();
        worker.send(Message::PointerMoved(2, 2)).unwrap();
        let replay: Vec<_> = worker.pointer.lock().unwrap().take_messages().collect();
        assert!(matches!(
            replay.as_slice(),
            [Message::PointerLeft, Message::PointerMoved(2, 2)]
        ));
        // A new capture drops the motion of the previous one.
        worker.send(motion(5.0, 5.0)).unwrap();
        worker
            .send(Message::MouseCapture(
                id,
                8,
                iced::window::MouseCaptureEvent::Motion { dx: 1.0, dy: 2.0 },
            ))
            .unwrap();
        let replay: Vec<_> = worker.pointer.lock().unwrap().take_messages().collect();
        assert!(matches!(
            replay.as_slice(),
            [Message::MouseCapture(_, 8, iced::window::MouseCaptureEvent::Motion { dx, dy })]
                if *dx == 1.0 && *dy == 2.0
        ));
    }

    #[test]
    fn worker_reports_setup_failure_before_opening_a_window() {
        assert!(
            matches!(Worker::spawn_with(|| Err("setup failed".into())), Err(error) if error == "setup failed")
        );
    }

    #[test]
    fn mailbox_keeps_only_the_latest_frame_and_retains_pending_configuration() {
        let mut engine = crate::app::Doslet::new();
        let mailbox = Mailbox::default();
        mailbox.publish(Packet {
            snapshot: engine.snapshot(true),
            effects: Effects::default(),
            sequence: 1,
        });
        engine.session.single_step();
        mailbox.publish(Packet {
            snapshot: engine.snapshot(false),
            effects: Effects::default(),
            sequence: 2,
        });
        let latest = mailbox.take().unwrap();
        assert_eq!(latest.sequence, 2);
        assert_eq!(latest.snapshot.session.steps(), 1);
        assert!(latest.snapshot.configurator.is_some());
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn older_snapshots_do_not_overwrite_newer_editor_input() {
        let (worker, mut state) = worker(false);
        let configurator = state.configurator.take().unwrap();
        let mut app = Doslet {
            disk_activity: crate::disk_activity::Activity::default(),
            worker,
            state,
            configurator,
            display_sizing: DisplaySizing::default(),
            config_sequence: 0,
            cpu_sequence: 0,
            tier_meter: crate::speed_tier_meter::Meter::default(),
            from_library: false,
        };
        let _ = app.update(Message::Config(crate::configurator::Edit::Name(
            "New name".into(),
        )));
        assert_eq!(app.configurator.draft.name, "New name");
        let mut old = crate::app::Doslet::new();
        app.worker.mailbox.publish(Packet {
            snapshot: old.snapshot(true),
            effects: Effects::default(),
            sequence: 0,
        });
        let _ = app.update(Message::GamepadsReady);
        assert_eq!(app.configurator.draft.name, "New name");
        let _ = app.update(Message::Cpu(crate::cpu_config::Edit::Grey(100)));
        let _ = app.update(Message::Cpu(crate::cpu_config::Edit::Hot(500)));
        assert_eq!(app.workspace.cpu.unwrap().grey, 100);
        app.worker.mailbox.publish(Packet {
            snapshot: old.snapshot(true),
            effects: Effects::default(),
            sequence: 0,
        });
        let _ = app.update(Message::GamepadsReady);
        assert_eq!(app.workspace.cpu.unwrap().grey, 100);
        assert_eq!(app.workspace.cpu.unwrap().hot, Some(500));
    }

    #[test]
    #[ignore = "requires the local HoMM profile and ISO; measures 35 seconds without a window/audio device"]
    fn local_heroes_worker_pacing() {
        use clap::Parser;
        let profile =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/HoMnM.toml");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            profile.into_os_string(),
            "--no-audio".into(),
        ]);
        let (worker, initial) = Worker::spawn(cli).unwrap();
        println!(
            "HoMM worker: {} MHz, no window/audio device, Rust synthesis retained",
            initial.configurator.unwrap().active.hardware.cpu_mhz
        );
        let start = Instant::now();
        for sample in 1..=7 {
            std::thread::sleep(Duration::from_secs(5));
            // Discard the previous presentation and request exactly one fresh
            // sample; repeated requests would leave a stale extra snapshot.
            worker.mailbox.take();
            worker.request.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(5);
            let packet = loop {
                if let Some(packet) = worker.mailbox.take() {
                    break packet;
                }
                assert!(
                    Instant::now() < deadline,
                    "worker did not publish a fresh sample"
                );
                std::thread::sleep(Duration::from_millis(1));
            };
            let s = packet.snapshot;
            assert_eq!(
                s.session.status(),
                SessionStatus::Running,
                "{:?}",
                s.session.fault()
            );
            println!(
                "sample={sample} host={:.3}s guest={:.3}s lag={:.1}ms steps={} movie={:?}",
                start.elapsed().as_secs_f64(),
                s.guest_nanos as f64 / 1e9,
                s.lag_nanos as f64 / 1e6,
                s.session.steps(),
                s.heroes_movie
            );
        }
    }
}
struct Packet {
    snapshot: Snapshot,
    effects: Effects<Message>,
    sequence: u64,
}
struct Command {
    sequence: u64,
    message: Message,
}
#[derive(Default)]
struct Mailbox(Mutex<Option<Packet>>);
impl Mailbox {
    fn publish(&self, mut packet: Packet) {
        let mut slot = self.0.lock().unwrap();
        if let Some(previous) = slot.take() {
            let mut effects = previous.effects;
            effects.append(packet.effects);
            packet.effects = effects;
            if packet.snapshot.configurator.is_none() {
                packet.snapshot.configurator = previous.snapshot.configurator;
            }
        }
        *slot = Some(packet);
    }
    fn take(&self) -> Option<Packet> {
        self.0.lock().unwrap().take()
    }
}

/// Pointer input where only the sum (captured motion) or the latest value
/// (absolute position) matters. It bypasses the bounded command queue, so a
/// worker that is busy for a while (AOT installation, disk work) cannot be
/// flooded by a high-rate mouse and have to pause the machine. Buttons and
/// keys keep their order in the queue.
#[derive(Default)]
struct PendingPointer {
    motion: Option<(iced::window::Id, u64, f64, f64)>,
    position: Option<(u16, u16)>,
    /// The pointer left the picture; replayed before a later position.
    left: bool,
}
impl PendingPointer {
    fn is_empty(&self) -> bool {
        self.motion.is_none() && self.position.is_none() && !self.left
    }
    /// Sums captured motion; a different capture replaces stale motion.
    fn add_motion(&mut self, id: iced::window::Id, generation: u64, dx: f64, dy: f64) {
        self.motion = Some(match self.motion {
            Some((old, old_generation, x, y)) if old == id && old_generation == generation => {
                (id, generation, x + dx, y + dy)
            }
            _ => (id, generation, dx, dy),
        });
    }
    /// Messages that replay the pending input; it is cleared.
    fn take_messages(&mut self) -> impl Iterator<Item = Message> {
        let pending = std::mem::take(self);
        let motion = pending.motion.map(|(id, generation, dx, dy)| {
            Message::MouseCapture(
                id,
                generation,
                iced::window::MouseCaptureEvent::Motion { dx, dy },
            )
        });
        let left = pending.left.then_some(Message::PointerLeft);
        let position = pending.position.map(|(x, y)| Message::PointerMoved(x, y));
        motion.into_iter().chain(left).chain(position)
    }
}
struct Worker {
    pointer: Arc<Mutex<PendingPointer>>,
    sender: mpsc::SyncSender<Command>,
    completion_sender: mpsc::SyncSender<Command>,
    sequence: u64,
    mailbox: Arc<Mailbox>,
    request: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    /// `FrameRate::code` of the user's presentation rate.
    frame_rate: Arc<AtomicU16>,
    signal: wakeup::Signal,
    thread: Option<std::thread::JoinHandle<()>>,
}
struct NotifyOnExit {
    exited: Arc<AtomicBool>,
    signal: wakeup::Signal,
}
impl Drop for NotifyOnExit {
    fn drop(&mut self) {
        self.exited.store(true, Ordering::Release);
        self.signal.notify();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Wake recv_timeout without waiting for the worker or a full command queue.
        let _ = self.sender.try_send(Command {
            sequence: self.sequence,
            message: Message::Tick,
        });
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Worker {
    /// Folds pointer input into the pending batch instead of queueing it.
    fn coalesce(&mut self, apply: impl FnOnce(&mut PendingPointer)) -> u64 {
        let mut pending = self
            .pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let wake = pending.is_empty();
        apply(&mut pending);
        drop(pending);
        // One wake-up per batch; a full queue already keeps the worker busy.
        if wake {
            let _ = self.sender.try_send(Command {
                sequence: self.sequence,
                message: Message::Tick,
            });
        }
        self.sequence
    }
    fn spawn(cli: crate::cli::Cli) -> Result<(Self, Snapshot), String> {
        Self::spawn_with(move || crate::app::Doslet::from_cli(cli).map_err(|e| e.to_string()))
    }
    fn spawn_with(
        boot: impl FnOnce() -> Result<Box<crate::app::Doslet>, String> + Send + 'static,
    ) -> Result<(Self, Snapshot), String> {
        let (sender, receiver) = mpsc::sync_channel::<Command>(COMMAND_CAPACITY);
        // One preparation can be pending. Its completion must survive an input flood.
        let (completion_sender, completion_receiver) = mpsc::sync_channel::<Command>(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let mailbox = Arc::new(Mailbox::default());
        let request = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let frame_rate = Arc::new(AtomicU16::new(FrameRate::default().code()));
        let signal = wakeup::Signal::default();
        let pointer = Arc::new(Mutex::new(PendingPointer::default()));
        let pending_pointer = pointer.clone();
        let (output, refresh, stopping, overflowed, wake, rate) = (
            mailbox.clone(),
            request.clone(),
            stop.clone(),
            overflow.clone(),
            signal.clone(),
            frame_rate.clone(),
        );
        let on_exit = NotifyOnExit {
            exited: exited.clone(),
            signal: signal.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("doslet-emulation".into())
            .spawn(move || {
                let _on_exit = on_exit;
                // Create thread-affine host/JIT/audio resources on their eventual owner.
                let mut engine = match boot() {
                    Ok(engine) => engine,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                engine.worker_owned = true;
                engine.display.refresh(&engine.session);
                if ready_tx.send(Ok(engine.snapshot(true))).is_err() {
                    return;
                }
                let mut effects = Effects::default();
                let mut config_dirty = false;
                let mut sequence = 0;
                // The ready snapshot covers the first interval.
                let mut next_publish = std::time::Instant::now()
                    + publish_interval(
                        FrameRate::from_code(rate.load(Ordering::Relaxed)),
                        engine.session.video_refresh_millihertz(),
                        engine.configurator.active.output.frame_limit,
                    );
                let mut sample_started = std::time::Instant::now();
                let mut sample_busy = std::time::Duration::ZERO;
                let mut sample_steps = engine.session.steps();
                let mut sample_parks = engine.session.v86_ram_waits().map_or(0, |(_, parks)| parks);
                let mut sample_vga = engine.session.v86_vga_polling().map_or(0, |(_, reads)| reads);
                let mut sample_mpu = engine.session.v86_mpu_polling().map_or(0, |(_, reads)| reads);
                let mut sample_input = engine.session.v86_input_polling().map_or(0, |(_, parks)| parks);
                while !stopping.load(Ordering::Acquire) {
                    let turn_started = std::time::Instant::now();
                    let overflow = overflowed.swap(false, Ordering::AcqRel);
                    // Coalesced pointer input comes before the queued buttons.
                    let pointer = pending_pointer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take_messages()
                        .collect::<Vec<_>>();
                    for message in pointer {
                        effects.push(engine.update(message));
                        refresh.store(true, Ordering::Release);
                    }
                    if let Ok(command) = completion_receiver.try_recv() {
                        sequence = sequence.max(command.sequence);
                        effects.push(engine.update(command.message));
                        config_dirty = true;
                        refresh.store(true, Ordering::Release);
                    }
                    // Command floods cannot starve execution or shutdown indefinitely.
                    for _ in 0..COMMAND_CAPACITY {
                        let command = match receiver.try_recv() {
                            Ok(command) => command,
                            Err(_) => break,
                        };
                        if !matches!(command.message, Message::Tick) {
                            sequence = sequence.max(command.sequence);
                            effects.push(engine.update(command.message));
                            config_dirty = true;
                            refresh.store(true, Ordering::Release);
                        }
                    }
                    if overflow {
                        engine.session.pause();
                        let _ = engine
                            .session
                            .release_input_source(doslet_input::InputSourceId(1));
                        effects.push(engine.update(Message::FocusLost));
                        engine.notice = Some(
                            "Input queue full; emulation paused and held input released.".into(),
                        );
                        effects.push(engine.update(Message::Tick));
                        refresh.store(true, Ordering::Release);
                    }
                    // No iced timer drives execution. Each turn checks commands again.
                    let before = (
                        engine.session.status(),
                        engine.session.storage_waiting(),
                        engine.presentation_revision,
                    );
                    let guest_before = engine.session.emulated_nanos();
                    effects.push(engine.worker_tick());
                    let progressed = engine.session.emulated_nanos() != guest_before;
                    let after = (
                        engine.session.status(),
                        engine.session.storage_waiting(),
                        engine.presentation_revision,
                    );
                    if before != after {
                        refresh.store(true, Ordering::Release);
                        config_dirty = true;
                    }
                    let now = std::time::Instant::now();
                    if refresh.load(Ordering::Acquire) && now >= next_publish {
                        refresh.store(false, Ordering::Release);
                        // Deadlines keep the average rate; a late or idle
                        // stretch restarts the schedule instead of bursting.
                        let interval = publish_interval(
                            FrameRate::from_code(rate.load(Ordering::Relaxed)),
                            engine.session.video_refresh_millihertz(),
                            engine.configurator.active.output.frame_limit,
                        );
                        next_publish = if now - next_publish < interval {
                            next_publish + interval
                        } else {
                            now + interval
                        };
                        output.publish(Packet {
                            snapshot: engine.snapshot(config_dirty),
                            effects: std::mem::take(&mut effects),
                            sequence,
                        });
                        config_dirty = false;
                        wake.notify();
                    }
                    // Pace idle/caught-up execution, but let input wake the worker early.
                    let mut wait = engine.worker_delay(progressed);
                    if refresh.load(Ordering::Acquire) {
                        wait = wait.min(next_publish.saturating_duration_since(now));
                    }
                    sample_busy += turn_started.elapsed();
                    match receiver.recv_timeout(wait) {
                        Ok(Command {
                            message: Message::Tick,
                            ..
                        }) => {}
                        Ok(command) => {
                            sequence = sequence.max(command.sequence);
                            effects.push(engine.update(command.message));
                            config_dirty = true;
                            refresh.store(true, Ordering::Release);
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let elapsed = sample_started.elapsed();
                    if elapsed >= std::time::Duration::from_secs(2) {
                        if let Some((enabled, parks)) = engine.session.v86_ram_waits()
                            && enabled
                            && engine.session.status() == SessionStatus::Running
                        {
                            let seconds = elapsed.as_secs_f64();
                            eprintln!(
                                "[INFO doslet] {}: v86 worker active {:.0}%, {:.0} steps/s, {:.0} RAM parks/s, {:.0} VGA parks/s, {:.0} MPU dense reads/s, {:.0} input parks/s, VGA trap={}, PC={:08x}",
                                engine.name,
                                100.0 * sample_busy.as_secs_f64() / seconds,
                                engine.session.steps().saturating_sub(sample_steps) as f64 / seconds,
                                parks.saturating_sub(sample_parks) as f64 / seconds,
                                engine.session.v86_vga_polling().map_or(0, |(_, reads)| reads).saturating_sub(sample_vga) as f64 / seconds,
                                engine.session.v86_mpu_polling().map_or(0, |(_, reads)| reads).saturating_sub(sample_mpu) as f64 / seconds,
                                engine.session.v86_input_polling().map_or(0, |(_, parks)| parks).saturating_sub(sample_input) as f64 / seconds,
                                engine.session.v86_vga_polling().is_some_and(|(enabled, _)| enabled),
                                engine.session.debug_pc(),
                            );
                        }
                        sample_started = std::time::Instant::now();
                        sample_busy = std::time::Duration::ZERO;
                        sample_steps = engine.session.steps();
                        sample_parks = engine.session.v86_ram_waits().map_or(0, |(_, parks)| parks);
                        sample_vga = engine.session.v86_vga_polling().map_or(0, |(_, reads)| reads);
                        sample_mpu = engine.session.v86_mpu_polling().map_or(0, |(_, reads)| reads);
                        sample_input = engine.session.v86_input_polling().map_or(0, |(_, parks)| parks);
                    }
                }
                engine.save_core_profile();
                engine.save_autosave();
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv().map_err(|e| e.to_string())? {
            Ok(snapshot) => Ok((
                Self {
                    pointer,
                    sender,
                    completion_sender,
                    sequence: 0,
                    mailbox,
                    request,
                    stop,
                    overflow,
                    exited,
                    frame_rate,
                    signal,
                    thread: Some(thread),
                },
                snapshot,
            )),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    fn send(&mut self, message: Message) -> Result<u64, &'static str> {
        match message {
            Message::MouseCapture(
                id,
                generation,
                iced::window::MouseCaptureEvent::Motion { dx, dy },
            ) => return Ok(self.coalesce(|p| p.add_motion(id, generation, dx, dy))),
            Message::PointerMoved(x, y) => {
                return Ok(self.coalesce(|p| p.position = Some((x, y))));
            }
            // Ordered with the coalesced positions: leaving drops older ones.
            Message::PointerLeft => {
                return Ok(self.coalesce(|p| {
                    p.position = None;
                    p.left = true;
                }));
            }
            _ => {}
        }
        let sequence = self.sequence + 1;
        let sender = if matches!(message, Message::MachinePrepared(_)) {
            &self.completion_sender
        } else {
            &self.sender
        };
        match sender.try_send(Command { sequence, message }) {
            Ok(()) => {
                self.sequence = sequence;
                Ok(sequence)
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.overflow.store(true, Ordering::Release);
                Err("Input queue full; waiting for emulation to pause.")
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err("Emulation thread stopped."),
        }
    }
}

pub struct Doslet {
    pub(crate) disk_activity: crate::disk_activity::Activity,
    worker: Worker,
    pub state: Snapshot,
    pub configurator: crate::configurator::ConfiguratorState,
    pub display_sizing: DisplaySizing,
    config_sequence: u64,
    cpu_sequence: u64,
    pub(crate) tier_meter: crate::speed_tier_meter::Meter,
    /// Started from the library, which the toolbar offers to return to.
    pub from_library: bool,
}
impl std::ops::Deref for Doslet {
    type Target = Snapshot;
    fn deref(&self) -> &Snapshot {
        &self.state
    }
}
impl Doslet {
    pub fn from_cli(cli: crate::cli::Cli) -> Result<Box<Self>, Box<dyn std::error::Error>> {
        let global = cli.global_settings.clone();
        let (worker, state) = Worker::spawn(cli)?;
        let mut frontend = Self::from_worker(worker, state);
        if let Some(global) = &global {
            frontend.display_sizing.apply_settings(global);
        }
        Ok(frontend)
    }
    /// A frontend for an engine built by `boot` on its emulation thread.
    #[cfg(test)]
    pub(crate) fn with_engine(
        boot: impl FnOnce() -> Result<Box<crate::app::Doslet>, String> + Send + 'static,
    ) -> Box<Self> {
        let (worker, state) = Worker::spawn_with(boot).unwrap();
        Self::from_worker(worker, state)
    }
    fn from_worker(worker: Worker, mut state: Snapshot) -> Box<Self> {
        let configurator = state.configurator.take().unwrap();
        let mut display_sizing = DisplaySizing::default();
        display_sizing.gpu = crate::presentation::hardware_gpu();
        display_sizing.set_output(&configurator.active.output);
        Box::new(Self {
            disk_activity: crate::disk_activity::Activity::new(
                state.disk_reads.0,
                state.disk_reads.1,
            ),
            worker,
            state,
            configurator,
            display_sizing,
            config_sequence: 0,
            cpu_sequence: 0,
            tier_meter: crate::speed_tier_meter::Meter::default(),
            from_library: false,
        })
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        if let Message::DisplaySizing(message) = message {
            if let crate::presentation::SizingMessage::FrameRate(rate) = message {
                self.worker.frame_rate.store(rate.code(), Ordering::Relaxed);
            }
            let leaving = matches!(
                message,
                crate::presentation::SizingMessage::Fullscreen(false)
            );
            let task = self
                .display_sizing
                .update(message, &self.state.display)
                .map(Message::DisplaySizing);
            // The capture lives in the engine; never leave full screen with a
            // locked or confined pointer.
            return if leaving {
                Task::batch([task, Task::done(Message::ReleaseMouse)])
            } else {
                task
            };
        }
        if matches!(message, Message::Tick | Message::GamepadsReady) {
            self.worker.signal.clear();
            let effects = if let Some(mut packet) = self.worker.mailbox.take() {
                if packet.sequence < self.cpu_sequence {
                    packet.snapshot.workspace.cpu = self.state.workspace.cpu;
                    packet.snapshot.workspace.cpu_profile_form =
                        self.state.workspace.cpu_profile_form.clone();
                }
                if let Some(mut configurator) = packet.snapshot.configurator.take()
                    && packet.sequence >= self.config_sequence
                {
                    let previous_filter = self.configurator.active.output.video_filter;
                    // Preserve selection/caret across snapshots with unchanged text.
                    if configurator.profile_text == self.configurator.profile_text {
                        std::mem::swap(
                            &mut configurator.profile_editor,
                            &mut self.configurator.profile_editor,
                        );
                    }
                    self.configurator = configurator;
                    if self.configurator.active.output.video_filter != previous_filter {
                        self.display_sizing
                            .set_output(&self.configurator.active.output);
                    }
                }
                self.state = packet.snapshot;
                self.disk_activity.observe(
                    self.state.disk_reads.0,
                    self.state.disk_reads.1,
                    web_time::Instant::now(),
                );
                self.tier_meter.sample(
                    std::time::Instant::now(),
                    self.state.speed_tiers,
                    self.state.session.status == SessionStatus::Running
                        && !self.state.gdb.is_some_and(|(_, connected)| connected),
                );
                Task::batch([
                    packet.effects.into_task(),
                    self.display_sizing
                        .refresh(&self.state.display)
                        .map(Message::DisplaySizing),
                ])
            } else {
                Task::none()
            };
            if self.session.status() == SessionStatus::Running || matches!(message, Message::Tick) {
                self.worker.request.store(true, Ordering::Release);
            }
            if self.worker.exited.load(Ordering::Acquire) {
                self.state.notice = Some("Emulation thread stopped.".into());
                self.state.session.status = SessionStatus::Paused;
            }
            return effects;
        }
        // Editor cursor actions are presentation state; execution sees the same edits.
        let config = matches!(message, Message::Config(_));
        let cpu = matches!(message, Message::Cpu(_) | Message::CpuProfile(_));
        if let Message::CpuProfile(crate::cpu_config::ProfileEdit::Path(path)) = &message
            && let Some(form) = &mut self.state.workspace.cpu_profile_form
        {
            form.path.clone_from(path);
            form.error = None;
        }
        if let Message::Cpu(edit) = message {
            if matches!(edit, crate::cpu_config::Edit::Revert) {
                self.state.workspace.cpu = Some(self.state.cpu_settings);
            } else if !matches!(edit, crate::cpu_config::Edit::Apply) {
                self.state
                    .workspace
                    .cpu
                    .get_or_insert(self.state.cpu_settings)
                    .edit(edit);
            }
        }
        if let Message::Config(edit) = &message {
            self.configurator.edit(edit);
        }
        let closing = if let Message::CloseRequested(id) = &message {
            Some(*id)
        } else {
            None
        };
        match self.worker.send(message) {
            Ok(sequence) => {
                if config {
                    self.config_sequence = sequence;
                }
                if cpu {
                    self.cpu_sequence = sequence;
                }
            }
            Err(error) => {
                self.state.notice = Some(error.into());
                if let Some(id) = closing {
                    return iced::window::close(id);
                }
            }
        }
        Task::none()
    }
    pub fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }
    pub fn theme(&self) -> Theme {
        crate::ui::theme()
    }
    pub fn audio_spectrum(&self) -> [f32; doslet_pcm::spectrum::BANDS] {
        self.spectrum
    }
    pub fn pointer_dimensions(&self) -> (u16, u16) {
        self.state.pointer_dimensions
    }
    pub fn has_mouse_capture(&self) -> bool {
        self.captured
    }
    pub fn hide_host_cursor(&self) -> bool {
        self.hide_cursor
    }
    pub fn audio_label(&self) -> String {
        self.state.audio_label.clone()
    }
    pub fn storage_label(&self) -> String {
        self.state.storage_label.clone()
    }
    pub fn midi_mode_label(&self) -> String {
        self.midi_label.clone()
    }
    pub fn guest_audio_description(&self) -> String {
        self.guest_audio.clone()
    }
    pub fn subscription(&self) -> Subscription<Message> {
        // While a native full screen is about to end, no new frames reach the
        // UI; the emulation thread keeps running and catches up afterwards.
        let frames = if self.display_sizing.frames_frozen() {
            Subscription::none()
        } else {
            Subscription::run_with(self.worker.signal.clone(), wakeup::Signal::stream)
                .map(|()| Message::GamepadsReady)
        };
        Subscription::batch([
            frames,
            iced::window::close_requests().map(Message::CloseRequested),
            DisplaySizing::subscription().map(Message::DisplaySizing),
            iced::event::listen_with(|event, status, _| {
                if matches!(event, iced::Event::Window(iced::window::Event::Unfocused)) {
                    return Some(Message::FocusLost);
                }
                if let iced::Event::Keyboard(event) = event {
                    Some(if status == iced::event::Status::Ignored {
                        Message::Keyboard(event)
                    } else {
                        Message::KeyboardCaptured(event)
                    })
                } else {
                    None
                }
            }),
        ])
    }
}
