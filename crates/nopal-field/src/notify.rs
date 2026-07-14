//! Parser for tmux control-mode (`tmux -C`) notification lines.
//!
//! The sidecar attaches with `-f no-output` and installs `-B` format
//! subscriptions, so the stream carries state notifications and command
//! replies, never a pane-output firehose. This module is pure: it turns
//! lines into typed [`Notification`]s and accumulates `%begin`/`%end`
//! command reply blocks. Verified empirically against tmux 3.6a.

/// One parsed control-mode notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    /// `%subscription-changed name $sid @wid widx %pid : value`
    SubscriptionChanged {
        name: String,
        pane_id: Option<String>,
        window_id: Option<String>,
        value: String,
    },
    /// `%window-add @id`
    WindowAdd { window_id: String },
    /// `%window-close @id` or `%unlinked-window-close @id`
    WindowClose { window_id: String },
    /// `%window-renamed @id new-name`
    WindowRenamed { window_id: String, name: String },
    /// `%window-pane-changed @id %id`
    WindowPaneChanged { window_id: String, pane_id: String },
    /// `%layout-change @id ...`
    LayoutChange { window_id: String },
    /// `%session-changed $id name`
    SessionChanged { session_id: String, name: String },
    /// `%sessions-changed`, `%session-closed`, `%session-renamed`:
    /// server-wide session topology changed; triggers a reconcile.
    SessionsChanged,
    /// A complete `%begin`..`%end`/`%error` command reply.
    CommandReply {
        num: u64,
        success: bool,
        output: Vec<String>,
    },
    /// `%exit [reason]` - the control client is going away.
    Exit { reason: String },
    /// Any other `%` notification we do not act on.
    Other { line: String },
}

/// Stateful line parser: buffers `%begin` blocks, passes the rest through.
#[derive(Debug, Default)]
pub struct Parser {
    block: Option<Block>,
}

#[derive(Debug)]
struct Block {
    num: u64,
    output: Vec<String>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one line (without trailing newline); returns a notification when
    /// one completes. Lines inside a `%begin` block buffer until `%end` or
    /// `%error`.
    pub fn feed(&mut self, line: &str) -> Option<Notification> {
        if let Some(block) = &mut self.block {
            if let Some(rest) = line.strip_prefix("%end ") {
                let num = second_field(rest);
                let block = self.block.take()?;
                return Some(Notification::CommandReply {
                    num: num.unwrap_or(block.num),
                    success: true,
                    output: block.output,
                });
            }
            if let Some(rest) = line.strip_prefix("%error ") {
                let num = second_field(rest);
                let block = self.block.take()?;
                return Some(Notification::CommandReply {
                    num: num.unwrap_or(block.num),
                    success: false,
                    output: block.output,
                });
            }
            block.output.push(line.to_owned());
            return None;
        }

        if let Some(rest) = line.strip_prefix("%begin ") {
            self.block = Some(Block {
                num: second_field(rest).unwrap_or(0),
                output: Vec::new(),
            });
            return None;
        }

        parse_notification(line)
    }
}

/// `%begin/%end/%error` arguments are `time num flags`; we key replies on num.
fn second_field(rest: &str) -> Option<u64> {
    rest.split_whitespace().nth(1)?.parse().ok()
}

fn parse_notification(line: &str) -> Option<Notification> {
    if !line.starts_with('%') {
        return None;
    }
    let (tag, rest) = match line.split_once(' ') {
        Some((tag, rest)) => (tag, rest),
        None => (line, ""),
    };
    let notification = match tag {
        "%subscription-changed" => parse_subscription_changed(rest)?,
        "%window-add" | "%unlinked-window-add" => Notification::WindowAdd {
            window_id: first_token(rest).to_owned(),
        },
        "%window-close" | "%unlinked-window-close" => Notification::WindowClose {
            window_id: first_token(rest).to_owned(),
        },
        "%window-renamed" | "%unlinked-window-renamed" => {
            let (window_id, name) = rest.split_once(' ')?;
            Notification::WindowRenamed {
                window_id: window_id.to_owned(),
                name: name.to_owned(),
            }
        }
        "%window-pane-changed" => {
            let (window_id, pane_id) = rest.split_once(' ')?;
            Notification::WindowPaneChanged {
                window_id: window_id.to_owned(),
                pane_id: pane_id.trim().to_owned(),
            }
        }
        "%layout-change" => Notification::LayoutChange {
            window_id: first_token(rest).to_owned(),
        },
        "%session-changed" => {
            let (session_id, name) = rest.split_once(' ')?;
            Notification::SessionChanged {
                session_id: session_id.to_owned(),
                name: name.to_owned(),
            }
        }
        "%sessions-changed" | "%session-closed" | "%session-renamed" => {
            Notification::SessionsChanged
        }
        "%exit" => Notification::Exit {
            reason: rest.trim().to_owned(),
        },
        _ => Notification::Other {
            line: line.to_owned(),
        },
    };
    Some(notification)
}

