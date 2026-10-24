//! The list of the elements of an output: layers, windows and `compositor-render` effects, from
//! the front to the back, the same for GLES and the CPU renderer.

use std::cell::Cell;
use std::time::Instant;

use compositor_render::animation;
use compositor_render::border::WindowBorder;
use compositor_render::cache::ShaderElement;
use compositor_render::corners::{RoundedSurface, WindowSurface};
use compositor_render::glass::{
    GlassCache, GlassElement, GlassQuality, PhysicalRegion, Tip, render_to_texture,
};
use compositor_render::renderer::{SilicaRenderer, SilicaTexture};
use compositor_render::shadow::WindowShadow;
use compositor_render::title::Title;
use compositor_render::titlebar::{Titlebar, TitlebarState};
use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Renderer, Texture};
use smithay::desktop::{LayerSurface, Space, Window, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::render_elements;
use smithay::utils::{Logical, Physical, Point, Rectangle, Size, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::Layer;
use tracing::warn;

use crate::decorations;
use crate::glass::{Shape, current_regions};

render_elements! {
    pub OutputElement<=SilicaRenderer>;
    Surface=WaylandSurfaceRenderElement<SilicaRenderer>,
    Rounded=RoundedSurface,
    /// Pixel-shader elements of the window: title bar and shadow.
    Shader=RescaleRenderElement<ShaderElement>,
    Glass=GlassElement,
    /// The gradient below the menu bar, above the desktop wallpaper.
    Tone=compositor_render::backdrop::ToneElement,
    /// Textures: bar titles, snapshots of closing windows.
    Texture=RescaleRenderElement<TextureRenderElement<SilicaTexture>>,
    /// The named cursor, which `DrmCompositor` can put on the cursor plane.
    Memory=MemoryRenderBufferRenderElement<SilicaRenderer>,
    /// A window minimizing into the Dock or coming out of it, deformed as a genie.
    Genie=compositor_render::genie::GenieElement,
    /// The window screenshot veil.
    Solid=SolidColorRenderElement,
    /// The whole frame zoomed or filtered (`accessibility.rs`).
    Filtered=compositor_render::filter::FilterElement,
}

/// What the frame needs besides the space.
pub struct RenderContext<'a> {
    /// The bar buttons of a window: symbols and pressed button.
    pub buttons: Option<ButtonsView>,
    /// ⌥ held: the green one shows "+".
    pub alt: bool,
    pub clear_color: [f32; 4],
    /// Multiplier of the animation durations (0: no animation).
    pub animation_speed: f64,
    pub closing: &'a [Closing],
    /// Windows minimizing into the Dock or coming out of it.
    pub genies: &'a [Genie],
    pub now: Instant,
    pub glass_quality: GlassQuality,
    /// How tinted the glass is (Settings › Appearance › Glass, 0–1).
    pub glass_level: f32,
    /// Dark appearance: windows, bars, shadows and glass.
    pub dark: bool,
    /// The icon of a drag between apps and the pointer position: above everything.
    pub drag_icon: Option<(WlSurface, Point<f64, Logical>)>,
    /// The window screenshot veil: the frame of the window under the pointer (K-05).
    pub window_veil: Option<Rectangle<i32, Logical>>,
    /// The session is locked: only these lock surfaces are drawn (none: the plain screen).
    pub lock_surfaces: Option<Vec<WlSurface>>,
}

/// The veil over the window to capture (K-05): `#88bae7` at alpha ~0.51, premultiplied.
const WINDOW_VEIL: [f32; 4] = [
    0x88 as f32 / 255.0 * 0.51,
    0xba as f32 / 255.0 * 0.51,
    0xe7 as f32 / 255.0 * 0.51,
    0.51,
];

/// The veil drawn over a window that has a sheet (its own shader cache).
#[derive(Default)]
struct SheetVeil(compositor_render::border::WindowBorder);

/// Stable id of the window screenshot veil, in the output data.
struct WindowVeilId(Id);

impl Default for WindowVeilId {
    fn default() -> Self {
        Self(Id::new())
    }
}

/// The bar buttons of a window in the frame.
#[derive(Clone)]
pub struct ButtonsView {
    pub window: Window,
    pub symbols: f32,
    pub pressed: Option<u8>,
}

/// How to draw a window's buttons.
#[derive(Clone, Copy, Default)]
struct ButtonsLook {
    symbols: f32,
    pressed: Option<u8>,
    alt: bool,
}

/// The buttons that make no sense: minimize and full screen for a dialog (it has a parent),
/// full screen only for a fixed-size window (minimum equal to maximum; the Calculator keeps
/// minimize, calculator.md).
pub fn buttons_disabled(window: &Window) -> compositor_render::titlebar::Disabled {
    use compositor_render::titlebar::Disabled;
    let Some(toplevel) = window.toplevel() else {
        return Disabled::None;
    };
    if toplevel.parent().is_some() {
        return Disabled::MinimizeAndZoom;
    }
    let fixed = with_states(toplevel.wl_surface(), |states| {
        let mut cached = states
            .cached_state
            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
        let current = cached.current();
        !current.min_size.is_empty() && current.min_size == current.max_size
    });
    if fixed {
        Disabled::Zoom
    } else {
        Disabled::None
    }
}

/// A closed window fading out: the snapshot of its last look.
pub struct Closing {
    pub id: Id,
    pub texture: SilicaTexture,
    /// Where the snapshot is in the output, in physical pixels.
    pub bbox: Rectangle<i32, Physical>,
    /// Center of the frame, around which the window shrinks.
    pub center: Point<i32, Physical>,
    pub start: Instant,
}

impl Closing {
    pub fn finished(&self, now: Instant, speed: f64) -> bool {
        animation::progress(now - self.start, animation::CLOSE, speed) >= 1.0
    }
}

/// How a window goes into the Dock (Dock setting, SH-DK-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MinimizeEffect {
    /// The funnel (windows.md).
    #[default]
    Genie,
    /// The window shrinks uniformly towards its Dock cell, keeping its aspect (dock.md,
    /// SH-DK-09: ~213 ms each way, about half the genie).
    Scale,
}

