//! Synthetic keys for `input.key` and `input.type`: from name or character to keycode.
//!
//! The keymap is the one the keyboard is using (the layouts of the `keyboard.*` settings, the active
//! input source): the inverse map keysym → (keycode, Shift, AltGr) is derived from it, so a
//! text typed under the Italian layout produces the same characters (#195). A character the
//! layout does not have is typed on a temporary key (`with_temporary_key`).

use std::collections::HashMap;

use smithay::input::keyboard::xkb;

/// xkb keycode (evdev + 8) of the left Shift key.
pub const SHIFT_LEFT: u32 = 50;

/// `ISO_Level3_Shift` (AltGr).
const LEVEL3_SHIFT: u32 = 0xfe03;

/// How to produce a keysym: the key, with Shift and/or the key that selects the third level
/// (AltGr) held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
    pub code: u32,
    pub shift: bool,
    /// The keycode of the AltGr key to hold, for the third and fourth levels.
    pub level3: Option<u32>,
}

pub struct KeyLookup {
    by_keysym: HashMap<u32, KeyStroke>,
}

impl KeyLookup {
    /// The default (US) keymap.
    pub fn new() -> Result<Self, String> {
        Self::for_names("", "", "", None, 0)
    }

    /// The keymap of these xkb names, in layout (group) `group`.
    pub fn for_names(
        layout: &str,
        variant: &str,
        model: &str,
        options: Option<String>,
        group: u32,
    ) -> Result<Self, String> {
        let keymap = compile(layout, variant, model, options)?;
        Ok(Self::from_keymap(&keymap, group))
    }

    fn from_keymap(keymap: &xkb::Keymap, group: u32) -> Self {
        let group = group.min(keymap.num_layouts().saturating_sub(1));
        let (min, max) = (keymap.min_keycode().raw(), keymap.max_keycode().raw());
        let level3 = (min..=max).find(|&code| {
            keymap
                .key_get_syms_by_level(xkb::Keycode::new(code), group, 0)
                .iter()
                .any(|sym| sym.raw() == LEVEL3_SHIFT)
        });
        let mut by_keysym = HashMap::new();
        // The base level first, then Shift, then AltGr: the simpler key wins.
        for (level, shift, third) in [
            (0, false, false),
            (1, true, false),
            (2, false, true),
            (3, true, true),
        ] {
            if third && level3.is_none() {
                continue;
            }
            for code in min..=max {
                for sym in keymap.key_get_syms_by_level(xkb::Keycode::new(code), group, level) {
                    by_keysym.entry(sym.raw()).or_insert(KeyStroke {
                        code,
                        shift,
                        level3: level3.filter(|_| third),
                    });
                }
            }
        }
        Self { by_keysym }
    }

    /// The stroke for a keysym name (`Return`, `BackSpace`, `a`) or a character.
    pub fn find(&self, key: &str) -> Option<KeyStroke> {
        let mut chars = key.chars();
        let keysym = match (chars.next(), chars.next()) {
            (Some(ch), None) => xkb::utf32_to_keysym(u32::from(ch)),
            _ => xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS),
        };
        self.by_keysym.get(&keysym.raw()).copied()
    }
}

fn compile(
    layout: &str,
    variant: &str,
    model: &str,
    options: Option<String>,
) -> Result<xkb::Keymap, String> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(&context, "", model, layout, variant, options, 0)
        .ok_or_else(|| "xkb keymap cannot be compiled".to_owned())
}

