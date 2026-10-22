//! Who may use what. Apps in a sandbox connect through a wp-security-context socket and never
//! see the privileged protocols (input injection, input methods, layer surfaces, the shell's
//! own); shell components and the clipboard daemon must also be system binaries from the
//! sealed `/usr`, so an environment variable alone does not make a process privileged.

use std::sync::Arc;

use smithay::reexports::wayland_server::Client;
use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
};
use tracing::{info, warn};

use crate::state::{ClientState, State};

/// The client came through a sandbox's security context.
pub fn sandboxed(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_none_or(|data| data.security_context.is_some())
}

/// A client outside any sandbox: it may use input methods, virtual keyboards and layer
/// surfaces, and create security contexts for the apps it starts.
pub fn unsandboxed(client: &Client) -> bool {
    !sandboxed(client)
}

/// The process runs a binary of the system image. Debug builds (the headless tests run the
/// clients from the build tree) trust any path.
pub fn system_binary(pid: i32) -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .is_ok_and(|exe| exe.starts_with("/usr/bin") || exe.starts_with("/usr/libexec"))
}

impl SecurityContextHandler for State {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        info!(
            "security context: {} {}",
            context.sandbox_engine.as_deref().unwrap_or("?"),
            context.app_id.as_deref().unwrap_or("?")
        );
        let inserted = self
            .loop_handle
            .insert_source(source, move |stream, _, data| {
                let pid = rustix::net::sockopt::socket_peercred(&stream)
                    .ok()
                    .map(|credentials| credentials.pid.as_raw_nonzero().get());
                let client = ClientState {
                    pid,
                    security_context: Some(context.clone()),
                    ..ClientState::default()
                };
                if let Err(err) = data
                    .state
                    .display_handle
                    .insert_client(stream, Arc::new(client))
                {
                    warn!("sandboxed client refused: {err}");
                }
            });
        if let Err(err) = inserted {
            warn!("security context listener: {err}");
        }
    }
}

smithay::delegate_security_context!(State);