impl MinimizeEffect {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "genie" => Some(Self::Genie),
            "scale" => Some(Self::Scale),
            _ => None,
        }
    }
}

/// A window minimizing into the Dock or coming out of it: the snapshot deformed as a genie.
pub struct Genie {
    pub id: Id,
    /// It changes on every frame: the shape changes even if the snapshot is the same.
    pub commit: std::cell::Cell<smithay::backend::renderer::utils::CommitCounter>,
    pub texture: SilicaTexture,
    /// The snapshot in the output, in physical pixels.
    pub snapshot: Rectangle<i32, Physical>,
    /// The window frame (without shadow) and the thumbnail, in physical pixels.
    pub frame: Rectangle<f64, Physical>,
    pub target: Rectangle<f64, Physical>,
    pub start: Instant,
    /// The restore: the window is already mapped but stays hidden until the genie ends.
    pub restoring: Option<Window>,
    pub effect: MinimizeEffect,
}

impl Genie {
    fn duration(&self) -> std::time::Duration {
        if self.effect == MinimizeEffect::Scale {
            animation::SCALE_EFFECT
        } else if self.restoring.is_some() {
            animation::RESTORE
        } else {
            animation::MINIMIZE
        }
    }

    pub fn finished(&self, now: Instant, speed: f64) -> bool {
        let gone = self
            .restoring
            .as_ref()
            .is_some_and(|w| !smithay::utils::IsAlive::alive(w));
        gone || animation::progress(now - self.start, self.duration(), speed) >= 1.0
    }

    /// The shape at instant `now`.
    pub fn shape(&self, now: Instant, speed: f64) -> animation::GenieShape {
        let duration = self.duration();
        let progress =
            animation::progress(now.saturating_duration_since(self.start), duration, speed);
        let ms = progress * duration.as_secs_f64() * 1000.0;
        if self.restoring.is_some() {
            animation::genie_restore(ms)
        } else {
            animation::genie_minimize(ms)
        }
    }

    /// The scale effect at instant `now`: the factor and the point that stays still, so the
    /// snapshot shrinks from the window frame into the target keeping its aspect.
    pub fn scale_at(&self, now: Instant, speed: f64) -> (f64, Point<i32, Physical>) {
        let progress = animation::progress(
            now.saturating_duration_since(self.start),
            self.duration(),
            speed,
        );
        scale_effect(
            self.frame,
            self.target,
            if self.restoring.is_some() {
                1.0 - progress
            } else {
                progress
            },
        )
    }

    /// The restoring window that the genie still hides.
    pub fn hides(&self, window: &Window) -> bool {
        self.restoring.as_ref() == Some(window)
    }
}

/// Full screen entering or exiting: the frame rectangle goes from `from` to `to`, in the
/// window data.
#[derive(Default)]
pub struct FullscreenMorph(pub std::cell::Cell<Option<Morph>>);

#[derive(Clone, Copy)]
pub struct Morph {
    pub from: Rectangle<f64, Logical>,
    pub to: Rectangle<f64, Logical>,
    pub start: Instant,
    pub kind: MorphKind,
}

/// What moves the frame: entering or leaving full screen, or a tiling from the popover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorphKind {
    Enter,
    Exit,
    Tile,
}

impl Morph {
    fn duration(&self) -> std::time::Duration {
        match self.kind {
            MorphKind::Enter => animation::FULLSCREEN_ENTER,
            MorphKind::Exit => animation::FULLSCREEN_EXIT,
            MorphKind::Tile => animation::TILE,
        }
    }

    /// The fraction of the way after `ms` of the animation.
    pub fn fraction(&self, ms: f64) -> f64 {
        match self.kind {
            MorphKind::Enter => animation::fullscreen_enter(ms),
            MorphKind::Exit => animation::fullscreen_exit(ms),
            MorphKind::Tile => animation::tile(ms),
        }
    }

    /// The rectangle at instant `now`; None once the animation is over.
    pub fn rect(&self, now: Instant, speed: f64) -> Option<Rectangle<f64, Logical>> {
        let duration = self.duration();
        let progress =
            animation::progress(now.saturating_duration_since(self.start), duration, speed);
        if progress >= 1.0 {
            return None;
        }
        let ms = progress * duration.as_secs_f64() * 1000.0;
        let f = self.fraction(ms);
        let lerp = |a: f64, b: f64| a + (b - a) * f;
        Some(Rectangle::new(
            (
                lerp(self.from.loc.x, self.to.loc.x),
                lerp(self.from.loc.y, self.to.loc.y),
            )
                .into(),
            (
                lerp(self.from.size.w, self.to.size.w),
                lerp(self.from.size.h, self.to.size.h),
            )
                .into(),
        ))
    }
}

