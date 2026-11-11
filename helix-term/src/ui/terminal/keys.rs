//! Keys as a terminal sends them to the program running in it.

use helix_view::{
    input::KeyEvent,
    keyboard::{KeyCode, KeyModifiers},
};

/// The bytes xterm sends for `key`, with cursor keys as `ESC O A` if `app_cursor`.
pub fn encode(key: KeyEvent, app_cursor: bool) -> Vec<u8> {
    let KeyEvent { code, modifiers } = key;
    let alt = modifiers.contains(KeyModifiers::ALT);
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    // The modifier parameter of `ESC [ 1 ; 5 A`: 1 plus shift 1, alt 2, ctrl 4 and super 8.
    let parameter = 1 + [
        KeyModifiers::SHIFT,
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
        KeyModifiers::SUPER,
    ]
    .iter()
    .enumerate()
    .filter(|(_, modifier)| modifiers.contains(**modifier))
    .map(|(bit, _)| 1 << bit)
    .sum::<u8>();
    let csi = |number: &str, last: char| {
        if parameter > 1 {
            format!("\x1b[{};{parameter}{last}", number.max("1"))
        } else {
            format!("\x1b[{number}{last}")
        }
    };
    let cursor = |last: char| {
        if parameter == 1 && app_cursor {
            format!("\x1bO{last}")
        } else {
            csi("", last)
        }
    };
    let prefixed = |bytes: &[u8]| {
        let mut prefixed = if alt { vec![0x1b] } else { Vec::new() };
        prefixed.extend_from_slice(bytes);
        prefixed
    };
    let sequence = match code {
        KeyCode::Char(c) if ctrl => return prefixed(&[control(c).unwrap_or(c as u8)]),
        KeyCode::Char(c) => return prefixed(c.encode_utf8(&mut [0; 4]).as_bytes()),
        KeyCode::Enter => return prefixed(b"\r"),
        KeyCode::Backspace if ctrl => return prefixed(b"\x08"),
        KeyCode::Backspace => return prefixed(b"\x7f"),
        KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => "\x1b[Z".to_owned(),
        KeyCode::Tab => return prefixed(b"\t"),
        KeyCode::Esc => return prefixed(b"\x1b"),
        KeyCode::Null => return vec![0],
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => csi("2", '~'),
        KeyCode::Delete => csi("3", '~'),
        KeyCode::PageUp => csi("5", '~'),
        KeyCode::PageDown => csi("6", '~'),
        KeyCode::F(n @ 1..=4) if parameter == 1 => format!("\x1bO{}", (b'O' + n) as char),
        KeyCode::F(n @ 1..=4) => csi("1", (b'O' + n) as char),
        KeyCode::F(n @ 5..=12) => {
            let number = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            csi(&number.to_string(), '~')
        }
        _ => return Vec::new(),
    };
    sequence.into_bytes()
}

/// The control code Ctrl and `c` send, like `0x03` for `c`.
fn control(c: char) -> Option<u8> {
    Some(match c {
        '@' | ' ' | '2' => 0,
        'a'..='z' | 'A'..='Z' => c.to_ascii_lowercase() as u8 - b'a' + 1,
        '['..='_' => c as u8 - b'@',
        '3'..='7' => c as u8 - b'3' + 0x1b,
        '8' | '?' => 0x7f,
        '/' => 0x1f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(key: &str, app_cursor: bool) -> String {
        let key: KeyEvent = key.parse().unwrap();
        String::from_utf8(encode(key, app_cursor)).unwrap()
    }

    #[test]
    fn keys_are_sent_as_xterm_sends_them() {
        for (key, sent) in [
            ("a", "a"),
            ("A", "A"),
            ("ä", "ä"),
            ("C-c", "\x03"),
            ("C-\\", "\x1c"),
            ("C-space", "\0"),
            ("A-b", "\x1bb"),
            ("A-C-x", "\x1b\x18"),
            ("ret", "\r"),
            ("backspace", "\x7f"),
            ("C-backspace", "\x08"),
            ("tab", "\t"),
            ("S-tab", "\x1b[Z"),
            ("esc", "\x1b"),
            ("up", "\x1b[A"),
            ("C-left", "\x1b[1;5D"),
            ("S-A-right", "\x1b[1;4C"),
            ("home", "\x1b[H"),
            ("del", "\x1b[3~"),
            ("C-pagedown", "\x1b[6;5~"),
            ("F1", "\x1bOP"),
            ("S-F2", "\x1b[1;2Q"),
            ("F5", "\x1b[15~"),
            ("F12", "\x1b[24~"),
        ] {
            assert_eq!(bytes(key, false), sent, "{key}");
        }
        // Programs may ask for cursor keys of their own.
        assert_eq!(bytes("up", true), "\x1bOA");
        assert_eq!(bytes("end", true), "\x1bOF");
        assert_eq!(bytes("C-up", true), "\x1b[1;5A");
    }
}
