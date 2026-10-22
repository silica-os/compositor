//! Input: keyboard and pointer to the windows.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    TouchEvent,
};
use smithay::input::keyboard::{FilterResult, Keycode, xkb};
use smithay::input::pointer::{self as ptr, AxisFrame, ButtonEvent, MotionEvent, PointerHandle};
use smithay::input::touch;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER};
use smithay::wayland::shell::wlr_layer::Layer;
use tracing::warn;

use compositor_render::titlebar::{self, Part};

use crate::decorations::{self, Hit};
use crate::state::State;

impl State {
    /// The cursor to draw: The compositor's own shape if it imposes one, otherwise the client's.
    pub fn effective_cursor(&self) -> smithay::input::pointer::CursorImageStatus {
        match self.cursor_override {
            Some(icon) => smithay::input::pointer::CursorImageStatus::Named(icon),
            None => self.cursor_status.clone(),
        }
    }

    /// The frame of the window under the pointer while the window screenshot mode is on.
    pub fn window_veil_view(&self) -> Option<Rectangle<i32, Logical>> {
        if !self.window_veil {
            return None;
        }
        let location = self.seat.get_pointer()?.current_location();
        let window = match self.window_hit(location)? {
            Hit::Titlebar(window, _, _) | Hit::Content(window) => window,
        };
        self.window_frame(&window)
    }

