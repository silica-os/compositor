//! The compositor, the Wayland compositor of Silica.

mod accessibility;
mod app_frames;
mod app_hide;
pub mod backend;
mod capture;
pub mod commands;
mod config;
mod control;
#[cfg(test)]
mod cpu_equivalence;
#[cfg(feature = "drm")]
mod cursor;
mod cursor_theme;
mod decorations;
mod desktop_entry;
pub mod display_layout;
mod displays;
mod dmabuf;
mod dock_control;
mod dock_reveal;
#[cfg(feature = "drm")]
mod drm;
mod egl;
mod gestures;
mod glass;
#[cfg(test)]
mod glass_equivalence;
mod handlers;
mod idle;
mod idle_notify;
mod image_copy;
mod input;
mod keep;
mod keymap;
pub mod keys;
mod layers;
mod lock;
mod menu_bar;
mod perf;
mod protocols;
mod reap;
mod render;
mod resize;
mod security;
mod session_bus;
pub mod settings_service;
mod shell;
pub mod state;
mod system;
mod systemd;
mod tablet;
mod transient;
mod window_appearance;
mod windows;

use std::io::Write;

use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use tracing::warn;

use crate::state::State;

/// Startup options, read from the command line in `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// No output device: automated tests and CI.
    Headless,
    /// The real session on a VT: DRM/KMS, libinput, libseat (logind).
    #[cfg(feature = "drm")]
    Drm,
}

impl Default for BackendKind {
    /// The real session when it is built in, otherwise headless.
    fn default() -> Self {
        #[cfg(feature = "drm")]
        return BackendKind::Drm;
        #[cfg(not(feature = "drm"))]
        return BackendKind::Headless;
    }
}

#[derive(Debug)]
pub struct Options {
    pub backend: BackendKind,
    /// Size of the headless output, in pixels.
    pub size: (i32, i32),
    /// Integer scale factor of the output (HiDPI).
    pub scale: i32,
    /// Path of the control channel's Unix socket (debug and tests).
    pub control: Option<std::path::PathBuf>,
    /// Commands to start as clients once the Wayland socket is open.
    pub spawn: Vec<String>,
    /// Shell components to start (`--spawn-shell`): they see the shell protocols.
    pub spawn_shell: Vec<String>,
    /// Shell components kept running (`--keep-shell`): started again when they exit.
    pub keep_shell: Vec<String>,
    /// Desktop color until there is a wallpaper (RGBA 0–1, sRGB values).
    pub background: [f32; 4],
    /// Multiplier of the animation durations (0: no animation).
    pub animation_scale: f64,
    /// Glass quality (`--glass auto|full|frosted|opaque`).
    pub glass: state::GlassSetting,
    /// Where the first window opens, from the top left of the usable area, in points
    /// (`--window-origin`, which also sets the cascade step); `None`: 4 pt and 29 pt (handlers.rs).
    pub window_origin: Option<i32>,
    /// Starts a private session bus with `silica-daemon` (`--session-bus`).
    pub session_bus: bool,
    /// A key acting as Logo (`--logo-key`), for keyboards without Super. `None`: no remap.
    pub logo_key: Option<keys::LogoKey>,
    /// Forced appearance (`--appearance light|dark`): true = dark. `None` follows silica-daemon.
    pub appearance: Option<bool>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            backend: BackendKind::default(),
            size: (1280, 800),
            scale: 1,
            control: None,
            spawn: Vec::new(),
            spawn_shell: Vec::new(),
            keep_shell: Vec::new(),
            background: state::CLEAR_COLOR,
            animation_scale: 1.0,
            glass: state::GlassSetting::Auto,
            window_origin: None,
            session_bus: false,
            logo_key: None,
            appearance: None,
        }
    }
}

