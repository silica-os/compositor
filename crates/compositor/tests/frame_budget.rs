//! Frame budget bench: the compositor's time per drawn frame on a desktop scene, scenario by
//! scenario, from `perf.render` with headless frames on.
//!
//! Ignored by default (it takes a minute and wants a release build and a quiet machine):
//!
//! ```text
//! SILICA_COMPOSITOR_HEADLESS_RENDERER=cpu|gpu \
//!     cargo test --release -p compositor --test frame_budget -- --ignored --nocapture
//! ```
//!
//! The scene is 1280 × 800: a wallpaper, a menu bar and a Dock of Full glass, one decorated
//! window, the cursor. Each scenario runs `FRAME_BUDGET_SECONDS` (default 4) and prints the
//! p50/p95/p99/max of the drawn frames and the p99 of each stage. The gate of the renderer is
//! asserted: `cpu` p99 < 2.42 ms; `gpu` compositor p99 < 0.5 ms and GPU p99 < 0.5 ms. GLES on
//! llvmpipe (no variable) has no gate: it is the baseline.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use std::time::{Duration, Instant};

use common::Headless;
use common::client::TestWindow;
use serde_json::{Value, json};
use silica_control::commands::{
    INPUT_POINTER_MOVE, PERF_RENDER, RenderTimes, UI_ANIMATION_SCALE, WINDOW_FOCUS, WINDOW_LIST,
    WINDOW_MINIMIZE, WINDOW_MOVE, WindowInfo,
};
use silica_protocols::glass::client::silica_glass_v1::Material;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

const SCREEN: (u32, u32) = (1280, 800);
const WINDOW: (u32, u32) = (640, 400);
const MENU_BAR: u32 = 24;
const DOCK: (u32, u32) = (640, 64);
const FRAME: Duration = Duration::from_micros(16_667);

