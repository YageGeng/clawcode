//! A workspace file's read-only preview with line numbers and syntax highlighting.

use std::path::Path;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::label::Label;
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Styled as _, TestSupportExt as _, Window, div,
    prelude::FluentBuilder as _, relative,
};
use protocol::WorkspaceFileContent;
use typed_builder::TypedBuilder;

use super::{ViewerSource, ViewerState};

/// One file's preview tab.
#[derive(TypedBuilder)]
pub struct FileView {
    /// The file and its loading state.
    source: ViewerSource,
    /// The read-only editor, once the file loaded as text.
    #[builder(default)]
    editor: Option<Entity<EditorState>>,
}

impl FileView {
    /// Creates the preview and starts loading the file.
    pub fn new(
        source: ViewerSource,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let mut view = Self::builder().source(source).build();
        view.reload(window, cx);
        view
    }

    /// The editor holding the file's text; `None` until it loaded as text.
    pub fn editor(&self) -> Option<&Entity<EditorState>> {
        self.editor.as_ref()
    }

    /// Reads the file again and shows the result.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some((client, workspace, path)) =
            self.source.begin(self.editor.is_some())
        else {
            return;
        };
        self.source._load = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.read_file(&workspace, &path).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.show(result, window, cx);
                cx.notify();
            });
        }));
    }

    /// Shows a read result: the text in the editor, or why there is none.
    fn show(
        &mut self,
        result: Result<WorkspaceFileContent, crate::backend::BackendError>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.source.state = match result {
            Ok(WorkspaceFileContent::Text { text }) => {
                match &self.editor {
                    // Setting a value resets the cursor and scroll, so an unchanged file keeps its place.
                    Some(editor) => editor.update(cx, |editor, cx| {
                        if editor.value() != text {
                            editor.set_value(text, window, cx)
                        }
                    }),
                    None => {
                        let language = self.language();
                        self.editor = Some(cx.new(|cx| {
                            EditorState::new(window, cx)
                                .language(language)
                                .line_number(true)
                                .default_value(text)
                        }));
                    }
                }
                ViewerState::Ready
            }
            Ok(WorkspaceFileContent::Binary) => {
                self.editor = None;
                ViewerState::Notice(rust_i18n::t!("Viewer.binary").into_owned())
            }
            Ok(WorkspaceFileContent::TooLarge { size }) => {
                self.editor = None;
                // Sizes over the 1 MiB limit read best in MiB with one decimal.
                let mebibytes = format!("{:.1}", size as f64 / 1_048_576.0);
                ViewerState::Notice(
                    rust_i18n::t!("Viewer.too_large", size = mebibytes)
                        .into_owned(),
                )
            }
            Err(error) => self.source.failed("preview", error),
        };
    }

    /// The highlighter language: the file's extension, which gpui-kit maps
    /// to a language and treats as plain text when it does not know it.
    fn language(&self) -> String {
        Path::new(&self.source.path)
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "text".to_owned())
    }
}

impl Render for FileView {
    /// The path above the read-only editor, or the state that replaces it.
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let placeholder =
            self.source.state.render_placeholder(Self::reload, cx);
        let code_weight = crate::theme::FontWeights::get(cx).code;
        div()
            .id("file-view")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Label::new(self.source.path.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    ),
            )
            .when_some(placeholder, |this, placeholder| this.child(placeholder))
            .when_some(
                self.editor
                    .clone()
                    .filter(|_| self.source.state == ViewerState::Ready),
                |this, editor| {
                    this.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            // Code text takes the code font weight (M3b spec §7.2).
                            .font_weight(code_weight)
                            // The preview fills the card without an input's frame.
                            .child(
                                Editor::new(&editor)
                                    .readonly(true)
                                    .appearance(false)
                                    .h(relative(1.)),
                            ),
                    )
                },
            )
    }
}
