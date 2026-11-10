use blitz_traits::{
    events::{BlitzImeEvent, BlitzKeyEvent},
    node_id::NodeId,
    shell::ShellProvider,
};
use keyboard_types::{Key, Modifiers, NamedKey};
use parley::{ContentWidths, FontContext, LayoutContext};

use crate::util::ACTION_MOD;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
/// Parley Brush type for Blitz which contains the Blitz node id
pub struct TextBrush {
    /// The node id for the span
    pub id: NodeId,
}

impl TextBrush {
    pub(crate) fn from_id(id: NodeId) -> Self {
        Self { id }
    }
}

#[derive(Clone, Default)]
pub struct TextLayout {
    pub text: String,
    pub content_widths: Option<ContentWidths>,
    pub layout: parley::layout::Layout<TextBrush>,
    /// Block-axis offset (in CSS px) of the line boxes from the top of the container's content
    /// box, as applied by `align-content`.
    pub block_offset: f32,
    /// The out-of-flow positions for which some inline span with an inline box below it is a
    /// containing block (as `SPAN_*_CB` flags). The inline root claims those boxes in its place.
    /// Set by each `RunMode::PerformLayout` run of inline layout.
    pub span_cb_flags: stylo_taffy::StyleFlags,
    /// Offset (in physical CSS px) of the unrounded content box origin from the rounded one in
    /// `final_layout`. Paint draws text from the unrounded origin so that it keeps its sub-pixel
    /// position while box edges stay snapped. Set by the `writing-mode` layout pass; zero otherwise.
    pub content_box_subpixel_offset: taffy::Point<f32>,
}

impl TextLayout {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn content_widths(&mut self) -> ContentWidths {
        *self
            .content_widths
            .get_or_insert_with(|| self.layout.calculate_content_widths())
    }
}

impl std::fmt::Debug for TextLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TextLayout")
    }
}

// TODO: support keypress events
pub enum GeneratedTextInputEvent {
    Input,
    Select,
    PreEditChange,
    Submit,
}

pub struct TextInputData {
    /// A parley TextEditor instance
    pub editor: Box<parley::PlainEditor<TextBrush>>,
    /// Whether the input is a singleline or multiline input
    pub is_multiline: bool,
    /// The scroll offset of the text content within the input, in CSS (unscaled) pixels.
    ///
    /// For single-line inputs this is a horizontal offset; for multi-line inputs it is a
    /// vertical offset. It is kept up to date so that the caret remains visible within the
    /// input's content box.
    pub scroll_offset: f32,
    /// Offset (in physical CSS px) of the unrounded content box origin from the rounded one in
    /// `final_layout`. Paint draws text from the unrounded origin so that it keeps its sub-pixel
    /// position while box edges stay snapped. Set by the `writing-mode` layout pass; zero otherwise.
    pub content_box_subpixel_offset: taffy::Point<f32>,
    /// A password input's text. Its editor holds one [`MASK`] for each character, so only
    /// those are laid out, painted and copied.
    pub secret: Option<String>,
}

/// What a password input shows for each character of its text.
pub const MASK: char = '\u{2022}';

// FIXME: Implement Clone for PlainEditor
impl Clone for TextInputData {
    fn clone(&self) -> Self {
        TextInputData::new(self.is_multiline)
    }
}

impl TextInputData {
    pub fn new(is_multiline: bool) -> Self {
        let editor = Box::new(parley::PlainEditor::new(16.0));
        Self {
            editor,
            is_multiline,
            scroll_offset: 0.0,
            content_box_subpixel_offset: taffy::Point::ZERO,
            secret: None,
        }
    }

    pub fn set_text(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        text: &str,
    ) {
        if let Some(secret) = &mut self.secret {
            if secret != text {
                text.clone_into(secret);
                self.editor.set_text(&masked(text.chars().count()));
                self.editor.driver(font_ctx, layout_ctx).refresh_layout();
            }
        } else if self.editor.text() != text {
            self.editor.set_text(text);
            self.editor.driver(font_ctx, layout_ctx).refresh_layout();
        }
    }

