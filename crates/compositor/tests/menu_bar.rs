//! The menu bar strip (silica-menu-bar-v1, #12): the frontmost app's titles at its start,
//! the status items lined up from the right, the windows kept below it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

mod common;

use common::client::{PopupSpec, TestWindow};
use common::{Headless, WIDTH, pixel, screenshot};
use serde_json::Value;
use silica_control::commands::{INPUT_CLICK, MENU_BAR, WINDOW_LIST, WindowInfo};
use silica_protocols::menu_bar::client::silica_menu_titles_v1::AppAction;

const BLUE: [u8; 4] = [0xff, 0, 0, 0xff];

fn strip(compositor: &Headless) -> Value {
    compositor.client().call(MENU_BAR, Value::Null).unwrap()
}

/// The (kind, x, width) of the strip's surfaces, left to right.
fn surfaces(compositor: &Headless) -> Vec<(String, i64, i64)> {
    let state = strip(compositor);
    let mut out: Vec<(String, i64, i64)> = state["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["x"].as_i64().unwrap(),
                s["width"].as_i64().unwrap(),
            )
        })
        .collect();
    out.sort_by_key(|s| s.1);
    out
}

#[test]
fn the_titles_show_only_while_their_app_is_in_front() {
    let compositor = Headless::start("menu-bar-titles");
    let mut first = TestWindow::open(&compositor.wayland, "first");
    first.add_to_menu_bar(None, 200, BLUE);
    let (room, shown) = first.titles_state();
    assert_eq!(room, Some((WIDTH - 8, 32)), "the room before the items");
    assert_eq!(shown, Some(true));
    assert_eq!(surfaces(&compositor), vec![("titles".to_owned(), 0, 200)]);

    // Another app comes to the front: its titles replace the first's.
    let mut second = TestWindow::open(&compositor.wayland, "second");
    second.add_to_menu_bar(None, 150, BLUE);
    assert_eq!(second.titles_state().1, Some(true));
    assert_eq!(first.titles_state().1, Some(false));
    assert_eq!(surfaces(&compositor), vec![("titles".to_owned(), 0, 150)]);

    // A click brings the first app back to the front.
    compositor
        .client()
        .call(INPUT_CLICK, serde_json::json!({"x": 54.0, "y": 50.0}))
        .unwrap();
    assert_eq!(first.titles_state().1, Some(true));
    assert_eq!(second.titles_state().1, Some(false));
    assert_eq!(surfaces(&compositor), vec![("titles".to_owned(), 0, 200)]);

    // Its titles go: the strip shows none.
    first.remove_from_menu_bar(0);
    assert_eq!(surfaces(&compositor), vec![]);
}

#[test]
fn status_items_line_up_from_the_right() {
    let compositor = Headless::start("menu-bar-items");
    let mut app = TestWindow::open(&compositor.wayland, "app");
    app.add_to_menu_bar(None, 60, BLUE);
    // Control Center's items in their order (0 rightmost: the clock), then the apps' items,
    // the newest leftmost.
    let _icon = app.add_to_menu_bar(Some((true, 1)), 36, BLUE);
    let _clock = app.add_to_menu_bar(Some((true, 0)), 120, BLUE);
    let older = app.add_to_menu_bar(Some((false, 0)), 40, BLUE);
    let _newer = app.add_to_menu_bar(Some((false, 0)), 50, BLUE);
    let right = i64::from(WIDTH) - 8;
    let clock = right - 120;
    let icon = clock - 36;
    let older_x = icon - 40;
    let newer_x = older_x - 50;
    assert_eq!(
        surfaces(&compositor),
        vec![
            ("titles".to_owned(), 0, 60),
            ("item".to_owned(), newer_x, 50),
            ("item".to_owned(), older_x, 40),
            ("item".to_owned(), icon, 36),
            ("item".to_owned(), clock, 120),
        ]
    );
    assert_eq!(app.titles_state().0, Some((newer_x as u32, 32)));

    // A hidden item leaves no gap.
    app.set_item_visible(older, false);
    let state = surfaces(&compositor);
    assert_eq!(state[1], ("item".to_owned(), icon - 50, 50), "{state:?}");
    assert_eq!(app.titles_state().0, Some(((icon - 50) as u32, 32)));
}

