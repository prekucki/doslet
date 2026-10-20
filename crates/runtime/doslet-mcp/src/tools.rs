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
            Ok(Response::CodeTags(value)) => Ok(CallToolResult::structured(value)),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(error)])),
            Ok(Response::Exiting) => Ok(CallToolResult::structured(
                serde_json::json!({"status": "Exiting"}),
            )),
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
    x: Option<u32>,
    y: Option<u32>,
    dx: Option<i16>,
    dy: Option<i16>,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SynchronizationMode {
    enabled: bool,
    #[serde(default)]
    adaptive: bool,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CpuClock {
    mhz: u16,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CodeRange {
    start: u32,
    end: u32,
    bits: u8,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CodeTag {
    start: u32,
    end: u32,
    bits: u8,
    tier: CodeTier,
}
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Click {
    x: Option<u32>,
    y: Option<u32>,
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
fn position(x: Option<u32>, y: Option<u32>) -> Result<Option<(u32, u32)>, ErrorData> {
    match (x, y) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        (None, None) => Ok(None),
        _ => Err(invalid("Supply both x and y, or neither")),
    }
}
impl Position {
    fn action(self) -> Result<Action, ErrorData> {
        match (position(self.x, self.y)?, self.dx, self.dy) {
            (Some((x, y)), None, None) => Ok(Action::Move { x, y }),
            (None, Some(dx), Some(dy)) => Ok(Action::MoveRelative { dx, dy }),
            _ => Err(invalid("Supply exactly x/y (absolute) or dx/dy (relative)")),
        }
    }
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
        description = "Inspect existing speed-tier labels whose instruction starts are in [start,end), using decimal physical addresses and bits=16 or 32. Maximum span 65536 bytes. Reports current-byte matches, conflicts and confirmed IRQ labels. Does not decode unknown code or change execution.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn inspect_code_tags(
        &self,
        Parameters(p): Parameters<CodeRange>,
    ) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::CodeTags {
            start: p.start,
            end: p.end,
            bits: p.bits,
            tier: None,
        })
        .await
    }
    #[tool(
        description = "Manually tag existing matching instruction labels in [start,end) as hot, cold or grey; decimal physical addresses, bits=16 or 32, maximum 65536 bytes. Inspect first. Requires enabled speed tiers and no GDB/preparation. Skips changed code, conflict guards and confirmed IRQs. Labels are global by PC, including shared callees. Applies live without resetting time/counters, protects edits from automatic learning and saves them to the active profile cache. Does not tag unknown code, patch guest bytes or change MHz.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn tag_code_range(
        &self,
        Parameters(p): Parameters<CodeTag>,
    ) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::CodeTags {
            start: p.start,
            end: p.end,
            bits: p.bits,
            tier: Some(p.tier),
        })
        .await
    }
    #[tool(
        description = "Enable bounded synchronization diagnostics or its experimental adaptive CPU controller. Resets evidence and disables competing heuristic waits. Disabled mode restores the configured MHz ceiling.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn set_synchronization_analysis(
        &self,
        Parameters(p): Parameters<SynchronizationMode>,
    ) -> Result<CallToolResult, ErrorData> {
        if p.adaptive && !p.enabled {
            return Err(invalid("adaptive requires enabled"));
        }
        self.perform(Action::Synchronization {
            enabled: p.enabled,
            adaptive: p.adaptive,
        })
        .await
    }
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
        description = "Close Doslet through normal desktop shutdown, saving the JIT profile and joining its workers. Returns Exiting when accepted; the MCP connection then closes. Works while paused or under GDB. Does not save unsaved game progress.",
        annotations(destructive_hint = true, open_world_hint = false)
    )]
    async fn exit(&self) -> Result<CallToolResult, ErrorData> {
        self.perform(Action::Exit).await
    }
    #[tool(
        description = "Move the guest mouse, wait 250 guest ms and return a fresh screenshot. Supply x/y for absolute native screenshot coordinates OR dx/dy for relative raw mouse motion (positive right/down). Games with their own cursor, such as Heroes, may ignore the driver's absolute position: use small dx/dy moves, inspect the returned cursor, then click with x/y omitted. Relative units are guest motion counts; game sensitivity can differ from pixels.",
        annotations(destructive_hint = false, open_world_hint = false)
    )]
    async fn move_mouse(
        &self,
        Parameters(p): Parameters<Position>,
    ) -> Result<CallToolResult, ErrorData> {
        self.perform(p.action()?).await
    }
    #[tool(
        description = "Click the current guest cursor when x/y are omitted (recommended after relative movement). With both x/y, first move the DOS driver pointer to native screenshot coordinates and wait 250 guest ms before pressing. button is left/right/middle (default left). Hold for 10..1000 guest ms, release, wait 250 guest ms and return a screenshot. For games with an independent cursor, use move_mouse dx/dy and inspect before clicking without coordinates. Requires a running guest.",
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
            position: position(p.x, p.y)?,
            button,
            hold_ms: duration(Some(p.hold_ms.unwrap_or(250)))?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn code_tag_request_reaches_owner_and_returns_counts() {
        let (send, receive) = mpsc::sync_channel(QUEUE);
        let tools = ComputerTools::new(send);
        let request = tools.tag_code_range(Parameters(CodeTag {
            start: 256,
            end: 272,
            bits: 16,
            tier: CodeTier::Hot,
        }));
        let owner = async {
            tokio::task::yield_now().await;
            let request = receive.try_recv().unwrap();
            assert!(matches!(
                request.action,
                Action::CodeTags {
                    start: 256,
                    end: 272,
                    bits: 16,
                    tier: Some(CodeTier::Hot),
                }
            ));
            request
                .reply
                .send(Ok(Response::CodeTags(serde_json::json!({"changed":2}))))
                .unwrap();
        };
        let (result, ()) = tokio::join!(request, owner);
        assert_eq!(result.unwrap().structured_content.unwrap()["changed"], 2);
        assert!(
            serde_json::from_str::<CodeTag>(r#"{"start":0,"end":16,"bits":16,"tier":"irq"}"#)
                .is_err()
        );
    }

    #[test]
    fn mouse_modes_are_exclusive_and_old_coordinates_remain_supported() {
        let parse = |s| serde_json::from_str::<Position>(s).unwrap().action();
        assert!(matches!(
            parse(r#"{"x":12,"y":34}"#),
            Ok(Action::Move { x: 12, y: 34 })
        ));
        assert!(matches!(
            parse(r#"{"dx":-12,"dy":34}"#),
            Ok(Action::MoveRelative { dx: -12, dy: 34 })
        ));
        for input in [
            r#"{}"#,
            r#"{"x":12}"#,
            r#"{"dx":12}"#,
            r#"{"x":1,"y":2,"dx":3,"dy":4}"#,
        ] {
            assert!(parse(input).is_err(), "{input}");
        }
        let click: Click = serde_json::from_str("{}").unwrap();
        assert_eq!(position(click.x, click.y).unwrap(), None);
        assert!(position(Some(1), None).is_err());
    }
}
