use crate::terminal::TerminalSnapshot;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProjection {
    pub label: &'static str,
    pub text: String,
}

impl OutputProjection {
    pub fn from_snapshot(snapshot: &TerminalSnapshot) -> Self {
        let lines = snapshot
            .rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                let mut column = 0usize;
                for run in &row.runs {
                    if run.start_column > column {
                        line.push_str(&" ".repeat(run.start_column - column));
                    }
                    for character in run
                        .text
                        .chars()
                        .filter(|character| is_output_character(*character))
                    {
                        line.push(character);
                    }
                    column = run.start_column + run.cell_width;
                }
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>();
        let first = lines.iter().position(|line| !line.is_empty());
        let last = lines.iter().rposition(|line| !line.is_empty());
        let text = match (first, last) {
            (Some(first), Some(last)) => lines[first..=last].join("\n"),
            _ => String::new(),
        };
        Self {
            label: "Live output projection",
            text,
        }
    }
}

fn is_output_character(character: char) -> bool {
    !character.is_control() && !is_private_use(character)
}

fn is_private_use(character: char) -> bool {
    matches!(
        character as u32,
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    use crate::terminal::{
        TerminalCursor, TerminalRow, TerminalRun, TerminalSnapshot, TerminalStyle,
    };

    use super::OutputProjection;

    fn style(foreground: Color, flags: Flags) -> TerminalStyle {
        TerminalStyle {
            foreground,
            background: Color::Named(NamedColor::Background),
            flags,
        }
    }

    #[test]
    fn projection_preserves_layout_but_ignores_terminal_style() {
        let snapshot = TerminalSnapshot {
            columns: 20,
            rows: vec![TerminalRow {
                line: 0,
                runs: vec![
                    TerminalRun {
                        start_column: 0,
                        cell_width: 3,
                        text: "red".to_owned(),
                        style: style(Color::Named(NamedColor::Red), Flags::BOLD),
                    },
                    TerminalRun {
                        start_column: 5,
                        cell_width: 4,
                        text: "plain".to_owned(),
                        style: style(Color::Named(NamedColor::Green), Flags::ITALIC),
                    },
                ],
            }],
            cursor: TerminalCursor {
                line: 0,
                column: 9,
                shape: alacritty_terminal::vte::ansi::CursorShape::Block,
            },
        };

        assert_eq!(
            OutputProjection::from_snapshot(&snapshot),
            OutputProjection {
                label: "Live output projection",
                text: "red  plain".to_owned(),
            }
        );
    }

    #[test]
    fn projection_filters_prompt_glyphs_controls_and_empty_edges() {
        let snapshot = TerminalSnapshot {
            columns: 20,
            rows: vec![
                TerminalRow {
                    line: 0,
                    runs: vec![],
                },
                TerminalRow {
                    line: 1,
                    runs: vec![TerminalRun {
                        start_column: 0,
                        cell_width: 9,
                        text: "\u{e0b0} hello\u{7}".to_owned(),
                        style: style(Color::Named(NamedColor::Foreground), Flags::empty()),
                    }],
                },
                TerminalRow {
                    line: 2,
                    runs: vec![],
                },
            ],
            cursor: TerminalCursor {
                line: 1,
                column: 8,
                shape: alacritty_terminal::vte::ansi::CursorShape::Hidden,
            },
        };

        let projection = OutputProjection::from_snapshot(&snapshot);
        assert_eq!(projection.text, " hello");
    }
}
