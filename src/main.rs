use std::{fs, path::PathBuf, time::Duration};

use anyhow::{Context as AnyhowContext, Result};
use clap::Parser;
use gpui_kit::component::{
    ActiveTheme, Root,
    button::{Button, ButtonVariants},
    dialog::Dialog,
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    v_flex,
};
use gpui_kit::{base::StyledExt as _, prelude::FluentBuilder as _, *};
actions!(jian, [CloseWindow]);

use jian::{
    backlink::{Backlink, find_backlinks},
    document::{FileStamp, file_stamp, save_source},
    editor::{SourceEditorEvent, SourceMarkdownEditor},
    export::{
        DEFAULT_CSS, ExportMode, PaperSize,
        pdf::{PdfExportSession, start as start_pdf_export},
        render_html_with_paper,
    },
    workspace::Workspace,
};

#[derive(Clone)]
struct ExportCss {
    name: String,
    source: String,
}

struct DocumentView {
    path: PathBuf,
    editor: Entity<SourceMarkdownEditor>,
    scroll: ScrollHandle,
    saved_source: String,
    disk_stamp: FileStamp,
    _editor_subscription: Subscription,
}

enum Screen {
    Welcome,
    Document(DocumentView),
}

struct Jian {
    screen: Screen,
    workspace: Option<Workspace>,
    dirty: bool,
    save_failed: bool,
    status: String,
    save_generation: u64,
    backlinks: Vec<Backlink>,
    show_backlinks: bool,
    show_workspace_sidebar: bool,
    file_filter: Entity<InputState>,
    _filter_subscription: Subscription,
    export_webview: Option<lb_wry::WebView>,
    export_generation: u64,
    show_export_panel: bool,
    export_css: Option<ExportCss>,
    paper_size: PaperSize,
    export_mode: ExportMode,
    _quit_subscription: Option<Subscription>,
}

