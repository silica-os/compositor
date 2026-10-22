//! The model of `silica-menu-v1`: the JSON an app sends with `set_model` and the shell
//! receives with `silica_shell_manager_v1.app_menu`. [`parse_model`] checks a model against
//! the protocol's limits and puts it in normal form; the compositor runs it on every model it
//! receives, the embedder before sending one (a model the compositor refuses ends the connection).

use serde::{Deserialize, Serialize};

use super::MAX_MODEL_SIZE;

/// The limits of a model (silica-menu-v1, `set_model`).
pub const MAX_MENUS: usize = 32;
pub const MAX_ITEMS: usize = 4096;
/// Submenu levels below a top-level menu.
pub const MAX_DEPTH: usize = 4;
pub const MAX_LABEL: usize = 256;
/// Roles and key names.
pub const MAX_NAME: usize = 32;

/// A model of menus, in the form the shell receives.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub menus: Vec<Menu>,
}

/// A top-level menu: its title in the bar and its items.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    #[serde(default)]
    pub label: String,
    /// `app` (the bold menu named after the app), `window`, `help`, or empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// An item, a submenu (with `items`) or a separator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub separator: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checked: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<Shortcut>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Item>>,
}

fn enabled_default() -> bool {
    true
}

/// A shortcut as the app handles it: a key name (`A`, `1`, `Left`, `Return`…) and the
/// modifiers; `meta` is ⌘ (the Logo key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub key: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub meta: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ctrl: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
}

fn check_text(what: &str, text: &str, max: usize) -> Result<(), String> {
    if text.len() > max {
        return Err(format!("{what} longer than {max} bytes"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{what} with control characters"));
    }
    Ok(())
}

/// Checks the items of one level and puts them in normal form; `count` is the items so far.
fn normalize_items(items: Vec<Item>, depth: usize, count: &mut usize) -> Result<Vec<Item>, String> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        *count = count.saturating_add(1);
        if *count > MAX_ITEMS {
            return Err(format!("more than {MAX_ITEMS} items"));
        }
        if item.separator {
            out.push(Item::separator());
            continue;
        }
        check_text("label", &item.label, MAX_LABEL)?;
        check_text("role", &item.role, MAX_NAME)?;
        let shortcut = match item.shortcut {
            Some(shortcut) if shortcut.key.is_empty() => None,
            Some(shortcut) => {
                check_text("shortcut key", &shortcut.key, MAX_NAME)?;
                Some(shortcut)
            }
            None => None,
        };
        let items = match item.items {
            Some(children) => {
                if depth >= MAX_DEPTH {
                    return Err(format!("submenus deeper than {MAX_DEPTH} levels"));
                }
                Some(normalize_items(children, depth + 1, count)?)
            }
            None => None,
        };
        out.push(Item {
            separator: false,
            shortcut,
            items,
            ..item
        });
    }
    Ok(out)
}

impl Item {
    pub fn separator() -> Self {
        Self {
            separator: true,
            id: None,
            label: String::new(),
            enabled: true,
            checked: false,
            role: String::new(),
            shortcut: None,
            items: None,
        }
    }
}