#[test]
fn windows_open_below_the_strip() {
    let compositor = Headless::start("menu-bar-windows");
    let mut first = TestWindow::open(&compositor.wayland, "first");
    first.add_to_menu_bar(None, 100, BLUE);
    assert_eq!(strip(&compositor)["visible"], Value::Bool(true));
    let _second = TestWindow::open(&compositor.wayland, "second");
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    let second = list
        .iter()
        .find(|w| w.title.as_deref() == Some("second"))
        .unwrap();
    assert!(second.y >= 32, "{list:?}");
}

#[test]
fn a_menu_hangs_from_its_title_and_stays_on_screen() {
    let compositor = Headless::start("menu-bar-popup");
    let mut app = TestWindow::open(&compositor.wayland, "app");
    let titles = app.add_to_menu_bar(None, 200, [0; 4]);
    // A menu under a title near the right edge: it slides back onto the screen, under the bar.
    app.open_strip_popup(
        titles,
        PopupSpec {
            anchor_rect: (180, 0, 20, 32),
            size: (100, 60),
            adjustment: 1,
            grab: false,
            nested: false,
            transparent: false,
        },
    );
    let configure = *app.popup_configures().last().expect("no configure");
    assert_eq!((configure.1, configure.2, configure.3), (32, 100, 60));
    assert!(
        configure.0 + 100 <= WIDTH as i32,
        "leaves the screen: {configure:?}"
    );
    // It is drawn there, over the window.
    let shot = screenshot(&compositor);
    let x = (configure.0 + 50) as u32;
    let [r, g, b, _] = pixel(&shot, x, 60);
    assert!(
        r > 200 && g < 60 && b < 60,
        "not the red menu at ({x}, 60): {r} {g} {b}"
    );
}

#[test]
fn the_front_app_hides_and_shows_apps_from_its_menu() {
    let compositor = Headless::start("menu-bar-app-actions");
    let app = |id: &str| common::client::APP_ID.with(|a| *a.borrow_mut() = id.to_owned());
    app("org.example.First");
    let mut first = TestWindow::open(&compositor.wayland, "first");
    let first_titles = first.add_to_menu_bar(None, 100, BLUE);
    app("org.example.Second");
    let mut second = TestWindow::open_sized(&compositor.wayland, "second", (240, 140));
    let second_titles = second.add_to_menu_bar(None, 100, BLUE);
    let list: Vec<WindowInfo> =
        serde_json::from_value(compositor.client().call(WINDOW_LIST, Value::Null).unwrap())
            .unwrap();
    let w = list
        .iter()
        .find(|w| w.title.as_deref() == Some("second"))
        .unwrap();
    let small = list
        .iter()
        .find(|w| w.title.as_deref() == Some("first"))
        .unwrap();
    // A corner of the big window, clear of the small one.
    let (x, y) = ((w.x + w.width - 8) as u32, (w.y + w.height - 8) as u32);
    assert!(
        x as i32 >= small.x + small.width || y as i32 >= small.y + small.height,
        "{list:?}"
    );
    let at = |c: &Headless| pixel(&screenshot(c), x, y);
    let shown = at(&compositor);

    // Only the app in front asks: the first's request does nothing.
    first.app_action(first_titles, AppAction::HideOthers);
    assert_eq!(at(&compositor), shown);

    // Hide: the second app leaves the screen and the first comes to the front.
    second.app_action(second_titles, AppAction::Hide);
    assert_ne!(at(&compositor), shown);
    assert_eq!(first.titles_state().1, Some(true));

    // Show All brings it back, behind the app in front.
    first.app_action(first_titles, AppAction::ShowAll);
    assert_eq!(at(&compositor), shown);
    assert_eq!(first.titles_state().1, Some(true));

    // Hide Others takes it away again.
    first.app_action(first_titles, AppAction::HideOthers);
    assert_ne!(at(&compositor), shown);
}
