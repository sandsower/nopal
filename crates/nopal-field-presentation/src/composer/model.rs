//! Pure text, selection, marked-text, and history model for one Composer draft.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
struct EditorSnapshot {
    text: String,
    selection_anchor: usize,
    selection_head: usize,
}

/// Exact editable state for one Core Session target.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ComposerDraft {
    text: String,
    selection_anchor: usize,
    selection_head: usize,
    marked_range: Option<Range<usize>>,
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
    mutation_generation: u64,
}

impl ComposerDraft {
    /// Returns the exact visible text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the ordered UTF-8 byte selection.
    pub fn selection(&self) -> Range<usize> {
        self.selection_anchor.min(self.selection_head)
            ..self.selection_anchor.max(self.selection_head)
    }

    /// Returns whether the active selection endpoint precedes its anchor.
    pub fn selection_reversed(&self) -> bool {
        self.selection_head < self.selection_anchor
    }

    /// Returns the UTF-8 byte selection anchor.
    pub fn selection_anchor(&self) -> usize {
        self.selection_anchor
    }

    /// Returns the UTF-8 byte active selection endpoint.
    pub fn selection_head(&self) -> usize {
        self.selection_head
    }

    /// Returns the active marked-text byte range.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    pub(crate) fn mutation_generation(&self) -> u64 {
        self.mutation_generation
    }

    /// Replaces the supplied range or the current selection.
    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.valid_range(range.unwrap_or_else(|| self.selection()));
        let cursor = range.start + text.len();
        if self.text[range.clone()] == *text
            && self.selection_anchor == cursor
            && self.selection_head == cursor
            && self.marked_range.is_none()
        {
            return;
        }
        self.record_edit();
        self.replace_without_history(range, text);
        self.note_mutation();
    }

    /// Replaces the supplied range and records the replacement as marked text.
    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.valid_range(range.unwrap_or_else(|| {
            self.marked_range
                .clone()
                .unwrap_or_else(|| self.selection())
        }));
        let marked = range.start..range.start + text.len();
        if self.text[range.clone()] == *text
            && self.selection_anchor == marked.end
            && self.selection_head == marked.end
            && self.marked_range.as_ref() == Some(&marked)
        {
            return;
        }
        if self.marked_range.is_none() {
            self.record_edit();
        }
        self.text.replace_range(range.clone(), text);
        self.selection_anchor = marked.end;
        self.selection_head = marked.end;
        self.marked_range = Some(marked);
        self.note_mutation();
    }

    /// Commits the active marked-text range without changing its text.
    pub fn unmark(&mut self) {
        if self.marked_range.take().is_some() {
            self.note_mutation();
        }
    }

    /// Inserts one hard line break.
    pub fn insert_newline(&mut self) {
        self.replace(None, "\n");
    }

    /// Deletes the selection or one preceding Unicode grapheme.
    pub fn delete_backward(&mut self) -> bool {
        let selection = self.selection();
        let range = if selection.is_empty() {
            let cursor = self.cursor();
            self.previous_grapheme(cursor)..cursor
        } else {
            selection
        };
        self.delete_range(range)
    }

    /// Deletes the selection or one following Unicode grapheme.
    pub fn delete_forward(&mut self) -> bool {
        let selection = self.selection();
        let range = if selection.is_empty() {
            let cursor = self.cursor();
            cursor..self.next_grapheme(cursor)
        } else {
            selection
        };
        self.delete_range(range)
    }

    /// Moves the active endpoint to one safe UTF-8 byte boundary.
    pub fn move_to(&mut self, offset: usize, extend: bool) {
        let offset = floor_char_boundary(&self.text, offset.min(self.text.len()));
        let (anchor, head) = if extend {
            (self.selection_anchor, offset)
        } else {
            (offset, offset)
        };
        if self.selection_anchor == anchor
            && self.selection_head == head
            && self.marked_range.is_none()
        {
            return;
        }
        self.selection_anchor = anchor;
        self.selection_head = head;
        self.marked_range = None;
        self.note_mutation();
    }

    /// Sets the selection anchor and active endpoint at safe byte boundaries.
    pub fn set_selection(&mut self, anchor: usize, head: usize) {
        let anchor = floor_char_boundary(&self.text, anchor.min(self.text.len()));
        let head = floor_char_boundary(&self.text, head.min(self.text.len()));
        if self.selection_anchor == anchor
            && self.selection_head == head
            && self.marked_range.is_none()
        {
            return;
        }
        self.selection_anchor = anchor;
        self.selection_head = head;
        self.marked_range = None;
        self.note_mutation();
    }

    /// Moves one grapheme or word to the left.
    pub fn move_left(&mut self, extend: bool, by_word: bool) {
        let selection = self.selection();
        let target = if !extend && !selection.is_empty() {
            selection.start
        } else if by_word {
            self.previous_word(self.cursor())
        } else {
            self.previous_grapheme(self.cursor())
        };
        self.move_to(target, extend);
    }

    /// Moves one grapheme or word to the right.
    pub fn move_right(&mut self, extend: bool, by_word: bool) {
        let selection = self.selection();
        let target = if !extend && !selection.is_empty() {
            selection.end
        } else if by_word {
            self.next_word(self.cursor())
        } else {
            self.next_grapheme(self.cursor())
        };
        self.move_to(target, extend);
    }

    /// Moves to the beginning of the current hard line.
    pub fn move_line_start(&mut self, extend: bool) {
        self.move_to(self.line_bounds(self.cursor()).start, extend);
    }

    /// Moves to the end of the current hard line.
    pub fn move_line_end(&mut self, extend: bool) {
        self.move_to(self.line_bounds(self.cursor()).end, extend);
    }

    /// Moves vertically by one hard line while retaining the grapheme column.
    pub fn move_vertical(&mut self, delta: i32, extend: bool) {
        let cursor = self.cursor();
        let bounds = self.line_bounds(cursor);
        let column = self.text[bounds.start..cursor].graphemes(true).count();
        let target_bounds = if delta < 0 {
            if bounds.start == 0 {
                bounds
            } else {
                self.line_bounds(bounds.start.saturating_sub(1))
            }
        } else if bounds.end == self.text.len() {
            bounds
        } else {
            self.line_bounds((bounds.end + 1).min(self.text.len()))
        };
        let target = self.text[target_bounds.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map(|(offset, _)| target_bounds.start + offset)
            .unwrap_or(target_bounds.end);
        self.move_to(target, extend);
    }

    /// Selects the complete draft.
    pub fn select_all(&mut self) {
        if self.selection_anchor == 0
            && self.selection_head == self.text.len()
            && self.marked_range.is_none()
        {
            return;
        }
        self.selection_anchor = 0;
        self.selection_head = self.text.len();
        self.marked_range = None;
        self.note_mutation();
    }

    /// Returns the exact selected text when the selection is non-empty.
    pub fn selected_text(&self) -> Option<&str> {
        let selection = self.selection();
        (!selection.is_empty()).then(|| &self.text[selection])
    }

    /// Restores the preceding bounded editor snapshot.
    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(snapshot);
        self.note_mutation();
        true
    }

    /// Reapplies the next bounded editor snapshot.
    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(snapshot);
        self.note_mutation();
        true
    }

    /// Returns the UTF-8 byte active endpoint.
    pub fn cursor(&self) -> usize {
        self.selection_head
    }

    /// Returns the zero-based hard-line index containing the cursor.
    pub fn cursor_line(&self) -> usize {
        self.text[..self.cursor()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    }

    /// Converts the current selection to UTF-16 offsets.
    pub fn utf16_selection(&self) -> Range<usize> {
        byte_range_to_utf16(&self.text, self.selection())
    }

    /// Converts a byte range to UTF-16 offsets.
    pub fn utf16_range(&self, range: Range<usize>) -> Range<usize> {
        byte_range_to_utf16(&self.text, self.valid_range(range))
    }

    /// Converts UTF-16 offsets to safe UTF-8 byte offsets.
    pub fn byte_range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        utf16_range_to_byte(&self.text, range)
    }

    fn delete_range(&mut self, range: Range<usize>) -> bool {
        if range.is_empty() {
            return false;
        }
        self.record_edit();
        self.replace_without_history(range, "");
        self.note_mutation();
        true
    }

    fn replace_without_history(&mut self, range: Range<usize>, text: &str) {
        self.text.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selection_anchor = cursor;
        self.selection_head = cursor;
        self.marked_range = None;
    }

    fn valid_range(&self, range: Range<usize>) -> Range<usize> {
        let start = floor_char_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_char_boundary(&self.text, range.end.min(self.text.len())).max(start);
        start..end
    }

    fn previous_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_grapheme(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.text.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        let mut index = offset;
        while index > 0 {
            let previous = self.previous_grapheme(index);
            if self.text[previous..index]
                .chars()
                .any(char::is_alphanumeric)
            {
                break;
            }
            index = previous;
        }
        while index > 0 {
            let previous = self.previous_grapheme(index);
            if !self.text[previous..index]
                .chars()
                .any(char::is_alphanumeric)
            {
                break;
            }
            index = previous;
        }
        index
    }

    fn next_word(&self, offset: usize) -> usize {
        let mut index = offset;
        while index < self.text.len() {
            let next = self.next_grapheme(index);
            if self.text[index..next].chars().any(char::is_alphanumeric) {
                break;
            }
            index = next;
        }
        while index < self.text.len() {
            let next = self.next_grapheme(index);
            if !self.text[index..next].chars().any(char::is_alphanumeric) {
                break;
            }
            index = next;
        }
        index
    }

    fn line_bounds(&self, offset: usize) -> Range<usize> {
        let start = self.text[..offset].rfind('\n').map_or(0, |index| index + 1);
        let end = self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index);
        start..end
    }

    fn record_edit(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn note_mutation(&mut self) {
        self.mutation_generation = self.mutation_generation.saturating_add(1);
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            selection_anchor: self.selection_anchor,
            selection_head: self.selection_head,
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        self.text = snapshot.text;
        self.selection_anchor = snapshot.selection_anchor;
        self.selection_head = snapshot.selection_head;
        self.marked_range = None;
    }
}

fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    text[..range.start].encode_utf16().count()..text[..range.end].encode_utf16().count()
}

fn utf16_range_to_byte(text: &str, range: Range<usize>) -> Range<usize> {
    utf16_index_to_byte(text, range.start)..utf16_index_to_byte(text, range.end)
}

fn utf16_index_to_byte(text: &str, target: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 >= target {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::ComposerDraft;

    #[test]
    fn grapheme_editing_never_corrupts_unicode() {
        let mut draft = ComposerDraft::default();
        draft.replace(None, "a👨‍👩‍👧‍👦b");
        assert!(draft.delete_backward());
        assert_eq!(draft.text(), "a👨‍👩‍👧‍👦");
        assert!(draft.delete_backward());
        assert_eq!(draft.text(), "a");
    }

    #[test]
    fn native_utf16_selection_round_trips_unicode() {
        let mut draft = ComposerDraft::default();
        draft.replace(None, "a😀b");
        draft.set_selection(1, 5);
        assert_eq!(draft.utf16_selection(), 1..3);
        assert_eq!(draft.byte_range_from_utf16(1..3), 1..5);
    }

    #[test]
    fn undo_redo_preserve_exact_text_and_selection() {
        let mut draft = ComposerDraft::default();
        draft.replace(None, "alpha");
        assert!(draft.delete_backward());
        assert_eq!(draft.text(), "alph");
        assert!(draft.undo());
        assert_eq!(draft.text(), "alpha");
        assert!(draft.redo());
        assert_eq!(draft.text(), "alph");
    }
}