/// The rectangle of the full screen in progress for a window (and removes a finished one).
fn morph_rect(window: &Window, context: &RenderContext<'_>) -> Option<Rectangle<f64, Logical>> {
    let morph = window.user_data().get::<FullscreenMorph>()?;
    let current = morph.0.get()?;
    let rect = current.rect(context.now, context.animation_speed);
    if rect.is_none() {
        morph.0.set(None);
    }
    rect
}

/// When the window showed its first buffer: the opening starts from there.
#[derive(Default)]
struct Opened(Cell<Option<Instant>>);

/// Where the glass of a surface goes: right behind its surfaces in the list.
struct GlassAnchor {
    index: usize,
    surface: WlSurface,
    /// Origin of the surface in the output, in physical pixels.
    origin: Point<i32, Physical>,
    /// The regions committed with the buffer on screen, when they are not the current ones
    /// (a layer resizing keeps its old buffer and its old glass, `layers::drawn_layer`).
    glass: Option<Vec<crate::glass::GlassRegion>>,
    /// The surface whose appearance the glass follows, when it is not `surface` itself (an app
    /// popup follows its window).
    dark_from: Option<WlSurface>,
}

/// The window has the active state (focus), even if the configure has not left yet.
fn is_active(window: &Window) -> bool {
    window.toplevel().is_some_and(|toplevel| {
        toplevel.with_pending_state(|state| state.states.contains(xdg_toplevel::State::Activated))
    })
}

/// The window title (`xdg_toplevel.set_title`), empty if none.
fn window_title(window: &Window) -> String {
    window
        .toplevel()
        .and_then(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .and_then(|data| data.title.clone())
            })
        })
        .unwrap_or_default()
}

/// The opening scale at this moment (1 at rest).
fn open_scale(window: &Window, context: &RenderContext<'_>) -> f64 {
    window.user_data().insert_if_missing(Opened::default);
    let Some(opened) = window.user_data().get::<Opened>() else {
        return 1.0;
    };
    let start = match opened.0.get() {
        Some(start) => start,
        // Without content there is nothing to show yet.
        None if window.geometry().size.is_empty() => return 1.0,
        None => {
            opened.0.set(Some(context.now));
            context.now
        }
    };
    let progress = animation::progress(
        context.now.saturating_duration_since(start),
        animation::OPEN,
        context.animation_speed,
    );
    animation::open_scale(progress)
}

/// A rectangle scaled around `center`.
fn scale_rect(
    rect: Rectangle<f64, Physical>,
    center: Point<f64, Physical>,
    factor: f64,
) -> Rectangle<f64, Physical> {
    let loc = center + (rect.loc - center).upscale(factor);
    Rectangle::new(loc, rect.size.upscale(factor))
}

/// Where a window of the Space is: position and geometry.
fn placed(
    space: &Space<Window>,
    window: &Window,
) -> Option<(Point<i32, Logical>, Rectangle<i32, Logical>)> {
    Some((
        space.element_location(window)?,
        space.element_geometry(window)?,
    ))
}

/// The frame of a window in points, from its geometry.
fn frame_rect(window: &Window, geometry: Rectangle<i32, Logical>) -> Rectangle<f64, Logical> {
    decorations::frame_of(window, geometry).to_f64()
}

/// Offset and scale that bring the frame `from` onto `to` (scale around the center).
fn fit(from: Rectangle<f64, Logical>, to: Rectangle<f64, Logical>) -> (Point<i32, Logical>, f64) {
    let center = |r: Rectangle<f64, Logical>| r.loc + r.size.downscale(2.0).to_point();
    let offset = (center(to) - center(from)).to_i32_round();
    let factor = if from.size.w > 0.0 {
        to.size.w / from.size.w
    } else {
        1.0
    };
    (offset, factor)
}

/// An app popup of a window (a menu, a popover): drawn above the window, with the glass it
/// declares in its own surface (silica-os/silica-desktop#4).
struct PopupLayer {
    surface: WlSurface,
    elements: Vec<OutputElement>,
    /// Origin of the popup's surface in the output, in physical pixels.
    origin: Point<i32, Physical>,
}

/// The popups of `window`, the front one first (a submenu above its menu). `render_location`
/// is where the window's surface goes (see `window_elements_uncropped`).
fn popup_layers(
    renderer: &mut SilicaRenderer,
    window: &Window,
    render_location: Point<i32, Physical>,
    scale: f64,
) -> Vec<PopupLayer> {
    let Some(toplevel) = window.toplevel() else {
        return Vec::new();
    };
    surface_popup_layers(
        renderer,
        toplevel.wl_surface(),
        window.geometry().loc,
        render_location,
        scale,
    )
}

