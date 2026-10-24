//! The standard Wayland protocols every app expects beyond the core: selection, activation,
//! timing (presentation, fifo, commit timing), buffers (single pixel, alpha), input (pointer
//! constraints, relative pointer, gestures, text input and input methods, virtual keyboards,
//! shortcut inhibitors), idle inhibitors and the small toplevel extras (icon, tag, bell).
//!
//! The states live together in [`Protocols`]; the handlers that need the compositor's
//! behaviour (focus, idle, popups) are implemented here too.

use std::time::Duration;

use smithay::desktop::{PopupKind, Window};
use smithay::input::pointer::PointerHandle;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{DisplayHandle, Resource};
use smithay::utils::{Clock, Logical, Monotonic, Point, Rectangle};
use smithay::wayland::alpha_modifier::AlphaModifierState;
use smithay::wayland::commit_timing::{CommitTimerBarrierStateUserData, CommitTimingManagerState};
use smithay::wayland::compositor;
use smithay::wayland::content_type::ContentTypeState;
use smithay::wayland::fifo::{FifoBarrierCachedState, FifoManagerState};
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::input_method::{
    InputMethodHandler, InputMethodManagerState, PopupSurface as ImePopupSurface,
};
use smithay::wayland::keyboard_shortcuts_inhibit::KeyboardShortcutsInhibitorSeat as _;
use smithay::wayland::keyboard_shortcuts_inhibit::{
    KeyboardShortcutsInhibitHandler, KeyboardShortcutsInhibitState, KeyboardShortcutsInhibitor,
};
use smithay::wayland::pointer_constraints::{
    PointerConstraint, PointerConstraintsHandler, PointerConstraintsState, with_pointer_constraint,
};
use smithay::wayland::pointer_gestures::PointerGesturesState;
use smithay::wayland::presentation::PresentationState;
use smithay::wayland::relative_pointer::RelativePointerManagerState;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::single_pixel_buffer::SinglePixelBufferState;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::xdg_system_bell::{XdgSystemBellHandler, XdgSystemBellState};
use smithay::wayland::xdg_toplevel_icon::{XdgToplevelIconHandler, XdgToplevelIconManager};
use smithay::wayland::xdg_toplevel_tag::{XdgToplevelTagHandler, XdgToplevelTagManager};
use tracing::{debug, info};

use crate::state::{ClientState, State};

/// How long an activation token stays good (the same as other compositors).
const TOKEN_LIFETIME: Duration = Duration::from_secs(10);

/// The states of the standard protocols.
pub struct Protocols {
    pub clock: Clock<Monotonic>,
    pub primary_selection: PrimarySelectionState,
    pub activation: XdgActivationState,
    pub presentation: PresentationState,
    pub single_pixel_buffer: SinglePixelBufferState,
    pub alpha_modifier: AlphaModifierState,
    pub fifo: FifoManagerState,
    pub commit_timing: CommitTimingManagerState,
    pub content_type: ContentTypeState,
    pub pointer_constraints: PointerConstraintsState,
    pub relative_pointer: RelativePointerManagerState,
    pub pointer_gestures: PointerGesturesState,
    pub text_input: TextInputManagerState,
    pub input_method: InputMethodManagerState,
    pub virtual_keyboard: VirtualKeyboardManagerState,
    pub idle_inhibit: IdleInhibitManagerState,
    pub keyboard_shortcuts_inhibit: KeyboardShortcutsInhibitState,
    pub toplevel_icon: XdgToplevelIconManager,
    pub toplevel_tag: XdgToplevelTagManager,
    pub system_bell: XdgSystemBellState,
    pub tablet: smithay::wayland::tablet_manager::TabletManagerState,
    pub fractional_scale: smithay::wayland::fractional_scale::FractionalScaleManagerState,
    /// wp-security-context: only apps outside a sandbox create contexts (`security.rs`).
    pub security_context: smithay::wayland::security_context::SecurityContextState,
    /// linux-drm-syncobj: only on a DRM device that supports it (drm.rs).
    pub syncobj: Option<smithay::wayland::drm_syncobj::DrmSyncobjState>,
    /// Surfaces with a live idle inhibitor: while one is mapped, the display stays on.
    pub idle_inhibitors: Vec<WlSurface>,
    /// The gesture in progress belongs to the client under the pointer, not to the compositor.
    pub client_gesture: bool,
}

