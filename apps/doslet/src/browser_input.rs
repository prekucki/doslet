//! A local browser file picker; no upload or host filesystem access.
use iced::futures::channel::oneshot;
use std::{cell::RefCell, future::Future, rc::Rc, sync::Arc};
use wasm_bindgen::{JsCast, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, File, HtmlInputElement};

pub type Selection = Result<Option<Arc<Vec<u8>>>, String>;

pub fn pick_zip() -> Result<impl Future<Output = Selection>, String> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("Browser document unavailable")?;
    let input: HtmlInputElement = document
        .create_element("input")
        .map_err(js_error)?
        .dyn_into()
        .map_err(|error: web_sys::Element| js_error(error.into()))?;
    input.set_type("file");
    input.set_accept(".zip,application/zip");
    input
        .set_attribute("style", "display:none")
        .map_err(js_error)?;
    let (sender, receiver) = oneshot::channel::<Option<File>>();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let selected = input.clone();
    let changed_sender = sender.clone();
    let changed = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(sender) = changed_sender.borrow_mut().take() {
            let _ = sender.send(selected.files().and_then(|files| files.item(0)));
        }
    });
    let cancelled = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(sender) = sender.borrow_mut().take() {
            let _ = sender.send(None);
        }
    });
    input
        .add_event_listener_with_callback("change", changed.as_ref().unchecked_ref())
        .map_err(js_error)?;
    input
        .add_event_listener_with_callback("cancel", cancelled.as_ref().unchecked_ref())
        .map_err(js_error)?;
    document
        .body()
        .ok_or("Browser body unavailable")?
        .append_child(&input)
        .map_err(js_error)?;
    input.click();
    Ok(async move {
        let result = receiver.await;
        let _ =
            input.remove_event_listener_with_callback("change", changed.as_ref().unchecked_ref());
        let _ =
            input.remove_event_listener_with_callback("cancel", cancelled.as_ref().unchecked_ref());
        input.remove();
        let Some(file) = result.map_err(|_| "ZIP picker closed unexpectedly")? else {
            return Ok(None);
        };
        if file.size() > (64 * 1024 * 1024) as f64 {
            return Err("ZIP exceeds the 64-MiB limit".into());
        }
        let buffer = JsFuture::from(file.array_buffer())
            .await
            .map_err(js_error)?;
        Ok(Some(Arc::new(js_sys::Uint8Array::new(&buffer).to_vec())))
    })
}
fn js_error(error: wasm_bindgen::JsValue) -> String {
    format!("Browser file access failed: {error:?}")
}