/// The popups hanging from `root` (a window's or a layer's surface), the front one first.
/// `inner` is the offset of the root's geometry in its surface (a window's), `render_location`
/// where the root's surface goes.
fn surface_popup_layers(
    renderer: &mut SilicaRenderer,
    root: &WlSurface,
    inner: Point<i32, Logical>,
    render_location: Point<i32, Physical>,
    scale: f64,
) -> Vec<PopupLayer> {
    let depth = |popup: &smithay::desktop::PopupKind| {
        let mut depth = 0;
        let mut current = popup.wl_surface().clone();
        while let Some(parent) = smithay::desktop::PopupManager::popups_for_surface(root)
            .find(|(p, _)| p.wl_surface() == &current)
            .and_then(|(p, _)| match p {
                smithay::desktop::PopupKind::Xdg(xdg) => xdg.get_parent_surface(),
                _ => None,
            })
        {
            depth += 1;
            current = parent;
            if depth > 16 {
                break;
            }
        }
        depth
    };
    let mut popups: Vec<_> = smithay::desktop::PopupManager::popups_for_surface(root)
        .map(|(popup, offset)| (depth(&popup), popup, offset))
        .collect();
    popups.sort_by_key(|(depth, _, _)| std::cmp::Reverse(*depth));
    popups
        .into_iter()
        .map(|(_, popup, popup_offset)| {
            let offset =
                (inner + popup_offset - popup.geometry().loc).to_physical_precise_round(scale);
            let origin = render_location + offset;
            let elements = render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                renderer,
                popup.wl_surface(),
                origin,
                scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(OutputElement::Surface)
            .collect();
            PopupLayer {
                surface: popup.wl_surface().clone(),
                elements,
                origin,
            }
        })
        .collect()
}

/// The elements of a window, from the front to the back (surfaces, bar, shadow), moved by
/// `offset` and scaled by `factor` around the center of the frame.
#[allow(clippy::too_many_arguments)]
fn window_elements(
    renderer: &mut SilicaRenderer,
    shaders: &compositor_render::Shaders,
    (location, geometry): (Point<i32, Logical>, Rectangle<i32, Logical>),
    window: &Window,
    output_origin: Point<i32, Logical>,
    output_size: Size<i32, Physical>,
    scale: f64,
    offset: Point<i32, Logical>,
    factor: f64,
    buttons: ButtonsLook,
    system_dark: bool,
) -> Vec<OutputElement> {
    // Everything drawn for the window (buttons, bar, title, border, shadow) follows the
    // window's own appearance when the app overrides the system's (`silica-window-v1`).
    let dark = crate::window_appearance::window_dark(window, system_dark);
    let render_location = (location - window.geometry().loc - output_origin + offset)
        .to_physical_precise_round(scale);
    // The window's own surfaces; its popups are drawn above it with their own glass
    // (`popup_layers`).
    let surfaces = match window.toplevel() {
        Some(toplevel) => render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
            renderer,
            toplevel.wl_surface(),
            render_location,
            scale,
            1.0,
            Kind::Unspecified,
        ),
        None => window.render_elements::<WaylandSurfaceRenderElement<SilicaRenderer>>(
            renderer,
            render_location,
            scale.into(),
            1.0,
        ),
    };
    // Full screen: no corners, bar, shadow or animation (as in the recordings). The client's
    // buffer can then go to the display as is (direct scanout) when nothing covers it.
    if crate::windows::is_fullscreen(window) {
        return surfaces.into_iter().map(OutputElement::Surface).collect();
    }
    let mut frame = decorations::frame_of(window, geometry);
    frame.loc -= output_origin;
    frame.loc += offset;
    let frame_physical = frame.to_f64().to_physical(scale);
    let center_f = frame_physical.loc + frame_physical.size.downscale(2.0).to_point();
    let center = center_f.to_i32_round();
    let mut elements: Vec<OutputElement> = surfaces
        .into_iter()
        .map(|surface| {
            OutputElement::Rounded(RoundedSurface::new(
                WindowSurface::from_element(surface, center, factor),
                scale_rect(frame_physical, center_f, factor),
                scale * factor,
                output_size,
                shaders.corners,
            ))
        })
        .collect();
    let active = is_active(window);
    if decorations::is_server_side(window)
        && let Some(program) = shaders.titlebar
    {
        let full_size = decorations::full_size_toolbar(window);
        window.user_data().insert_if_missing(Title::default);
        // With full-size content the app writes the title in its own bar.
        if full_size.is_none()
            && let Some(title) = window.user_data().get::<Title>()
            && let Some(text) = title.element(
                renderer,
                &window_title(window),
                frame,
                compositor_render::title::color(active, dark),
                scale,
            )
        {
            elements.push(OutputElement::Texture(RescaleRenderElement::from_element(
                text, center, factor,
            )));
        }
        window.user_data().insert_if_missing(Titlebar::default);
        if let Some(titlebar) = window.user_data().get::<Titlebar>() {
            let state = TitlebarState {
                active,
                symbols: buttons.symbols,
                pressed: buttons.pressed,
                alt: buttons.alt,
                disabled: buttons_disabled(window),
                scale,
                bar: full_size.is_none(),
                bar_height: decorations::button_bar_height(window) as f32,
                dark,
            };
            let element = OutputElement::Shader(RescaleRenderElement::from_element(
                titlebar.element(program, frame, state),
                center,
                factor,
            ));
            // With full-size content the buttons sit above the app's content (the list goes
            // from the front down).
            if full_size.is_some() {
                elements.insert(0, element);
            } else {
                elements.push(element);
            }
        }
    }
    // The 1 pt rim (Line/Border), above the content.
    if let Some(style) = compositor_render::border::style(active, dark)
        && let Some(program) = shaders.border
    {
        window.user_data().insert_if_missing(WindowBorder::default);
        if let Some(border) = window.user_data().get::<WindowBorder>() {
            elements.insert(
                0,
                OutputElement::Shader(RescaleRenderElement::from_element(
                    border.element(program, frame, style, scale),
                    center,
                    factor,
                )),
            );
        }
    }
    if let Some(program) = shaders.shadow {
        let style = compositor_render::shadow::style(active, dark);
        window.user_data().insert_if_missing(WindowShadow::default);
        if let Some(shadow) = window.user_data().get::<WindowShadow>() {
            elements.push(OutputElement::Shader(RescaleRenderElement::from_element(
                shadow.element(program, frame, style),
                center,
                factor,
            )));
        }
    }
    elements
}