    /// Moves the pointer to a point of the desktop (logical pixels).
    pub fn pointer_move_to(&mut self, location: Point<f64, Logical>, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if self.locked {
            self.locked_pointer_motion(location, time);
            return;
        }
        if self.resizing.is_some() {
            self.resize_to(location);
        }
        self.hot_corner_check(location);
        // Dragging by the bar: the frame follows the grab point.
        if let Some((window, grab)) = self.moving.clone() {
            let bar = decorations::titlebar_height(&window);
            let mut frame = (location - grab).to_i32_round::<i32>();
            // The frame (bar included) never goes above the menu bar while it moves (#188).
            frame.y = frame.y.max(self.usable_area().loc.y);
            self.space
                .map_element(window, frame + Point::from((0, bar)), false);
        }
        let over_layer = self
            .layer_under(&[Layer::Overlay, Layer::Top], location)
            .is_some();
        // In Panorama the windows do not receive the pointer (they are drawn elsewhere).
        let in_panorama = self.panorama.is_some() && !self.over_dock_layer(location);
        if self.panorama.is_some() {
            self.panorama_pointer(location, std::time::Instant::now());
        }
        // An open menu or panel (#149): its own app's windows lose the pointer (no hover, no
        // tooltips), and everything outside it shows the arrow (the window below does not
        // have the keyboard, so no text cursor).
        let (hover_blocked, outside_transients) = self.hover_on_transients(location);
        let hit = if over_layer || in_panorama || hover_blocked {
            None
        } else {
            self.window_hit(location)
        };
        let hover = match &hit {
            Some(Hit::Titlebar(window, _, local))
                if titlebar::over_buttons(
                    *local,
                    crate::decorations::button_bar_height(window),
                ) =>
            {
                Some(window.clone())
            }
            _ => None,
        };
        // Symbols: they appear on entering, they disappear shortly after leaving.
        let now = std::time::Instant::now();
        let same = matches!((&hover, &self.buttons_hover),
            (Some(w), Some(h)) if *w == h.window && h.left.is_none());
        if !same {
            match &hover {
                Some(window) => {
                    self.buttons_hover = Some(crate::state::ButtonsHover {
                        window: window.clone(),
                        entered: now,
                        left: None,
                    });
                }
                None => {
                    if let Some(h) = self.buttons_hover.as_mut()
                        && h.left.is_none()
                    {
                        h.left = Some(now);
                    }
                }
            }
        }
        self.titlebar_press_inside = match (&self.titlebar_press, &hit) {
            (Some((pressed, part)), Some(Hit::Titlebar(window, over, _))) => {
                pressed == window && part == over
            }
            _ => false,
        };
        self.titlebar_hover = hover;
        // Resting on the green button opens the arrangement popover (TL-GLASS, #94).
        let on_zoom = match &hit {
            Some(Hit::Titlebar(window, Part::Zoom, _))
                if !crate::render::buttons_disabled(window).zoom() =>
            {
                Some(window.clone())
            }
            _ => None,
        };
        match on_zoom {
            Some(window) => {
                if self.zoom_hover.as_ref().map(|(w, _)| w) != Some(&window) {
                    self.arm_zoom_hover(&window);
                }
            }
            None => self.zoom_hover = None,
        }
        // The compositor's own shapes: the resize edges (MU-08) win over the client below, the bar
        // and the desktop have the arrow; elsewhere the client's choice.
        let edges = if let Some(resize) = &self.resizing {
            Some((resize.window.clone(), resize.edges))
        } else if !over_layer && !in_panorama && !hover_blocked && self.moving.is_none() {
            self.resize_hit(location)
        } else {
            None
        };
        let on_bar = matches!(hit, Some(Hit::Titlebar(..)));
        // On the bar the pointer is not on any client surface; during a resize the whole grab
        // belongs to the compositor (the desktop below must not see the motion, #214).
        let under = match hit {
            Some(Hit::Titlebar(..)) => None,
            _ if in_panorama || hover_blocked || self.resizing.is_some() => None,
            _ => self.surface_under(location),
        };
        self.cursor_override = match edges {
            Some((window, edges)) => Some(self.resize_cursor(&window, edges)),
            None if on_bar || under.is_none() || self.moving.is_some() || outside_transients => {
                Some(smithay::input::pointer::CursorIcon::Default)
            }
            None => None,
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// Presses or releases a button; the press gives the focus to the window below.
    pub fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let location = pointer.current_location();
        // Locked: the lock surface alone gets the button.
        if self.locked {
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial,
                    time,
                },
            );
            pointer.frame(self);
            return;
        }
        // Panorama takes the clicks, except those on the Dock and on the panels.
        if state == ButtonState::Pressed && self.panorama_open() && !self.over_dock_layer(location)
        {
            self.panorama_press = true;
            self.panorama_press_at(location, std::time::Instant::now());
            return;
        }
        if state == ButtonState::Released && self.panorama_press {
            self.panorama_press = false;
            self.panorama_release(location, std::time::Instant::now());
            return;
        }
        // An open menu or panel (#149): a press outside it ends it; per transient and target
        // the press goes on, only activates the window or stops there, and then its release
        // (and the drag in between) too.
        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            match self.press_on_transients(location, button == BTN_LEFT) {
                crate::transient::Press::Pass => {}
                crate::transient::Press::Activate => {
                    self.activate_under(location);
                    self.transient_press = true;
                    self.refresh_pointer();
                    return;
                }
                crate::transient::Press::Close => {
                    self.transient_press = true;
                    self.refresh_pointer();
                    return;
                }
            }
        }
        if state == ButtonState::Released && self.transient_press {
            self.transient_press = false;
            return;
        }
        // A resize owns its press, the motion and the release: no client (the desktop below
        // the edge included) sees them (#214).
        let was_resizing = self.resizing.is_some();
        let mut grab = false;
        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            match self.layer_under(&[Layer::Overlay, Layer::Top], location) {
                // A click on a layer does not touch the windows; the focus only if it asks for it.
                Some((layer, _)) => {
                    if layer.can_receive_keyboard_focus() {
                        self.focus_layer(&layer);
                    }
                }
                // The frame edges resize, even just outside (MU-08).
                None if !self.panorama_open()
                    && let Some((window, edges)) = self.resize_hit(location) =>
                {
                    self.focus_window(Some(window.clone()));
                    self.start_resize(window, edges, location);
                    grab = true;
                }
                None => match self.window_hit(location) {
                    Some(Hit::Titlebar(window, part, local)) => {
                        self.focus_window(Some(window.clone()));
                        if part == Part::Bar {
                            if button == BTN_LEFT && self.bar_double_click(&window, time, location)
                            {
                                self.bar_double_clicked(&window);
                            } else {
                                self.moving = Some((window, local));
                            }
                        } else {
                            self.titlebar_press = Some((window.clone(), part));
                            self.titlebar_press_inside = true;
                            if part == Part::Zoom
                                && !crate::render::buttons_disabled(&window).zoom()
                            {
                                self.arm_window_menu(&window);
                            }
                        }
                    }
                    Some(Hit::Content(window)) => self.focus_window(Some(window)),
                    None => self.desktop_pressed(),
                },
            }
        }
        if state == ButtonState::Pressed {
            self.press_serial = Some(serial);
        }
        if state == ButtonState::Released {
            self.press_serial = None;
            self.moving = None;
            self.end_resize();
            if let Some((window, part)) = self.titlebar_press.take()
                && let Some(Hit::Titlebar(over, over_part, _)) = self.window_hit(location)
                && over == window
                && over_part == part
            {
                self.titlebar_action(&window, part);
            }
        }
        if grab || (state == ButtonState::Released && was_resizing) {
            return;
        }
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// A press on a window's title bar: whether it is the second of a double click (the same
    /// window, within the double click time and a few points). Remembers the press otherwise.
    fn bar_double_click(
        &mut self,
        window: &smithay::desktop::Window,
        time: u32,
        location: Point<f64, Logical>,
    ) -> bool {
        let within = self
            .input_config
            .double_click_ms
            .unwrap_or(crate::config::DEFAULT_DOUBLE_CLICK_MS);
        let double = self.bar_press.as_ref().is_some_and(|(last, at, place)| {
            last == window
                && time.wrapping_sub(*at) <= within
                && (place.x - location.x).abs() <= 6.0
                && (place.y - location.y).abs() <= 6.0
        });
        self.bar_press = if double {
            None
        } else {
            Some((window.clone(), time, location))
        };
        double
    }

    /// The double click on the title bar does what Settings › Desktop & Dock says.
    fn bar_double_clicked(&mut self, window: &smithay::desktop::Window) {
        use crate::config::TitleBarDoubleClick as Action;
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
        let result = match self.dock_config.title_bar_double_click {
            Action::Nothing => Ok(()),
            Action::Minimize => {
                if crate::render::buttons_disabled(window).minimize() {
                    Ok(())
                } else {
                    self.minimize_window(window)
                }
            }
            Action::Zoom => {
                if crate::render::buttons_disabled(window).zoom() {
                    Ok(())
                } else {
                    let zoomed = window.toplevel().is_some_and(|t| {
                        t.current_state()
                            .states
                            .contains(xdg_toplevel::State::Maximized)
                    });
                    self.zoom_window(window, !zoomed)
                }
            }
        };
        if let Err(err) = result {
            warn!("title bar double click: {err}");
        }
    }

    /// The wheel or the touchpad, real or synthetic (`input.scroll`), where the pointer is.
    /// An open menu drops it on its own app's windows; it never closes anything (#149).
    pub fn pointer_axis(&mut self, frame: AxisFrame) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        if self.locked {
            pointer.axis(self, frame);
            pointer.frame(self);
            return;
        }
        if self.scroll_on_transients(pointer.current_location()) {
            return;
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// A key, real or synthetic (`input.key`): first the catalog shortcuts, which run their
    /// command and do not reach the app; the rest goes to the focused surface.
    pub fn handle_key(&mut self, code: Keycode, state: KeyState, time: u32) {
        let code = self.logo_key.remap(code);
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        // Locked: the keys go to the lock surface and to nobody else, whatever had the focus.
        if self.locked {
            self.lock_focus();
        }
        let pressed = state == KeyState::Pressed;
        // Settings recording a new shortcut gets the keys, whatever they are bound to.
        let locked = self.locked;
        let capturing = self.shortcuts_capture
            && self.front_app_id().as_deref() == Some(crate::config::SHORTCUT_CAPTURE_APP);
        // An app that inhibits shortcuts (a remote desktop, a VM) gets them while focused.
        let inhibited = self.shortcuts_inhibited();
        let shortcuts = if capturing || locked || inhibited {
            Vec::new()
        } else {
            self.shortcuts.clone()
        };
        // The input source shortcut belongs to the apps while there is only one source.
        let one_source = self.keyboard_config.layouts.len() < 2;
        // Esc closes the open view: its own command is run.
        let switcher = self.switcher_active && !locked;
        let logo_released = std::cell::Cell::new(false);
        let vt = std::cell::Cell::new(None);
        let close_view = self
            .panorama_mode()
            .filter(|_| !locked)
            .map(|mode| match mode {
                crate::panorama::Mode::Panorama => silica_control::commands::PANORAMA_TOGGLE,
                crate::panorama::Mode::App => silica_control::commands::PANORAMA_APP,
                crate::panorama::Mode::Desktop => silica_control::commands::DESKTOP_SHOW,
            });
        let command = keyboard.input::<&'static str, _>(
            self,
            code,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |_, modifiers, keysym| {
                let key = xkb::keysym_get_name(
                    keysym
                        .raw_latin_sym_or_raw_current_sym()
                        .unwrap_or(keysym.modified_sym()),
                );
                // Ctrl+Alt+Fn: another VT (only the real session has VTs).
                if !locked && let Some(n) = vt_switch(keysym.modified_sym()) {
                    if pressed {
                        vt.set(Some(n));
                    }
                    return FilterResult::Intercept(VT_SWITCH);
                }
                // App switcher: Esc cancels, the released Logo key confirms
                // (the release reaches the app anyway, which saw the press).
                if switcher {
                    if pressed && key == "Escape" {
                        return FilterResult::Intercept(silica_control::commands::SWITCHER_CANCEL);
                    }
                    // Up or Down: App Exposé of the chosen app (the switcher goes away and
                    // Exposé stays after the Logo key is released).
                    if pressed && (key == "Up" || key == "Down") {
                        return FilterResult::Intercept(silica_control::commands::SWITCHER_EXPOSE);
                    }
                    if !pressed && (key == "Super_L" || key == "Super_R") {
                        logo_released.set(true);
                    }
                }
                if pressed
                    && key == "Escape"
                    && let Some(id) = close_view
                {
                    return FilterResult::Intercept(id);
                }
                match shortcuts.iter().find(|(shortcut, id)| {
                    pressed
                        && !(one_source && *id == silica_control::commands::KEYBOARD_NEXT_SOURCE)
                        && shortcut.matches(
                            modifiers.logo,
                            modifiers.ctrl,
                            modifiers.alt,
                            modifiers.shift,
                            &key,
                        )
                }) {
                    Some((_, id)) => FilterResult::Intercept(*id),
                    None => FilterResult::Forward,
                }
            },
        );
        // An xkb option (grp:…) may have moved to another source: the shell shows the active one.
        if !one_source {
            self.announce_input_sources();
        }
        if command == Some(VT_SWITCH) {
            if let Some(n) = vt.get() {
                self.change_vt(n);
            }
            return;
        }
        let command = match command {
            None if logo_released.get() => Some(silica_control::commands::SWITCHER_COMMIT),
            other => other,
        };
        if let Some(id) = command {
            let response = self.execute_command(id, serde_json::Value::Null);
            if let Err(err) = response {
                warn!("shortcut {id}: {err}");
            }
        }
    }

    /// The pointer is on a front layer that is not the menu bar (the Dock).
    fn over_dock_layer(&self, location: Point<f64, Logical>) -> bool {
        self.layer_under(&[Layer::Overlay, Layer::Top], location)
            .is_some_and(|(layer, _)| layer.namespace() != "menubar")
    }

    /// A gesture event for the app under the pointer, framed.
    fn client_gesture(&mut self, send: impl FnOnce(&mut Self, &PointerHandle<Self>)) {
        if self.locked {
            return;
        }
        if let Some(pointer) = self.seat.get_pointer() {
            send(self, &pointer);
            pointer.frame(self);
        }
    }

    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        if !matches!(
            event,
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. }
        ) {
            self.user_activity();
        }
        match event {
            InputEvent::DeviceAdded { device } => self.tablet_added(&device),
            InputEvent::DeviceRemoved { device } => self.tablet_removed(&device),
            InputEvent::TabletToolAxis { event } => self.tablet_axis(event),
            InputEvent::TabletToolProximity { event } => self.tablet_proximity(event),
            InputEvent::TabletToolTip { event } => self.tablet_tip(event),
            InputEvent::TabletToolButton { event } => self.tablet_button(event),
            InputEvent::Keyboard { event } => {
                self.handle_key(event.key_code(), event.state(), Event::time_msec(&event));
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(geometry) = self.space.output_geometry(&self.output) else {
                    return;
                };
                let location = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                self.pointer_move_to(location, event.time_msec());
            }
            // Relative motion (a mouse or touchpad through libinput): from where the pointer
            // is, kept inside the output.
            InputEvent::PointerMotion { event } => {
                let (Some(pointer), Some(geometry)) = (
                    self.seat.get_pointer(),
                    self.space.output_geometry(&self.output),
                ) else {
                    return;
                };
                let from = pointer.current_location();
                let location = clamp_to(from + event.delta(), geometry);
                let location = self.constrained_motion(from, location);
                // Games and 3D views read the unclamped movement (relative-pointer).
                let focus = self.surface_under(from);
                pointer.relative_motion(
                    self,
                    focus,
                    &ptr::RelativeMotionEvent {
                        delta: event.delta(),
                        delta_unaccel: event.delta_unaccel(),
                        utime: event.time(),
                    },
                );
                self.pointer_move_to(location, event.time_msec());
            }
            InputEvent::PointerButton { event } => {
                self.pointer_button(event.button_code(), event.state(), event.time_msec());
            }
            InputEvent::PointerAxis { event } => {
                let source = event.source();
                // Scroll speed (input.*): wheels are mice, fingers are touchpads.
                let factor = match source {
                    AxisSource::Finger | AxisSource::Continuous => {
                        self.input_config.touchpad.scroll_speed
                    }
                    _ => self.input_config.mouse.scroll_speed,
                };
                let mut frame = AxisFrame::new(event.time_msec()).source(source);
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let v120 = event.amount_v120(axis).map(|v| v * factor);
                    let amount = event
                        .amount(axis)
                        .map(|a| a * factor)
                        .unwrap_or_else(|| v120.unwrap_or(0.0) * 15.0 / 120.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(discrete) = v120 {
                            frame = frame.v120(axis, discrete as i32);
                        }
                    } else if source == AxisSource::Finger {
                        frame = frame.stop(axis);
                    }
                }
                self.pointer_axis(frame);
            }
            InputEvent::GestureSwipeBegin { event } => {
                // Three or more fingers belong to the system (spaces, Panorama); fewer go to
                // the app under the pointer (pointer-gestures).
                let fingers = event.fingers();
                self.protocols.client_gesture = fingers < 3;
                if self.protocols.client_gesture {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_swipe_begin(
                            state,
                            &ptr::GestureSwipeBeginEvent {
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                                fingers,
                            },
                        );
                    });
                } else {
                    self.gesture_begin(crate::gestures::GestureKind::Swipe, fingers);
                }
            }
            InputEvent::GestureSwipeUpdate { event } => {
                let delta = event.delta();
                if self.protocols.client_gesture {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_swipe_update(
                            state,
                            &ptr::GestureSwipeUpdateEvent {
                                time: event.time_msec(),
                                delta,
                            },
                        );
                    });
                } else {
                    self.gesture_update(delta.x, delta.y, None);
                }
            }
            InputEvent::GestureSwipeEnd { event } => {
                if std::mem::take(&mut self.protocols.client_gesture) {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_swipe_end(
                            state,
                            &ptr::GestureSwipeEndEvent {
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                                cancelled: event.cancelled(),
                            },
                        );
                    });
                } else {
                    self.gesture_end(event.cancelled());
                }
            }
            InputEvent::GesturePinchBegin { event } => {
                // Two-finger pinches zoom in the app; four fingers are the system's.
                let fingers = event.fingers();
                self.protocols.client_gesture = fingers < 4;
                if self.protocols.client_gesture {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_pinch_begin(
                            state,
                            &ptr::GesturePinchBeginEvent {
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                                fingers,
                            },
                        );
                    });
                } else {
                    self.gesture_begin(crate::gestures::GestureKind::Pinch, fingers);
                }
            }
            InputEvent::GesturePinchUpdate { event } => {
                if self.protocols.client_gesture {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_pinch_update(
                            state,
                            &ptr::GesturePinchUpdateEvent {
                                time: event.time_msec(),
                                delta: event.delta(),
                                scale: event.scale(),
                                rotation: event.rotation(),
                            },
                        );
                    });
                } else {
                    self.gesture_update(0.0, 0.0, Some(event.scale()));
                }
            }
            InputEvent::GesturePinchEnd { event } => {
                if std::mem::take(&mut self.protocols.client_gesture) {
                    self.client_gesture(|state, pointer| {
                        pointer.gesture_pinch_end(
                            state,
                            &ptr::GesturePinchEndEvent {
                                serial: SERIAL_COUNTER.next_serial(),
                                time: event.time_msec(),
                                cancelled: event.cancelled(),
                            },
                        );
                    });
                } else {
                    self.gesture_end(event.cancelled());
                }
            }
            InputEvent::GestureHoldBegin { event } => {
                self.client_gesture(|state, pointer| {
                    pointer.gesture_hold_begin(
                        state,
                        &ptr::GestureHoldBeginEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                            fingers: event.fingers(),
                        },
                    );
                });
            }
            InputEvent::GestureHoldEnd { event } => {
                self.client_gesture(|state, pointer| {
                    pointer.gesture_hold_end(
                        state,
                        &ptr::GestureHoldEndEvent {
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                            cancelled: event.cancelled(),
                        },
                    );
                });
            }
            InputEvent::TouchDown { event } => {
                let Some(geometry) = self.space.output_geometry(&self.output) else {
                    return;
                };
                let location = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                // A touch on a window brings it forward, like a click.
                let window = self.space.element_under(location).map(|(w, _)| w.clone());
                if let Some(window) = window {
                    self.space.raise_element(&window, true);
                    self.focus_window(Some(window));
                }
                let focus = if self.locked {
                    None
                } else {
                    self.surface_under(location)
                };
                if let Some(touch) = self.seat.get_touch() {
                    touch.down(
                        self,
                        focus,
                        &touch::DownEvent {
                            slot: event.slot(),
                            location,
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                        },
                    );
                }
            }
            InputEvent::TouchMotion { event } => {
                let Some(geometry) = self.space.output_geometry(&self.output) else {
                    return;
                };
                let location = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                let focus = self.surface_under(location);
                if let Some(touch) = self.seat.get_touch() {
                    touch.motion(
                        self,
                        focus,
                        &touch::MotionEvent {
                            slot: event.slot(),
                            location,
                            time: event.time_msec(),
                        },
                    );
                }
            }
            InputEvent::TouchUp { event } => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.up(
                        self,
                        &touch::UpEvent {
                            slot: event.slot(),
                            serial: SERIAL_COUNTER.next_serial(),
                            time: event.time_msec(),
                        },
                    );
                }
            }
            InputEvent::TouchFrame { .. } => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.frame(self);
                }
            }
            InputEvent::TouchCancel { .. } => {
                if let Some(touch) = self.seat.get_touch() {
                    touch.cancel(self);
                }
            }
            _ => {}
        }
    }
}