impl Jian {
    fn new(window: &mut Window, cx: &mut Context<Self>, initial_path: Option<PathBuf>) -> Self {
        let file_filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let filter_subscription = cx.subscribe(&file_filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut this = Self {
            screen: Screen::Welcome,
            workspace: None,
            dirty: false,
            save_failed: false,
            status: "打开文件或文件夹开始".into(),
            save_generation: 0,
            backlinks: Vec::new(),
            show_backlinks: false,
            show_workspace_sidebar: true,
            file_filter,
            _filter_subscription: filter_subscription,
            export_webview: None,
            export_generation: 0,
            show_export_panel: false,
            export_css: None,
            paper_size: PaperSize::A4,
            export_mode: ExportMode::Paginated,
            _quit_subscription: None,
        };
        if let Some(path) = initial_path
            && let Err(error) = this.open_file(path, window, cx)
        {
            this.status = format!("Open failed: {error:#}");
        }
        this
    }

    fn prompt_open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Markdown file".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    if let Err(error) = this.open_file(path, window, cx) {
                        this.status = format!("Open failed: {error:#}");
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn prompt_open_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open folder as workspace".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, window, cx| {
                    if let Err(error) = this.open_folder(path, window, cx) {
                        this.status = format!("Open folder failed: {error:#}");
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn open_file(
        &mut self,
        path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if !self.save_now(cx) {
            anyhow::bail!("current document has unsaved changes that could not be saved");
        }
        let path = fs::canonicalize(&path)
            .with_context(|| format!("failed to resolve {}", path.display()))?;
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let disk_stamp = file_stamp(&path)?;
        let (workspace, status) =
            match Workspace::discover(path.parent().unwrap_or_else(|| std::path::Path::new("."))) {
                Ok(workspace) => (workspace, "Opened".to_owned()),
                Err(error) => (None, format!("Opened without workspace: {error:#}")),
            };
        let source_editor = SourceMarkdownEditor::new(source.clone())?;
        let editor = cx.new(|_| source_editor);
        let subscription = cx.subscribe(&editor, |this, _, event: &SourceEditorEvent, cx| {
            if matches!(event, SourceEditorEvent::Changed) {
                this.dirty = true;
                this.save_failed = false;
                this.status = "有未保存更改".into();
                self_schedule_autosave(this, cx);
            }
        });

        self.save_generation += 1;
        self.screen = Screen::Document(DocumentView {
            path,
            editor,
            scroll: ScrollHandle::new(),
            saved_source: source,
            disk_stamp,
            _editor_subscription: subscription,
        });
        self.workspace = workspace;
        self.refresh_backlinks();
        self.dirty = false;
        self.save_failed = false;
        self.status = status.replace("Opened", "已保存");
        cx.notify();
        Ok(())
    }

    fn open_folder(
        &mut self,
        path: PathBuf,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if !self.save_now(cx) {
            anyhow::bail!("current document has unsaved changes that could not be saved");
        }
        let path = fs::canonicalize(&path)
            .with_context(|| format!("failed to resolve {}", path.display()))?;
        let workspace = Workspace::open_folder(&path)?;
        let status = if workspace.root() == path {
            "工作区已打开".to_owned()
        } else {
            format!(
                "Opened ancestor workspace at {}",
                workspace.root().display()
            )
        };
        self.workspace = Some(workspace);
        self.screen = Screen::Welcome;
        self.backlinks.clear();
        self.dirty = false;
        self.save_failed = false;
        self.status = status;
        self.save_generation += 1;
        cx.notify();
        Ok(())
    }

    fn save_now(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.dirty {
            return true;
        }
        let Screen::Document(document) = &mut self.screen else {
            return true;
        };
        let current = document.editor.read(cx).source().to_owned();
        match save_source(
            &document.path,
            &document.saved_source,
            &current,
            document.disk_stamp,
        ) {
            Ok(stamp) => {
                document.saved_source = current;
                document.disk_stamp = stamp;
                self.dirty = false;
                self.save_failed = false;
                self.status = "已保存".into();
                cx.notify();
                true
            }
            Err(error) => {
                self.save_failed = true;
                self.status = format!("Autosave stopped: {error:#}");
                cx.notify();
                false
            }
        }
    }

    fn toggle_export_panel(&mut self, cx: &mut Context<Self>) {
        self.show_export_panel = !self.show_export_panel;
        cx.notify();
    }

    fn export_input(&self, cx: &App) -> Result<(String, String, PathBuf)> {
        let Screen::Document(document) = &self.screen else {
            anyhow::bail!("no open document");
        };
        let title = document
            .path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Document")
            .to_owned();
        let base_directory = document
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let css = self
            .export_css
            .as_ref()
            .map(|css| css.source.as_str())
            .unwrap_or(DEFAULT_CSS);
        let html = render_html_with_paper(
            document.editor.read(cx).source(),
            &title,
            &base_directory,
            css,
            self.paper_size,
        )?;
        Ok((title, html, base_directory))
    }

    fn prompt_css(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose CSS stylesheet".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update_in(cx, |this, _, cx| {
                    match fs::read_to_string(&path) {
                        Ok(source) => {
                            let name = path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("Custom CSS")
                                .to_owned();
                            this.export_css = Some(ExportCss { name, source });
                            this.status = "CSS stylesheet selected".into();
                        }
                        Err(error) => {
                            this.status = format!("Failed to read CSS: {error}");
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn prompt_export_html(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_export_panel = false;
        if !self.save_now(cx) {
            return;
        }
        let (title, html, base_directory) = match self.export_input(cx) {
            Ok(input) => input,
            Err(error) => {
                self.status = format!("HTML export failed: {error:#}");
                cx.notify();
                return;
            }
        };
        let receiver = cx.prompt_for_new_path(&base_directory, Some(&format!("{title}.html")));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(output))) = receiver.await {
                this.update(cx, |this, cx| {
                    this.status = match fs::write(&output, html) {
                        Ok(()) => format!("HTML exported to {}", output.display()),
                        Err(error) => format!("HTML export failed: {error}"),
                    };
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn prompt_export_pdf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_export_panel = false;
        if self.export_webview.is_some() {
            self.status = "PDF export is already running".into();
            cx.notify();
            return;
        }
        if !self.save_now(cx) {
            return;
        }
        let (title, html, base_directory) = match self.export_input(cx) {
            Ok(input) => input,
            Err(error) => {
                self.status = format!("PDF export failed: {error:#}");
                cx.notify();
                return;
            }
        };
        let receiver = cx.prompt_for_new_path(&base_directory, Some(&format!("{title}.pdf")));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(output))) = receiver.await {
                this.update_in(cx, |this, window, cx| {
                    match start_pdf_export(html, output, window, this.paper_size, this.export_mode)
                    {
                        Ok(PdfExportSession {
                            webview,
                            completion,
                            ..
                        }) => {
                            this.export_generation += 1;
                            let generation = this.export_generation;
                            this.export_webview = Some(webview);
                            this.status = "Rendering PDF…".into();
                            cx.spawn_in(window, async move |this, cx| {
                                let result = cx
                                    .background_executor()
                                    .spawn(async move { completion.recv() })
                                    .await;
                                this.update_in(cx, |this, _, cx| {
                                    if generation == this.export_generation {
                                        this.export_webview = None;
                                        this.status = match result {
                                            Ok(Ok(path)) => {
                                                format!("PDF exported to {}", path.display())
                                            }
                                            Ok(Err(error)) => format!("PDF export failed: {error}"),
                                            Err(error) => {
                                                format!("PDF export channel failed: {error}")
                                            }
                                        };
                                        cx.notify();
                                    }
                                })
                                .ok();
                            })
                            .detach();
                            cx.spawn_in(window, async move |this, cx| {
                                cx.background_executor()
                                    .timer(Duration::from_secs(30))
                                    .await;
                                this.update_in(cx, |this, _, cx| {
                                    if generation == this.export_generation
                                        && this.export_webview.is_some()
                                    {
                                        this.export_webview = None;
                                        this.status = "PDF export timed out".into();
                                        cx.notify();
                                    }
                                })
                                .ok();
                            })
                            .detach();
                        }
                        Err(error) => {
                            this.status = format!("PDF export failed: {error:#}");
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn refresh_backlinks(&mut self) {
        let Some(workspace) = &self.workspace else {
            self.backlinks.clear();
            return;
        };
        let Screen::Document(document) = &self.screen else {
            self.backlinks.clear();
            return;
        };
        match find_backlinks(workspace, &document.path) {
            Ok(backlinks) => self.backlinks = backlinks,
            Err(error) => {
                log::warn!("failed to compute backlinks: {error:#}");
                self.backlinks.clear();
            }
        }
    }

    fn toggle_backlinks(&mut self, cx: &mut Context<Self>) {
        self.show_backlinks = !self.show_backlinks;
        if self.show_backlinks {
            self.refresh_backlinks();
        }
        cx.notify();
    }

    fn close_after_flush(&mut self, cx: &mut Context<Self>) -> bool {
        self.save_now(cx)
    }

    fn render_welcome(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(div().text_2xl().font_bold().child("Jian"))
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Open a Markdown file or a folder to begin"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("open-file")
                            .label("Open File…")
                            .primary()
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.prompt_open_file(window, cx)
                                }),
                            ),
                    )
                    .child(Button::new("open-folder").label("Open Folder…").on_click(
                        cx.listener(|this, _, window, cx| this.prompt_open_folder(window, cx)),
                    )),
            )
    }

    fn render_export_dialog(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let css_name = self
            .export_css
            .as_ref()
            .map(|css| css.name.clone())
            .unwrap_or_else(|| "Choose CSS…".into());
        let close_view = cx.weak_entity();
        Dialog::new(cx)
            .title("Export")
            .width(px(480.))
            .child(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().child("Stylesheet"))
                            .child(Button::new("default-css").label("Default CSS").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.export_css = None;
                                    this.status = "Using default CSS".into();
                                    cx.notify();
                                }),
                            ))
                            .child(Button::new("choose-css").label(css_name).on_click(
                                cx.listener(|this, _, window, cx| this.prompt_css(window, cx)),
                            )),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .child(div().flex_1().child("Paper size"))
                            .child(
                                Button::new("export-mode")
                                    .label(self.export_mode.to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.export_mode = match this.export_mode {
                                            ExportMode::Paginated => ExportMode::Continuous,
                                            ExportMode::Continuous => ExportMode::Paginated,
                                        };
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("paper-size")
                                    .label(self.paper_size.to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.paper_size = match this.paper_size {
                                            PaperSize::A4 => PaperSize::Letter,
                                            PaperSize::Letter => PaperSize::A4,
                                        };
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("HTML and PDF use the same stylesheet and paper size."),
                    ),
            )
            .footer(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-export")
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_export_panel = false;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("export-html").label("HTML").on_click(
                        cx.listener(|this, _, window, cx| this.prompt_export_html(window, cx)),
                    ))
                    .child(Button::new("export-pdf").label("PDF").primary().on_click(
                        cx.listener(|this, _, window, cx| this.prompt_export_pdf(window, cx)),
                    )),
            )
            .on_close(move |_, _, cx| {
                close_view
                    .update(cx, |this, cx| {
                        this.show_export_panel = false;
                        cx.notify();
                    })
                    .ok();
            })
            .into_any_element()
    }

    fn render_document(
        &mut self,
        editor: Entity<SourceMarkdownEditor>,
        scroll: ScrollHandle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source_mode = editor.read(cx).source_mode();
        let source_editor = editor.clone();
        let document = v_flex()
            .size_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex().flex_1().child(
                            Button::new("toggle-files")
                                .label(if self.show_workspace_sidebar {
                                    "Hide Files"
                                } else {
                                    "Show Files"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_workspace_sidebar = !this.show_workspace_sidebar;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Button::new("source-mode")
                            .label(if source_mode { "Rendered" } else { "Source" })
                            .on_click(cx.listener(move |_, _, window, cx| {
                                source_editor
                                    .update(cx, |editor, cx| editor.toggle_source_mode(window, cx));
                            })),
                    )
                    .child(
                        Button::new("backlinks")
                            .label("Backlinks")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_backlinks(cx))),
                    )
                    .child(
                        Button::new("export")
                            .label("Export")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_export_panel(cx))),
                    ),
            )
            .child(
                v_flex().flex_1().min_h_0().overflow_hidden().child(
                    v_flex()
                        .id("document-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&scroll)
                        .vertical_scrollbar(&scroll)
                        .child(
                            div()
                                .w_full()
                                .max_w(px(820.))
                                .flex_shrink_0()
                                .mx_auto()
                                .p_6()
                                .child(editor),
                        ),
                ),
            )
            .into_any_element();
        h_flex()
            .size_full()
            .items_start()
            .overflow_hidden()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(document),
            )
            .when(self.show_backlinks, |this| {
                this.child(self.render_backlinks_panel(cx))
            })
            .into_any_element()
    }

    fn render_backlinks_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex().w_full().gap_2();
        for (index, backlink) in self.backlinks.clone().into_iter().enumerate() {
            let Some(workspace) = &self.workspace else {
                continue;
            };
            let path = workspace.root().join(&backlink.source);
            list = list.child(
                v_flex()
                    .id(("backlink", index))
                    .gap_1()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().accent))
                    .child(backlink.source.display().to_string())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} · {}", backlink.line, backlink.snippet)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if let Err(error) = this.open_file(path.clone(), window, cx) {
                                this.status = format!("Open failed: {error:#}");
                                cx.notify();
                            }
                        }),
                    ),
            );
        }
        if self.backlinks.is_empty() {
            list = list.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("No backlinks"),
            );
        }
        v_flex()
            .w(px(260.))
            .h_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .p_3()
            .gap_3()
            .child(div().font_bold().child("Backlinks"))
            .child(list)
            .into_any_element()
    }

    fn render_sidebar(&mut self, workspace: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let query = self.file_filter.read(cx).value().to_string().to_lowercase();
        let mut files = v_flex().w_full().gap_1();
        for (index, relative) in workspace.markdown_files().iter().enumerate() {
            if !query.is_empty()
                && !relative
                    .display()
                    .to_string()
                    .to_lowercase()
                    .contains(&query)
            {
                continue;
            }
            let path = workspace.root().join(relative);
            files = files.child(
                div()
                    .id(index)
                    .px_2()
                    .py_1()
                    .rounded(cx.theme().radius)
                    .cursor_pointer()
                    .hover(|this| this.bg(cx.theme().accent))
                    .child(relative.display().to_string())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if let Err(error) = this.open_file(path.clone(), window, cx) {
                                this.status = format!("Open failed: {error:#}");
                                cx.notify();
                            }
                        }),
                    ),
            );
        }

        v_flex()
            .w(px(230.))
            .h_full()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .p_2()
            .child(div().p_2().font_bold().child(workspace.name()))
            .child(Input::new(&self.file_filter).w_full())
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Markdown files"),
            )
            .child(files)
            .into_any_element()
    }
}

