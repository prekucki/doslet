use super::*;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use std::time::Duration;

#[derive(Clone)]
pub struct ComputerTools {
    send: mpsc::SyncSender<Request>,
    tool_router: ToolRouter<Self>,
}
impl ComputerTools {
    pub fn new(send: mpsc::SyncSender<Request>) -> Self {
        Self {
            send,
            tool_router: Self::tool_router(),
        }
    }
    async fn perform(&self, action: Action) -> Result<CallToolResult, ErrorData> {
        let (reply, received) = oneshot::channel();
        self.send
            .try_send(Request { action, reply })
            .map_err(|e| ErrorData::internal_error(format!("Desktop control queue: {e}"), None))?;
        let result = match tokio::time::timeout(Duration::from_secs(30), received).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Desktop control disabled or the application closed".into()),
            Err(_) => Err("Desktop did not complete the action within 30 seconds".into()),
        };
        match result {
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(error)])),
            Ok(Response::Status(status)) => Ok(CallToolResult::structured(
                serde_json::to_value(status).unwrap(),
            )),
            Ok(Response::Screen(screen)) => {
                let mut png = Vec::new();
                {
                    let mut encoder =
                        png::Encoder::new(&mut png, screen.status.width, screen.status.height);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder.set_compression(png::Compression::Fast);
                    let mut writer = encoder
                        .write_header()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    writer
                        .write_image_data(&screen.rgba)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                }
                use base64::Engine;
                let metadata = serde_json::to_string(&screen.status).unwrap();
                Ok(CallToolResult::success(vec![
                    ContentBlock::text(metadata),
                    ContentBlock::image(
                        base64::engine::general_purpose::STANDARD.encode(png),
                        "image/png",
                    ),
                ]))
            }
        }
    }
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Position {
    x: u32,
    y: u32,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CpuClock {
    mhz: u16,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Click {
    x: u32,
    y: u32,
    button: Option<String>,
    hold_ms: Option<u16>,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Key {
    key: String,
    #[serde(default)]
    modifiers: Vec<String>,
    hold_ms: Option<u16>,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Text {
    text: String,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Wait {
    milliseconds: u16,
}
fn invalid(error: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(error.into(), None)
}
fn duration(value: Option<u16>) -> Result<u16, ErrorData> {
    let value = value.unwrap_or(80);
    if !(10..=1000).contains(&value) {
        return Err(invalid("hold_ms must be 10..1000"));
    }
    Ok(value)
}
#[tool_router]
impl ComputerTools {
    #[tool(
        description = "Change guest CPU MHz (1..1000) live without resetting the game or device clocks. Returns status; refused while GDB is connected.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn set_cpu_mhz(
        &self,
        Parameters(p): Parameters<CpuClock>,
    ) -> Result<CallToolResult, ErrorData> {
        if !(1..=1000).contains(&p.mhz) {
            return Err(invalid("mhz must be 1..1000"));
        }
        self.perform(Action::CpuMhz { mhz: p.mhz }).await
    }
    #[tool(
        description = "Read the current guest framebuffer as a native-resolution PNG and state metadata. Coordinates of mouse tools refer to this image; no host desktop is captured.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn screenshot(&self) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Screenshot).await
    }
    #[tool(
        description = "Read execution status, guest time, steps, PC and screen dimensions without capturing pixels.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn status(&self) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Status).await
    }
    #[tool(
        description = "Move the guest mouse to native screenshot pixel coordinates and return a screenshot.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn move_mouse(
        &self,
        Parameters(p): Parameters<Position>,
    ) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Move { x: p.x, y: p.y }).await
    }
    #[tool(
        description = "Click at native screenshot coordinates. button is left/right/middle (default left). Hold for 10..1000 guest milliseconds, then release, settle and return a screenshot. Requires a running guest.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn click(&self, Parameters(p): Parameters<Click>) -> Result<CallToolResult, ErrorData> {
        let button = match p.button.as_deref().unwrap_or("left") {
            "left" => 0,
            "right" => 1,
            "middle" => 2,
            _ => return Err(invalid("button must be left, right or middle")),
        };
        self.perform(Action::Click {
            x: p.x,
            y: p.y,
            button,
            hold_ms: duration(p.hold_ms)?,
        })
        .await
    }
    #[tool(
        description = "Press and release a guest key: ASCII character, Enter, Escape, Space, Tab, Backspace, arrows, Home/End, PageUp/PageDown, Insert/Delete or F1..F10. Optional modifiers: ctrl, alt, shift. Hold for 10..1000 guest ms, then return a screenshot. Requires a running guest.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn key_press(&self, Parameters(p): Parameters<Key>) -> Result<CallToolResult, ErrorData> {
        let stroke = key_stroke(&p.key, &p.modifiers).map_err(invalid)?;
        self.perform(Action::Keys {
            strokes: vec![stroke],
            hold_ms: duration(p.hold_ms)?,
        })
        .await
    }
    #[tool(
        description = "Type 1..64 ASCII characters using the guest keyboard, including newline and tab. US keyboard layout. Return a screenshot after all keys are released. Requires a running guest.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn type_text(
        &self,
        Parameters(p): Parameters<Text>,
    ) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Keys {
            strokes: text_strokes(&p.text).map_err(invalid)?,
            hold_ms: 30,
        })
        .await
    }
    #[tool(
        description = "Wait for 1..5000 milliseconds of guest time and return a screenshot. This does not change CPU speed or step through a stopped guest.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn wait(&self, Parameters(p): Parameters<Wait>) -> Result<CallToolResult, ErrorData> {
        if !(1..=5000).contains(&p.milliseconds) {
            return Err(invalid("milliseconds must be 1..5000"));
        }
        self.perform(Action::Wait {
            milliseconds: p.milliseconds,
        })
        .await
    }
    #[tool(
        description = "Pause the guest using the desktop controls. Refused while GDB controls execution.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn pause(&self) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Pause).await
    }
    #[tool(
        description = "Resume a ready or paused guest. Refused while GDB controls execution; it does not reset a halted, faulted or exited session.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn resume(&self) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Resume).await
    }
}
#[tool_handler(router = self.tool_router)]
impl ServerHandler for ComputerTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Control only the running Doslet guest, not the host desktop. Read screenshot/status first; use native screenshot pixel coordinates. Input gestures are finite and advance with guest time. Mutations can affect game state and saves under its configured drive policy. GDB owns execution when connected: screenshots/status remain available, but resume through GDB. Ask the user before changing their goal or starting unrelated gameplay."
        )
    }
}