/// `%subscription-changed sidebar $3 @6 2 %6 : %6|@6|seat-1|sleep|alpha|0`
/// The fields between the name and ` : ` vary with subscription scope
/// (session, window, pane); we pull out whichever ids are present.
fn parse_subscription_changed(rest: &str) -> Option<Notification> {
    let (head, value) = match rest.split_once(" : ") {
        Some((head, value)) => (head, value),
        // An empty value renders as a trailing " : " with nothing after it.
        None => (rest.strip_suffix(" :")?, ""),
    };
    let mut fields = head.split_whitespace();
    let name = fields.next()?.to_owned();
    let mut pane_id = None;
    let mut window_id = None;
    for field in fields {
        if field.starts_with('%') {
            pane_id = Some(field.to_owned());
        } else if field.starts_with('@') {
            window_id = Some(field.to_owned());
        }
    }
    Some(Notification::SubscriptionChanged {
        name,
        pane_id,
        window_id,
        value: value.to_owned(),
    })
}

fn first_token(rest: &str) -> &str {
    rest.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(lines: &[&str]) -> Vec<Notification> {
        let mut parser = Parser::new();
        lines.iter().filter_map(|l| parser.feed(l)).collect()
    }

    #[test]
    fn subscription_changed_pane_scope() {
        let got =
            parse_all(&["%subscription-changed sidebar $3 @6 2 %6 : %6|@6|seat-1|sleep|alpha|0"]);
        assert_eq!(
            got,
            vec![Notification::SubscriptionChanged {
                name: "sidebar".to_owned(),
                pane_id: Some("%6".to_owned()),
                window_id: Some("@6".to_owned()),
                value: "%6|@6|seat-1|sleep|alpha|0".to_owned(),
            }]
        );
    }

    #[test]
    fn subscription_changed_value_may_contain_colons() {
        let got = parse_all(&["%subscription-changed s $1 @2 1 %3 : a : b"]);
        match &got[0] {
            Notification::SubscriptionChanged { value, .. } => assert_eq!(value, "a : b"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn subscription_changed_empty_value() {
        let got = parse_all(&["%subscription-changed s $1 @2 1 %3 :"]);
        match &got[0] {
            Notification::SubscriptionChanged { value, .. } => assert_eq!(value, ""),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn begin_end_block_buffers_output() {
        let got = parse_all(&[
            "%begin 1783381309 1156 1",
            "%5 field",
            "%6 seat-1",
            "%end 1783381309 1156 1",
        ]);
        assert_eq!(
            got,
            vec![Notification::CommandReply {
                num: 1156,
                success: true,
                output: vec!["%5 field".to_owned(), "%6 seat-1".to_owned()],
            }]
        );
    }

    #[test]
    fn begin_error_block_marks_failure() {
        let got = parse_all(&["%begin 1 7 1", "bad command", "%error 1 7 1"]);
        assert_eq!(
            got,
            vec![Notification::CommandReply {
                num: 7,
                success: false,
                output: vec!["bad command".to_owned()],
            }]
        );
    }

    #[test]
    fn notifications_inside_block_are_buffered_not_lost() {
        // tmux serializes replies; anything between begin/end belongs to the
        // reply body even if it looks like a notification.
        let got = parse_all(&["%begin 1 2 1", "%window-add @9", "%end 1 2 1"]);
        assert_eq!(
            got,
            vec![Notification::CommandReply {
                num: 2,
                success: true,
                output: vec!["%window-add @9".to_owned()],
            }]
        );
    }

    #[test]
    fn window_lifecycle() {
        let got = parse_all(&[
            "%window-add @7",
            "%window-renamed @7 seat:alpha",
            "%window-pane-changed @7 %9",
            "%layout-change @7 b25d,208x59,0,0,7",
            "%window-close @7",
            "%unlinked-window-close @8",
        ]);
        assert_eq!(
            got,
            vec![
                Notification::WindowAdd {
                    window_id: "@7".to_owned()
                },
                Notification::WindowRenamed {
                    window_id: "@7".to_owned(),
                    name: "seat:alpha".to_owned()
                },
                Notification::WindowPaneChanged {
                    window_id: "@7".to_owned(),
                    pane_id: "%9".to_owned()
                },
                Notification::LayoutChange {
                    window_id: "@7".to_owned()
                },
                Notification::WindowClose {
                    window_id: "@7".to_owned()
                },
                Notification::WindowClose {
                    window_id: "@8".to_owned()
                },
            ]
        );
    }

    #[test]
    fn session_changed_and_exit() {
        let got = parse_all(&["%session-changed $3 nopal", "%exit"]);
        assert_eq!(
            got,
            vec![
                Notification::SessionChanged {
                    session_id: "$3".to_owned(),
                    name: "nopal".to_owned()
                },
                Notification::Exit {
                    reason: String::new()
                },
            ]
        );
    }

    #[test]
    fn unknown_percent_lines_are_other_and_plain_lines_are_dropped() {
        let mut parser = Parser::new();
        assert_eq!(
            parser.feed("%client-detached ttys001"),
            Some(Notification::Other {
                line: "%client-detached ttys001".to_owned()
            })
        );
        assert_eq!(parser.feed("stray text"), None);
    }
}