/// The primary button (linux/input-event-codes.h).
const BTN_LEFT: u32 = 0x110;

/// The marker returned by the key filter for a VT switch: not a command.
const VT_SWITCH: &str = "session.vt";

/// A point kept inside `area` (the last pixel included).
fn clamp_to(point: Point<f64, Logical>, area: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    let (x0, y0) = (f64::from(area.loc.x), f64::from(area.loc.y));
    let x1 = x0 + f64::from(area.size.w.max(1)) - 1.0;
    let y1 = y0 + f64::from(area.size.h.max(1)) - 1.0;
    (point.x.clamp(x0, x1), point.y.clamp(y0, y1)).into()
}

/// The VT of the XF86Switch_VT_1…12 keysyms (Ctrl+Alt+F1…F12 in the default keymaps).
fn vt_switch(keysym: xkb::Keysym) -> Option<i32> {
    let first = xkb::Keysym::XF86_Switch_VT_1.raw();
    let last = xkb::Keysym::XF86_Switch_VT_12.raw();
    let raw = keysym.raw();
    (first..=last)
        .contains(&raw)
        .then(|| i32::try_from(raw - first + 1).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{clamp_to, vt_switch};
    use smithay::input::keyboard::xkb;
    use smithay::utils::Rectangle;

    #[test]
    fn relative_motion_stays_on_the_output() {
        let area = Rectangle::new((0, 0).into(), (1280, 800).into());
        assert_eq!(clamp_to((-5.0, 900.0).into(), area), (0.0, 799.0).into());
        assert_eq!(clamp_to((10.5, 20.0).into(), area), (10.5, 20.0).into());
    }

    #[test]
    fn switch_keysyms_name_their_vt() {
        assert_eq!(vt_switch(xkb::Keysym::XF86_Switch_VT_1), Some(1));
        assert_eq!(vt_switch(xkb::Keysym::XF86_Switch_VT_12), Some(12));
        assert_eq!(vt_switch(xkb::Keysym::F1), None);
    }
}