impl Protocols {
    pub fn new(dh: &DisplayHandle) -> Self {
        let clock = Clock::<Monotonic>::new();
        Self {
            primary_selection: PrimarySelectionState::new::<State>(dh),
            activation: XdgActivationState::new::<State>(dh),
            presentation: PresentationState::new::<State>(dh, clock.id() as u32),
            clock,
            single_pixel_buffer: SinglePixelBufferState::new::<State>(dh),
            alpha_modifier: AlphaModifierState::new::<State>(dh),
            fifo: FifoManagerState::new::<State>(dh),
            commit_timing: CommitTimingManagerState::new::<State>(dh),
            content_type: ContentTypeState::new::<State>(dh),
            pointer_constraints: PointerConstraintsState::new::<State>(dh),
            relative_pointer: RelativePointerManagerState::new::<State>(dh),
            pointer_gestures: PointerGesturesState::new::<State>(dh),
            text_input: TextInputManagerState::new::<State>(dh),
            input_method: InputMethodManagerState::new::<State, _>(
                dh,
                crate::security::unsandboxed,
            ),
            virtual_keyboard: VirtualKeyboardManagerState::new::<State, _>(
                dh,
                crate::security::unsandboxed,
            ),
            idle_inhibit: IdleInhibitManagerState::new::<State>(dh),
            keyboard_shortcuts_inhibit: KeyboardShortcutsInhibitState::new::<State>(dh),
            toplevel_icon: XdgToplevelIconManager::new::<State>(dh),
            toplevel_tag: XdgToplevelTagManager::new::<State>(dh),
            system_bell: XdgSystemBellState::new::<State>(dh),
            tablet: smithay::wayland::tablet_manager::TabletManagerState::new::<State>(dh),
            syncobj: None,
            security_context: smithay::wayland::security_context::SecurityContextState::new::<
                State,
                _,
            >(dh, crate::security::unsandboxed),
            fractional_scale: smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<
                State,
            >(dh),
            idle_inhibitors: Vec::new(),
            client_gesture: false,
        }
    }
}