/// The genie of a window still in the space, towards (or from) `target` in output points.
pub fn snapshot_genie(
    renderer: &mut SilicaRenderer,
    space: &Space<Window>,
    output: &Output,
    window: &Window,
    target: Rectangle<f64, Logical>,
    restoring: bool,
    dark: bool,
) -> Option<Genie> {
    let snapshot = snapshot_window(renderer, space, output, window, dark)?;
    let scale = output.current_scale().fractional_scale();
    let origin = space.output_geometry(output)?.loc.to_f64();
    let mut frame = frame_rect(window, space.element_geometry(window)?);
    frame.loc -= origin;
    let mut target = target;
    target.loc -= origin;
    Some(Genie {
        id: Id::new(),
        commit: std::cell::Cell::default(),
        texture: snapshot.texture,
        snapshot: snapshot.bbox,
        frame: frame.to_physical(scale),
        target: target.to_physical(scale),
        start: Instant::now(),
        restoring: restoring.then(|| window.clone()),
        effect: MinimizeEffect::Genie,
    })
}

/// The scale effect at `progress` (0 window, 1 in the Dock cell): the factor, and the point
/// that stays still so the frame's center lands on the target's center while the frame shrinks
/// to fit the target with its aspect kept.
pub fn scale_effect(
    frame: Rectangle<f64, Physical>,
    target: Rectangle<f64, Physical>,
    progress: f64,
) -> (f64, Point<i32, Physical>) {
    let eased = animation::smoothstep(progress);
    let (fw, fh) = (frame.size.w.max(1.0), frame.size.h.max(1.0));
    let end = (target.size.w / fw)
        .min(target.size.h / fh)
        .clamp(0.01, 1.0);
    let factor = 1.0 + (end - 1.0) * eased;
    // o + (frame center − o) · end = target center.
    let fc = (frame.loc.x + fw / 2.0, frame.loc.y + fh / 2.0);
    let tc = (
        target.loc.x + target.size.w / 2.0,
        target.loc.y + target.size.h / 2.0,
    );
    let k = 1.0 - end;
    let origin = if k > 1e-6 {
        ((tc.0 - end * fc.0) / k, (tc.1 - end * fc.1) / k)
    } else {
        fc
    };
    (
        factor,
        Point::from((origin.0.round() as i32, origin.1.round() as i32)),
    )
}

/// The snapshot of a window (surfaces, bar and shadow) for the closing animation.
pub fn snapshot_window(
    renderer: &mut SilicaRenderer,
    space: &Space<Window>,
    output: &Output,
    window: &Window,
    dark: bool,
) -> Option<Closing> {
    let shaders = compositor_render::shaders(renderer);
    let scale = output.current_scale().fractional_scale();
    let output_size = output.current_mode()?.size;
    let output_origin = space.output_geometry(output)?.loc;
    let elements = window_elements(
        renderer,
        &shaders,
        placed(space, window)?,
        window,
        output_origin,
        output_size,
        scale,
        Point::default(),
        1.0,
        ButtonsLook::default(),
        dark,
    );
    let bbox = elements
        .iter()
        .map(|element| {
            smithay::backend::renderer::element::Element::geometry(element, scale.into())
        })
        .reduce(|a, b| a.merge(b))?
        .intersection(Rectangle::from_size(output_size))?;
    let mut frame = decorations::frame_of(window, space.element_geometry(window)?);
    frame.loc -= output_origin;
    let frame = frame.to_f64().to_physical(scale);
    let center = (frame.loc + frame.size.downscale(2.0).to_point()).to_i32_round();
    match render_to_texture(renderer, &elements, bbox, scale, [0.0; 4]) {
        Ok(texture) => Some(Closing {
            id: Id::new(),
            texture,
            bbox,
            center,
            start: Instant::now(),
        }),
        Err(err) => {
            warn!("snapshot of the closed window failed: {err:?}");
            None
        }
    }
}

/// The index of a glass after an element was inserted at `at`, in front of the wallpaper:
/// only the glass of the background layers moves (index beyond `at`). That of a layer ending
/// exactly where the wallpaper begins (no window in between, like the menu bar on an empty
/// desktop) stays in front of the inserted element.
fn after_insert(index: usize, at: usize) -> usize {
    if index > at { index + 1 } else { index }
}

