//! wlr-layer-shell: shell surfaces (menu bar, Dock, wallpaper) anchored to the output
//! edges, above or below the windows.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::delegate_layer_shell;
use smithay::desktop::{LayerMap, LayerSurface, Window, WindowSurfaceType, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::{
    Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface,
    LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState,
};
use tracing::{info, trace, warn};

use crate::glass::{GlassRegion, current_regions};
use crate::state::State;

/// How long a layer whose buffer does not have its configured size stays off screen at most:
/// a client that never draws at the configured size is still shown (a safety net, not a
/// measured value).
pub const FIT_GRACE: Duration = Duration::from_millis(500);

/// [`FIT_GRACE`], or `SILICA_COMPOSITOR_LAYER_FIT_GRACE_MS` when set (tests on a loaded machine take
/// their screenshots later than 500 ms after a commit: they raise it).
fn fit_grace() -> Duration {
    static GRACE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *GRACE.get_or_init(|| {
        std::env::var("SILICA_COMPOSITOR_LAYER_FIT_GRACE_MS")
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(FIT_GRACE, Duration::from_millis)
    })
}

/// Where a layer is drawn, and with which glass.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    /// The layer's geometry origin in the output (as `LayerMap::layer_geometry`).
    pub loc: Point<i32, Logical>,
    /// The glass to draw with it: `None` for the surface's current regions, `Some` for the
    /// regions committed with the buffer still on screen.
    pub glass: Option<Vec<GlassRegion>>,
}

/// The last placement where the layer's buffer had its configured size.
#[derive(Default)]
struct Placement(RefCell<PlacementState>);

/// Where a layer's buffer last fitted: its origin, the output size then, its glass.
struct Fitted {
    loc: Point<i32, Logical>,
    output: Size<i32, Logical>,
    glass: Vec<GlassRegion>,
}

#[derive(Default)]
struct PlacementState {
    /// The last commit whose buffer fitted.
    fitted: Option<Fitted>,
    /// Since when the committed buffer does not fit the configured size.
    unfit_since: Option<Instant>,
}

/// The buffer on screen has the size the compositor configured: the client has drawn for the
/// current arrangement (not the size it asked before the configure, nor the old size while it
/// resizes).
fn fits(layer: &LayerSurface) -> bool {
    let configured = layer.layer_surface().with_pending_state(|state| state.size);
    let drawn =
        with_renderer_surface_state(layer.wl_surface(), |state| state.surface_size()).flatten();
    matches!((configured, drawn), (Some(configured), Some(drawn)) if configured == drawn)
}

/// The Dock's layer (namespace `dock`).
pub fn is_dock(layer: &LayerSurface) -> bool {
    layer.namespace() == "dock"
}

/// Where and how a layer is drawn on an output of `output` logical size, `None` if it must not
/// be drawn yet.
///
/// A shell surface is never shown before its first frame at the configured size: before the
/// configure the client draws at the size it guessed (the Dock 1280 × 200, anchored nowhere
/// yet), and the layer would appear in the wrong place for a few frames. While it resizes
/// (the Dock growing to the screen height for a menu) the old buffer stays where it fitted,
/// with the glass committed with it, until the frame at the new size arrives. After an
/// output change the old placement no longer counts. A client that never fits is shown
/// after [`FIT_GRACE`].
pub fn drawn_layer(
    map: &LayerMap,
    layer: &LayerSurface,
    output: Size<i32, Logical>,
) -> Option<Drawn> {
    let geometry = map.layer_geometry(layer)?;
    if fits(layer) {
        return Some(Drawn {
            loc: geometry.loc,
            glass: None,
        });
    }
    let placement = layer.user_data().get::<Placement>()?;
    let state = placement.0.borrow();
    if let Some(fitted) = &state.fitted
        && fitted.output == output
    {
        return Some(Drawn {
            loc: fitted.loc,
            glass: Some(fitted.glass.clone()),
        });
    }
    state
        .unfit_since
        .is_some_and(|since| since.elapsed() >= fit_grace())
        .then(|| Drawn {
            loc: unfit_location(layer, geometry.loc),
            glass: None,
        })
}