fn self_schedule_autosave(this: &mut Jian, cx: &mut Context<Jian>) {
    this.save_generation += 1;
    let generation = this.save_generation;
    cx.spawn(async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(800))
            .await;
        this.update(cx, |this, cx| {
            if generation == this.save_generation {
                this.save_now(cx);
            }
        })
        .ok();
    })
    .detach();
}

impl Render for Jian {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match &self.screen {
            Screen::Welcome => "Jian".to_owned(),
            Screen::Document(document) => {
                let filename = document
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Untitled.md");
                if self.dirty {
                    format!("{filename} •")
                } else {
                    filename.to_owned()
                }
            }
        };
        window.set_window_title(&title);
        let content = match &self.screen {
            Screen::Welcome => self.render_welcome(cx).into_any_element(),
            Screen::Document(document) => {
                self.render_document(document.editor.clone(), document.scroll.clone(), cx)
            }
        };
        let workspace = self
            .workspace
            .clone()
            .filter(|_| self.show_workspace_sidebar);
        h_flex()
            .size_full()
            .items_start()
            .overflow_hidden()
            .bg(cx.theme().background)
            .when_some(workspace, |this, workspace| {
                this.child(self.render_sidebar(&workspace, cx))
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .w_full()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(content),
                    )
                    .child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .text_xs()
                            .text_color(if self.save_failed {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(self.status.clone()),
                    ),
            )
            .when(self.show_export_panel, |this| {
                this.child(self.render_export_dialog(cx))
            })
    }
}