/// A scenario: its name and what drives it.
type Scenario = (&'static str, fn(&mut Scene));

struct Scene {
    compositor: Headless,
    window: TestWindow,
    _layers: Vec<TestWindow>,
    id: u64,
}

impl Scene {
    fn open() -> Self {
        let compositor = Headless::start_with_env(
            "frame-budget",
            SCREEN,
            &["--glass", "full"],
            &[("SILICA_COMPOSITOR_HEADLESS_FRAMES", "1")],
        );
        let socket = compositor.wayland.clone();
        // A wallpaper with detail, so the glass has something to blur.
        let wallpaper = TestWindow::open_layer(&socket, Layer::Background, "wallpaper", None, {
            |x, y, w, h| {
                let r = (x * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                let b = if (x / 37 + y / 23) % 2 == 0 { 90 } else { 170 };
                [b, g, r, 255]
            }
        });
        let mut menu_bar = TestWindow::open_layer_at(
            &socket,
            Layer::Top,
            "menu-bar",
            Anchor::Top | Anchor::Left | Anchor::Right,
            (0, MENU_BAR),
            None,
            |_, _, _, _| [0; 4],
        );
        menu_bar
            .set_glass(&[(
                0,
                0,
                SCREEN.0 as i32,
                MENU_BAR as i32,
                0.0,
                Material::Regular,
            )])
            .unwrap();
        let mut dock = TestWindow::open_layer_at(
            &socket,
            Layer::Top,
            "dock",
            Anchor::Bottom,
            DOCK,
            None,
            |_, _, _, _| [0; 4],
        );
        dock.set_glass(&[(0, 0, DOCK.0 as i32, DOCK.1 as i32, 18.0, Material::Regular)])
            .unwrap();
        let window = TestWindow::open_decorated(&socket, "frame budget", WINDOW);
        let id = windows(&compositor)
            .into_iter()
            .find(|w| w.title.as_deref() == Some("frame budget"))
            .expect("the window is listed")
            .id;
        let mut scene = Scene {
            compositor,
            window,
            _layers: vec![wallpaper, menu_bar, dock],
            id,
        };
        // The animations at their real pace (the test harness turns them off).
        scene.call(UI_ANIMATION_SCALE, json!({ "scale": 1.0 }));
        scene.call(WINDOW_MOVE, json!({ "id": id, "x": 320, "y": 160 }));
        scene.call(INPUT_POINTER_MOVE, json!({ "x": 640, "y": 400 }));
        // Warm up: the first frames compile the shaders and fill the caches.
        scene.window.commit_damaged(30, FRAME, (0, 0, 1, 1));
        scene.call(INPUT_POINTER_MOVE, json!({ "x": 600, "y": 380 }));
        std::thread::sleep(Duration::from_millis(200));
        scene
    }

    fn call(&self, command: &str, params: Value) -> Value {
        self.compositor.client().call(command, params).unwrap()
    }

    /// Runs `step(scene, frame index)` once per frame for the scenario's time, reading the
    /// client's events now and then.
    fn each_frame(&mut self, mut step: impl FnMut(&mut Scene, u32)) {
        let frames = frames();
        let start = Instant::now();
        for i in 0..frames {
            step(self, i);
            if i % 30 == 29 {
                let _ = self.window.configured_size();
            }
            let next = start + FRAME * (i + 1);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    }

    /// The drawn frames of `drive`, measured from a clean `perf.render`.
    fn measure(&mut self, drive: impl FnOnce(&mut Scene)) -> RenderTimes {
        self.call(PERF_RENDER, json!({ "reset": true }));
        drive(self);
        // The last frames still on their way.
        std::thread::sleep(FRAME * 4);
        serde_json::from_value(self.call(PERF_RENDER, json!({ "reset": true }))).unwrap()
    }
}

fn windows(compositor: &Headless) -> Vec<WindowInfo> {
    serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap()).unwrap()
}

fn frames() -> u32 {
    let seconds: f64 = std::env::var("FRAME_BUDGET_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4.0);
    (seconds * 60.0).round() as u32
}

/// Toggles `on` and `off` every `period` frames.
fn alternate(scene: &mut Scene, i: u32, period: u32, on: impl Fn(&Scene), off: impl Fn(&Scene)) {
    if i.is_multiple_of(period) {
        if (i / period).is_multiple_of(2) {
            on(scene);
        } else {
            off(scene);
        }
    }
}

fn spinner(scene: &mut Scene) {
    let (w, h) = (WINDOW.0 as i32, WINDOW.1 as i32);
    scene
        .window
        .commit_damaged(frames(), FRAME, (w / 2 - 16, h / 2 - 16, 32, 32));
}

fn hover(scene: &mut Scene) {
    scene.each_frame(|scene, i| {
        let t = f64::from(i) / 30.0;
        let (x, y) = (640.0 + 200.0 * t.cos(), 380.0 + 120.0 * t.sin());
        scene.call(INPUT_POINTER_MOVE, json!({ "x": x, "y": y }));
    });
}

/// The window moves under the menu bar and the Dock: full damage, live glass.
fn drag(scene: &mut Scene) {
    let id = scene.id;
    scene.each_frame(|scene, i| {
        let t = f64::from(i) / 40.0;
        let x = 320.0 + 260.0 * t.sin();
        let y = 220.0 + 240.0 * (t * 0.7).sin();
        scene.call(
            WINDOW_MOVE,
            json!({ "id": id, "x": x as i32, "y": y as i32 }),
        );
    });
    scene.call(WINDOW_MOVE, json!({ "id": id, "x": 320, "y": 160 }));
}

fn genie(scene: &mut Scene) {
    let id = scene.id;
    scene.each_frame(|scene, i| {
        alternate(
            scene,
            i,
            60,
            |s| drop(s.call(WINDOW_MINIMIZE, json!({ "id": id }))),
            |s| drop(s.call(WINDOW_FOCUS, json!({ "id": id }))),
        );
    });
    scene.call(WINDOW_FOCUS, json!({ "id": id }));
}

fn stage_p99(times: &RenderTimes, name: &str) -> Option<f64> {
    stage_at(times, name, 99)
}

/// A stage's 50th or 99th percentile.
fn stage_at(times: &RenderTimes, name: &str, at: u8) -> Option<f64> {
    times
        .stages
        .iter()
        .find(|s| s.stage == name)
        .map(|s| if at == 50 { s.p50_ms } else { s.p99_ms })
}

#[test]
#[ignore = "a bench: run it in release, see the module docs"]
fn frame_budget() {
    let renderer = std::env::var("SILICA_COMPOSITOR_HEADLESS_RENDERER").unwrap_or_default();
    let mut scene = Scene::open();
    let scenarios: [Scenario; 4] = [
        ("spinner", spinner),
        ("hover", hover),
        ("drag", drag),
        ("genie", genie),
    ];
    let mut failed = Vec::new();
    println!(
        "renderer {}: {}×{}, {} frames per scenario",
        if renderer.is_empty() {
            "llvmpipe"
        } else {
            &renderer
        },
        SCREEN.0,
        SCREEN.1,
        frames()
    );
    println!(
        "{:<9} {:>6} {:>7} {:>7} {:>7} {:>7} | stage {:>6} {:>6} {:>6} {:>6} {:>6} | glass {:>6} {:>6} {:>6}",
        "scenario",
        "frames",
        "p50",
        "p95",
        "p99",
        "max",
        "build",
        "draw",
        "submit",
        "wait",
        "gpu",
        "bdrop",
        "blur",
        "matrl"
    );
    for (name, drive) in scenarios {
        let times = scene.measure(drive);
        // Each stage at p50 and at p99, on two rows.
        for (row, at) in [("p50", 50), ("p99", 99)] {
            let stage = |s| stage_at(&times, s, at).map_or("-".to_owned(), |v| format!("{v:.3}"));
            let head = if at == 50 {
                format!(
                    "{:<9} {:>6} {:>7.3} {:>7.3} {:>7.3} {:>7.3}",
                    name, times.frames, times.p50_ms, times.p95_ms, times.p99_ms, times.max_ms
                )
            } else {
                format!("{:<46}", "")
            };
            println!(
                "{head} | {row:>5} {:>6} {:>6} {:>6} {:>6} {:>6} |       {:>6} {:>6} {:>6}",
                stage("build"),
                stage("draw"),
                stage("submit"),
                stage("wait"),
                stage("gpu"),
                stage("glass.backdrop"),
                stage("glass.blur"),
                stage("glass.material"),
            );
        }
        assert!(times.frames > 0, "{name}: no frame drawn");
        let over = match renderer.as_str() {
            "cpu" => times.p99_ms >= 2.42,
            "gpu" => times.p99_ms >= 0.5 || stage_p99(&times, "gpu").is_some_and(|g| g >= 0.5),
            _ => false,
        };
        if over {
            failed.push(name);
        }
    }
    assert!(failed.is_empty(), "over the frame budget: {failed:?}");
}
