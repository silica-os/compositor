//! Test support: the headless compositor driven through the control channel.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

pub mod client;

use compositor::state::CLEAR_COLOR;
use silica_control::{Client, ScreenshotParams};

pub const WIDTH: u32 = 320;
pub const HEIGHT: u32 = 200;

/// A the headless compositor with its runtime directory; it dies with the test.
pub struct Headless {
    child: Child,
    pub dir: PathBuf,
    pub control: String,
    /// Absolute path of the compositor's Wayland socket.
    pub wayland: PathBuf,
}

impl Headless {
    pub fn start(name: &str) -> Self {
        Self::start_with(name, (WIDTH, HEIGHT), &[])
    }

    /// Like `start`, with another output size and more arguments for the compositor.
    pub fn start_with(name: &str, size: (u32, u32), extra: &[&str]) -> Self {
        Self::start_with_env(name, size, extra, &[])
    }

    /// Like `start_with`, with environment variables for the compositor.
    pub fn start_with_env(
        name: &str,
        (width, height): (u32, u32),
        extra: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("compositor-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut child = Command::new(env!("CARGO_BIN_EXE_silica-compositor"))
            .args([
                "--backend",
                "headless",
                "--size",
                &format!("{width}x{height}"),
            ])
            // Deterministic screenshots: no animations, unless a test turns them back on.
            .args(["--animation-scale", "0"])
            // Full glass even on llvmpipe (the automatic level there would be frosted).
            .args(["--glass", "full"])
            .args(extra)
            .arg("--control")
            .arg(dir.join("control.sock"))
            .env("XDG_RUNTIME_DIR", &dir)
            // The user's settings (keyboard.json, displays.json…) never reach the tests.
            .env("XDG_CONFIG_HOME", dir.join("config"))
            // Nor the user's session bus: The compositor's reports to silica-daemon go nowhere.
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", dir.join("no-bus").display()),
            )
            .env("RUST_LOG", "warn")
            .envs(env.iter().copied())
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut wayland = None;
        let mut control = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.unwrap();
            if let Some(name) = line.strip_prefix("WAYLAND_DISPLAY=") {
                wayland = Some(dir.join(name));
            }
            if let Some(addr) = line.strip_prefix("SILICA_COMPOSITOR_CONTROL=") {
                control = Some(addr.to_owned());
                break;
            }
        }
        Self {
            child,
            wayland: wayland.expect("The compositor did not announce the Wayland socket"),
            control: control.expect("The compositor exited without opening the control channel"),
            dir,
        }
    }

    pub fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap()
    }

    pub fn client(&self) -> Client {
        Client::connect(&self.control).unwrap()
    }
}

/// A private session bus with a Settings service on it (`dev.frascella.Silica.Settings1`, the
/// interface of silica-daemon, here a small stand-in holding what it is given), for an the compositor
/// started with [`SettingsBus::env`]: the settings the compositor reads from the service. The compositor does not
/// depend on silica-daemon (the layers of `xtask`), so the tests do not either.
pub struct SettingsBus {
    bus: Child,
    pub address: String,
    connection: zbus::blocking::Connection,
}

const SETTINGS_PATH: &str = "/dev/frascella/Silica/Settings1";
const SETTINGS_INTERFACE: &str = "dev.frascella.Silica.Settings1";

/// The stand-in service: GetAll, Set and Reset, and Changed for every change.
struct FakeSettings {
    values: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    defaults: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
}

#[zbus::interface(name = "dev.frascella.Silica.Settings1")]
impl FakeSettings {
    fn get_all(&self) -> std::collections::HashMap<String, zbus::zvariant::OwnedValue> {
        self.values
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.try_clone().ok()?)))
            .collect()
    }

    async fn set(
        &mut self,
        key: &str,
        value: zbus::zvariant::Value<'_>,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let owned = value
            .try_to_owned()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        self.values.insert(key.to_owned(), owned);
        Self::changed(&emitter, key, value).await?;
        Ok(())
    }

    async fn reset(
        &mut self,
        key: &str,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let default = self
            .defaults
            .get(key)
            .and_then(|v| v.try_clone().ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no default for {key}")))?;
        let value: zbus::zvariant::Value<'_> = default
            .try_clone()
            .map(Into::into)
            .map_err(|e: zbus::zvariant::Error| zbus::fdo::Error::Failed(e.to_string()))?;
        self.values.insert(key.to_owned(), default);
        Self::changed(&emitter, key, value).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn changed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        key: &str,
        value: zbus::zvariant::Value<'_>,
    ) -> zbus::Result<()>;
}

