//! silica-cli: sends commands to Silica from the terminal, or exposes them over MCP.
//!
//! ```text
//! silica-cli commands                        lists the catalog
//! silica-cli window.list                     runs a command
//! silica-cli window.move x=10 y=20           key=value parameters (JSON or text values)
//! silica-cli window.move '{"x":10,"y":20}'   or a JSON object
//! silica-cli screenshot [PATH]               captures the output into a PNG
//! silica-cli mcp                             MCP server on stdio
//! ```
//!
//! The control channel's socket is `--control PATH`, or `SILICA_CONTROL`, or
//! `$XDG_RUNTIME_DIR/silica/control.sock`.

use std::io::Write;
use std::process::ExitCode;

use serde_json::{Map, Value};
use silica_control::commands::{ENGINE_COMMANDS, UI_SCREENSHOT};
use silica_control::{Client, Error, mcp};

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut control = std::env::var("SILICA_CONTROL")
        .ok()
        .or_else(|| silica_control::default_socket_path().map(|p| p.display().to_string()))
        .unwrap_or_default();
    if let Some(pos) = args.iter().position(|arg| arg == "--control") {
        let Some(path) = args.get(pos + 1).cloned() else {
            eprintln!("silica-cli: --control needs a socket path");
            return ExitCode::from(2);
        };
        control = path;
        args.drain(pos..pos + 2);
    }

    let result = match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            println!("{}", usage());
            Ok(())
        }
        Some("mcp") => run_mcp(control),
        Some("commands") => call(&control, ENGINE_COMMANDS, Value::Null),
        Some("screenshot") => {
            let path = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "silica.png".to_owned());
            let path = std::path::absolute(&path)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or(path);
            call(&control, UI_SCREENSHOT, serde_json::json!({ "path": path }))
        }
        Some(command) => match params(args.get(1..).unwrap_or_default()) {
            Ok(params) => call(&control, command, params),
            Err(err) => Err(err),
        },
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("silica-cli: {err}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str {
    "usage: silica-cli [--control ADDRESS] <commands | screenshot [PATH] | mcp | COMMAND [key=value... | JSON]>"
}

/// Reads the parameters: a whole JSON object, or `key=value` pairs.
fn params(args: &[String]) -> Result<Value, String> {
    match args {
        [] => Ok(Value::Null),
        [json] if json.trim_start().starts_with('{') => {
            serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))
        }
        pairs => {
            let mut map = Map::new();
            for pair in pairs {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("expected key=value, found {pair}"))?;
                let value =
                    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
                map.insert(key.to_owned(), value);
            }
            Ok(Value::Object(map))
        }
    }
}

fn call(control: &str, command: &str, params: Value) -> Result<(), String> {
    let mut client = Client::connect(control)
        .map_err(|e| format!("compositor unreachable at {control}: {e}"))?;
    let result = client.call(command, params).map_err(|e| e.to_string())?;
    if !result.is_null() {
        let text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
        println!("{text}");
    }
    Ok(())
}

fn run_mcp(control: String) -> Result<(), String> {
    // The connection opens on the first call and reopens if it drops: the compositor may
    // start after the MCP server or be restarted.
    let mut client: Option<Client> = None;
    let execute = |command: &str, params: Value| -> Result<Value, String> {
        if client.is_none() {
            client = Some(
                Client::connect(control.as_str())
                    .map_err(|e| format!("compositor unreachable at {control}: {e}"))?,
            );
        }
        let Some(connected) = client.as_mut() else {
            return Err("no connection".to_owned());
        };
        match connected.call(command, params) {
            Ok(value) => Ok(value),
            Err(Error::Command(message)) => Err(message),
            Err(err) => {
                client = None;
                Err(err.to_string())
            }
        }
    };
    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let served = mcp::serve(stdin, &mut stdout, execute).map_err(|e| e.to_string());
    let _ = stdout.flush();
    served
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn params_from_pairs_and_json() {
        assert_eq!(params(&[]).unwrap(), Value::Null);
        assert_eq!(
            params(&strings(&["x=10", "title=hello", "on=true"])).unwrap(),
            json!({"x": 10, "title": "hello", "on": true})
        );
        assert_eq!(params(&strings(&[r#"{"x":1}"#])).unwrap(), json!({"x": 1}));
        assert!(params(&strings(&["no-equals-sign"])).is_err());
    }
}