/// The text of the keymap of these names with one more key, `<SILT>`, that types `ch` in every
/// layout, and its keycode: the temporary key for a character the layout lacks. None for a
/// control character or a keymap whose text is not as expected.
pub fn keymap_with_key_for(
    layout: &str,
    variant: &str,
    model: &str,
    options: Option<String>,
    ch: char,
) -> Option<(String, u32)> {
    if ch.is_control() {
        return None;
    }
    let keymap = compile(layout, variant, model, options).ok()?;
    let groups = keymap.num_layouts().max(1);
    let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
    let keysym = xkb::utf32_to_keysym(u32::from(ch));
    let name = xkb::keysym_get_name(keysym);
    if name.is_empty() || name == "NoSymbol" {
        return None;
    }
    // Keycodes: one past the maximum.
    let at = text.find("maximum = ")?;
    let end = at + text.get(at..)?.find(';')?;
    let maximum: u32 = text
        .get(at + "maximum = ".len()..end)?
        .trim()
        .parse()
        .ok()?;
    let code = maximum.checked_add(1).filter(|c| *c < 0xffff)?;
    let line_end = end + text.get(end..)?.find('\n')?;
    let mut patched = String::with_capacity(text.len() + 160);
    patched.push_str(text.get(..at)?);
    patched.push_str(&format!("maximum = {code};"));
    patched.push_str(text.get(end + 1..line_end)?);
    patched.push_str(&format!("\n\t<SILT> = {code};"));
    let rest = text.get(line_end..)?;
    // The end of the symbols section.
    let symbols = rest.find("xkb_symbols")?;
    let close = symbols + rest.get(symbols..)?.find("\n};")?;
    patched.push_str(rest.get(..close)?);
    let mut key =
        String::from("\n\tkey <SILT> {\n\t\ttype= \"ONE_LEVEL\",\n\t\tsymbols[Group1]= [ ");
    key.push_str(&name);
    key.push_str(" ]");
    for group in 2..=groups {
        key.push_str(&format!(",\n\t\tsymbols[Group{group}]= [ {name} ]"));
    }
    key.push_str("\n\t};");
    patched.push_str(&key);
    patched.push_str(rest.get(close..)?);
    Some((patched, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(code: u32, shift: bool) -> Option<KeyStroke> {
        Some(KeyStroke {
            code,
            shift,
            level3: None,
        })
    }

    #[test]
    fn finds_keys_by_name_and_character() {
        let keys = KeyLookup::new().unwrap();
        // xkb keycode = evdev + 8: KEY_A = 30, KEY_ENTER = 28.
        assert_eq!(keys.find("a"), stroke(38, false));
        assert_eq!(keys.find("A"), stroke(38, true));
        assert_eq!(keys.find("Return"), stroke(36, false));
        assert_eq!(keys.find(" "), stroke(65, false));
        assert_eq!(keys.find("Shift_L"), stroke(SHIFT_LEFT, false));
        assert_eq!(keys.find("NonEsiste"), None);
        // Not on the US layout.
        assert_eq!(keys.find("ò"), None);
    }

    #[test]
    fn the_active_layout_decides_where_a_character_is() {
        // Italian: ';' is Shift+',' (KEY_COMMA = 51 → 59), ò is its own key (KEY_SEMICOLON = 39
        // → 47) and '@' needs AltGr.
        let italian = KeyLookup::for_names("it", "", "", None, 0).unwrap();
        assert_eq!(italian.find(";"), stroke(59, true));
        assert_eq!(italian.find("ò"), stroke(47, false));
        let at = italian.find("@").unwrap();
        assert!(at.level3.is_some(), "AltGr: {at:?}");
        // Two layouts: the group chooses.
        let two = |group| KeyLookup::for_names("us,it", ",", "", None, group).unwrap();
        assert_eq!(two(0).find("ò"), None);
        assert_eq!(two(1).find("ò"), stroke(47, false));
    }

    #[test]
    fn a_temporary_key_types_a_missing_character() {
        let (text, code) = keymap_with_key_for("us", "", "", None, 'ò').unwrap();
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the patched keymap compiles");
        let syms = keymap.key_get_syms_by_level(xkb::Keycode::new(code), 0, 0);
        assert_eq!(
            syms.first().map(|s| s.raw()),
            Some(xkb::utf32_to_keysym(0xf2).raw())
        );
        assert!(keymap_with_key_for("us", "", "", None, '\n').is_none());
        // Several layouts: the key is in each group.
        let (text, code) = keymap_with_key_for("us,it", ",", "", None, '€').unwrap();
        let keymap = xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        for group in 0..2 {
            assert_eq!(
                keymap
                    .key_get_syms_by_level(xkb::Keycode::new(code), group, 0)
                    .first()
                    .map(|s| s.raw()),
                Some(xkb::utf32_to_keysym(0x20ac).raw())
            );
        }
    }
}

/// A key that becomes the Logo key (left Super) before anything else: with the compositor nested
/// in GNOME or KDE the combinations with Super are taken by the desktop below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoKey {
    None,
    RightCtrl,
    RightAlt,
}

/// xkb codes (evdev + 8).
const KEY_RIGHTCTRL: u32 = 97 + 8;
const KEY_RIGHTALT: u32 = 100 + 8;
const KEY_LEFTMETA: u32 = 125 + 8;

impl LogoKey {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "none" => Some(Self::None),
            "rightctrl" => Some(Self::RightCtrl),
            "rightalt" => Some(Self::RightAlt),
            _ => None,
        }
    }

    pub fn remap(
        self,
        code: smithay::input::keyboard::Keycode,
    ) -> smithay::input::keyboard::Keycode {
        let from = match self {
            Self::None => return code,
            Self::RightCtrl => KEY_RIGHTCTRL,
            Self::RightAlt => KEY_RIGHTALT,
        };
        if code.raw() == from {
            KEY_LEFTMETA.into()
        } else {
            code
        }
    }
}

#[cfg(test)]
mod logo_tests {
    use super::*;

    #[test]
    fn the_logo_key_is_remapped_only_when_chosen() {
        let ctrl_r = smithay::input::keyboard::Keycode::new(KEY_RIGHTCTRL);
        assert_eq!(LogoKey::RightCtrl.remap(ctrl_r).raw(), KEY_LEFTMETA);
        assert_eq!(LogoKey::None.remap(ctrl_r).raw(), KEY_RIGHTCTRL);
        let a = smithay::input::keyboard::Keycode::new(38);
        assert_eq!(LogoKey::RightCtrl.remap(a).raw(), 38);
        assert_eq!(LogoKey::parse("rightalt"), Some(LogoKey::RightAlt));
        assert_eq!(LogoKey::parse("x"), None);
    }
}
