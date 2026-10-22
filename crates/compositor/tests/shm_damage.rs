//! A client that repaints with a new shm buffer and only the changed rectangle as damage
//! (what the embedder does since silica-os/silica-desktop#205) shows every update on the screen
//! (silica-os/silica-desktop#233).

// Test code: an error must make the test fail (AGENTS.md applies to non-test code).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::{RED_BGRA, TestWindow, WINDOW};
use common::{Headless, screenshot};

const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [255, 0, 0, 255];

fn frame(patches: &[((u32, u32), [u8; 4])]) -> Vec<u8> {
    let (w, h) = WINDOW;
    let mut pixels = RED_BGRA.repeat((w * h) as usize);
    for ((px, py), color) in patches {
        for y in *py..py + 20 {
            for x in *px..px + 20 {
                let at = ((y * w + x) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(color);
            }
        }
    }
    pixels
}

/// How many pixels of the screenshot are exactly `color` (RGBA).
fn count(shot: &[u8], color: [u8; 4]) -> usize {
    shot.as_chunks::<4>()
        .0
        .iter()
        .filter(|px| **px == color)
        .count()
}

#[test]
fn every_partially_damaged_frame_is_on_the_screen() {
    let compositor = Headless::start("shm_damage");
    let mut window = TestWindow::open(&compositor.wayland, "damage");
    let green = [0, 255, 0, 255];
    let blue = [0, 0, 255, 255];

    // Frame 2: a green patch, damaged alone.
    let two = frame(&[((10, 10), GREEN)]);
    window.repaint(&two, Some((10, 10, 20, 20)));
    let shot = screenshot(&compositor);
    assert_eq!(count(&shot, green), 400, "green patch after frame 2");

    // Frame 3: a blue patch elsewhere, damaged alone.
    let three = frame(&[((10, 10), GREEN), ((60, 30), BLUE)]);
    window.repaint(&three, Some((60, 30, 20, 20)));
    let shot = screenshot(&compositor);
    assert_eq!(count(&shot, blue), 400, "blue patch after frame 3");
    assert_eq!(count(&shot, green), 400, "green patch kept after frame 3");

    // Frame 4: the green patch goes, damaged alone.
    let four = frame(&[((60, 30), BLUE)]);
    window.repaint(&four, Some((10, 10, 20, 20)));
    let shot = screenshot(&compositor);
    assert_eq!(count(&shot, green), 0, "green patch gone after frame 4");
    assert_eq!(count(&shot, blue), 400, "blue patch kept after frame 4");
}
