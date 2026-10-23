struct Logger;
static LOGGER: Logger = Logger;
impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let message = format!("{}: {}", record.target(), record.args());
        match record.level() {
            log::Level::Error => web_sys::console::error_1(&message.into()),
            log::Level::Warn => web_sys::console::warn_1(&message.into()),
            _ => web_sys::console::info_1(&message.into()),
        }
    }
    fn flush(&self) {}
}
pub fn init() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    log::info!("Starting Doslet with wgpu (WebGPU/WebGL), with Canvas2D fallback");
}

/// Check the context already owned by iced, rather than testing a separate
/// canvas that could support GPU rendering even if iced fell back to Canvas2D.
pub fn gpu_canvas_active() -> bool {
    use wasm_bindgen::JsCast;
    let Some(canvas) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector("canvas").ok().flatten())
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
    else {
        return false;
    };
    ["webgl2", "webgpu"]
        .iter()
        .any(|kind| canvas.get_context(kind).ok().flatten().is_some())
}

/// A ROTT run with RAVU on Chromium WebGL reached a wgpu-core buffer-alignment
/// panic. Select the configured simple filter on that renderer; Firefox WebGL
/// and Chromium WebGPU are left eligible for RAVU.
pub fn chromium_webgl_ravu_unstable() -> bool {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return false;
    };
    let chrome = window
        .navigator()
        .user_agent()
        .is_ok_and(|agent| agent.contains("Chrome/") || agent.contains("Chromium/"));
    if !chrome {
        return false;
    }
    window
        .document()
        .and_then(|document| document.query_selector("canvas").ok().flatten())
        .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .is_some_and(|canvas| canvas.get_context("webgl2").ok().flatten().is_some())
}
