use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

const HISTORY_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorSnapshot {
    text: String,
    selection: Range<usize>,
    selection_reversed: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Composer {
    text: String,
    selection: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    undo: Vec<EditorSnapshot>,
    redo: Vec<EditorSnapshot>,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn selection_reversed(&self) -> bool {
        self.selection_reversed
    }

    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    pub fn replace(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.valid_range(range.unwrap_or_else(|| self.selection.clone()));
        self.record_edit();
        self.replace_without_history(range, text);
    }

    fn replace_without_history(&mut self, range: Range<usize>, text: &str) {
        self.text.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selection = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
    }

    pub fn replace_and_mark(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.valid_range(range.unwrap_or_else(|| {
            self.marked_range
                .clone()
                .unwrap_or_else(|| self.selection.clone())
        }));
        if self.marked_range.is_none() {
            self.record_edit();
        }
        self.text.replace_range(range.clone(), text);
        let marked = range.start..range.start + text.len();
        self.selection = marked.end..marked.end;
        self.selection_reversed = false;
        self.marked_range = Some(marked);
    }

    pub fn unmark(&mut self) {
        self.marked_range = None;
    }

    pub fn insert_newline(&mut self) {
        self.replace(None, "\n");
    }

    pub fn delete_backward(&mut self) -> bool {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor();
            self.previous_grapheme(cursor)..cursor
        } else {
            self.selection.clone()
        };
        self.delete_range(range)
    }

    pub fn delete_forward(&mut self) -> bool {
        let range = if self.selection.is_empty() {
            let cursor = self.cursor();
            cursor..self.next_grapheme(cursor)
        } else {
            self.selection.clone()
        };
        self.delete_range(range)
    }

    fn delete_range(&mut self, range: Range<usize>) -> bool {
        if range.is_empty() {
            return false;
        }
        self.record_edit();
        self.replace_without_history(range, "");
        true
    }

    pub fn move_to(&mut self, offset: usize, extend: bool) {
        let offset = floor_char_boundary(&self.text, offset.min(self.text.len()));
        if extend {
            self.select_to(offset);
        } else {
            self.selection = offset..offset;
            self.selection_reversed = false;
        }
        self.marked_range = None;
    }

    #[cfg(test)]
    pub fn set_selection(&mut self, range: Range<usize>, reversed: bool) {
        self.selection = self.valid_range(range);
        self.selection_reversed = reversed && !self.selection.is_empty();
        self.marked_range = None;
    }

    pub fn move_left(&mut self, extend: bool, by_word: bool) {
        let target = if !extend && !self.selection.is_empty() {
            self.selection.start
        } else if by_word {
            self.previous_word(self.cursor())
        } else {
            self.previous_grapheme(self.cursor())
        };
        self.move_to(target, extend);
    }

    pub fn move_right(&mut self, extend: bool, by_word: bool) {
        let target = if !extend && !self.selection.is_empty() {
            self.selection.end
        } else if by_word {
            self.next_word(self.cursor())
        } else {
            self.next_grapheme(self.cursor())
        };
        self.move_to(target, extend);
    }

    pub fn move_line_start(&mut self, extend: bool) {
        self.move_to(self.line_bounds(self.cursor()).start, extend);
    }

    pub fn move_line_end(&mut self, extend: bool) {
        self.move_to(self.line_bounds(self.cursor()).end, extend);
    }

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

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
        self.selection_reversed = false;
    }

    pub fn selected_text(&self) -> Option<&str> {
        (!self.selection.is_empty()).then(|| &self.text[self.selection.clone()])
    }

    pub fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(snapshot);
        true
    }

    pub fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn cursor_line(&self) -> usize {
        self.text[..self.cursor()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
    }

    pub fn take_submission(&mut self) -> Option<String> {
        let submission = self.text.trim().to_owned();
        if submission.is_empty() {
            return None;
        }
        self.text.clear();
        self.selection = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.undo.clear();
        self.redo.clear();
        Some(submission)
    }

    pub fn utf16_selection(&self) -> Range<usize> {
        byte_range_to_utf16(&self.text, self.selection.clone())
    }

    pub fn utf16_range(&self, range: Range<usize>) -> Range<usize> {
        byte_range_to_utf16(&self.text, range)
    }

    pub fn byte_range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        utf16_range_to_byte(&self.text, range)
    }

    fn valid_range(&self, range: Range<usize>) -> Range<usize> {
        let start = floor_char_boundary(&self.text, range.start.min(self.text.len()));
        let end = floor_char_boundary(&self.text, range.end.min(self.text.len())).max(start);
        start..end
    }

    fn select_to(&mut self, offset: usize) {
        let offset = floor_char_boundary(&self.text, offset.min(self.text.len()));
        if self.selection_reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.selection_reversed = !self.selection_reversed;
            self.selection = self.selection.end..self.selection.start;
        }
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

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn restore(&mut self, snapshot: EditorSnapshot) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.selection_reversed = snapshot.selection_reversed;
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
    use super::Composer;

    #[test]
    fn backspace_deletes_selection_or_one_grapheme_without_corrupting_unicode() {
        let mut composer = Composer::default();
        composer.replace(None, "a👨‍👩‍👧‍👦b");

        assert!(composer.delete_backward());
        assert_eq!(composer.text(), "a👨‍👩‍👧‍👦");
        assert!(composer.delete_backward());
        assert_eq!(composer.text(), "a");
        composer.replace(None, "lpha");
        composer.set_selection(1..5, false);
        assert!(composer.delete_backward());
        assert_eq!(composer.text(), "a");
        assert!(composer.delete_backward());
        assert!(!composer.delete_backward());
    }

    #[test]
    fn forward_delete_removes_the_next_grapheme() {
        let mut composer = Composer::default();
        composer.replace(None, "ñ👍🏽z");
        composer.move_to(2, false);

        assert!(composer.delete_forward());
        assert_eq!(composer.text(), "ñz");
        assert_eq!(composer.selection(), 2..2);
    }

    #[test]
    fn navigation_moves_and_extends_selection_across_words_and_lines() {
        let mut composer = Composer::default();
        composer.replace(None, "one two\nñine");

        composer.move_left(false, true);
        assert_eq!(composer.selection(), 8..8);
        composer.move_to(12, false);
        composer.move_vertical(-1, false);
        assert_eq!(composer.selection(), 3..3);
        composer.move_line_start(false);
        assert_eq!(composer.selection(), 0..0);
        composer.move_right(true, true);
        assert_eq!(composer.selection(), 0..3);
        composer.move_line_end(true);
        assert_eq!(composer.selection(), 0..7);
    }

    #[test]
    fn reverse_selection_tracks_the_active_cursor() {
        let mut composer = Composer::default();
        composer.replace(None, "abc");
        composer.move_left(true, false);
        composer.move_left(true, false);

        assert_eq!(composer.selection(), 1..3);
        assert!(composer.selection_reversed());
        assert_eq!(composer.cursor(), 1);
        composer.move_right(true, false);
        assert_eq!(composer.selection(), 2..3);
    }

    #[test]
    fn undo_redo_restore_edits_and_new_edits_invalidate_redo() {
        let mut composer = Composer::default();
        composer.replace(None, "alpha");
        composer.delete_backward();
        assert_eq!(composer.text(), "alph");

        assert!(composer.undo());
        assert_eq!(composer.text(), "alpha");
        assert!(composer.redo());
        assert_eq!(composer.text(), "alph");
        assert!(composer.undo());
        composer.replace(None, "!");
        assert_eq!(composer.text(), "alpha!");
        assert!(!composer.redo());
    }

    #[test]
    fn select_all_exposes_exact_clipboard_text() {
        let mut composer = Composer::default();
        composer.replace(None, "one\ntwo");
        composer.select_all();

        assert_eq!(composer.selected_text(), Some("one\ntwo"));
    }

    #[test]
    fn edits_multiline_text_and_replaces_a_selection() {
        let mut composer = Composer::default();
        composer.replace(None, "hello");
        composer.insert_newline();
        composer.replace(None, "world");
        composer.selection = 6..11;
        composer.replace(None, "Nopal");

        assert_eq!(composer.text(), "hello\nNopal");
        assert_eq!(composer.selection(), 11..11);
    }

    #[test]
    fn marked_text_can_be_replaced_and_committed() {
        let mut composer = Composer::default();
        composer.replace_and_mark(None, "n");
        composer.replace_and_mark(None, "ñ");
        assert_eq!(composer.text(), "ñ");
        assert_eq!(composer.marked_range(), Some(0..2));

        composer.unmark();
        assert_eq!(composer.marked_range(), None);
    }

    #[test]
    fn submission_trims_edges_and_clears_the_composer() {
        let mut composer = Composer::default();
        composer.replace(None, "  run this\ncarefully  ");

        assert_eq!(
            composer.take_submission().as_deref(),
            Some("run this\ncarefully")
        );
        assert_eq!(composer.text(), "");
        assert_eq!(composer.take_submission(), None);
    }

    #[test]
    fn native_utf16_ranges_round_trip_unicode() {
        let mut composer = Composer::default();
        composer.replace(None, "a😀b");
        composer.selection = 1..5;

        assert_eq!(composer.utf16_selection(), 1..3);
        assert_eq!(composer.byte_range_from_utf16(1..3), 1..5);
    }
}
