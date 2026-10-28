use std::path::Path;

use anyhow::Result;
use markdown::{CompileOptions, Constructs, Options, ParseOptions, to_html_with_options};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaperSize {
    A4,
    Letter,
}

impl PaperSize {
    pub fn points(self) -> (f64, f64) {
        match self {
            Self::A4 => (595.44, 841.68),
            Self::Letter => (612.0, 792.0),
        }
    }

    fn css_size(self) -> &'static str {
        match self {
            Self::A4 => "A4",
            Self::Letter => "letter",
        }
    }
}

impl std::fmt::Display for PaperSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::A4 => "A4",
            Self::Letter => "Letter",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportMode {
    Paginated,
    Continuous,
}

impl std::fmt::Display for ExportMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Paginated => "Paginated",
            Self::Continuous => "Continuous",
        })
    }
}

pub const DEFAULT_CSS: &str = r#"
@page { size: A4; margin: 22mm; }
:root { color-scheme: light; }
body {
  max-width: 760px;
  margin: 40px auto;
  color: #171717;
  font: 16px/1.75 -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif;
}
h1, h2, h3, h4 { line-height: 1.35; font-weight: 600; }
h1 { font-size: 2em; } h2 { font-size: 1.5em; } h3 { font-size: 1.2em; }
a { color: #2563eb; text-decoration: none; }
blockquote { margin-left: 0; padding-left: 1em; border-left: 3px solid #d4d4d4; color: #525252; }
code { font: 0.9em ui-monospace, SFMono-Regular, Menlo, monospace; background: #f5f5f5; }
pre { padding: 14px; overflow-wrap: anywhere; white-space: pre-wrap; background: #f5f5f5; }
pre code { background: transparent; }
table { width: 100%; border-collapse: collapse; }
th, td { padding: 6px 8px; border: 1px solid #d4d4d4; text-align: left; }
img { max-width: 100%; height: auto; }
"#;

pub fn render_html(source: &str, title: &str, base_directory: &Path) -> Result<String> {
    render_html_with_css(source, title, base_directory, DEFAULT_CSS)
}

pub fn render_html_with_css(
    source: &str,
    title: &str,
    base_directory: &Path,
    css: &str,
) -> Result<String> {
    render_html_with_paper(source, title, base_directory, css, PaperSize::A4)
}

pub fn render_html_with_paper(
    source: &str,
    title: &str,
    base_directory: &Path,
    css: &str,
    paper_size: PaperSize,
) -> Result<String> {
    let options = Options {
        parse: ParseOptions {
            constructs: Constructs {
                frontmatter: true,
                ..Constructs::gfm()
            },
            ..ParseOptions::gfm()
        },
        compile: CompileOptions::gfm(),
    };
    let body = to_html_with_options(source, &options)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let base_directory = std::fs::canonicalize(base_directory)
        .map_err(|_| anyhow::anyhow!("invalid export base directory"))?;
    let base = Url::from_directory_path(base_directory)
        .map_err(|_| anyhow::anyhow!("invalid export base directory"))?;
    let paper_css = format!("{css}\n@page {{ size: {}; }}", paper_size.css_size());
    Ok(format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src file: data:; style-src 'unsafe-inline'\">\
         <base href=\"{base}\"><title>{title}</title><style>{paper_css}</style></head>\
         <body>{body}</body></html>",
        base = base,
        title = escape_html(title),
        body = body,
        paper_css = paper_css,
    ))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "macos")]
pub mod pdf {
    use std::{
        cell::RefCell,
        fs,
        path::PathBuf,
        rc::Rc,
        sync::mpsc::{Receiver, Sender, channel},
    };

    use super::{ExportMode, PaperSize};
    use anyhow::{Context, Result};
    use block2::RcBlock;
    use lb_wry::{
        BackgroundThrottlingPolicy, PageLoadEvent, Rect, WebView, WebViewBuilder, WebViewExtMacOS,
        WryWebView, dpi,
    };
    use objc2::DefinedClass;
    use objc2_app_kit::{NSPrintInfo, NSPrintOperation, NSPrintingPaginationMode};
    use objc2_foundation::{
        MainThreadMarker, NSData, NSError, NSObjectProtocol, NSPoint, NSRect, NSRunLoop, NSSize,
        NSString, NSURL,
    };

    pub struct PrintDelegateIvars {
        output: PathBuf,
        sender: Sender<std::result::Result<PathBuf, String>>,
    }

    objc2::define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[name = "JianPrintDelegate"]
        #[thread_kind = objc2::MainThreadOnly]
        #[ivars = PrintDelegateIvars]
        pub struct PrintDelegate;

        unsafe impl NSObjectProtocol for PrintDelegate {}

        impl PrintDelegate {
            #[unsafe(method(printOperationDidRun:success:contextInfo:))]
            fn did_run(
                &self,
                _operation: &NSPrintOperation,
                success: bool,
                _context: *mut std::ffi::c_void,
            ) {
                let result = if success {
                    Ok(self.ivars().output.clone())
                } else {
                    Err("WebKit print operation failed".into())
                };
                let _ = self.ivars().sender.send(result);
            }
        }
    );

    impl PrintDelegate {
        fn new(
            output: PathBuf,
            sender: Sender<std::result::Result<PathBuf, String>>,
            mtm: MainThreadMarker,
        ) -> objc2::rc::Retained<Self> {
            let delegate = mtm
                .alloc::<Self>()
                .set_ivars(PrintDelegateIvars { output, sender });
            unsafe { objc2::msg_send![super(delegate), init] }
        }
    }

    pub struct PdfExportSession {
        pub webview: WebView,
        pub completion: Receiver<std::result::Result<PathBuf, String>>,
        pub delegate: objc2::rc::Retained<PrintDelegate>,
    }

    /// Load HTML in an off-screen WKWebView and return its native PDF data.
    /// The session must be retained until `completion` yields a result.
    pub fn start(
        html: String,
        output: PathBuf,
        window: &gpui_kit::Window,
        paper_size: PaperSize,
        mode: ExportMode,
    ) -> Result<PdfExportSession> {
        let (sender, completion) = channel();
        let delegate = PrintDelegate::new(
            output.clone(),
            sender.clone(),
            MainThreadMarker::new().expect("PDF export must run on the main thread"),
        );
        let delegate_on_load = delegate.clone();
        let sender_on_load = sender.clone();
        let native_slot: Rc<RefCell<Option<objc2::rc::Retained<WryWebView>>>> =
            Rc::new(RefCell::new(None));
        let native_on_load = native_slot.clone();

        let builder = WebViewBuilder::new()
            .with_html(html)
            .with_background_throttling(BackgroundThrottlingPolicy::Disabled)
            .with_on_page_load_handler(move |event, _url| {
                if !matches!(event, PageLoadEvent::Finished) {
                    return;
                }
                let Some(native) = native_on_load.borrow().clone() else {
                    let _ = sender_on_load.send(Err("WKWebView is unavailable".into()));
                    return;
                };
                let output_for_pdf = output.clone();
                let sender_for_pdf = sender_on_load.clone();
                let delegate_for_pdf = delegate_on_load.clone();
                let block = RcBlock::new(move || {
                    write_pdf(
                        &native,
                        output_for_pdf.clone(),
                        sender_for_pdf.clone(),
                        paper_size,
                        mode,
                        &delegate_for_pdf,
                    );
                });
                unsafe {
                    NSRunLoop::currentRunLoop().performBlock(&block);
                }
            });

        let webview = builder
            .build_as_child(window)
            .context("failed to create WKWebView")?;
        webview
            .set_bounds(Rect {
                position: dpi::LogicalPosition::new(
                    if mode == ExportMode::Continuous {
                        -10_000.0
                    } else {
                        0.0
                    },
                    0.0,
                )
                .into(),
                size: dpi::LogicalSize::new(794.0, 1123.0).into(),
            })
            .context("failed to size WKWebView")?;
        *native_slot.borrow_mut() = Some(webview.webview());

        Ok(PdfExportSession {
            webview,
            completion,
            delegate,
        })
    }

    fn write_pdf(
        webview: &WryWebView,
        output: PathBuf,
        sender: Sender<std::result::Result<PathBuf, String>>,
        paper_size: PaperSize,
        mode: ExportMode,
        delegate: &PrintDelegate,
    ) {
        if mode == ExportMode::Continuous {
            let completion = RcBlock::new(move |data: *mut NSData, error: *mut NSError| {
                let result = unsafe {
                    if !error.is_null() {
                        Err((*error).localizedDescription().to_string())
                    } else if data.is_null() {
                        Err("WKWebView returned empty PDF data".into())
                    } else {
                        fs::write(&output, (*data).to_vec())
                            .map(|()| output.clone())
                            .map_err(|error| error.to_string())
                    }
                };
                let _ = sender.send(result);
            });
            unsafe {
                webview.createPDFWithConfiguration_completionHandler(None, &completion);
            }
            return;
        }

        let (width, height) = paper_size.points();
        let print_info = NSPrintInfo::new();
        print_info.setPaperSize(NSSize::new(width, height));
        print_info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
        print_info.setVerticalPagination(NSPrintingPaginationMode::Automatic);
        print_info.setScalingFactor(1.0);
        print_info.setJobDisposition(unsafe { objc2_app_kit::NSPrintSaveJob });
        let output_url = NSURL::fileURLWithPath(&NSString::from_str(&output.to_string_lossy()));
        let dictionary = unsafe { print_info.dictionary() };
        let key = unsafe { objc2_app_kit::NSPrintJobSavingURL };
        unsafe {
            let _: () = objc2::msg_send![&*dictionary, setObject: &*output_url, forKey: key];
        }
        let operation = unsafe { webview.printOperationWithPrintInfo(&print_info) };
        operation.setShowsPrintPanel(false);
        operation.setShowsProgressPanel(false);
        operation.setCanSpawnSeparateThread(true);
        if let Some(view) = operation.view() {
            view.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(width, height),
            ));
        }
        let window = webview
            .window()
            .expect("WKWebView must have a window for printing");
        unsafe {
            operation.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                &window,
                Some(delegate),
                Some(objc2::sel!(printOperationDidRun:success:contextInfo:)),
                std::ptr::null_mut(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_css_replaces_the_default_stylesheet() {
        let directory = tempfile::tempdir().unwrap();
        let html = render_html_with_css(
            "# Title",
            "Test",
            directory.path(),
            "body { font-size: 21px; }",
        )
        .unwrap();
        assert!(html.contains("font-size: 21px"));
        assert!(html.contains("size: A4"));
    }

    #[test]
    fn paper_selection_overrides_stylesheet_size() {
        let directory = tempfile::tempdir().unwrap();
        let html = render_html_with_paper(
            "# Title",
            "Test",
            directory.path(),
            "@page { size: A4; }",
            PaperSize::Letter,
        )
        .unwrap();
        assert!(html.contains("size: letter"));
        assert!(html.rfind("size: letter").unwrap() > html.rfind("size: A4").unwrap());
    }

    #[test]
    fn frontmatter_is_not_rendered_as_document_content() {
        let directory = tempfile::tempdir().unwrap();
        let html = render_html(
            "---\ntitle: Metadata\n---\n\n# Visible",
            "Test",
            directory.path(),
        )
        .unwrap();
        assert!(html.contains("<h1>Visible</h1>"));
        assert!(!html.contains("Metadata"));
        assert!(!html.contains("<hr"));
    }

    #[test]
    fn html_contains_css_base_and_rendered_markdown() {
        let directory = tempfile::tempdir().unwrap();
        let html = render_html("# Title\n\n**bold**", "A <doc>", directory.path()).unwrap();
        let base =
            Url::from_directory_path(std::fs::canonicalize(directory.path()).unwrap()).unwrap();
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("A &lt;doc&gt;"));
        assert!(html.contains("@page"));
        assert!(html.contains(&format!("<base href=\"{base}\">")));
        assert!(!html.contains("<script"));
    }
}