/// Reads a model sent by an app: JSON within the limits, in normal form.
pub fn parse_model(bytes: &[u8]) -> Result<Model, String> {
    if bytes.is_empty() || bytes.len() > MAX_MODEL_SIZE {
        return Err(format!("size {} outside 1–{MAX_MODEL_SIZE}", bytes.len()));
    }
    let model: Model = serde_json::from_slice(bytes).map_err(|err| format!("JSON: {err}"))?;
    if model.menus.len() > MAX_MENUS {
        return Err(format!("more than {MAX_MENUS} menus"));
    }
    let mut count = 0;
    let mut menus = Vec::with_capacity(model.menus.len());
    for menu in model.menus {
        check_text("menu label", &menu.label, MAX_LABEL)?;
        check_text("menu role", &menu.role, MAX_NAME)?;
        let items = normalize_items(menu.items, 1, &mut count)?;
        menus.push(Menu { items, ..menu });
    }
    Ok(Model { menus })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn a_model_is_read_and_put_in_normal_form() {
        let model = parse_model(
            br#"{"menus": [
                {"label": "Calculator", "role": "app", "items": [
                    {"id": 1, "label": "About Calculator"},
                    {"separator": true, "label": "dropped"},
                    {"label": "Quit Calculator", "role": "quit",
                     "shortcut": {"key": "Q", "meta": true}}
                ]},
                {"label": "View", "extra": 3, "items": [
                    {"id": 2, "label": "Basic", "checked": true,
                     "shortcut": {"key": "1", "meta": true}},
                    {"id": 3, "label": "Decimal Places", "items": [
                        {"id": 4, "label": "0", "enabled": false}
                    ]},
                    {"id": 5, "label": "No key", "shortcut": {"key": ""}}
                ]}
            ]}"#,
        )
        .unwrap();
        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(
            json,
            json!({"menus": [
                {"label": "Calculator", "role": "app", "items": [
                    {"id": 1, "label": "About Calculator", "enabled": true},
                    {"separator": true, "enabled": true},
                    {"label": "Quit Calculator", "role": "quit", "enabled": true,
                     "shortcut": {"key": "Q", "meta": true}}
                ]},
                {"label": "View", "items": [
                    {"id": 2, "label": "Basic", "enabled": true, "checked": true,
                     "shortcut": {"key": "1", "meta": true}},
                    {"id": 3, "label": "Decimal Places", "enabled": true, "items": [
                        {"id": 4, "label": "0", "enabled": false}
                    ]},
                    {"id": 5, "label": "No key", "enabled": true}
                ]}
            ]})
        );
    }

    #[test]
    fn hostile_models_are_refused() {
        assert!(parse_model(b"").is_err());
        assert!(parse_model(b"not json").is_err());
        assert!(parse_model(b"[1, 2]").is_err());
        assert!(parse_model(&vec![b' '; MAX_MODEL_SIZE + 1]).is_err());
        // Too many menus, too many items, too deep, labels too long or with control
        // characters.
        let menus: Vec<Value> = (0..=MAX_MENUS).map(|_| json!({"label": "M"})).collect();
        assert!(parse_model(json!({"menus": menus}).to_string().as_bytes()).is_err());
        let items: Vec<Value> = (0..=MAX_ITEMS).map(|i| json!({"id": i})).collect();
        let many = json!({"menus": [{"label": "M", "items": items}]});
        assert!(parse_model(many.to_string().as_bytes()).is_err());
        let mut deep = json!({"id": 1, "label": "leaf"});
        for _ in 0..MAX_DEPTH {
            deep = json!({"label": "sub", "items": [deep]});
        }
        let model = json!({"menus": [{"label": "M", "items": [deep]}]});
        assert!(parse_model(model.to_string().as_bytes()).is_err());
        let long = "x".repeat(MAX_LABEL + 1);
        let model = json!({"menus": [{"label": "M", "items": [{"label": long}]}]});
        assert!(parse_model(model.to_string().as_bytes()).is_err());
        let model = json!({"menus": [{"label": "M\u{7}"}]});
        assert!(parse_model(model.to_string().as_bytes()).is_err());
        // A JSON nesting bomb stops at serde_json's recursion limit, without a crash.
        let bomb = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(parse_model(bomb.as_bytes()).is_err());
    }

    #[test]
    fn the_deepest_allowed_submenu_passes() {
        let mut deep = json!({"id": 1, "label": "leaf"});
        for _ in 1..MAX_DEPTH {
            deep = json!({"label": "sub", "items": [deep]});
        }
        let model = json!({"menus": [{"label": "M", "items": [deep]}]});
        assert!(parse_model(model.to_string().as_bytes()).is_ok());
    }
}
