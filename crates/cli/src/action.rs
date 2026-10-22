//! silica-action: runs one user-facing Silica action through the compositor's actions socket
//! and exits. The Apps application is `silica-action launcher.toggle`, Screenshot is
//! `silica-action screenshot.toolbar` (see `silica_control::actions`).

use std::process::ExitCode;

use silica_control::actions;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [command] = args.as_slice() else {
        eprintln!(
            "usage: silica-action <action>\nactions: {}",
            actions::ACTIONS.join(", ")
        );
        return ExitCode::from(2);
    };
    if !actions::is_action(command) {
        eprintln!("silica-action: {command} is not an action");
        return ExitCode::from(2);
    }
    match actions::run(command) {
        Ok(_) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("silica-action: {err}");
            ExitCode::FAILURE
        }
    }
}
