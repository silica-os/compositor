//! Graphics tablets (tablet-v2): pens get proximity, motion, pressure, tilt, the tip and the
//! buttons, with the surface under them as focus.

use smithay::backend::input::{
    AbsolutePositionEvent, Device, DeviceCapability, Event, InputBackend, ProximityState,
    TabletToolButtonEvent, TabletToolEvent, TabletToolProximityEvent, TabletToolTipEvent,
    TabletToolTipState,
};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::tablet_manager::{TabletDescriptor, TabletSeatTrait};

use crate::state::State;

impl State {
    /// A tablet was plugged in.
    pub fn tablet_added(&mut self, device: &impl Device) {
        if device.has_capability(DeviceCapability::TabletTool) {
            let dh = self.display_handle.clone();
            self.seat
                .tablet_seat()
                .add_tablet::<State>(&dh, &TabletDescriptor::from(device));
        }
    }

    /// A tablet was unplugged; its tools go with the last one.
    pub fn tablet_removed(&mut self, device: &impl Device) {
        if device.has_capability(DeviceCapability::TabletTool) {
            let tablets = self.seat.tablet_seat();
            tablets.remove_tablet(&TabletDescriptor::from(device));
            if tablets.count_tablets() == 0 {
                tablets.clear_tools();
            }
        }
    }

    /// Where a tool is on the output.
    fn tool_location<B: InputBackend>(
        &self,
        event: &impl AbsolutePositionEvent<B>,
    ) -> Option<Point<f64, Logical>> {
        let geometry = self.space.output_geometry(&self.output)?;
        Some(event.position_transformed(geometry.size) + geometry.loc.to_f64())
    }

    /// A tool moved or one of its axes changed.
    pub fn tablet_axis<B: InputBackend>(&mut self, event: impl TabletToolEvent<B> + Event<B>) {
        let Some(location) = self.tool_location(&event) else {
            return;
        };
        let tablets = self.seat.tablet_seat();
        let tablet = tablets.get_tablet(&TabletDescriptor::from(&event.device()));
        let tool = tablets.get_tool(&event.tool());
        // The pen also moves the pointer, so hover and the cursor follow it.
        self.pointer_move_to(location, event.time_msec());
        let (Some(tablet), Some(tool)) = (tablet, tool) else {
            return;
        };
        if event.pressure_has_changed() {
            tool.pressure(event.pressure());
        }
        if event.distance_has_changed() {
            tool.distance(event.distance());
        }
        if event.tilt_has_changed() {
            tool.tilt(event.tilt());
        }
        if event.slider_has_changed() {
            tool.slider_position(event.slider_position());
        }
        if event.rotation_has_changed() {
            tool.rotation(event.rotation());
        }
        if event.wheel_has_changed() {
            tool.wheel(event.wheel_delta(), event.wheel_delta_discrete());
        }
        let focus = self.tool_focus(location);
        tool.motion(
            location,
            focus,
            &tablet,
            SERIAL_COUNTER.next_serial(),
            event.time_msec(),
        );
    }

    /// A tool came near the tablet or left it.
    pub fn tablet_proximity<B: InputBackend>(&mut self, event: impl TabletToolProximityEvent<B>) {
        let Some(location) = self.tool_location(&event) else {
            return;
        };
        let dh = self.display_handle.clone();
        let tablets = self.seat.tablet_seat();
        let Some(tablet) = tablets.get_tablet(&TabletDescriptor::from(&event.device())) else {
            return;
        };
        let tool = tablets.add_tool::<State>(self, &dh, &event.tool());
        match event.state() {
            ProximityState::In => {
                self.pointer_move_to(location, event.time_msec());
                if let Some(focus) = self.tool_focus(location) {
                    tool.proximity_in(
                        location,
                        focus,
                        &tablet,
                        SERIAL_COUNTER.next_serial(),
                        event.time_msec(),
                    );
                }
            }
            ProximityState::Out => tool.proximity_out(event.time_msec()),
        }
    }

    /// The pen's tip touched the tablet or lifted.
    pub fn tablet_tip<B: InputBackend>(&mut self, event: impl TabletToolTipEvent<B>) {
        let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) else {
            return;
        };
        match event.tip_state() {
            TabletToolTipState::Down => {
                // Like a click: the window under the pen comes forward.
                if let Some(location) = self.tool_location(&event) {
                    let window = self.space.element_under(location).map(|(w, _)| w.clone());
                    if let Some(window) = window {
                        self.space.raise_element(&window, true);
                        self.focus_window(Some(window));
                    }
                }
                tool.tip_down(SERIAL_COUNTER.next_serial(), event.time_msec());
            }
            TabletToolTipState::Up => tool.tip_up(event.time_msec()),
        }
    }

    /// A button on the pen.
    pub fn tablet_button<B: InputBackend>(&mut self, event: impl TabletToolButtonEvent<B>) {
        if let Some(tool) = self.seat.tablet_seat().get_tool(&event.tool()) {
            tool.button(
                event.button(),
                event.button_state(),
                SERIAL_COUNTER.next_serial(),
                event.time_msec(),
            );
        }
    }

    /// The surface a tool points at.
    fn tool_focus(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(
        smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
        Point<f64, Logical>,
    )> {
        self.surface_under(location)
    }
}
