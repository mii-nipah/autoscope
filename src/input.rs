use smithay::{
    backend::input::{InputTime, KeyState},
    input::keyboard::{FilterResult, Keycode},
    utils::SERIAL_COUNTER,
};

use crate::state::Autoscope;

impl Autoscope {
    pub(crate) fn type_text(&mut self, text: &str) -> Result<(), String> {
        let keys: Vec<_> = text
            .chars()
            .map(char_key)
            .collect::<Option<_>>()
            .ok_or_else(|| "type supports the printable US-ASCII keymap".to_string())?;
        for (code, shifted) in keys {
            if shifted {
                self.send_key(42, KeyState::Pressed);
            }
            self.send_key(code, KeyState::Pressed);
            self.send_key(code, KeyState::Released);
            if shifted {
                self.send_key(42, KeyState::Released);
            }
        }
        Ok(())
    }

    pub(crate) fn key_combo(&mut self, combo: &str) -> Result<(), String> {
        let parts: Vec<_> = combo
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        let (key_name, modifiers) = parts
            .split_last()
            .ok_or_else(|| "empty key combo".to_string())?;
        let mut held = Vec::new();
        for modifier in modifiers {
            held.push(
                named_key(modifier)
                    .filter(|code| matches!(code, 29 | 42 | 56 | 125))
                    .ok_or_else(|| format!("unknown modifier {modifier:?}"))?,
            );
        }
        let (key, implied_shift) = if key_name.chars().count() == 1 {
            combo_char(key_name.chars().next().unwrap(), !modifiers.is_empty()).unwrap()
        } else {
            (
                named_key(key_name).ok_or_else(|| format!("unknown key {key_name:?}"))?,
                false,
            )
        };
        if implied_shift && !held.contains(&42) {
            held.push(42);
        }
        for code in &held {
            self.send_key(*code, KeyState::Pressed);
        }
        self.send_key(key, KeyState::Pressed);
        self.send_key(key, KeyState::Released);
        for code in held.into_iter().rev() {
            self.send_key(code, KeyState::Released);
        }
        Ok(())
    }

    fn send_key(&mut self, evdev_code: u32, state: KeyState) {
        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard.input::<(), _>(
            self,
            Keycode::new(evdev_code + 8),
            state,
            SERIAL_COUNTER.next_serial(),
            InputTime::now(),
            |_, _, _| FilterResult::Forward,
        );
    }
}

fn combo_char(character: char, has_modifiers: bool) -> Option<(u32, bool)> {
    char_key(if has_modifiers && character.is_ascii_alphabetic() {
        character.to_ascii_lowercase()
    } else {
        character
    })
}

fn named_key(name: &str) -> Option<u32> {
    Some(match name.to_ascii_uppercase().as_str() {
        "ESC" | "ESCAPE" => 1,
        "BACKSPACE" => 14,
        "TAB" => 15,
        "ENTER" | "RETURN" => 28,
        "CTRL" | "CONTROL" => 29,
        "SHIFT" => 42,
        "ALT" => 56,
        "SPACE" => 57,
        "HOME" => 102,
        "UP" => 103,
        "PAGEUP" => 104,
        "LEFT" => 105,
        "RIGHT" => 106,
        "END" => 107,
        "DOWN" => 108,
        "PAGEDOWN" => 109,
        "DELETE" => 111,
        "META" | "SUPER" => 125,
        "F1" => 59,
        "F2" => 60,
        "F3" => 61,
        "F4" => 62,
        "F5" => 63,
        "F6" => 64,
        "F7" => 65,
        "F8" => 66,
        "F9" => 67,
        "F10" => 68,
        "F11" => 87,
        "F12" => 88,
        _ => return None,
    })
}

fn char_key(character: char) -> Option<(u32, bool)> {
    let letters = "qwertyuiopasdfghjklzxcvbnm";
    let codes = [
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47,
        48, 49, 50,
    ];
    let lower = character.to_ascii_lowercase();
    if let Some(index) = letters.find(lower) {
        return Some((codes[index], character.is_ascii_uppercase()));
    }
    let (code, shifted) = match character {
        '1' | '!' => (2, character == '!'),
        '2' | '@' => (3, character == '@'),
        '3' | '#' => (4, character == '#'),
        '4' | '$' => (5, character == '$'),
        '5' | '%' => (6, character == '%'),
        '6' | '^' => (7, character == '^'),
        '7' | '&' => (8, character == '&'),
        '8' | '*' => (9, character == '*'),
        '9' | '(' => (10, character == '('),
        '0' | ')' => (11, character == ')'),
        '-' | '_' => (12, character == '_'),
        '=' | '+' => (13, character == '+'),
        '[' | '{' => (26, character == '{'),
        ']' | '}' => (27, character == '}'),
        ';' | ':' => (39, character == ':'),
        '\'' | '"' => (40, character == '"'),
        '`' | '~' => (41, character == '~'),
        '\\' | '|' => (43, character == '|'),
        ',' | '<' => (51, character == '<'),
        '.' | '>' => (52, character == '>'),
        '/' | '?' => (53, character == '?'),
        ' ' => (57, false),
        '\n' => (28, false),
        '\t' => (15, false),
        _ => return None,
    };
    Some((code, shifted))
}

#[cfg(test)]
mod tests {
    use super::{char_key, combo_char, named_key};

    #[test]
    fn text_mapping_preserves_case_and_symbols() {
        assert_eq!(char_key('a'), Some((30, false)));
        assert_eq!(char_key('A'), Some((30, true)));
        assert_eq!(char_key('?'), Some((53, true)));
        assert_eq!(char_key('é'), None);
        assert_eq!(named_key("ctrl"), Some(29));
        assert_eq!(combo_char('A', true), char_key('a'));
        assert_eq!(combo_char('A', false), char_key('A'));
    }
}