    /// Makes this a password input, which shows a [`MASK`] for each character, or a plain
    /// one again.
    pub fn set_masked(&mut self, masked_now: bool) {
        if masked_now == self.secret.is_some() {
            return;
        }
        if masked_now {
            let text = self.editor.raw_text().to_owned();
            self.editor.set_text(&masked(text.chars().count()));
            self.secret = Some(text);
        } else if let Some(text) = self.secret.take() {
            self.editor.set_text(&text);
        }
    }

    /// The input's text: a password input's own, not its mask.
    pub fn value(&self) -> String {
        match &self.secret {
            Some(secret) => secret.clone(),
            None => self.editor.raw_text().to_owned(),
        }
    }

    /// The selection, in characters: a password input's, before an edit.
    fn selected_chars(&self) -> std::ops::Range<usize> {
        let bytes = self.editor.raw_selection().text_range();
        let chars = |end: usize| self.editor.raw_text()[..end].chars().count();
        chars(bytes.start)..chars(bytes.end)
    }

    /// After an edit of a password input's editor, takes what it changed into
    /// [`Self::secret`] and masks the editor again. `selected` is the selection before the
    /// edit, in characters.
    ///
    /// An edit replaces the selection: what now lies between the masks kept before and
    /// after it is the new text, as typed or pasted. Deleting with nothing selected
    /// leaves fewer masks, with the caret where they were.
    fn unmask_edit(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        selected: Option<std::ops::Range<usize>>,
    ) {
        let Some(selected) = selected.filter(|_| self.secret.is_some()) else {
            return;
        };
        let now = self.selected_chars().start;
        let Some(secret) = &mut self.secret else {
            return;
        };
        let old: Vec<char> = secret.chars().collect();
        let shown: Vec<char> = self.editor.raw_text().chars().collect();
        if shown.len() == old.len() && shown.iter().all(|&c| c == MASK) {
            return;
        }
        let (kept_before, kept_after) = (selected.start, old.len() - selected.end);
        let (next, caret): (String, usize) = if shown.len() >= kept_before + kept_after {
            let end = shown.len() - kept_after;
            let next = old[..kept_before]
                .iter()
                .chain(&shown[kept_before..end])
                .chain(&old[selected.end..])
                .collect();
            (next, end)
        } else {
            let caret = now.min(shown.len());
            let gone = old.len() - shown.len();
            let next = old[..caret].iter().chain(&old[caret + gone..]).collect();
            (next, caret)
        };
        let length = next.chars().count();
        *secret = next;
        self.editor.set_text(&masked(length));
        let mut driver = self.editor.driver(font_ctx, layout_ctx);
        driver.move_to_byte(caret * MASK.len_utf8());
    }

    /// Recompute [`Self::scroll_offset`] so that the caret stays visible within the input's
    /// content box.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn clamp_scroll_offset(&mut self, content_box_width: f32, content_box_height: f32) {
        let Some(layout) = self.editor.try_layout() else {
            return;
        };
        // Parley lays out at the editor's scale, so its geometry is in scaled (device) pixels.
        // We convert into CSS (unscaled) pixels to match `scroll_offset` and the content box.
        let scale = layout.scale();

        // The caret geometry relative to the start of the text content.
        let Some(caret) = self.editor.cursor_geometry(1.5) else {
            return;
        };

        // Caret bounds and content/viewport extents along the scrolling axis (CSS pixels).
        let (caret_start, caret_end, content, viewport) = if self.is_multiline {
            (
                caret.y0 as f32 / scale,
                caret.y1 as f32 / scale,
                layout.height() / scale,
                content_box_height,
            )
        } else {
            (
                caret.x0 as f32 / scale,
                caret.x1 as f32 / scale,
                layout.full_width() / scale,
                content_box_width,
            )
        };

        let mut offset = self.scroll_offset;

        // Scroll so that both edges of the caret are within the visible region.
        if caret_end > offset + viewport {
            offset = caret_end - viewport;
        }
        if caret_start < offset {
            offset = caret_start;
        }