impl State {
    /// After a frame: the fifo and commit-timing barriers of every surface are signalled, so
    /// the commits waiting on them apply. Each surface also learns the scale and transform of
    /// the display it is on (wp-fractional-scale, wl_surface v6).
    pub fn signal_frame_barriers(&mut self) {
        let now = self.protocols.clock.now();
        let mut surfaces = Vec::new();
        let mut collect = |surface: &WlSurface, output: &smithay::output::Output| {
            let scale = output.current_scale();
            let transform = output.current_transform();
            compositor::with_surface_tree_downward(
                surface,
                (),
                |_, _, _| compositor::TraversalAction::DoChildren(()),
                |surface, states, _| {
                    smithay::wayland::fractional_scale::with_fractional_scale(states, |fs| {
                        fs.set_preferred_scale(scale.fractional_scale());
                    });
                    compositor::send_surface_state(
                        surface,
                        states,
                        scale.integer_scale(),
                        transform,
                    );
                    let mut signalled = false;
                    if let Some(barrier) = states
                        .cached_state
                        .get::<FifoBarrierCachedState>()
                        .current()
                        .barrier
                        .take()
                    {
                        barrier.signal();
                        signalled = true;
                    }
                    if let Some(timer) = states.data_map.get::<CommitTimerBarrierStateUserData>()
                        && let Ok(mut timer) = timer.lock()
                    {
                        signalled |= timer.signal_until(now);
                    }
                    if signalled {
                        surfaces.push(surface.clone());
                    }
                },
                |_, _, _| true,
            );
        };
        for window in self.space.elements() {
            let output = self
                .space
                .outputs_for_element(window)
                .into_iter()
                .next()
                .unwrap_or_else(|| self.output.clone());
            if let Some(surface) = window.wl_surface() {
                collect(&surface, &output);
            }
        }
        for (window, _) in &self.hidden {
            if let Some(surface) = window.wl_surface() {
                collect(&surface, &self.output);
            }
        }
        for layer in smithay::desktop::layer_map_for_output(&self.output).layers() {
            collect(layer.wl_surface(), &self.output);
        }
        for surface in self.menu_bar.surfaces() {
            collect(&surface, &self.output);
            for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(&surface) {
                collect(popup.wl_surface(), &self.output);
            }
        }
        let dh = self.display_handle.clone();
        for surface in surfaces {
            let Some(client) = surface.client() else {
                continue;
            };
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor_state.blocker_cleared(self, &dh);
            }
        }
    }

    /// The presentation feedback of everything drawn on the output, to be marked presented
    /// when the frame reaches the screen.
    pub fn take_presentation_feedback(
        &self,
    ) -> smithay::desktop::utils::OutputPresentationFeedback {
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        let output = &self.output;
        let mut feedback = smithay::desktop::utils::OutputPresentationFeedback::new(output);
        let on_output = |_: &WlSurface, _: &compositor::SurfaceData| Some(output.clone());
        let kind = |_: &WlSurface, _: &compositor::SurfaceData| Kind::Vsync;
        for window in self.space.elements() {
            window.take_presentation_feedback(&mut feedback, on_output, kind);
        }
        for layer in smithay::desktop::layer_map_for_output(output).layers() {
            layer.take_presentation_feedback(&mut feedback, on_output, kind);
        }
        feedback
    }

    /// Marks a frame's feedback presented now, at the output's refresh rate.
    pub fn presented(&self, mut feedback: smithay::desktop::utils::OutputPresentationFeedback) {
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        use smithay::wayland::presentation::Refresh;
        let refresh = self
            .output
            .current_mode()
            .and_then(|mode| u64::try_from(mode.refresh).ok())
            .filter(|millihertz| *millihertz > 0)
            .map(|millihertz| Refresh::Fixed(Duration::from_nanos(1_000_000_000_000 / millihertz)))
            .unwrap_or(Refresh::Unknown);
        feedback.presented(
            self.protocols.clock.now(),
            refresh,
            0,
            Kind::Vsync | Kind::HwClock | Kind::HwCompletion,
        );
    }

    /// A live, mapped surface holds an idle inhibitor: the idle count does not run.
    pub fn idle_inhibited(&mut self) -> bool {
        self.protocols.idle_inhibitors.retain(|s| s.is_alive());
        let inhibitors = &self.protocols.idle_inhibitors;
        self.space.elements().any(|window| {
            window
                .wl_surface()
                .is_some_and(|surface| inhibitors.iter().any(|s| owned_by(s, &surface)))
        }) || smithay::desktop::layer_map_for_output(&self.output)
            .layers()
            .any(|layer| inhibitors.iter().any(|s| owned_by(s, layer.wl_surface())))
    }

    /// The pointer moved by `delta`: where it may go, given the focused surface's constraint
    /// (none when locked, only inside the region when confined).
    pub fn constrained_motion(
        &mut self,
        from: Point<f64, Logical>,
        to: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let Some(pointer) = self.seat.get_pointer() else {
            return to;
        };
        let Some(focus) = pointer.current_focus() else {
            return to;
        };
        let Some((under, origin)) = self.surface_under(from) else {
            return to;
        };
        if under != focus {
            return to;
        }
        enum Rule {
            Free,
            Locked,
            Confined(Option<smithay::wayland::compositor::RegionAttributes>),
        }
        let rule = with_pointer_constraint(&focus, &pointer, |constraint| match constraint {
            Some(c) if c.is_active() => match &*c {
                PointerConstraint::Locked(_) => Rule::Locked,
                PointerConstraint::Confined(confined) => Rule::Confined(confined.region().cloned()),
            },
            _ => Rule::Free,
        });
        match rule {
            Rule::Free => to,
            Rule::Locked => from,
            Rule::Confined(region) => {
                let inside = self
                    .surface_under(to)
                    .is_some_and(|(surface, _)| surface == focus);
                let local = (to - origin).to_i32_round();
                let in_region = region.is_none_or(|region| region.contains(local));
                if inside && in_region { to } else { from }
            }
        }
    }

    /// The focused app inhibits the compositor's shortcuts.
    pub fn shortcuts_inhibited(&self) -> bool {
        self.seat.keyboard_shortcuts_inhibited()
    }

    /// The constraint of the surface under the pointer starts working once it has the focus.
    pub fn activate_pointer_constraint(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let Some(focus) = pointer.current_focus() else {
            return;
        };
        let keyboard_focus = self.seat.get_keyboard().and_then(|k| k.current_focus());
        if keyboard_focus.as_ref() != Some(&focus) {
            return;
        }
        with_pointer_constraint(&focus, &pointer, |constraint| {
            if let Some(c) = constraint
                && !c.is_active()
            {
                c.activate();
            }
        });
    }

    /// The window an activation token's surface belongs to, raised and focused.
    fn activate_surface(&mut self, surface: &WlSurface) {
        let Some(window) = self.window_for(surface) else {
            return;
        };
        self.space.raise_element(&window, true);
        self.focus_window(Some(window));
    }

    /// A fresh token for an app the compositor launches, passed as `XDG_ACTIVATION_TOKEN`.
    pub fn launch_activation_token(&mut self) -> String {
        let (token, _) = self.protocols.activation.create_external_token(None);
        token.to_string()
    }
}