/// The output elements, from the front to the back.
pub fn output_elements(
    renderer: &mut SilicaRenderer,
    space: &Space<Window>,
    output: &Output,
    context: &RenderContext<'_>,
) -> Vec<OutputElement> {
    let shaders = compositor_render::shaders(renderer);
    let scale = output.current_scale().fractional_scale();
    let output_size = output
        .current_mode()
        .map(|mode| mode.size)
        .unwrap_or_default();
    let output_origin = space
        .output_geometry(output)
        .map(|geo| geo.loc)
        .unwrap_or_default();
    // Locked: the lock surfaces alone, no window, layer, glass or drag icon.
    if let Some(surfaces) = &context.lock_surfaces {
        return surfaces
            .iter()
            .flat_map(|surface| {
                render_elements_from_surface_tree::<_, WaylandSurfaceRenderElement<_>>(
                    renderer,
                    surface,
                    (0, 0),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .map(OutputElement::Surface)
            .collect();
    }
    let mut elements = Vec::new();
    let mut anchors: Vec<GlassAnchor> = Vec::new();

    let layer_map = layer_map_for_output(output);
    let layers_output = space
        .output_geometry(output)
        .map(|geo| geo.size)
        .unwrap_or_default();
    let push_layers = |elements: &mut Vec<OutputElement>,
                       anchors: &mut Vec<GlassAnchor>,
                       renderer: &mut SilicaRenderer,
                       level| {
        for layer in layer_map.layers_on(level).rev() {
            // Not before its first frame at the configured size (the Dock in the middle of
            // the screen at session start), and where its buffer fits while it resizes.
            let Some(drawn) = crate::layers::drawn_layer(&layer_map, layer, layers_output) else {
                continue;
            };
            let location = drawn.loc.to_f64().to_physical_precise_round(scale);
            // The layer's menus and popovers (silica-os/silica-desktop#222) are in front of it,
            // each with the glass it declares behind it.
            for popup in surface_popup_layers(
                renderer,
                layer.wl_surface(),
                Point::default(),
                location,
                scale,
            ) {
                elements.extend(popup.elements);
                anchors.push(GlassAnchor {
                    index: elements.len(),
                    surface: popup.surface,
                    origin: popup.origin,
                    glass: None,
                    dark_from: Some(layer.wl_surface().clone()),
                });
            }
            elements.extend(
                AsRenderElements::<SilicaRenderer>::render_elements::<
                    WaylandSurfaceRenderElement<SilicaRenderer>,
                >(
                    layer as &LayerSurface,
                    renderer,
                    location,
                    scale.into(),
                    1.0,
                )
                .into_iter()
                .map(OutputElement::Surface),
            );
            anchors.push(GlassAnchor {
                index: elements.len(),
                surface: layer.wl_surface().clone(),
                origin: location,
                glass: drawn.glass.clone(),
                dark_from: None,
            });
        }
    };
    push_layers(&mut elements, &mut anchors, renderer, Layer::Overlay);
    // Below the screenshot tools (an overlay layer: the camera stays clear), above the
    // window: only its frame, shadow excluded.
    if let Some(frame) = context.window_veil {
        let rect = Rectangle::new(frame.loc - output_origin, frame.size)
            .to_f64()
            .to_physical_precise_round(scale);
        elements.push(OutputElement::Solid(SolidColorRenderElement::new(
            output
                .user_data()
                .get_or_insert(WindowVeilId::default)
                .0
                .clone(),
            rect,
            CommitCounter::default(),
            WINDOW_VEIL,
            Kind::Unspecified,
        )));
    }
    let top_start = elements.len();
    push_layers(&mut elements, &mut anchors, renderer, Layer::Top);

    // Closed windows fade out above the others.
    let context_id = renderer.context_id();
    for closing in context.closing {
        let progress = animation::progress(
            context.now.saturating_duration_since(closing.start),
            animation::CLOSE,
            context.animation_speed,
        );
        let texture = TextureRenderElement::from_static_texture(
            closing.id.clone(),
            context_id.clone(),
            closing.bbox.loc.to_f64(),
            closing.texture.clone(),
            1,
            Transform::Normal,
            Some(animation::close_alpha(progress)),
            None,
            Some(closing.texture.size().to_logical(1, Transform::Normal)),
            None,
            Kind::Unspecified,
        );
        elements.push(OutputElement::Texture(RescaleRenderElement::from_element(
            texture,
            closing.center,
            animation::close_scale(progress),
        )));
    }

    // Windows going into the Dock or coming out of it, above the others (and above the Dock:
    // the tail of the funnel shows inside the cell).
    let genie_program = compositor_render::shaders(renderer).genie;
    for genie in context.genies {
        let mut commit = genie.commit.get();
        commit.increment();
        genie.commit.set(commit);
        if genie.effect == MinimizeEffect::Scale {
            let (factor, origin) = genie.scale_at(context.now, context.animation_speed);
            let texture = TextureRenderElement::from_static_texture(
                genie.id.clone(),
                context_id.clone(),
                genie.snapshot.loc.to_f64(),
                genie.texture.clone(),
                1,
                Transform::Normal,
                None,
                None,
                Some(genie.texture.size().to_logical(1, Transform::Normal)),
                None,
                Kind::Unspecified,
            );
            elements.insert(
                top_start,
                OutputElement::Texture(RescaleRenderElement::from_element(texture, origin, factor)),
            );
            for anchor in &mut anchors {
                anchor.index = after_insert(anchor.index, top_start);
            }
            continue;
        }
        if let Some(element) =
            compositor_render::genie::GenieElement::new(compositor_render::genie::GenieParts {
                id: genie.id.clone(),
                commit,
                context_id: context_id.clone(),
                texture: genie.texture.clone(),
                snapshot: genie.snapshot,
                window: genie.frame,
                target: genie.target,
                shape: genie.shape(context.now, context.animation_speed),
                output: output_size,
                program: genie_program,
            })
        {
            elements.insert(top_start, OutputElement::Genie(element));
            for anchor in &mut anchors {
                anchor.index = after_insert(anchor.index, top_start);
            }
        }
    }

    // Windows with a sheet (a modal dialog of theirs on screen) are veiled under it.
    let veiled: Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> = space
        .elements()
        .filter(|w| crate::windows::is_modal(w))
        .filter_map(crate::windows::parent_of)
        .collect();
    for window in space.elements().rev() {
        // A window coming out of the Dock shows only in the genie, until the end.
        if context.genies.iter().any(|genie| genie.hides(window)) {
            continue;
        }
        let mut factor = open_scale(window, context);
        let mut offset = Point::<i32, Logical>::default();
        // Full screen in progress: the current frame goes onto the animation rectangle.
        if let Some(rect) = morph_rect(window, context)
            && let Some(geometry) = space.element_geometry(window)
        {
            let (shift, scale) = fit(frame_rect(window, geometry), rect);
            offset = shift;
            factor *= scale;
        }
        let buttons = match &context.buttons {
            Some(view) if &view.window == window => ButtonsLook {
                symbols: view.symbols,
                pressed: view.pressed,
                alt: context.alt,
            },
            _ => ButtonsLook::default(),
        };
        let Some(at) = placed(space, window) else {
            continue;
        };
        if let (Some(program), Some(toplevel)) = (shaders.border, window.toplevel())
            && veiled.contains(toplevel.wl_surface())
        {
            window.user_data().insert_if_missing(SheetVeil::default);
            if let Some(veil) = window.user_data().get::<SheetVeil>() {
                let mut frame = decorations::frame_of(window, at.1);
                frame.loc -= output_origin;
                let dark = crate::window_appearance::window_dark(window, context.dark);
                elements.push(OutputElement::Shader(RescaleRenderElement::from_element(
                    veil.0.veil_element(
                        program,
                        frame,
                        compositor_render::border::veil(dark),
                        scale,
                    ),
                    frame.to_f64().to_physical(scale).to_i32_round().loc,
                    1.0,
                )));
            }
        }
        // The window's popups above it, each with its own glass behind it.
        if let Some(toplevel) = window.toplevel() {
            let render_location = (at.0 - window.geometry().loc - output_origin + offset)
                .to_physical_precise_round(scale);
            for layer in popup_layers(renderer, window, render_location, scale) {
                elements.extend(layer.elements);
                anchors.push(GlassAnchor {
                    index: elements.len(),
                    surface: layer.surface,
                    origin: layer.origin,
                    glass: None,
                    dark_from: Some(toplevel.wl_surface().clone()),
                });
            }
        }
        elements.extend(window_elements(
            renderer,
            &shaders,
            at,
            window,
            output_origin,
            output_size,
            scale,
            offset,
            factor,
            buttons,
            context.dark,
        ));
        if let Some(toplevel) = window.toplevel()
            && let Some(location) = space.element_location(window)
        {
            anchors.push(GlassAnchor {
                index: elements.len(),
                surface: toplevel.wl_surface().clone(),
                origin: (location - window.geometry().loc - output_origin + offset)
                    .to_physical_precise_round(scale),
                glass: None,
                dark_from: None,
            });
        }
    }

    // From here down only the desktop wallpaper: it is what the frosted glass sees.
    let static_start = elements.len();
    push_layers(&mut elements, &mut anchors, renderer, Layer::Bottom);
    let background_start = elements.len();
    push_layers(&mut elements, &mut anchors, renderer, Layer::Background);

    // With the menu bar, the gradient that compresses the highlights at the top of the wallpaper.
    let has_menubar = layer_map
        .layers()
        .any(|layer| layer.namespace() == "menubar");
    if has_menubar && let Some(program) = shaders.tone {
        let backdrop = output
            .user_data()
            .get_or_insert(compositor_render::backdrop::Backdrop::default);
        let behind = elements.get(background_start..).unwrap_or_default();
        match backdrop.element(
            renderer,
            program,
            behind,
            output_size,
            scale,
            context.clear_color,
        ) {
            Ok(tone) => {
                elements.insert(background_start, OutputElement::Tone(tone));
                for anchor in &mut anchors {
                    anchor.index = after_insert(anchor.index, background_start);
                }
            }
            Err(err) => warn!("bar backdrop skipped: {err:?}"),
        }
    }
    // The glass from the back towards the front: a higher glass sees those below.
    if shaders.glass.is_some() {
        let output_rect = Rectangle::from_size(output_size);
        for anchor in anchors.iter().rev() {
            // The glass a window declares follows that window's appearance.
            let dark = crate::window_appearance::surface_dark(
                anchor.dark_from.as_ref().unwrap_or(&anchor.surface),
                context.dark,
            );
            let regions: Vec<PhysicalRegion> = anchor
                .glass
                .clone()
                .unwrap_or_else(|| current_regions(&anchor.surface))
                .iter()
                .map(|region| PhysicalRegion {
                    rect: Rectangle::new(
                        anchor.origin
                            + Point::<i32, Logical>::from((region.x, region.y))
                                .to_physical_precise_round(scale),
                        Size::<i32, Logical>::from((region.width, region.height))
                            .to_physical_precise_round(scale),
                    ),
                    radius: region.radius * scale,
                    smoothing: region.smoothing,
                    // The kind's Amalfi material (the declared material only picks the kind
                    // when the region declares none).
                    material: region.kind().kind().material(dark),
                    opacity: region.opacity as f32,
                    scale,
                    tip: match region.shape {
                        Shape::Rounded => None,
                        Shape::Tip => Some(Tip::Down),
                        Shape::UpwardTip => Some(Tip::Up),
                        Shape::LeftTip => Some(Tip::Left),
                        Shape::RightTip => Some(Tip::Right),
                    },
                    group: region.group,
                    rotation: region.rotation.to_radians(),
                    // The kind's settings; its shadow only where the client asked the compositor to
                    // draw it (it draws its own otherwise).
                    profile: {
                        let profile = region
                            .kind()
                            .kind()
                            .profile(dark)
                            .with_glass_level(context.glass_level);
                        if region.shadow {
                            profile
                        } else {
                            profile.without_shadow()
                        }
                    },
                })
                .collect();
            if regions.is_empty() {
                continue;
            }
            let index = anchor.index.min(elements.len());
            let behind = elements.get(index..).unwrap_or_default();
            let behind_static = elements.get(index.max(static_start)..).unwrap_or_default();
            // The glass blurs inside the frame only where the frame is drawn as it is.
            let front = (output.current_transform() == Transform::Normal)
                .then(|| elements.get(..index).unwrap_or_default());
            let glass = with_states(&anchor.surface, |states| {
                states.data_map.insert_if_missing(GlassCache::default);
                states.data_map.get::<GlassCache>().map(|cache| {
                    cache.elements(
                        renderer,
                        &regions,
                        front,
                        behind,
                        behind_static,
                        context.glass_quality,
                        output_rect,
                        scale,
                        context.clear_color,
                    )
                })
            });
            match glass {
                Some(Ok(glass)) => {
                    // The regions declared later are on top (a submenu on its menu): in the
                    // list, from the front down, they come first.
                    let glass: Vec<OutputElement> =
                        glass.into_iter().rev().map(OutputElement::Glass).collect();
                    elements.splice(index..index, glass);
                }
                Some(Err(err)) => warn!("glass skipped in this frame: {err:?}"),
                None => {}
            }
        }
    }
    if let Some((icon, at)) = &context.drag_icon {
        let location = (*at - output_origin.to_f64()).to_physical_precise_round(scale);
        let icon: Vec<OutputElement> = render_elements_from_surface_tree(
            renderer,
            icon,
            location,
            scale,
            1.0,
            Kind::Unspecified,
        )
        .into_iter()
        .map(OutputElement::Surface)
        .collect();
        elements.splice(0..0, icon);
    }
    elements
}

#[cfg(test)]
mod tests {
    use super::{MinimizeEffect, MorphKind, after_insert, scale_effect};
    use smithay::utils::{Logical, Physical, Rectangle};
    use std::time::Instant;

    #[test]
    fn a_tiling_morph_ends_at_the_target_without_overshoot() {
        let from = Rectangle::<f64, Logical>::new((100.0, 100.0).into(), (400.0, 300.0).into());
        let to = Rectangle::<f64, Logical>::new((0.0, 30.0).into(), (1600.0, 870.0).into());
        let start = Instant::now();
        let morph = super::Morph {
            from,
            to,
            start,
            kind: MorphKind::Tile,
        };
        let at = |ms: u64| morph.rect(start + std::time::Duration::from_millis(ms), 1.0);
        // Nothing moves for the first ~40 ms, then it slows down towards the exact frame.
        assert_eq!(at(0), Some(from));
        assert_eq!(at(30), Some(from));
        let mid = at(190).expect("halfway");
        assert!(mid.size.w > 400.0 && mid.size.w < 1600.0);
        let late = at(370).expect("almost there");
        assert!(late.size.w <= 1600.0 && late.size.w > 1590.0);
        // After ~380 ms the animation is over: the real frame is the target.
        assert_eq!(at(380), None);
        assert_eq!(at(1000), None);
    }

    #[test]
    fn scale_effect_lands_the_window_in_its_cell_keeping_the_aspect() {
        let frame = Rectangle::<f64, Physical>::new((830.0, 420.0).into(), (900.0, 600.0).into());
        let target = Rectangle::<f64, Physical>::new((1600.0, 1370.0).into(), (57.0, 47.0).into());
        let (start, _) = scale_effect(frame, target, 0.0);
        assert!((start - 1.0).abs() < 1e-9);
        let (end, origin) = scale_effect(frame, target, 1.0);
        // Fits the cell's width (57 / 900), not its height: the aspect is kept.
        assert!((end - 57.0 / 900.0).abs() < 1e-9);
        // The frame center lands on the target center.
        let center = (
            f64::from(origin.x) + (1275.0 - f64::from(origin.x)) * end,
            f64::from(origin.y) + (720.0 - f64::from(origin.y)) * end,
        );
        assert!((center.0 - 1628.5).abs() < 1.0, "{center:?}");
        assert!((center.1 - 1393.5).abs() < 1.0, "{center:?}");
        assert_eq!(MinimizeEffect::parse("scale"), Some(MinimizeEffect::Scale));
        assert_eq!(MinimizeEffect::parse("magic"), None);
    }

    #[test]
    fn glass_ending_where_the_background_starts_stays_in_front() {
        // Menu bar without windows: its glass goes at 1 and the backdrop is inserted at 1.
        assert_eq!(after_insert(1, 1), 1);
        // The glass of a background layer, behind the backdrop, moves by one.
        assert_eq!(after_insert(3, 1), 4);
        assert_eq!(after_insert(0, 1), 0);
    }
}
