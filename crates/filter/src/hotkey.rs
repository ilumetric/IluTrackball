//! Global hotkeys, read by polling the key state.
//!
//! Polling needs no hook, no window and no message loop, so it keeps working
//! when every other thread of the program is stuck.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CANCEL, VK_CONTROL, VK_F1, VK_MENU, VK_PAUSE, VK_SHIFT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: u16,
}

impl Hotkey {
    pub fn is_down(&self) -> bool {
        (!self.ctrl || key_down(VK_CONTROL))
            && (!self.alt || key_down(VK_MENU))
            && (!self.shift || key_down(VK_SHIFT))
            && key_down(self.key)
    }
}

fn key_down(key: u16) -> bool {
    (unsafe { GetAsyncKeyState(key as i32) } as u16) & 0x8000 != 0
}

/// `Ctrl+Alt+Pause`. Windows reports Pause as Cancel while Ctrl is held.
pub fn emergency_is_down() -> bool {
    key_down(VK_CONTROL) && key_down(VK_MENU) && (key_down(VK_PAUSE) || key_down(VK_CANCEL))
}

/// Parses text such as `Ctrl+Alt+F11`.
pub fn parse_hotkey(text: &str) -> Result<Hotkey, String> {
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        key: 0,
    };
    for part in text.split('+') {
        let part = part.trim().to_ascii_uppercase();
        match part.as_str() {
            "CTRL" => hotkey.ctrl = true,
            "ALT" => hotkey.alt = true,
            "SHIFT" => hotkey.shift = true,
            name => {
                if hotkey.key != 0 {
                    return Err(format!("в сочетании две клавиши: `{text}`"));
                }
                hotkey.key = key_code(name).ok_or_else(|| format!("неизвестная клавиша `{part}`"))?;
            }
        }
    }
    if hotkey.key == 0 {
        return Err(format!("в сочетании нет клавиши: `{text}`"));
    }
    Ok(hotkey)
}

fn key_code(name: &str) -> Option<u16> {
    if name == "PAUSE" {
        return Some(VK_PAUSE);
    }
    if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&number).then_some(VK_F1 + number - 1);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() || c.is_ascii_digit() => Some(c as u16),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_default_hotkey() {
        let hotkey = parse_hotkey("Ctrl+Alt+F11").unwrap();
        assert!(hotkey.ctrl && hotkey.alt && !hotkey.shift);
        assert_eq!(hotkey.key, VK_F1 + 10);
    }

    #[test]
    fn parses_letters_digits_and_spaces() {
        assert_eq!(parse_hotkey("shift + a").unwrap().key, b'A' as u16);
        assert_eq!(parse_hotkey("Ctrl+7").unwrap().key, b'7' as u16);
    }

    #[test]
    fn rejects_bad_hotkeys_with_a_clear_message() {
        assert!(parse_hotkey("Ctrl+Alt").unwrap_err().contains("нет клавиши"));
        assert!(parse_hotkey("Ctrl+Banana").unwrap_err().contains("неизвестная"));
        assert!(parse_hotkey("A+B").unwrap_err().contains("две клавиши"));
        assert!(parse_hotkey("F25").unwrap_err().contains("неизвестная"));
    }
}