/// `surface` is `root` or one of its subsurfaces.
fn owned_by(surface: &WlSurface, root: &WlSurface) -> bool {
    let mut current = surface.clone();
    loop {
        if &current == root {
            return true;
        }
        match compositor::get_parent(&current) {
            Some(parent) => current = parent,
            None => return false,
        }
    }
}

/// Before a commit applies, its dmabuf must be ready: the client's acquire point (explicit
/// sync) or the buffer's implicit fence. The commit waits on a blocker meanwhile, so the
/// renderer never samples a buffer the GPU is still writing.
pub fn add_buffer_blockers(surface: &WlSurface) {
    compositor::add_pre_commit_hook::<State, _>(surface, |state, _dh, surface| {
        use smithay::wayland::compositor::{BufferAssignment, SurfaceAttributes};
        use smithay::wayland::drm_syncobj::DrmSyncobjCachedState;
        let (acquire, dmabuf) = compositor::with_states(surface, |states| {
            let acquire = states
                .cached_state
                .get::<DrmSyncobjCachedState>()
                .pending()
                .acquire_point
                .clone();
            let dmabuf = states
                .cached_state
                .get::<SurfaceAttributes>()
                .pending()
                .buffer
                .as_ref()
                .and_then(|assignment| match assignment {
                    BufferAssignment::NewBuffer(buffer) => {
                        smithay::wayland::dmabuf::get_dmabuf(buffer).cloned().ok()
                    }
                    BufferAssignment::Removed => None,
                });
            (acquire, dmabuf)
        });
        let Some(dmabuf) = dmabuf else {
            return;
        };
        let Some(client) = surface.client() else {
            return;
        };
        let clear = move |state: &mut State| {
            let dh = state.display_handle.clone();
            if let Some(data) = client.get_data::<ClientState>() {
                data.compositor_state.blocker_cleared(state, &dh);
            }
        };
        if let Some(acquire) = acquire
            && let Ok((blocker, source)) = acquire.generate_blocker()
        {
            let inserted = state.loop_handle.insert_source(source, move |_, _, data| {
                clear(&mut data.state);
                Ok(())
            });
            if inserted.is_ok() {
                compositor::add_blocker(surface, blocker);
            }
            return;
        }
        if let Ok((blocker, source)) =
            dmabuf.generate_blocker(smithay::reexports::calloop::Interest::READ)
        {
            let inserted = state.loop_handle.insert_source(source, move |_, _, data| {
                clear(&mut data.state);
                Ok(())
            });
            if inserted.is_ok() {
                compositor::add_blocker(surface, blocker);
            }
        }
    });
}

impl smithay::wayland::drm_syncobj::DrmSyncobjHandler for State {
    fn drm_syncobj_state(&mut self) -> Option<&mut smithay::wayland::drm_syncobj::DrmSyncobjState> {
        self.protocols.syncobj.as_mut()
    }
}

