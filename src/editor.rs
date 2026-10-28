use crate::source::SourceDocument;
use anyhow::Result;
use gpui_kit::base::text::TextView;
use gpui_kit::component::{
    ActiveTheme,
    input::{InputEvent, Textarea, TextareaState},
    v_flex,
};
use gpui_kit::*;

#[derive(Clone, Copy, Debug)]
pub enum SourceEditorEvent {
    Changed,
}

/// Minimal source-first Markdown editor: rendered blocks are inactive, while
/// the clicked block is edited as its original Markdown source.
pub struct SourceMarkdownEditor {
    document: SourceDocument,
    active_index: Option<usize>,
    active_state: Option<Entity<TextareaState>>,
    active_subscription: Option<Subscription>,
    source_mode: bool,
    source_state: Option<Entity<TextareaState>>,
    source_subscription: Option<Subscription>,
    generation: u64,
}

impl SourceMarkdownEditor {
    pub fn new(source: String) -> Result<Self> {
        Ok(Self {
            document: SourceDocument::new(source)?,
            active_index: None,
            active_state: None,
            active_subscription: None,
            source_mode: false,
            source_state: None,
            source_subscription: None,
            generation: 0,
        })
    }

    pub fn source(&self) -> &str {
        self.document.text()
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active_index
    }

    pub fn source_mode(&self) -> bool {
        self.source_mode
    }

    pub fn toggle_source_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source_mode {
            self.source_mode = false;
            cx.notify();
            return;
        }

        self.active_index = None;
        self.active_state = None;
        self.active_subscription = None;
        let source = self.document.text().to_owned();
        let state = if let Some(state) = &self.source_state {
            state.update(cx, |state, cx| {
                state.set_value(source, window, cx);
                state.focus(window, cx);
            });
            state.clone()
        } else {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 10_000)
                    .default_value(source)
            });
            let subscription = cx.subscribe(&state, |this, state, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = state.read(cx).value().to_string();
                this.document = SourceDocument::opaque(value);
                cx.emit(SourceEditorEvent::Changed);
                cx.notify();
            });
            self.source_subscription = Some(subscription);
            state.update(cx, |state, cx| state.focus(window, cx));
            state
        };
        self.source_state = Some(state);
        self.source_mode = true;
        cx.notify();
    }

    pub fn active_source(&self, cx: &App) -> Option<String> {
        self.active_state
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
    }

    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_index == Some(index) {
            return;
        }
        if self.document.block_source(index).is_none() {
            return;
        }
        if let Err(error) = self.document.reparse() {
            log::warn!("failed to reparse Markdown before block activation: {error}");
            return;
        }
        let Some(raw) = self.document.block_source(index).map(str::to_owned) else {
            return;
        };

        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 10_000)
                .default_value(raw)
        });
        let subscription = cx.subscribe(&state, move |this, state, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = state.read(cx).value().to_string();
            if let Err(error) = this.document.replace_block(index, &value) {
                log::warn!("failed to update active Markdown block: {error}");
                return;
            }
            cx.emit(SourceEditorEvent::Changed);
            cx.notify();
        });
        self.active_subscription = Some(subscription);
        state.update(cx, |state, cx| state.focus(window, cx));
        self.active_index = Some(index);
        self.active_state = Some(state);
        self.generation += 1;
        cx.notify();
    }
}

impl EventEmitter<SourceEditorEvent> for SourceMarkdownEditor {}

impl Render for SourceMarkdownEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.source_mode
            && let Some(state) = &self.source_state
        {
            return v_flex().w_full().flex_shrink_0().child(
                Textarea::new(state)
                    .appearance(false)
                    .bordered(false)
                    .w_full()
                    .font_family(cx.theme().mono_font_family.clone()),
            );
        }
        let mut blocks = v_flex().w_full().gap_3().flex_shrink_0();
        for index in 0..self.document.block_count() {
            if self.active_index == Some(index) {
                if let Some(state) = &self.active_state {
                    blocks = blocks.child(
                        div().w_full().flex_shrink_0().child(
                            Textarea::new(state)
                                .appearance(false)
                                .bordered(false)
                                .w_full()
                                .font_family(cx.theme().mono_font_family.clone()),
                        ),
                    );
                }
            } else if let Some(source) = self.document.block_source(index).map(str::to_owned) {
                let empty = source.trim().is_empty();
                blocks = blocks.child(
                    div()
                        .id(index)
                        .w_full()
                        .flex_shrink_0()
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                this.activate(index, window, cx)
                            }),
                        )
                        .child(if empty {
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child("Click to start writing")
                                .into_any_element()
                        } else {
                            TextView::markdown(
                                (
                                    ElementId::from(("markdown-block", index)),
                                    self.generation.to_string(),
                                ),
                                source,
                            )
                            .selectable(false)
                            .on_link_click(|url, event, _, cx| {
                                if event.modifiers().platform {
                                    cx.open_url(url);
                                }
                            })
                            .into_any_element()
                        }),
                );
            }
        }
        blocks
    }
}