/// Where a buffer that does not have the configured size goes: against the edges the layer is
/// anchored to. A Dock anchored to the bottom that asked for the whole screen height and has
/// not drawn at it yet keeps its buffer at the bottom of the screen, not at the top of the
/// new geometry.
fn unfit_location(layer: &LayerSurface, arranged: Point<i32, Logical>) -> Point<i32, Logical> {
    let configured = layer.layer_surface().with_pending_state(|state| state.size);
    let drawn =
        with_renderer_surface_state(layer.wl_surface(), |state| state.surface_size()).flatten();
    let anchor = layer.cached_state().anchor;
    match (configured, drawn) {
        (Some(configured), Some(drawn)) => anchored_location(arranged, configured, drawn, anchor),
        _ => arranged,
    }
}

/// `arranged` moved so that a `drawn` buffer sits against the anchored edges of a
/// `configured` area (only one of two opposite edges counts; both or none: as arranged).
fn anchored_location(
    arranged: Point<i32, Logical>,
    configured: Size<i32, Logical>,
    drawn: Size<i32, Logical>,
    anchor: Anchor,
) -> Point<i32, Logical> {
    let mut loc = arranged;
    if anchor.contains(Anchor::BOTTOM) && !anchor.contains(Anchor::TOP) {
        loc.y += configured.h - drawn.h;
    }
    if anchor.contains(Anchor::RIGHT) && !anchor.contains(Anchor::LEFT) {
        loc.x += configured.w - drawn.w;
    }
    loc
}

/// After a layer's commit (and the arrangement): remembers where its buffer fits, or since
/// when it does not.
fn note_placement(map: &LayerMap, layer: &LayerSurface, output: Size<i32, Logical>) {
    layer.user_data().insert_if_missing(Placement::default);
    let Some(placement) = layer.user_data().get::<Placement>() else {
        return;
    };
    let mut state = placement.0.borrow_mut();
    let fit = fits(layer);
    if fit {
        if let Some(geometry) = map.layer_geometry(layer) {
            state.fitted = Some(Fitted {
                loc: geometry.loc,
                output,
                glass: current_regions(layer.wl_surface()),
            });
        }
        state.unfit_since = None;
    } else {
        let has_buffer = with_renderer_surface_state(layer.wl_surface(), |s| s.buffer().is_some())
            .unwrap_or(false);
        if has_buffer && state.unfit_since.is_none() {
            state.unfit_since = Some(Instant::now());
        }
    }
    if !fit {
        trace!(
            "layer {:?} not drawn as committed: buffer {:?}, configured {:?}",
            layer.namespace(),
            with_renderer_surface_state(layer.wl_surface(), |s| s.surface_size()).flatten(),
            layer.layer_surface().with_pending_state(|s| s.size),
        );
    }
}

/// The namespace of the file manager's desktop surface (#147): `desktop-icons:<app id>`, on the
/// bottom layer (above the wallpaper, below the windows). On the reference the desktop icons are
/// a window of the file manager, and a click on the desktop makes it the frontmost app.
pub const DESKTOP_ICONS: &str = "desktop-icons";

/// The owner app of a desktop surface's namespace: `Some(app id)` for `desktop-icons:<app id>`,
/// `Some("")` for a bare `desktop-icons`, `None` for any other surface.
pub fn desktop_owner(namespace: &str) -> Option<&str> {
    let rest = namespace.strip_prefix(DESKTOP_ICONS)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix(':')
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.output.clone());
        info!("new layer {namespace:?} ({layer:?})");
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            warn!("layer not mapped: {err}");
        }
    }

    /// A menu or popover of a shell layer (silica-os/silica-desktop#222): its parent is known only now
    /// (`xdg_surface.get_popup` came without one), so it is placed and tracked here.
    fn new_popup(
        &mut self,
        _parent: WlrLayerSurface,
        popup: smithay::wayland::shell::xdg::PopupSurface,
    ) {
        let positioner = popup.with_pending_state(|state| state.positioner);
        let kind = smithay::desktop::PopupKind::from(popup.clone());
        let geometry = self.popup_geometry(&kind, positioner);
        popup.with_pending_state(|state| {
            state.geometry = geometry;
        });
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let mut map = layer_map_for_output(&self.output);
        let layer = map
            .layers()
            .find(|layer| layer.layer_surface() == &surface)
            .cloned();
        if let Some(layer) = layer {
            map.unmap_layer(&layer);
        }
    }
}

delegate_layer_shell!(State);