impl SettingsBus {
    /// The service with `defaults` (silica-daemon's defaults of the keys a test reads).
    pub fn start(defaults: &[(&str, zbus::zvariant::Value<'_>)]) -> Self {
        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon (package dbus) is required for these tests");
        let mut line = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_owned();
        let defaults: std::collections::HashMap<_, _> = defaults
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.try_to_owned().unwrap()))
            .collect();
        let values = defaults
            .iter()
            .map(|(k, v)| (k.clone(), v.try_clone().unwrap()))
            .collect();
        let connection = zbus::blocking::connection::Builder::address(address.as_str())
            .unwrap()
            .name("dev.frascella.Silica")
            .unwrap()
            .serve_at(SETTINGS_PATH, FakeSettings { values, defaults })
            .unwrap()
            .build()
            .unwrap();
        Self {
            bus,
            address,
            connection,
        }
    }

    /// The environment for the compositor: this bus as its session bus.
    pub fn env(&self) -> [(&str, &str); 1] {
        [("DBUS_SESSION_BUS_ADDRESS", self.address.as_str())]
    }

    /// `Set(key, value)` on the service, as Settings does.
    pub fn set(&self, key: &str, value: zbus::zvariant::Value<'_>) {
        self.connection
            .call_method(
                Some("dev.frascella.Silica"),
                SETTINGS_PATH,
                Some(SETTINGS_INTERFACE),
                "Set",
                &(key, value),
            )
            .unwrap();
    }

    /// `Reset(key)`: back to the default.
    pub fn reset(&self, key: &str) {
        self.connection
            .call_method(
                Some("dev.frascella.Silica"),
                SETTINGS_PATH,
                Some(SETTINGS_INTERFACE),
                "Reset",
                &(key,),
            )
            .unwrap();
    }
}

impl Drop for SettingsBus {
    fn drop(&mut self) {
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn decode(png_bytes: &[u8]) -> (png::OutputInfo, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    (info, pixels)
}

pub fn assert_clear_color(pixels: &[u8], x: u32, y: u32) {
    let offset = ((y * WIDTH + x) * 4) as usize;
    let pixel = &pixels[offset..offset + 4];
    for (channel, expected) in pixel.iter().zip(CLEAR_COLOR) {
        let expected = (expected * 255.0).round() as i32;
        assert!(
            (i32::from(*channel) - expected).abs() <= 1,
            "pixel ({x}, {y}) = {pixel:?}, expected the background color {CLEAR_COLOR:?}"
        );
    }
}

/// The pixel is outside the test window (red): background, shadow or shadow ring.
pub fn assert_outside_window(pixels: &[u8], x: u32, y: u32) {
    let [r, g, b, _] = pixel(pixels, x, y);
    assert!(
        r < 64 && r.abs_diff(g) < 32 && r.abs_diff(b) < 32,
        "pixel ({x}, {y}) = {:?}: expected outside the window",
        [r, g, b]
    );
}

/// RGBA of the pixel (x, y) in a `WIDTH`×`HEIGHT` screenshot.
pub fn pixel(shot: &[u8], x: u32, y: u32) -> [u8; 4] {
    pixel_in(shot, WIDTH, x, y)
}

/// RGBA of the pixel (x, y) in a screenshot `width` wide.
pub fn pixel_in(shot: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * width + x) * 4) as usize;
    shot[offset..offset + 4].try_into().unwrap()
}

/// A screenshot on file, decoded to RGBA.
pub fn screenshot(compositor: &Headless) -> Vec<u8> {
    let path = compositor.dir.join("shot.png");
    compositor
        .client()
        .screenshot(&ScreenshotParams {
            path: Some(path.to_string_lossy().into_owned()),
        })
        .unwrap();
    decode(&std::fs::read(&path).unwrap()).1
}