        // Never scroll past the content, and never scroll into negative space. The content
        // extent includes the caret so that a caret at the very end remains fully visible
        // (its rendered width extends slightly past the text).
        let max_offset = (content.max(caret_end) - viewport).max(0.0);
        self.scroll_offset = offset.clamp(0.0, max_offset);
    }

    /// The maximum valid value of [`Self::scroll_offset`] (in CSS pixels) given the input's
    /// content box, i.e. the extent by which the text content overflows the content box along
    /// the input's scroll axis.
    ///
    /// `content_box_width` and `content_box_height` are the dimensions of the input's content
    /// box in CSS (unscaled) pixels.
    pub fn max_scroll_offset(&self, content_box_width: f32, content_box_height: f32) -> f32 {
        let Some(layout) = self.editor.try_layout() else {
            return 0.0;
        };
        let scale = layout.scale();
        let (content, viewport) = if self.is_multiline {
            (layout.height() / scale, content_box_height)
        } else {
            (layout.full_width() / scale, content_box_width)
        };
        (content - viewport).max(0.0)
    }

    /// Scroll the input's text content by `delta` CSS pixels along its scroll axis (horizontal
    /// for single-line inputs, vertical for multi-line inputs), clamping to the scrollable
    /// range.
    ///
    /// Returns the portion of `delta` that could not be consumed (because the input was already
    /// scrolled to its limit), so the caller can bubble it up to an ancestor scroller.
    pub fn scroll_by(
        &mut self,
        delta: f32,
        content_box_width: f32,
        content_box_height: f32,
    ) -> f32 {
        let max_offset = self.max_scroll_offset(content_box_width, content_box_height);
        if max_offset <= 0.0 {
            return delta;
        }

        // Match the sign convention used for block scrolling: a positive delta decreases the
        // scroll offset.
        let new_offset = (self.scroll_offset - delta).clamp(0.0, max_offset);
        let consumed = self.scroll_offset - new_offset;
        self.scroll_offset = new_offset;
        delta - consumed
    }

    pub(crate) fn apply_keypress_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        event: BlitzKeyEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // A password is neither copied nor cut, as in a browser.
        if self.secret.is_some()
            && event.modifiers.contains(ACTION_MOD)
            && matches!(&event.key, Key::Character(c) if matches!(c.to_lowercase().as_str(), "c" | "x"))
        {
            return None;
        }
        let selected = self.secret.is_some().then(|| self.selected_chars());
        let generated = self.apply_keypress(font_ctx, layout_ctx, shell_provider, event);
        self.unmask_edit(font_ctx, layout_ctx, selected);
        generated
    }

    fn apply_keypress(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        event: BlitzKeyEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // Do nothing if it is a keyup event
        if !event.state.is_pressed() {
            return None;
        }

        let mods = event.modifiers;
        let shift = mods.contains(Modifiers::SHIFT);
        let action_mod = mods.contains(ACTION_MOD);

        let is_multiline = self.is_multiline;
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        match event.key {
            Key::Character(c) if action_mod && matches!(c.as_str(), "c" | "x" | "v") => {
                match c.to_lowercase().as_str() {
                    "c" => {
                        if let Some(text) = driver.editor.selected_text() {
                            let _ = shell_provider.set_clipboard_text(text.to_owned());
                        }
                    }
                    "x" => {
                        if let Some(text) = driver.editor.selected_text() {
                            let _ = shell_provider.set_clipboard_text(text.to_owned());
                            driver.delete_selection()
                        }
                    }
                    "v" => {
                        let _ = shell_provider.request_clipboard_text();
                        return None;
                    }
                    _ => unreachable!(),
                }

                return Some(GeneratedTextInputEvent::Input);
            }
            Key::Character(c) if action_mod && matches!(c.to_lowercase().as_str(), "a") => {
                if shift {
                    driver.collapse_selection()
                } else {
                    driver.select_all()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowLeft) => {
                if action_mod {
                    if shift {
                        driver.select_word_left()
                    } else {
                        driver.move_word_left()
                    }
                } else if shift {
                    driver.select_left()
                } else {
                    driver.move_left()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowRight) => {
                if action_mod {
                    if shift {
                        driver.select_word_right()
                    } else {
                        driver.move_word_right()
                    }
                } else if shift {
                    driver.select_right()
                } else {
                    driver.move_right()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowUp) => {
                if shift {
                    driver.select_up()
                } else {
                    driver.move_up()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::ArrowDown) => {
                if shift {
                    driver.select_down()
                } else {
                    driver.move_down()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::Home) => {
                if action_mod {
                    if shift {
                        driver.select_to_text_start()
                    } else {
                        driver.move_to_text_start()
                    }
                } else if shift {
                    driver.select_to_line_start()
                } else {
                    driver.move_to_line_start()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::End) => {
                if action_mod {
                    if shift {
                        driver.select_to_text_end()
                    } else {
                        driver.move_to_text_end()
                    }
                } else if shift {
                    driver.select_to_line_end()
                } else {
                    driver.move_to_line_end()
                }
                return Some(GeneratedTextInputEvent::Select);
            }
            Key::Named(NamedKey::Delete) => {
                if action_mod {
                    driver.delete_word()
                } else {
                    driver.delete()
                }
                return Some(GeneratedTextInputEvent::Input);
            }

            // On macOS this is handled by the apple standard keybindings
            #[cfg(not(target_os = "macos"))]
            Key::Named(NamedKey::Backspace) => {
                if action_mod {
                    driver.backdelete_word()
                } else {
                    driver.backdelete()
                }
                return Some(GeneratedTextInputEvent::Input);
            }

            Key::Character(c) if c == "\n" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Named(NamedKey::Enter) => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            Key::Character(s)
                if !mods.contains(Modifiers::CONTROL) && !mods.contains(Modifiers::META) =>
            {
                driver.insert_or_replace_selection(&s);
                return Some(GeneratedTextInputEvent::Input);
            }
            _ => {}
        };

        None
    }

    pub(crate) fn apply_apple_standard_keybinding(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        command: &str,
    ) -> Option<GeneratedTextInputEvent> {
        if self.secret.is_some() && command == "yank:" {
            return None;
        }
        let selected = self.secret.is_some().then(|| self.selected_chars());
        let generated = self.apply_keybinding(font_ctx, layout_ctx, shell_provider, command);
        self.unmask_edit(font_ctx, layout_ctx, selected);
        generated
    }

    fn apply_keybinding(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        shell_provider: &dyn ShellProvider,
        command: &str,
    ) -> Option<GeneratedTextInputEvent> {
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);
        let is_multiline = self.is_multiline;

        match command {
            // Inserting Content

            // Inserts a backtab character.
            "insertBacktab:" => {}
            // Inserts a container break, such as a new page break.
            "insertContainerBreak:" => {}
            // Inserts a double quotation mark without substituting a curly quotation mark.
            "insertDoubleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("\"");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a line break character.
            "insertLineBreak:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a newline character.
            "insertNewline:" => {
                if is_multiline {
                    driver.insert_or_replace_selection("\n");
                    return Some(GeneratedTextInputEvent::Input);
                } else {
                    return Some(GeneratedTextInputEvent::Submit);
                }
            }
            // Inserts a newline character without invoking the field editor’s normal handling to end editing.
            "insertNewlineIgnoringFieldEditor:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a paragraph separator.
            "insertParagraphSeparator:" => {
                driver.insert_or_replace_selection("\n");
                return Some(GeneratedTextInputEvent::Input);
            }
            "insertSingleQuoteIgnoringSubstitution:" => {
                driver.insert_or_replace_selection("'");
                return Some(GeneratedTextInputEvent::Input);
            }
            // Inserts a tab character.
            "insertTab:" | "insertTabIgnoringFieldEditor:" => {
                // Ignore for now seeing as parley has poor support for laying out tabs
            }
            // Inserts the text you specify.
            "insertText:" => {}

            // Deleting Content

            // Deletes content moving backward from the current insertion point.
            // TODO: handle deleteBackwardByDecomposingPreviousCharacter separately
            "deleteBackward:" | "deleteBackwardByDecomposingPreviousCharacter:" => {
                driver.backdelete();
                return Some(GeneratedTextInputEvent::Input);
            }
            "deleteForward:" => {
                driver.delete();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the beginning of the current line.
            "deleteToBeginningOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the beginning of the current paragraph.
            "deleteToEndOfLine:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            "deleteToBeginningOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_start();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }

            // Deletes content from the insertion point to the end of the current line.
            "deleteToEndOfParagraph:" => {
                if driver.editor.raw_selection().is_collapsed() {
                    driver.select_to_hard_line_end();
                }
                driver.delete_selection();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes content from the insertion point to the end of the current paragraph.
            "deleteWordBackward:" => {
                driver.backdelete_word();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes the word preceding the current insertion point.
            "deleteWordForward:" => {
                driver.delete_word();
                return Some(GeneratedTextInputEvent::Input);
            }
            // Deletes the current selection, placing it in a temporary buffer, such as the Clipboard.
            "yank:" => {
                if let Some(text) = driver.editor.selected_text() {
                    let _ = shell_provider.set_clipboard_text(text.to_owned());
                    driver.delete_selection();
                    return Some(GeneratedTextInputEvent::Input);
                }
            }

            // Moving the Insertion Pointer

            // Moves the insertion pointer backward in the current content.
            "moveBackward:" => {
                driver.move_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moves the insertion pointer down in the current content.
            "moveDown:" => {
                driver.move_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer forward in the current content.
            "moveForward:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            } // TODO: Bidi-aware

            // Moves the insertion pointer left in the current content.
            "moveLeft:" => {
                driver.move_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer right in the current content.
            "moveRight:" => {
                driver.move_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Moves the insertion pointer up in the current content.
            "moveUp:" => {
                driver.move_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Modifying the Selection

            // Extends the selection to include the content before the current selection.
            "moveBackwardAndModifySelection:" => {
                driver.select_left(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content below the current selection.
            "moveDownAndModifySelection:" => {
                driver.select_down();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content after the current selection.
            "moveForwardAndModifySelection:" => {
                driver.select_right(); // TODO: Bidi-aware
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the left of the current selection.
            "moveLeftAndModifySelection:" => {
                driver.select_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content to the right of the current selection.
            "moveRightAndModifySelection:" => {
                driver.select_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            // Extends the selection to include the content above the current selection.
            "moveUpAndModifySelection:" => {
                driver.select_up();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Changing the Selection
            "selectAll:" => {
                driver.select_all();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectLine:" => {
                driver.move_to_line_start();
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectParagraph:" => {
                driver.move_to_hard_line_start();
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "selectWord:" => {
                // TODO
            }

            // Moving the Selection in Documents
            "moveToBeginningOfDocument:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfDocumentAndModifySelection:" => {
                driver.select_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocument:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfDocumentAndModifySelection:" => {
                driver.move_to_text_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Paragraphs
            "moveParagraphBackwardAndModifySelection:" => {}
            "moveParagraphForwardAndModifySelection:" => {}
            "moveToBeginningOfParagraph:" => {
                driver.move_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraph:" => {
                driver.move_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfParagraphAndModifySelection:" => {
                driver.select_to_hard_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection in Lines of Text
            "moveToBeginningOfLine:" => {
                driver.move_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToBeginningOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLine:" => {
                driver.move_to_text_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToLeftEndOfLineAndModifySelection:" => {
                driver.select_to_line_start();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLine:" => {
                driver.move_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveToRightEndOfLineAndModifySelection:" => {
                driver.select_to_line_end();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Moving the Selection by Word Boundaries
            "moveWordBackward:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordBackwardAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForward:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordForwardAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeft:" => {
                driver.move_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordLeftAndModifySelection:" => {
                driver.select_word_left();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRight:" => {
                driver.move_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }
            "moveWordRightAndModifySelection:" => {
                driver.select_word_right();
                return Some(GeneratedTextInputEvent::Select);
            }

            // Scrolling Content

            // Scrolls the content down by a page.
            "scrollPageDown:" => {}
            // Scrolls the content up by a page.
            "scrollPageUp:" => {}
            // Scrolls the content down by a line.
            "scrollLineDown:" => {}
            // Scrolls the content up by a line.
            "scrollLineUp:" => {}
            // Scrolls the content to the beginning of the document.
            "scrollToBeginningOfDocument:" => {}
            // Scrolls the content to the end of the document.
            "scrollToEndOfDocument:" => {}
            // Moves the visible content region down by a page.
            "pageDown:" => {}
            // Moves the visible content region up by a page.
            "pageUp:" => {}
            // Moves the visible content region down by a page, and extends the current selection.
            "pageDownAndModifySelection:" => {}
            // Moves the visible content region up by a page, and extends the current selection.
            "pageUpAndModifySelection:" => {}
            // Moves the visible content region so the current selection is visually centered.
            "centerSelectionInVisibleArea:" => {}

            // Transposing Elements

            // Transposes the content around the current selection.
            "transpose:" => {}
            // Transposes the words around the current selection.
            "transposeWords:" => {}

            // Indenting Content
            // Indents the content at the current selection.
            "indent:" => {}

            // Canceling Operations
            // Cancels the current operation.
            "cancelOperation:" => {}

            // Supporting QuickLook
            // Invokes QuickLook to preview the current selection.
            "quickLookPreviewItems:" => {}

            // Supporting Writing Directions
            "makeBaseWritingDirectionLeftToRight:" => {}
            "makeBaseWritingDirectionNatural:" => {}
            "makeBaseWritingDirectionRightToLeft:" => {}
            "makeTextWritingDirectionLeftToRight:" => {}
            "makeTextWritingDirectionNatural:" => {}
            "makeTextWritingDirectionRightToLeft:" => {}

            // Changing Capitalization
            "capitalizeWord:" => {}
            "changeCaseOfLetter:" => {}
            "lowercaseWord:" => {}
            "uppercaseWord:" => {}

            // Supporting Marked Selections
            "setMark:" => {}
            "selectToMark:" => {}
            "deleteToMark:" => {}
            "swapWithMark:" => {}

            // Supporting Autocomplete
            "complete:" => {}

            // Instance Methods
            "showContextMenuForSelection:" => {}

            // Unknown command
            _ => {}
        };

        None
    }

    pub(crate) fn apply_ime_event(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        event: BlitzImeEvent,
    ) -> Option<GeneratedTextInputEvent> {
        // A password isn't composed where it shows: only committed text goes in.
        if self.secret.is_some() && matches!(event, BlitzImeEvent::Preedit(..)) {
            return None;
        }
        let selected = self.secret.is_some().then(|| self.selected_chars());
        let generated = self.apply_ime(font_ctx, layout_ctx, event);
        self.unmask_edit(font_ctx, layout_ctx, selected);
        generated
    }

    fn apply_ime(
        &mut self,
        font_ctx: &mut FontContext,
        layout_ctx: &mut LayoutContext<TextBrush>,
        event: BlitzImeEvent,
    ) -> Option<GeneratedTextInputEvent> {
        let editor = &mut self.editor;
        let mut driver = editor.driver(font_ctx, layout_ctx);

        match event {
            BlitzImeEvent::Enabled => {
                // Do nothing
                None
            }
            BlitzImeEvent::Disabled => {
                driver.clear_compose();
                Some(GeneratedTextInputEvent::PreEditChange)
            }
            BlitzImeEvent::Commit(text) => {
                driver.clear_compose();
                driver.insert_or_replace_selection(&text);
                Some(GeneratedTextInputEvent::Input)
            }
            BlitzImeEvent::Preedit(text, cursor) => {
                if text.is_empty() {
                    driver.clear_compose();
                } else {
                    driver.set_compose(&text, cursor);
                }
                Some(GeneratedTextInputEvent::PreEditChange)
            }
            BlitzImeEvent::DeleteSurrounding {
                before_bytes,
                after_bytes,
            } => {
                let _ = before_bytes;
                let _ = after_bytes;
                // TODO
                None
            }
        }
    }
}

/// `length` masks.
fn masked(length: usize) -> String {
    std::iter::repeat_n(MASK, length).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_commit_replaces_preedit() {
        for committed in ["完成", ""] {
            for cursor in [Some((3, 3)), None] {
                let mut input = TextInputData::new(false);
                let mut font_ctx = FontContext::new();
                let mut layout_ctx = LayoutContext::new();
                input.apply_ime_event(
                    &mut font_ctx,
                    &mut layout_ctx,
                    BlitzImeEvent::Preedit("仮".into(), cursor),
                );
                input.apply_ime_event(
                    &mut font_ctx,
                    &mut layout_ctx,
                    BlitzImeEvent::Commit(committed.into()),
                );
                assert_eq!(input.editor.text(), committed);
                input.apply_ime_event(&mut font_ctx, &mut layout_ctx, BlitzImeEvent::Disabled);
                assert_eq!(input.editor.text(), committed);
            }
        }
    }

    #[test]
    fn ime_commit_without_preedit() {
        let mut input = TextInputData::new(false);
        input.apply_ime_event(
            &mut FontContext::new(),
            &mut LayoutContext::new(),
            BlitzImeEvent::Commit("text".into()),
        );
        assert_eq!(input.editor.text(), "text");
    }

    fn press(input: &mut TextInputData, key: Key, modifiers: Modifiers) {
        input.apply_keypress_event(
            &mut FontContext::new(),
            &mut LayoutContext::new(),
            &blitz_traits::shell::DummyShellProvider,
            BlitzKeyEvent {
                text: match &key {
                    Key::Character(c) => Some(c.as_str().into()),
                    _ => None,
                },
                key,
                code: keyboard_types::Code::Unidentified,
                modifiers,
                location: keyboard_types::Location::Standard,
                is_auto_repeating: false,
                is_composing: false,
                state: blitz_traits::events::KeyState::Pressed,
            },
        );
    }

    fn typed(input: &mut TextInputData, text: &str) {
        for c in text.chars() {
            press(input, Key::Character(c.to_string()), Modifiers::empty());
        }
    }

    #[test]
    fn a_password_shows_masks_and_keeps_its_text() {
        let (mut font_ctx, mut layout_ctx) = (FontContext::new(), LayoutContext::new());
        let mut input = TextInputData::new(false);
        input.set_masked(true);
        typed(&mut input, "pässword");
        assert_eq!(input.value(), "pässword");
        assert_eq!(input.editor.raw_text(), "••••••••");

        press(
            &mut input,
            Key::Named(NamedKey::Backspace),
            Modifiers::empty(),
        );
        assert_eq!(input.value(), "pässwor");
        // In the middle: before "w", then a paste there.
        for _ in 0..3 {
            press(
                &mut input,
                Key::Named(NamedKey::ArrowLeft),
                Modifiers::empty(),
            );
        }
        press(&mut input, Key::Named(NamedKey::Delete), Modifiers::empty());
        assert_eq!(input.value(), "pässor");
        input.apply_ime_event(
            &mut font_ctx,
            &mut layout_ctx,
            BlitzImeEvent::Commit("W•".into()),
        );
        assert_eq!(input.value(), "pässW•or");
        press(
            &mut input,
            Key::Named(NamedKey::Backspace),
            Modifiers::empty(),
        );
        assert_eq!(input.value(), "pässWor");
        typed(&mut input, "x");
        assert_eq!(input.value(), "pässWxor");

        // Composing shows nothing in a password; only the commit goes in.
        input.apply_ime_event(
            &mut font_ctx,
            &mut layout_ctx,
            BlitzImeEvent::Preedit("仮".into(), None),
        );
        assert_eq!(input.editor.raw_text(), "••••••••");

        press(&mut input, Key::Character("a".into()), ACTION_MOD);
        typed(&mut input, "new");
        assert_eq!(input.value(), "new");
        assert_eq!(input.editor.raw_text(), "•••");

        input.set_text(&mut font_ctx, &mut layout_ctx, "set");
        assert_eq!(input.value(), "set");
        assert_eq!(input.editor.raw_text(), "•••");
        input.set_masked(false);
        assert_eq!(input.editor.raw_text(), "set");
    }

    #[test]
    fn a_password_is_not_copied_or_cut() {
        let mut input = TextInputData::new(false);
        input.set_masked(true);
        typed(&mut input, "secret");
        press(&mut input, Key::Character("a".into()), ACTION_MOD);
        press(&mut input, Key::Character("x".into()), ACTION_MOD);
        assert_eq!(input.value(), "secret");
    }
}