#[derive(Parser)]
#[command(name = "jian", version, about = "A native macOS Markdown editor")]
struct Cli {
    /// Markdown file to open
    #[arg(value_name = "FILE")]
    path: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let initial_path = cli.path;
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        cx.bind_keys([KeyBinding::new("cmd-w", CloseWindow, None)]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Jian".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::centered(size(px(1000.), px(720.)), cx)),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Jian::new(window, cx, initial_path.clone()));
                let close_view = view.downgrade();
                cx.on_action(move |_: &CloseWindow, cx| {
                    close_view
                        .update(cx, |this, cx| {
                            if this.close_after_flush(cx) {
                                cx.quit();
                            }
                        })
                        .ok();
                });
                let root = cx.new(|cx| Root::new(view.clone(), window, cx));
                let quit_view = view.downgrade();
                let quit_subscription = cx.on_app_quit(move |cx| {
                    quit_view
                        .update(cx, |this, cx| {
                            this.save_now(cx);
                        })
                        .ok();
                    async {}
                });
                view.update(cx, |this, cx| {
                    this._quit_subscription = Some(quit_subscription);
                    cx.notify();
                });
                let weak_view = view.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    weak_view
                        .update(cx, |this, cx| this.close_after_flush(cx))
                        .unwrap_or(true)
                });
                root
            },
        )
        .expect("failed to open window");
    });
    Ok(())
}
