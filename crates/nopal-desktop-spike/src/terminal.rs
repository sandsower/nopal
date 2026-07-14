use std::collections::BTreeSet;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi;
use alacritty_terminal::vte::ansi::{Color, CursorShape};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalStyle {
    pub foreground: Color,
    pub background: Color,
    pub flags: Flags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRun {
    pub start_column: usize,
    pub cell_width: usize,
    pub text: String,
    pub style: TerminalStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    pub line: usize,
    pub runs: Vec<TerminalRun>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCursor {
    pub line: usize,
    pub column: usize,
    pub shape: CursorShape,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub columns: usize,
    pub rows: Vec<TerminalRow>,
    pub cursor: TerminalCursor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Damage {
    pub rows: BTreeSet<usize>,
    pub cursor_changed: bool,
    pub full: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalModes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    pub sgr_mouse: bool,
}

pub struct TerminalSurface {
    term: Term<VoidListener>,
    parser: ansi::Processor,
    columns: usize,
    rows: usize,
    last_snapshot: TerminalSnapshot,
}

#[derive(Debug, Clone, Copy)]
struct GridSize {
    columns: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows + 5_000
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

impl TerminalSurface {
    pub fn new(columns: usize, rows: usize) -> Self {
        let size = GridSize { columns, rows };
        let term = Term::new(
            Config {
                scrolling_history: 5_000,
                ..Default::default()
            },
            &size,
            VoidListener,
        );
        let last_snapshot = snapshot_term(&term, columns, rows);
        Self {
            term,
            parser: ansi::Processor::new(),
            columns,
            rows,
            last_snapshot,
        }
    }

    pub fn apply_output(&mut self, bytes: &[u8]) -> Damage {
        let before = self.last_snapshot.clone();
        self.parser.advance(&mut self.term, bytes);
        let after = snapshot_term(&self.term, self.columns, self.rows);
        let rows = before
            .rows
            .iter()
            .zip(&after.rows)
            .filter_map(|(before, after)| (before != after).then_some(after.line))
            .collect();
        let damage = Damage {
            rows,
            cursor_changed: before.cursor != after.cursor,
            full: false,
        };
        self.last_snapshot = after;
        damage
    }

    pub fn resize(&mut self, columns: usize, rows: usize) -> Damage {
        self.columns = columns;
        self.rows = rows;
        self.term.resize(GridSize { columns, rows });
        self.last_snapshot = snapshot_term(&self.term, columns, rows);
        Damage {
            rows: (0..rows).collect(),
            cursor_changed: true,
            full: true,
        }
    }

    pub fn snapshot(&self) -> TerminalSnapshot {
        self.last_snapshot.clone()
    }

    pub fn input_mode(&self) -> TerminalModes {
        let mode = self.term.mode();
        TerminalModes {
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            mouse_reporting: mode.intersects(TermMode::MOUSE_MODE),
            sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.renderable_content().display_offset
    }

    pub fn scroll_lines(&mut self, lines: i32) -> bool {
        if lines == 0 {
            return false;
        }
        let before = self.display_offset();
        self.term.scroll_display(Scroll::Delta(lines));
        self.refresh_snapshot();
        self.display_offset() != before
    }

    pub fn scroll_to_bottom(&mut self) -> bool {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::Bottom);
        self.refresh_snapshot();
        before != 0
    }

    pub fn text_between(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        let mut selected = Vec::new();
        for line in start.0..=end.0.min(self.last_snapshot.rows.len().saturating_sub(1)) {
            let row = row_cells(&self.last_snapshot.rows[line], self.columns);
            let from = if line == start.0 { start.1 } else { 0 };
            let to = if line == end.0 {
                end.1.saturating_add(1)
            } else {
                self.columns
            };
            selected.push(
                row[from.min(self.columns)..to.min(self.columns)]
                    .iter()
                    .flatten()
                    .cloned()
                    .collect::<String>()
                    .trim_end()
                    .to_owned(),
            );
        }
        selected.join("\n")
    }

    fn refresh_snapshot(&mut self) {
        self.last_snapshot = snapshot_term(&self.term, self.columns, self.rows);
    }
}

fn row_cells(row: &TerminalRow, columns: usize) -> Vec<Option<String>> {
    let mut cells = vec![Some(" ".to_owned()); columns];
    for run in &row.runs {
        let mut column = run.start_column;
        let mut last_leading_cell: Option<usize> = None;
        for character in run.text.chars() {
            let width = character.width().unwrap_or(0);
            if width == 0 {
                if let Some(Some(cell)) = last_leading_cell.and_then(|index| cells.get_mut(index)) {
                    cell.push(character);
                }
                continue;
            }
            if column >= columns {
                break;
            }
            cells[column] = Some(character.to_string());
            last_leading_cell = Some(column);
            for spacer in 1..width {
                if column + spacer < columns {
                    cells[column + spacer] = None;
                }
            }
            column += width;
        }
    }
    cells
}

fn snapshot_term(term: &Term<VoidListener>, columns: usize, row_count: usize) -> TerminalSnapshot {
    let content = term.renderable_content();
    let display_offset = content.display_offset as i32;
    let cursor_line = (content.cursor.point.line.0 + display_offset).max(0) as usize;
    let cursor = TerminalCursor {
        line: cursor_line.min(row_count.saturating_sub(1)),
        column: content.cursor.point.column.0.min(columns.saturating_sub(1)),
        shape: content.cursor.shape,
    };
    let mut cells = vec![vec![None; columns]; row_count];
    for indexed in content.display_iter {
        let line = indexed.point.line.0 + display_offset;
        if line < 0 || line as usize >= row_count {
            continue;
        }
        let column = indexed.point.column.0;
        if column >= columns || indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let mut text = String::new();
        text.push(if indexed.cell.c == '\0' {
            ' '
        } else {
            indexed.cell.c
        });
        if let Some(zerowidth) = indexed.cell.zerowidth() {
            text.extend(zerowidth);
        }
        let cell_width = if indexed.cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let mut paint_flags = indexed.cell.flags;
        paint_flags
            .remove(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        cells[line as usize][column] = Some((
            text,
            cell_width,
            TerminalStyle {
                foreground: indexed.cell.fg,
                background: indexed.cell.bg,
                flags: paint_flags,
            },
        ));
    }

    let rows = cells
        .into_iter()
        .enumerate()
        .map(|(line, cells)| TerminalRow {
            line,
            runs: batch_row(cells),
        })
        .collect();
    TerminalSnapshot {
        columns,
        rows,
        cursor,
    }
}

fn batch_row(cells: Vec<Option<(String, usize, TerminalStyle)>>) -> Vec<TerminalRun> {
    let last_meaningful = cells.iter().rposition(|cell| {
        cell.as_ref().is_some_and(|(text, _, style)| {
            text != " "
                || style.background != Color::Named(ansi::NamedColor::Background)
                || !style.flags.is_empty()
        })
    });
    let Some(last_meaningful) = last_meaningful else {
        return Vec::new();
    };

    let mut runs: Vec<TerminalRun> = Vec::new();
    for (column, cell) in cells.into_iter().take(last_meaningful + 1).enumerate() {
        let Some((text, cell_width, style)) = cell else {
            continue;
        };
        if let Some(run) = runs.last_mut()
            && run.style == style
        {
            run.text.push_str(&text);
            run.cell_width += cell_width;
            continue;
        }
        runs.push(TerminalRun {
            start_column: column,
            cell_width,
            text,
            style,
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::vte::ansi::{Color, Rgb};

    use super::TerminalSurface;

    #[test]
    fn parses_true_color_and_batches_adjacent_cells_with_the_same_style() {
        let mut terminal = TerminalSurface::new(20, 3);

        terminal.apply_output(b"\x1b[38;2;12;34;56mhello\x1b[0m world");
        let snapshot = terminal.snapshot();

        assert_eq!(snapshot.rows[0].runs.len(), 2);
        assert_eq!(snapshot.rows[0].runs[0].text, "hello");
        assert_eq!(
            snapshot.rows[0].runs[0].style.foreground,
            Color::Spec(Rgb {
                r: 12,
                g: 34,
                b: 56
            })
        );
        assert_eq!(snapshot.rows[0].runs[1].text, " world");
    }

    #[test]
    fn keeps_wide_and_combining_text_without_painting_spacer_cells() {
        let mut terminal = TerminalSurface::new(10, 2);

        terminal.apply_output("界e\u{301}".as_bytes());
        let snapshot = terminal.snapshot();

        assert_eq!(snapshot.rows[0].runs[0].text, "界e\u{301}");
        assert_eq!(snapshot.cursor.column, 3);
    }

    #[test]
    fn reports_changed_rows_and_cursor_motion_after_output() {
        let mut terminal = TerminalSurface::new(10, 3);

        let damage = terminal.apply_output(b"one\r\ntwo");

        assert_eq!(damage.rows.into_iter().collect::<Vec<_>>(), vec![0, 1]);
        assert!(damage.cursor_changed);
        assert!(!damage.full);
    }

    #[test]
    fn resize_is_an_explicit_full_redraw_boundary() {
        let mut terminal = TerminalSurface::new(10, 3);

        let damage = terminal.resize(20, 4);
        let snapshot = terminal.snapshot();

        assert!(damage.full);
        assert_eq!(
            damage.rows.into_iter().collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(snapshot.columns, 20);
        assert_eq!(snapshot.rows.len(), 4);
    }

    #[test]
    fn exposes_terminal_modes_and_bounded_scrollback() {
        let mut terminal = TerminalSurface::new(8, 2);
        terminal.apply_output(b"one\r\ntwo\r\nthree\r\nfour");
        terminal.apply_output(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h");

        assert!(terminal.input_mode().application_cursor);
        assert!(terminal.input_mode().bracketed_paste);
        assert!(terminal.input_mode().mouse_reporting);
        assert!(terminal.input_mode().sgr_mouse);
        assert_eq!(terminal.display_offset(), 0);
        assert!(terminal.scroll_lines(1));
        assert_eq!(terminal.display_offset(), 1);
        assert!(terminal.scroll_to_bottom());
        assert_eq!(terminal.display_offset(), 0);
        assert!(!terminal.scroll_to_bottom());
    }

    #[test]
    fn extracts_forward_reverse_and_multiline_cell_selection() {
        let mut terminal = TerminalSurface::new(8, 3);
        terminal.apply_output(b"one two\r\nthree\r\nlast");

        assert_eq!(terminal.text_between((0, 4), (0, 6)), "two");
        assert_eq!(terminal.text_between((1, 4), (0, 4)), "two\nthree");
    }

    #[test]
    fn selection_does_not_copy_wide_character_spacer_cells() {
        let mut terminal = TerminalSurface::new(8, 2);
        terminal.apply_output("界ab".as_bytes());

        assert_eq!(terminal.text_between((0, 0), (0, 2)), "界a");
    }
}