impl smithay::wayland::fractional_scale::FractionalScaleHandler for State {}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.protocols.primary_selection
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.protocols.activation
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        // A token is good when it comes from a recent input event of the seat (the app was
        // clicked or typed into), as the protocol asks.
        let Some((serial, seat)) = data.serial else {
            return false;
        };
        let Some(seat) = smithay::input::Seat::<State>::from_resource(&seat) else {
            return false;
        };
        let keyboard_ok = seat.get_keyboard().is_some_and(|k| {
            k.last_enter()
                .is_some_and(|last| serial.is_no_older_than(&last))
        });
        let pointer_ok = seat.get_pointer().is_some_and(|p| {
            p.last_enter()
                .is_some_and(|last| serial.is_no_older_than(&last))
        });
        keyboard_ok || pointer_ok
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed() < TOKEN_LIFETIME {
            self.activate_surface(&surface);
        } else {
            debug!("activation token expired");
        }
        self.protocols.activation.remove_token(&token);
    }
}

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.push(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.protocols.idle_inhibitors.retain(|s| *s != surface);
    }
}

impl KeyboardShortcutsInhibitHandler for State {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.protocols.keyboard_shortcuts_inhibit
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Remote desktops and VMs: the app gets the shortcuts while it has the focus. The
        // VT switch and the lock keep working (they are not shortcuts, see input.rs).
        inhibitor.activate();
    }
}

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        self.activate_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: Point<f64, Logical>,
    ) {
        // A locked pointer reappears where the app says, once unlocked.
        let locked = with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active() && matches!(&*c, PointerConstraint::Locked(_)))
        });
        if !locked {
            return;
        }
        let origin = self.space.elements().find_map(|window| {
            (window.wl_surface().as_deref() == Some(surface))
                .then(|| self.space.element_location(window))
                .flatten()
        });
        if let Some(origin) = origin {
            pointer.set_location(origin.to_f64() + location);
        }
    }
}

impl InputMethodHandler for State {
    fn new_popup(&mut self, surface: ImePopupSurface) {
        if let Err(err) = self.popups.track_popup(PopupKind::from(surface)) {
            debug!("input method popup: {err}");
        }
    }

    fn popup_repositioned(&mut self, _surface: ImePopupSurface) {}

    fn dismiss_popup(&mut self, surface: ImePopupSurface) {
        if let Some(parent) = surface.get_parent().map(|p| p.surface.clone()) {
            let _ =
                smithay::desktop::PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.window_for(parent)
            .map(|window: Window| window.geometry())
            .unwrap_or_default()
    }
}

impl XdgToplevelIconHandler for State {
    fn set_icon(&mut self, toplevel: XdgToplevel, _surface: WlSurface) {
        // The Dock shows the app's own icon from its bundle; the toplevel icon is noted only.
        debug!("toplevel icon set for {:?}", toplevel.id());
    }
}

impl XdgToplevelTagHandler for State {}

impl XdgSystemBellHandler for State {
    fn ring(&mut self, _surface: Option<WlSurface>) {
        // The alert sound belongs to the sound service; the bell is logged until it exists.
        info!("system bell");
    }
}

smithay::delegate_primary_selection!(State);
smithay::delegate_xdg_activation!(State);
smithay::delegate_presentation!(State);
smithay::delegate_single_pixel_buffer!(State);
smithay::delegate_alpha_modifier!(State);
smithay::delegate_fifo!(State);
smithay::delegate_commit_timing!(State);
smithay::delegate_content_type!(State);
smithay::delegate_pointer_constraints!(State);
smithay::delegate_relative_pointer!(State);
smithay::delegate_pointer_gestures!(State);
smithay::delegate_text_input_manager!(State);
smithay::delegate_input_method_manager!(State);
smithay::delegate_virtual_keyboard_manager!(State);
smithay::delegate_idle_inhibit!(State);
smithay::delegate_keyboard_shortcuts_inhibit!(State);
smithay::delegate_xdg_toplevel_icon!(State);
smithay::delegate_xdg_toplevel_tag!(State);
smithay::delegate_xdg_system_bell!(State);
smithay::delegate_tablet_manager!(State);
smithay::delegate_drm_syncobj!(State);
smithay::delegate_fractional_scale!(State);
