//! Apps for `app.launch {app}`: XDG desktop entries, read in process so that the compositor's
//! thread never waits on the bus. The command is the entry's `Exec` with the field codes
//! stripped, so `--app-id` and the like are kept. Silica's own app format comes later and
//! replaces this lookup.

use std::path::PathBuf;

/// The command that runs the app `id` (`org.mozilla.firefox`, with or without `.desktop`).
pub fn command_for(id: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let id = id.strip_suffix(".desktop").unwrap_or(id);
    if !valid_id(id) {
        return Err(format!("not a desktop entry id: {id}"));
    }
    let entry = data_dirs(env)
        .into_iter()
        .map(|dir| dir.join("applications").join(format!("{id}.desktop")))
        .find_map(|path| std::fs::read_to_string(path).ok())
        .ok_or_else(|| format!("no app with the id {id}"))?;
    let command = exec_of(&entry).map(strip_field_codes).unwrap_or_default();
    if command.is_empty() {
        return Err(format!("{id}: not a launchable application"));
    }
    Ok(command)
}

/// A desktop entry id: dot-separated parts of letters, digits, `-` and `_`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// `$XDG_DATA_HOME` first, then `$XDG_DATA_DIRS`, with the XDG defaults.
fn data_dirs(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match env("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(home) => dirs.push(PathBuf::from(home)),
        None => {
            if let Some(home) = env("HOME").filter(|v| !v.is_empty()) {
                dirs.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }
    let system = env("XDG_DATA_DIRS")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// `Exec` of the `[Desktop Entry]` group, for `Type=Application` entries.
fn exec_of(entry: &str) -> Option<&str> {
    let mut in_group = false;
    let mut exec = None;
    let mut application = false;
    for line in entry.lines().map(str::trim) {
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some(value) = line.strip_prefix("Exec=") {
            exec = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("Type=") {
            application = value.trim() == "Application";
        }
    }
    exec.filter(|_| application)
}

/// Drops the field codes (`%f`, `%U`, ...) and turns `%%` into `%`.
fn strip_field_codes(exec: &str) -> String {
    let mut words = Vec::new();
    for word in exec.split_whitespace() {
        let mut out = String::new();
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            if let Some('%') = chars.next() {
                out.push('%');
            }
        }
        if !out.is_empty() {
            words.push(out);
        }
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_for_searches_the_registry() {
        let root = std::env::temp_dir().join(format!("silica-entry-{}", std::process::id()));
        let apps = root.join("share/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join("dev.x.Calc.desktop"),
            "[Desktop Entry]\nType=Application\nName=C\nExec=calc --app-id dev.x.Calc %U\n",
        )
        .unwrap();
        let share = root.join("share").to_string_lossy().into_owned();
        let env = move |k: &str| match k {
            "XDG_DATA_DIRS" => Some(share.clone()),
            "HOME" => Some("/nonexistent".to_owned()),
            _ => None,
        };
        assert_eq!(
            command_for("dev.x.Calc", &env).unwrap(),
            "calc --app-id dev.x.Calc"
        );
        assert_eq!(
            command_for("dev.x.Calc.desktop", &env).unwrap(),
            "calc --app-id dev.x.Calc"
        );
        assert!(command_for("missing", &env).is_err());
        assert!(command_for("../etc/passwd", &env).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