pub fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<'static, _> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut compositor = match options.backend {
        BackendKind::Headless => backend::init_headless(&mut event_loop, display, options.size)?,
        #[cfg(feature = "drm")]
        BackendKind::Drm => drm::init_drm(&mut event_loop, display)?,
    };

    compositor.state.clear_color = options.background;
    compositor.state.animation_speed = options.animation_scale;
    compositor.state.glass_setting = options.glass;
    if let Some(origin) = options.window_origin {
        // The cascade step is the origin (32 gives the old (32, 32), (64, 64)… layout).
        compositor.state.window_origin = origin;
        compositor.state.cascade_step = origin;
    }
    compositor.state.appearance = options.appearance;
    compositor.state.logo_key = options.logo_key.unwrap_or(keys::LogoKey::None);
    if options.scale != 1 {
        compositor.state.output.change_current_state(
            None,
            None,
            Some(smithay::output::Scale::Integer(options.scale)),
            None,
        );
    }

    // The settings still kept in files (displays.json, power.json), applied now and on change.
    if let Some(dir) = config::config_dir() {
        match config::watch(&event_loop.handle(), &dir) {
            Ok(()) => {
                compositor.state.config_dir = Some(dir);
                compositor.state.apply_all_config();
            }
            Err(err) => warn!("settings folder: {err}"),
        }
    }

    // Energy: the idle times of power.json.
    event_loop.handle().insert_source(
        smithay::reexports::calloop::timer::Timer::from_duration(idle::CHECK),
        |_, _, compositor| {
            compositor.state.idle_check();
            smithay::reexports::calloop::timer::TimeoutAction::ToDuration(idle::CHECK)
        },
    )?;

    // The lines on stdout are the interface for scripts and tests: one per variable.
    let socket = compositor.state.socket_name.clone();
    println!("WAYLAND_DISPLAY={}", socket.to_string_lossy());
    // Lives until the end of `run`: closes the bus and the daemon on exit.
    let _session_bus = if options.session_bus {
        let bus = session_bus::SessionBus::start(&socket)?;
        println!("DBUS_SESSION_BUS_ADDRESS={}", bus.address);
        compositor.state.session_bus = Some(bus.address.clone());
        Some(bus)
    } else {
        None
    };
    // The look of silica-daemon (the glass, the accessibility of the display) whenever it
    // changes, and the settings of the Settings service.
    let (look_tx, look_rx) = smithay::reexports::calloop::channel::channel::<()>();
    event_loop
        .handle()
        .insert_source(look_rx, |event, _, compositor| {
            if let smithay::reexports::calloop::channel::Event::Msg(()) = event {
                compositor.state.apply_theme_config();
                compositor.state.apply_input_config();
            }
        })
        .map_err(|e| format!("look source: {e}"))?;
    compositor.state.system =
        system::SystemLink::start(compositor.state.session_bus.clone(), Some(look_tx));
    let (settings_tx, settings_rx) = smithay::reexports::calloop::channel::channel();
    event_loop
        .handle()
        .insert_source(settings_rx, |event, _, compositor| {
            if let smithay::reexports::calloop::channel::Event::Msg(event) = event {
                compositor.state.settings_event(event);
            }
        })
        .map_err(|e| format!("settings source: {e}"))?;
    settings_service::start(compositor.state.session_bus.clone(), settings_tx);
    if let Some(path) = &options.control {
        control::listen(&event_loop.handle(), path)?;
        println!("SILICA_COMPOSITOR_CONTROL={}", path.display());
    }
    // The actions socket, always: the Apps, Mission Control and Screenshot applications use it.
    if let Some(path) = silica_control::actions::socket_path(&socket.to_string_lossy())
        && let Err(err) = control::listen_actions(&event_loop.handle(), &path)
    {
        warn!("actions socket {}: {err}", path.display());
    }
    std::io::stdout().flush()?;
    systemd::announce(&socket);
    for command in &options.spawn {
        if let Err(err) = compositor.state.launch(command) {
            warn!("{err}");
        }
    }
    for command in &options.spawn_shell {
        if let Err(err) = compositor.state.launch_with(command, true) {
            warn!("{err}");
        }
    }
    for command in &options.keep_shell {
        compositor.state.keep_shell(command);
    }

    event_loop.run(None, &mut compositor, |compositor| {
        compositor.state.space.refresh();
        compositor.state.popups.cleanup();
        // The Dock (silica-dock-v1) learns what changed in the windows and layers.
        compositor.state.dock_sync();
        if let Err(err) = compositor.display.flush_clients() {
            warn!("client flush failed: {err}");
        }
    })?;
    systemd::stopping();
    Ok(())
}
