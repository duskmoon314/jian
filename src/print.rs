use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use anyhow::{Context, Result};
use lb_wry::{BackgroundThrottlingPolicy, PageLoadEvent, Rect, WebView, WebViewBuilder, dpi};

/// Keeps the WebView alive while its native print dialog is in use.
pub struct PrintSession {
    _webview: Rc<WebView>,
}

/// Load HTML and open the platform print dialog after rendering finishes.
pub fn open_print_dialog(html: &str, window: &gpui_kit::Window) -> Result<PrintSession> {
    let webview_slot = Rc::new(RefCell::new(None::<Weak<WebView>>));
    let slot_on_load = webview_slot.clone();
    let webview = Rc::new(
        WebViewBuilder::new()
            .with_background_throttling(BackgroundThrottlingPolicy::Disabled)
            .with_on_page_load_handler(move |event, _| {
                if !matches!(event, PageLoadEvent::Finished) {
                    return;
                }
                let Some(webview) = slot_on_load.borrow().as_ref().and_then(Weak::upgrade) else {
                    return;
                };
                if let Err(error) = webview.print() {
                    log::error!("failed to open print dialog: {error}");
                }
            })
            .build_as_child(window)
            .context("failed to create print WebView")?,
    );
    webview
        .set_bounds(Rect {
            position: dpi::LogicalPosition::new(-10_000.0, 0.0).into(),
            size: dpi::LogicalSize::new(794.0, 1123.0).into(),
        })
        .context("failed to size print WebView")?;
    *webview_slot.borrow_mut() = Some(Rc::downgrade(&webview));
    webview
        .load_html(html)
        .context("failed to load print HTML")?;
    Ok(PrintSession { _webview: webview })
}
