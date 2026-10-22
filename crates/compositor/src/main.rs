//! The compositor startup.
//!
//! ```text
//! silica-compositor [--backend headless|drm] [--size WxH] [--scale N] [--control SOCKET]
//!     [--background RRGGBB] [--animation-scale F] [--window-origin PT] [--glass auto|full|frosted|opaque]
//!     [--session-bus] [--logo-key none|rightctrl|rightalt] [--appearance light|dark]
//!     [--spawn COMMAND]... [--spawn-shell COMMAND]... [--keep-shell COMMAND]...
//! ```
//!
//! Without `--backend`: `drm`, the real session on a VT.

use std::process::ExitCode;

use compositor::{BackendKind, Options};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("compositor: {err}");
            return ExitCode::from(2);
        }
    };
    #[cfg(feature = "drm")]
    if options.backend == BackendKind::Drm {
        software_rasterizer_without_jit();
    }
    match compositor::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// When no GPU renders (no Mesa driver for the DRM device, or virtio-gpu without 3D), Mesa falls
/// back to llvmpipe, whose shaders are machine code generated at run time: IPE refuses to execute
/// anonymous memory, so llvmpipe crashes. softpipe interprets the shaders instead.
/// `GALLIUM_DRIVER` picks only the software driver; hardware drivers ignore it. The clients the
/// compositor starts inherit it. Called while the process has one thread.
#[cfg(feature = "drm")]
#[allow(unsafe_code)]
fn software_rasterizer_without_jit() {
    if std::env::var_os("GALLIUM_DRIVER").is_none() {
        // SAFETY: no other thread exists yet, nothing reads the environment concurrently.
        unsafe { std::env::set_var("GALLIUM_DRIVER", "softpipe") };
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut backend = None;
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--backend" => {
                backend = Some(match value()?.as_str() {
                    "headless" => BackendKind::Headless,
                    #[cfg(feature = "drm")]
                    "drm" => BackendKind::Drm,
                    other => return Err(format!("unknown backend: {other}")),
                })
            }
            "--size" => {
                let size = value()?;
                let (w, h) = size
                    .split_once('x')
                    .ok_or(format!("invalid size: {size}"))?;
                let parse = |n: &str| n.parse::<i32>().ok().filter(|n| (1..=16384).contains(n));
                options.size = parse(w)
                    .zip(parse(h))
                    .ok_or(format!("invalid size: {size}"))?;
            }
            "--scale" => {
                let scale = value()?;
                options.scale = scale
                    .parse()
                    .ok()
                    .filter(|s| (1..=4).contains(s))
                    .ok_or(format!("invalid scale (1-4): {scale}"))?;
            }
            "--control" => {
                options.control = Some(value()?.into());
            }
            "--animation-scale" => {
                let text = value()?;
                options.animation_scale = text
                    .parse::<f64>()
                    .ok()
                    .filter(|scale| scale.is_finite() && (0.0..=1000.0).contains(scale))
                    .ok_or(format!("invalid animation scale (0–1000): {text}"))?;
            }
            "--window-origin" => {
                let text = value()?;
                options.window_origin = Some(
                    text.parse::<i32>()
                        .ok()
                        .filter(|n| (0..=1000).contains(n))
                        .ok_or(format!("invalid window origin (0-1000): {text}"))?,
                );
            }
            "--glass" => {
                let text = value()?;
                options.glass = compositor::state::GlassSetting::parse(&text).ok_or(format!(
                    "unknown glass quality (auto, full, frosted, opaque): {text}"
                ))?;
            }
            "--background" => {
                let hex = value()?;
                options.background =
                    parse_color(&hex).ok_or(format!("invalid color (RRGGBB): {hex}"))?;
            }
            "--spawn" => {
                let command = value()?;
                options.spawn.push(command);
            }
            "--session-bus" => options.session_bus = true,
            "--logo-key" => {
                let name = value()?;
                options.logo_key = Some(
                    compositor::keys::LogoKey::parse(&name)
                        .ok_or(format!("--logo-key: {name} (none, rightctrl, rightalt)"))?,
                );
            }
            "--appearance" => {
                options.appearance = match value()?.as_str() {
                    "light" => Some(false),
                    "dark" => Some(true),
                    other => return Err(format!("unknown appearance (light, dark): {other}")),
                }
            }
            "--spawn-shell" => {
                let command = value()?;
                options.spawn_shell.push(command);
            }
            "--keep-shell" => {
                let command = value()?;
                options.keep_shell.push(command);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    options.backend = backend.unwrap_or_default();
    Ok(options)
}

/// `RRGGBB` (with or without `#`) as RGBA 0–1.
fn parse_color(text: &str) -> Option<[f32; 4]> {
    let hex = text.strip_prefix('#').unwrap_or(text);
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let channel = |at: usize| {
        hex.get(at..at + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map(|value| f32::from(value) / 255.0)
    };
    Some([channel(0)?, channel(2)?, channel(4)?, 1.0])
}

#[cfg(test)]
mod tests {
    use super::parse_color;

    #[test]
    fn colors_parse_from_hex() {
        assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color("00ff00"), Some([0.0, 1.0, 0.0, 1.0]));
        assert_eq!(parse_color("#7a7b8"), None);
        assert_eq!(parse_color("zzzzzz"), None);
        assert_eq!(parse_color("éééé"), None);
    }
}
