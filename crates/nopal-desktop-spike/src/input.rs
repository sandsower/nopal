/// Renderer-neutral key modifiers used by the Terminal escape hatch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub platform: bool,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// Renderer-neutral keystroke normalized before Terminal encoding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keystroke {
    pub key: String,
    pub key_char: Option<String>,
    pub modifiers: Modifiers,
}

impl Keystroke {
    #[cfg(test)]
    pub fn parse(specification: &str) -> Result<Self, String> {
        let (binding, key_char) = specification
            .split_once("->")
            .map_or((specification, None), |(binding, key_char)| {
                (binding, Some(key_char.to_owned()))
            });
        let mut parts = binding.split('-').collect::<Vec<_>>();
        let key = parts
            .pop()
            .filter(|key| !key.is_empty())
            .ok_or_else(|| "keystroke requires a key".to_owned())?;
        let mut modifiers = Modifiers::default();
        for modifier in parts {
            match modifier {
                "cmd" | "super" => modifiers.platform = true,
                "shift" => modifiers.shift = true,
                "alt" => modifiers.alt = true,
                "ctrl" | "control" => modifiers.control = true,
                unknown => return Err(format!("unknown modifier {unknown}")),
            }
        }
        Ok(Self {
            key: key.to_owned(),
            key_char: key_char.or_else(|| (key.chars().count() == 1).then(|| key.to_owned())),
            modifiers,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalInputMode {
    pub application_cursor: bool,
}

#[cfg(test)]
pub fn encode_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
    encode_keystroke_for_mode(keystroke, TerminalInputMode::default())
}

pub fn encode_keystroke_for_mode(
    keystroke: &Keystroke,
    mode: TerminalInputMode,
) -> Option<Vec<u8>> {
    if keystroke.modifiers.platform {
        return None;
    }

    if keystroke.key == "tab" && keystroke.modifiers.shift {
        return Some(b"\x1b[Z".to_vec());
    }

    let modifier = 1
        + usize::from(keystroke.modifiers.shift)
        + usize::from(keystroke.modifiers.alt) * 2
        + usize::from(keystroke.modifiers.control) * 4;
    if modifier > 1 {
        let final_byte = match keystroke.key.as_str() {
            "up" => Some('A'),
            "down" => Some('B'),
            "right" => Some('C'),
            "left" => Some('D'),
            "home" => Some('H'),
            "end" => Some('F'),
            _ => None,
        };
        if let Some(final_byte) = final_byte {
            return Some(format!("\x1b[1;{modifier}{final_byte}").into_bytes());
        }
        let number = match keystroke.key.as_str() {
            "insert" => Some(2),
            "delete" => Some(3),
            "pageup" => Some(5),
            "pagedown" => Some(6),
            "f5" => Some(15),
            "f6" => Some(17),
            "f7" => Some(18),
            "f8" => Some(19),
            "f9" => Some(20),
            "f10" => Some(21),
            "f11" => Some(23),
            "f12" => Some(24),
            _ => None,
        };
        if let Some(number) = number {
            return Some(format!("\x1b[{number};{modifier}~").into_bytes());
        }
        let function_final = match keystroke.key.as_str() {
            "f1" => Some('P'),
            "f2" => Some('Q'),
            "f3" => Some('R'),
            "f4" => Some('S'),
            _ => None,
        };
        if let Some(final_byte) = function_final {
            return Some(format!("\x1b[1;{modifier}{final_byte}").into_bytes());
        }
    }

    if keystroke.modifiers.control {
        let control = match keystroke.key.as_str() {
            "space" | "@" => Some(0),
            "[" => Some(0x1b),
            "\\" => Some(0x1c),
            "]" => Some(0x1d),
            "^" => Some(0x1e),
            "_" => Some(0x1f),
            "?" => Some(0x7f),
            key if key.len() == 1 && key.is_ascii() => {
                Some(key.as_bytes()[0].to_ascii_uppercase() & 0x1f)
            }
            _ => None,
        }?;
        let mut bytes = Vec::with_capacity(2);
        if keystroke.modifiers.alt {
            bytes.push(0x1b);
        }
        bytes.push(control);
        return Some(bytes);
    }

    let named = match keystroke.key.as_str() {
        "enter" => Some(b"\r".to_vec()),
        "backspace" => Some(vec![0x7f]),
        "tab" => Some(b"\t".to_vec()),
        "escape" => Some(vec![0x1b]),
        "up" => Some(
            if mode.application_cursor {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
            .to_vec(),
        ),
        "down" => Some(
            if mode.application_cursor {
                b"\x1bOB"
            } else {
                b"\x1b[B"
            }
            .to_vec(),
        ),
        "right" => Some(
            if mode.application_cursor {
                b"\x1bOC"
            } else {
                b"\x1b[C"
            }
            .to_vec(),
        ),
        "left" => Some(
            if mode.application_cursor {
                b"\x1bOD"
            } else {
                b"\x1b[D"
            }
            .to_vec(),
        ),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "insert" => Some(b"\x1b[2~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if named.is_some() {
        return named;
    }

    let mut bytes = Vec::new();
    if keystroke.modifiers.alt {
        bytes.push(0x1b);
    }
    bytes.extend(keystroke.key_char.as_ref()?.as_bytes());
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::{Keystroke, TerminalInputMode, encode_keystroke, encode_keystroke_for_mode};

    #[test]
    fn encodes_printable_unicode_and_terminal_navigation() {
        assert_eq!(
            encode_keystroke(&Keystroke::parse("a->å").expect("keystroke")),
            Some("å".as_bytes().to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("up").expect("keystroke")),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn encodes_control_and_alt_without_intercepting_platform_shortcuts() {
        assert_eq!(
            encode_keystroke(&Keystroke::parse("ctrl-c").expect("keystroke")),
            Some(vec![0x03])
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("alt-x->x").expect("keystroke")),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("cmd-c").expect("keystroke")),
            None
        );
    }

    #[test]
    fn encodes_full_screen_navigation_and_function_keys() {
        let application = TerminalInputMode {
            application_cursor: true,
        };
        assert_eq!(
            encode_keystroke_for_mode(&Keystroke::parse("up").expect("key"), application),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("shift-tab").expect("key")),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("insert").expect("key")),
            Some(b"\x1b[2~".to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("f12").expect("key")),
            Some(b"\x1b[24~".to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("ctrl-up").expect("key")),
            Some(b"\x1b[1;5A".to_vec())
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("ctrl-space").expect("key")),
            Some(vec![0])
        );
        assert_eq!(
            encode_keystroke(&Keystroke::parse("ctrl-[").expect("key")),
            Some(vec![0x1b])
        );
    }
}