impl State {
    /// On a layer's commit: rearranges the layers and sends the first configure. A layer
    /// with exclusive keyboard above the windows takes the focus.
    pub fn layer_commit(&mut self, surface: &WlSurface) {
        let (layer, first) = {
            let mut map = layer_map_for_output(&self.output);
            let Some(layer) = map
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .cloned()
            else {
                return;
            };
            map.arrange();
            let sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .is_some_and(|data| data.initial_configure_sent)
            });
            if !sent {
                layer.layer_surface().send_configure();
            }
            let output = self
                .space
                .output_geometry(&self.output)
                .map(|g| g.size)
                .unwrap_or_default();
            note_placement(&map, &layer, output);
            (layer, !sent)
        };
        let exclusive = layer.cached_state().keyboard_interactivity
            == KeyboardInteractivity::Exclusive
            && matches!(layer.layer(), Layer::Top | Layer::Overlay);
        // A layer may ask for the keyboard after the first commit (Faro opening) and
        // release it (Faro closing): the transition counts, not only the start.
        layer
            .user_data()
            .insert_if_missing(|| WasExclusive(std::cell::Cell::new(false)));
        let was = layer
            .user_data()
            .get::<WasExclusive>()
            .is_some_and(|flag| flag.0.replace(exclusive));
        if first && desktop_owner(layer.namespace()).is_some() {
            self.desktop_owner_fallback(None);
        }
        if exclusive && (first || !was) {
            self.focus_layer(&layer);
        } else if was && !exclusive {
            let focused = self
                .seat
                .get_keyboard()
                .and_then(|keyboard| keyboard.current_focus());
            if focused.as_ref() == Some(layer.wl_surface()) {
                let top = self.space.elements().last().cloned();
                self.focus_window(top);
            }
        }
    }

    /// The Top or Overlay layer asking for the keyboard exclusively, if any.
    pub fn exclusive_layer_surface(&self) -> Option<WlSurface> {
        let map = layer_map_for_output(&self.output);
        map.layers()
            .rev()
            .find(|layer| {
                matches!(layer.layer(), Layer::Top | Layer::Overlay)
                    && layer.cached_state().keyboard_interactivity
                        == KeyboardInteractivity::Exclusive
            })
            .map(|layer| layer.wl_surface().clone())
    }

    pub fn focus_layer(&mut self, layer: &LayerSurface) {
        if self.locked {
            return;
        }
        let surface = layer.wl_surface().clone();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        }
    }

    /// The layer of that level under a point, with its position.
    pub fn layer_under(
        &self,
        layers: &[Layer],
        pos: Point<f64, Logical>,
    ) -> Option<(LayerSurface, Point<i32, Logical>)> {
        let map = layer_map_for_output(&self.output);
        let output = self.space.output_geometry(&self.output)?;
        let origin = output.loc;
        // The input region counts, not the rectangle: a hidden shell component (a closed
        // Faro) is transparent and full screen but with an empty region. A layer not shown yet
        // takes no input; one resizing takes it where it is drawn.
        let dock_hidden = self.front_is_fullscreen();
        layers.iter().find_map(|level| {
            map.layers_on(*level).rev().find_map(|layer| {
                if dock_hidden && is_dock(layer) {
                    return None;
                }
                let drawn = drawn_layer(&map, layer, output.size)?;
                let local = pos - (drawn.loc + origin).to_f64();
                layer
                    .surface_under(local, smithay::desktop::WindowSurfaceType::ALL)
                    .map(|_| (layer.clone(), drawn.loc + origin))
            })
        })
    }

    pub fn layer_list(&self) -> Vec<silica_control::commands::LayerInfo> {
        let focus = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus());
        let map = layer_map_for_output(&self.output);
        let origin = self
            .space
            .output_geometry(&self.output)
            .map(|g| g.loc)
            .unwrap_or_default();
        map.layers()
            .filter_map(|layer| {
                let geometry = map.layer_geometry(layer)?;
                let state = layer.cached_state();
                Some(silica_control::commands::LayerInfo {
                    namespace: layer.namespace().to_owned(),
                    layer: match layer.layer() {
                        Layer::Background => "background",
                        Layer::Bottom => "bottom",
                        Layer::Top => "top",
                        Layer::Overlay => "overlay",
                    }
                    .to_owned(),
                    x: geometry.loc.x + origin.x,
                    y: geometry.loc.y + origin.y,
                    width: geometry.size.w,
                    height: geometry.size.h,
                    exclusive_zone: match state.exclusive_zone {
                        ExclusiveZone::Exclusive(zone) => i32::try_from(zone).unwrap_or(i32::MAX),
                        ExclusiveZone::Neutral => 0,
                        ExclusiveZone::DontCare => -1,
                    },
                    focused: focus.as_ref() == Some(layer.wl_surface()),
                })
            })
            .collect()
    }

    /// The file manager's desktop surface, if one is mapped, with its owner app id.
    pub fn desktop_layer(&self) -> Option<(LayerSurface, String)> {
        let map = layer_map_for_output(&self.output);
        [Layer::Bottom, Layer::Background].iter().find_map(|level| {
            map.layers_on(*level).find_map(|layer| {
                desktop_owner(layer.namespace()).map(|owner| (layer.clone(), owner.to_owned()))
            })
        })
    }

    /// With no app frontmost (no focused window other than `closing`, no windowless app) the
    /// desktop's owner is, as the Finder on macOS (#147). A window in use is never displaced.
    pub fn desktop_owner_fallback(&mut self, closing: Option<&Window>) {
        if self.windowless_front.is_some() {
            return;
        }
        if self.focused_window().is_some_and(|w| Some(&w) != closing) {
            return;
        }
        // The windowless app that was in front before (its Open panel, a window of another
        // process, closed): it is frontmost again while it still runs (a hidden window).
        while let Some(app) = self.windowless_history.pop() {
            let running = self
                .hidden
                .iter()
                .any(|(w, _)| crate::windows::window_app(w).as_deref() == Some(app.as_str()));
            if running {
                self.set_windowless_front(Some(app));
                return;
            }
        }
        if let Some((_, owner)) = self.desktop_layer()
            && !owner.is_empty()
        {
            self.set_windowless_front(Some(owner));
        }
    }

    /// A press on the desktop (no layer above, no window): the windows lose the focus and the
    /// desktop's owner becomes the frontmost app, with the keyboard (#147). Without a desktop
    /// owner no app stays frontmost.
    pub fn desktop_pressed(&mut self) {
        // The owner first: the shell never sees the window on top as focused in between.
        let desktop = self.desktop_layer();
        self.set_windowless_front(
            desktop
                .as_ref()
                .map(|(_, owner)| owner.clone())
                .filter(|owner| !owner.is_empty()),
        );
        self.focus_window(None);
        if let Some((layer, _)) = desktop
            && layer.can_receive_keyboard_focus()
        {
            self.focus_layer(&layer);
        }
    }

    /// The output changed mode: the layers are arranged again for the new size (and receive
    /// their configure now, not at their next commit). Until they draw at the new size they
    /// are not shown ([`drawn_layer`]).
    pub fn output_resized(&mut self) {
        layer_map_for_output(&self.output).arrange();
    }

    /// The area free of layers with an exclusive zone: the windows go there.
    pub fn usable_area(&self) -> Rectangle<i32, Logical> {
        let map = layer_map_for_output(&self.output);
        let mut zone = map.non_exclusive_zone();
        // The menu bar's strip (#12) is out of the windows' way.
        let bar = crate::menu_bar::HEIGHT;
        if self.menu_bar_visible() && zone.loc.y < bar {
            zone.size.h -= bar - zone.loc.y;
            zone.loc.y = bar;
        }
        if let Some(geometry) = self.space.output_geometry(&self.output) {
            zone.loc += geometry.loc;
        }
        zone
    }
}

/// Whether the layer asked for the keyboard exclusively on the previous commit.
struct WasExclusive(std::cell::Cell<bool>);

#[cfg(test)]
mod tests {
    use super::desktop_owner;

    #[test]
    fn the_desktop_surface_names_its_owner() {
        assert_eq!(
            desktop_owner("desktop-icons:dev.frascella.Mustache"),
            Some("dev.frascella.Mustache")
        );
        assert_eq!(desktop_owner("desktop-icons"), Some(""));
        // An empty owner is a desktop, but no app to bring to the front.
        assert_eq!(desktop_owner("desktop-icons:"), Some(""));
        assert_eq!(desktop_owner("wallpaper"), None);
        assert_eq!(desktop_owner("desktop-iconsx"), None);
    }
}
