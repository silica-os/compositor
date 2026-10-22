//! The list of the elements of an output with GLES: layers, windows and `compositor-render`
//! effects, from the front to the back. With pixman (headless without EGL)
//! `space_render_elements` is still used, without effects.

use std::cell::Cell;
use std::time::Instant;

use compositor_render::animation;
use compositor_render::border::WindowBorder;
use compositor_render::corners::{RoundedSurface, WindowSurface};
use compositor_render::glass::{
    GlassCache, GlassElement, GlassQuality, PhysicalRegion, Tip, render_to_texture,
};
use compositor_render::shadow::WindowShadow;
use compositor_render::title::Title;
use compositor_render::titlebar::{Titlebar, TitlebarState};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{CropRenderElement, RescaleRenderElement};
use smithay::backend::renderer::element::{AsRenderElements, Element, Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
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
    pub OutputElement<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Rounded=RoundedSurface,
    /// Pixel-shader elements of the window: title bar and shadow.
    Shader=RescaleRenderElement<PixelShaderElement>,
    Glass=GlassElement,
    /// The gradient below the menu bar, above the desktop wallpaper.
    Tone=compositor_render::backdrop::ToneElement,
    /// Textures: bar titles, snapshots of closing windows.
    Texture=RescaleRenderElement<TextureRenderElement<GlesTexture>>,
    /// A window minimizing into the Dock or coming out of it, deformed as a genie.
    Genie=compositor_render::genie::GenieElement,
    /// The black band between two scrolling spaces.
    Solid=SolidColorRenderElement,
    /// A space's desktop wallpaper shrunk into its thumbnail in the spaces bar.
    /// The elements of a window inside a space thumbnail, clipped to its cell (#181).
    CroppedSurface=CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    CroppedRounded=CropRenderElement<RoundedSurface>,
    CroppedShader=CropRenderElement<RescaleRenderElement<PixelShaderElement>>,
    CroppedTexture=CropRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>,
    Thumbnail=CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    /// A blurred capture: the Panorama wallpaper, the spaces bar material.
    Blurred=compositor_render::backdrop::BlurredElement,
}

/// The glass of the spaces bar (Glass/Thick, over the wallpaper only), between frames.
#[derive(Default)]
struct BarGlass(GlassCache);

/// The opacity of a shell layer while Panorama is open: the menu bar and the desktop (its
/// icons) fade out as Panorama opens and come back as it closes (`missioncontrol.md`: in
/// Mission Control the menu bar and what is on the desktop disappear, the Dock stays).
fn layer_alpha_in_panorama(namespace: &str, mode: crate::panorama::Mode, progress: f64) -> f32 {
    use crate::panorama::Mode;
    let fades = match mode {
        Mode::Panorama => namespace == "menubar" || namespace.starts_with("desktop-icons"),
        Mode::App => namespace == "menubar",
        Mode::Desktop => false,
    };
    if fades {
        (1.0 - progress).clamp(0.0, 1.0) as f32
    } else {
        1.0
    }
}

/// Shrinks an element drawn at output size (origin `origin`) by `factor` around that origin
/// and clips it to `cell`, the thumbnail in physical pixels. `None` when nothing is left.
fn fit_into_thumbnail<E: Element>(
    element: E,
    origin: Point<i32, Physical>,
    factor: f64,
    cell: Rectangle<i32, Physical>,
    scale: f64,
) -> Option<CropRenderElement<RescaleRenderElement<E>>> {
    CropRenderElement::from_element(
        RescaleRenderElement::from_element(element, origin, factor),
        scale,
        cell,
    )
}

/// Clips the elements of a window to `cell` (physical pixels): what is left outside, the
/// shadow included, is not drawn. What the clip does not cover (glass, genies...) is not
/// part of a window drawn in a thumbnail and is dropped.
fn crop_window_elements(
    elements: Vec<OutputElement>,
    scale: f64,
    cell: Rectangle<i32, Physical>,
) -> Vec<OutputElement> {
    elements
        .into_iter()
        .filter_map(|element| match element {
            OutputElement::Surface(e) => {
                CropRenderElement::from_element(e, scale, cell).map(OutputElement::CroppedSurface)
            }
            OutputElement::Rounded(e) => e.cropped(scale, cell).map(OutputElement::CroppedRounded),
            OutputElement::Shader(e) => {
                CropRenderElement::from_element(e, scale, cell).map(OutputElement::CroppedShader)
            }
            OutputElement::Texture(e) => {
                CropRenderElement::from_element(e, scale, cell).map(OutputElement::CroppedTexture)
            }
            _ => None,
        })
        .collect()
}

/// The layers that belong to the space and scroll with it: the wallpaper and the menu bar
/// (in the reference the Dock and the shell panels stay still).
fn moves_with_space(layer: &LayerSurface, level: Layer) -> bool {
    matches!(level, Layer::Background | Layer::Bottom) || layer.namespace() == "menubar"
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
    /// Space switch in progress: wallpaper and menu bar scroll with the windows.
    pub space_shift: Option<crate::workspaces::SpaceShift>,
    /// Panorama open or closing: arranged windows, spaces bar.
    pub panorama: Option<crate::panorama::PanoramaView>,
    /// Number of spaces and active space, for Panorama's bar.
    pub spaces: (usize, usize),
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
    /// The window dragged in Panorama: it casts the active window's shadow, scaled with it
    /// (BACKLOG-1 / #74).
    lifted: bool,
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
    pub texture: GlesTexture,
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
    pub texture: GlesTexture,
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

/// Stable id of the black band, in the output data.
struct SpaceGapId(Id);

impl Default for SpaceGapId {
    fn default() -> Self {
        Self(Id::new())
    }
}

/// The gradient at the top of App Exposé, in the output data.
#[derive(Default)]
struct AppShade(compositor_render::sprite::Sprite);

/// App Exposé's label of a window, in its data.
#[derive(Default)]
struct AppLabel(compositor_render::sprite::Sprite);

/// The titles of App Exposé's windows, below the arranged frames.
fn app_labels(
    renderer: &mut GlesRenderer,
    elements: &mut Vec<OutputElement>,
    panorama: &crate::panorama::PanoramaView,
    scale: f64,
) {
    use compositor_render::sprite::{Weight, text_width};
    let size = crate::panorama::APP_LABEL_SIZE;
    for placement in &panorama.placements {
        let title = window_title(&placement.window);
        if title.is_empty() {
            continue;
        }
        let rect = crate::panorama::interpolate(placement, panorama.progress);
        let width = f64::from(text_width(&title, size, Weight::Regular)).ceil() + 4.0;
        let height = f64::from(size) + 8.0;
        // Capitals (~9 pt) starting 14 pt below the frame.
        let baseline = f64::from(size) * 0.75 + 2.0;
        let location = Point::from((
            rect.loc.x + (rect.size.w - width) / 2.0,
            rect.loc.y + rect.size.h + crate::panorama::APP_LABEL_GAP + 9.0 - baseline,
        ));
        placement
            .window
            .user_data()
            .insert_if_missing(AppLabel::default);
        let Some(label) = placement.window.user_data().get::<AppLabel>() else {
            continue;
        };
        let element = label.0.element(
            renderer,
            &title,
            Size::from((width, height)),
            location,
            scale,
            panorama.progress as f32,
            |canvas| {
                // White with a dark shadow below.
                canvas.text(
                    [2.0, baseline as f32 + 1.0],
                    &title,
                    size,
                    Weight::Regular,
                    [0, 0, 0, 90],
                );
                canvas.text(
                    [2.0, baseline as f32],
                    &title,
                    size,
                    Weight::Regular,
                    [0xfe, 0xfe, 0xfe, 0xff],
                );
            },
        );
        if let Some(element) = element {
            elements.push(OutputElement::Texture(RescaleRenderElement::from_element(
                element,
                Point::default(),
                1.0,
            )));
        }
    }
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
    /// Opacity of the layer (the menu bar fades with Panorama).
    opacity: f32,
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

/// Whether the window casts the active shadow: the active window, and the one dragged in
/// Panorama.
fn shadow_active(active: bool, look: ButtonsLook) -> bool {
    active || look.lifted
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
    renderer: &mut GlesRenderer,
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
    renderer: &mut GlesRenderer,
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
    renderer: &mut GlesRenderer,
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
    crop: Option<Rectangle<i32, Physical>>,
) -> Vec<OutputElement> {
    let elements = window_elements_uncropped(
        renderer,
        shaders,
        (location, geometry),
        window,
        output_origin,
        output_size,
        scale,
        offset,
        factor,
        buttons,
        system_dark,
    );
    match crop {
        Some(cell) => crop_window_elements(elements, scale, cell),
        None => elements,
    }
}

#[allow(clippy::too_many_arguments)]
fn window_elements_uncropped(
    renderer: &mut GlesRenderer,
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
        None => window.render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(
            renderer,
            render_location,
            scale.into(),
            1.0,
        ),
    };
    // Full screen: no corners, bar, shadow or animation (as in the recordings).
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
                shaders.corners.clone(),
            ))
        })
        .collect();
    let active = is_active(window);
    if decorations::is_server_side(window)
        && let Some(program) = &shaders.titlebar
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
        && let Some(program) = &shaders.border
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
    if let Some(program) = &shaders.shadow {
        let style = compositor_render::shadow::style(shadow_active(active, buttons), dark);
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
    renderer: &mut GlesRenderer,
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
    renderer: &mut GlesRenderer,
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
        None,
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
    renderer: &mut GlesRenderer,
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
                       renderer: &mut GlesRenderer,
                       level| {
        for layer in layer_map.layers_on(level).rev() {
            // Not before its first frame at the configured size (the Dock in the middle of
            // the screen at session start), and where its buffer fits while it resizes.
            let Some(drawn) = crate::layers::drawn_layer(&layer_map, layer, layers_output) else {
                continue;
            };
            // During a space switch the space layers are drawn twice: with the desktop
            // leaving and with the one entering.
            let offsets: Vec<f64> = match context.space_shift {
                Some(shift) if moves_with_space(layer, level) => {
                    vec![shift.outgoing, shift.incoming]
                }
                _ => vec![0.0],
            };
            let alpha = context.panorama.as_ref().map_or(1.0, |panorama| {
                layer_alpha_in_panorama(layer.namespace(), panorama.mode, panorama.progress)
            });
            for offset in offsets {
                let location = (drawn.loc.to_f64() + Point::from((offset, 0.0)))
                    .to_physical_precise_round(scale);
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
                        opacity: alpha,
                        glass: None,
                        dark_from: Some(layer.wl_surface().clone()),
                    });
                }
                elements.extend(
                    AsRenderElements::<GlesRenderer>::render_elements::<
                        WaylandSurfaceRenderElement<GlesRenderer>,
                    >(
                        layer as &LayerSurface,
                        renderer,
                        location,
                        scale.into(),
                        alpha,
                    )
                    .into_iter()
                    .map(OutputElement::Surface),
                );
                anchors.push(GlassAnchor {
                    index: elements.len(),
                    surface: layer.wl_surface().clone(),
                    origin: location,
                    opacity: alpha,
                    glass: drawn.glass.clone(),
                    dark_from: None,
                });
            }
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
    // Panorama: the dragged window, the space thumbnails and the spaces bar, which comes
    // down over the fading menu bar.
    let mut bar_id: Option<Id> = None;
    let mut zoom_wallpaper: Vec<OutputElement> = Vec::new();
    let mut bar_behind: Option<OutputElement> = None;
    if let Some(panorama) = &context.panorama {
        let output_logical = output_size.to_f64().to_logical(scale);
        if let Some((window, rect)) = &panorama.drag
            && let Some(at) = placed(space, window)
        {
            let (offset, factor) = fit(frame_rect(window, at.1), *rect);
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
                ButtonsLook {
                    lifted: true,
                    ..ButtonsLook::default()
                },
                context.dark,
                None,
            ));
        }
        // The close buttons, in front of the thumbnails.
        let close_sprite = output
            .user_data()
            .get_or_insert(crate::panorama::CloseSprite::default);
        if let Some(buttons) = crate::panorama::close_buttons_element(
            renderer,
            close_sprite,
            &panorama.close_buttons,
            output_logical.w,
            scale,
        ) {
            elements.push(OutputElement::Texture(RescaleRenderElement::from_element(
                buttons,
                Point::default(),
                1.0,
            )));
        }
        // The ring and the title of the window under the pointer, in front of the windows.
        if let Some(hover) = &panorama.hover_window {
            let sprite = output
                .user_data()
                .get_or_insert(crate::panorama::HoverSprite::default);
            if let Some(element) =
                crate::panorama::hover_window_element(renderer, sprite, hover, scale)
            {
                elements.insert(
                    0,
                    OutputElement::Texture(RescaleRenderElement::from_element(
                        element,
                        Point::default(),
                        1.0,
                    )),
                );
            }
        }
        for thumbnail in &panorama.thumbnails {
            let s = thumbnail.rect.size.w / output_logical.w.max(1.0);
            for (window, location) in thumbnail.windows.iter().rev() {
                let geometry = Rectangle::new(*location, window.geometry().size);
                let frame = frame_rect(window, geometry);
                let target = Rectangle::new(
                    thumbnail.rect.loc + (frame.loc - output_origin.to_f64()).upscale(s),
                    frame.size.upscale(s),
                );
                let (offset, factor) = fit(frame, target);
                elements.extend(window_elements(
                    renderer,
                    &shaders,
                    (*location, geometry),
                    window,
                    output_origin,
                    output_size,
                    scale,
                    offset,
                    factor,
                    ButtonsLook::default(),
                    context.dark,
                    Some(thumbnail.rect.to_physical_precise_round(scale)),
                ));
            }
            // The desktop wallpaper, small. The surface element keeps its buffer at the
            // output size whatever scale it is created with (only the subsurface offsets
            // follow it), so it is drawn at full size from the thumbnail corner, shrunk
            // around that corner and clipped to the thumbnail.
            let cell = thumbnail.rect.to_physical_precise_round(scale);
            for layer in layer_map.layers_on(Layer::Background).rev() {
                let Some(drawn) = crate::layers::drawn_layer(&layer_map, layer, layers_output)
                else {
                    continue;
                };
                let origin = thumbnail.rect.loc.to_physical_precise_round(scale);
                let location = origin + drawn.loc.to_f64().to_physical_precise_round(scale);
                elements.extend(
                    AsRenderElements::<GlesRenderer>::render_elements::<
                        WaylandSurfaceRenderElement<GlesRenderer>,
                    >(
                        layer as &LayerSurface,
                        renderer,
                        location,
                        scale.into(),
                        1.0,
                    )
                    .into_iter()
                    .filter_map(|surface| fit_into_thumbnail(surface, origin, s, cell, scale))
                    .map(OutputElement::Thumbnail),
                );
            }
        }
        // The space magnifying out of its thumbnail: its desktop wallpaper, behind its
        // windows (inserted with the wallpaper below).
        if let Some((_, rect)) = panorama.zoom {
            let s = rect.size.w / output_logical.w.max(1.0);
            let cell = rect.to_physical_precise_round(scale);
            let origin = rect.loc.to_physical_precise_round(scale);
            for layer in layer_map.layers_on(Layer::Background).rev() {
                let Some(drawn) = crate::layers::drawn_layer(&layer_map, layer, layers_output)
                else {
                    continue;
                };
                let location = origin + drawn.loc.to_f64().to_physical_precise_round(scale);
                zoom_wallpaper.extend(
                    AsRenderElements::<GlesRenderer>::render_elements::<
                        WaylandSurfaceRenderElement<GlesRenderer>,
                    >(
                        layer as &LayerSurface,
                        renderer,
                        location,
                        scale.into(),
                        1.0,
                    )
                    .into_iter()
                    .filter_map(|surface| fit_into_thumbnail(surface, origin, s, cell, scale))
                    .map(OutputElement::Thumbnail),
                );
            }
        }
        if panorama.mode == crate::panorama::Mode::App {
            app_labels(renderer, &mut elements, panorama, scale);
        }
        let sprite = output
            .user_data()
            .get_or_insert(compositor_render::sprite::Sprite::default);
        let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        let look = crate::panorama::BarLook {
            width: output_logical.w,
            thumbnails: panorama.thumbnail_rects.clone(),
            active: context.spaces.1,
            plus: panorama.plus,
            ghost: panorama.ghost,
            ghost_pressed: panorama.ghost_pressed,
            height: output_logical.h,
            hidden: panorama.zoom.map(|(index, _)| index),
            expansion: panorama.expansion,
            backdrop: context.clear_color.map(to_u8),
            labels: panorama.labels.clone(),
            dark: context.dark,
        };
        if panorama.mode == crate::panorama::Mode::Panorama
            && let Some(bar) =
                crate::panorama::bar_element(renderer, sprite, &look, panorama.progress, scale)
        {
            bar_id = Some(bar.id().clone());
            let bar = OutputElement::Texture(RescaleRenderElement::from_element(
                bar,
                Point::default(),
                1.0,
            ));
            // A space magnifying out of its thumbnail comes out in front of the bar: the bar
            // goes behind the windows, just above the wallpaper.
            if panorama.zoom.is_some() {
                bar_behind = Some(bar);
            } else {
                elements.push(bar);
            }
        }
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
                program: genie_program.clone(),
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
        // The window dragged in Panorama is already on top.
        if context
            .panorama
            .as_ref()
            .and_then(|p| p.drag.as_ref())
            .is_some_and(|(dragged, _)| dragged == window)
        {
            continue;
        }
        // App Exposé: the windows of the other apps disappear.
        if let Some(panorama) = &context.panorama
            && panorama.mode == crate::panorama::Mode::App
            && panorama.progress > 0.0
            && !panorama.placements.iter().any(|p| &p.window == window)
        {
            continue;
        }
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
        // In Panorama the frame goes from its position to the arranged one.
        if let Some(panorama) = &context.panorama
            && let Some(placement) = panorama.placements.iter().find(|p| &p.window == window)
            && placement.from.size.w > 0.0
        {
            let now = crate::panorama::interpolate(placement, panorama.progress);
            let (shift, scale) = fit(placement.from, now);
            offset = shift;
            factor *= scale;
        }
        let buttons = match &context.buttons {
            Some(view) if &view.window == window => ButtonsLook {
                symbols: view.symbols,
                pressed: view.pressed,
                alt: context.alt,
                lifted: false,
            },
            _ => ButtonsLook::default(),
        };
        let Some(at) = placed(space, window) else {
            continue;
        };
        if let (Some(program), Some(toplevel)) = (&shaders.border, window.toplevel())
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
                    opacity: 1.0,
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
            None,
        ));
        if let Some(toplevel) = window.toplevel()
            && let Some(location) = space.element_location(window)
        {
            anchors.push(GlassAnchor {
                index: elements.len(),
                surface: toplevel.wl_surface().clone(),
                origin: (location - window.geometry().loc - output_origin + offset)
                    .to_physical_precise_round(scale),
                opacity: 1.0,
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

    // App Exposé: at the top the wallpaper darkens (above the wallpaper, below the windows).
    if let Some(panorama) = &context.panorama
        && panorama.mode == crate::panorama::Mode::App
    {
        let width = f64::from(output_size.w) / scale;
        let shade = output.user_data().get_or_insert(AppShade::default);
        if let Some(element) = shade.0.element(
            renderer,
            "shade",
            Size::from((width, crate::panorama::APP_SHADE_HEIGHT)),
            Point::default(),
            scale,
            panorama.progress as f32,
            |canvas| {
                let rows = crate::panorama::APP_SHADE_HEIGHT as usize;
                for row in 0..rows {
                    let t = 1.0 - row as f32 / rows as f32;
                    let alpha = (crate::panorama::APP_SHADE_ALPHA * t * 255.0).round() as u8;
                    canvas.fill_rect([0.0, row as f32, width as f32, 1.0], [0, 0, 0, alpha]);
                }
            },
        ) {
            elements.insert(
                background_start,
                OutputElement::Texture(RescaleRenderElement::from_element(
                    element,
                    Point::default(),
                    1.0,
                )),
            );
            for anchor in &mut anchors {
                anchor.index = after_insert(anchor.index, background_start);
            }
        }
    }

    // The black band between the two desktops, below everything: the two desktops do not cover it.
    // Before the bar backdrop, which must see it behind itself.
    if let Some(shift) = context.space_shift {
        let gap = Rectangle::<f64, Logical>::new(
            (shift.gap, 0.0).into(),
            (
                f64::from(crate::workspaces::SPACE_GAP),
                f64::from(output_size.h) / scale,
            )
                .into(),
        )
        .to_physical_precise_round(scale);
        elements.push(OutputElement::Solid(SolidColorRenderElement::new(
            output
                .user_data()
                .get_or_insert(SpaceGapId::default)
                .0
                .clone(),
            gap,
            CommitCounter::default(),
            [0.0, 0.0, 0.0, 1.0],
            Kind::Unspecified,
        )));
    }

    // With the menu bar, the gradient that compresses the highlights at the top of the wallpaper.
    let has_menubar = layer_map
        .layers()
        .any(|layer| layer.namespace() == "menubar");
    if has_menubar && let Some(program) = &shaders.tone {
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
    if let Some(bar) = bar_behind {
        elements.insert(background_start, bar);
        for anchor in &mut anchors {
            anchor.index = after_insert(anchor.index, background_start);
        }
    }
    // Above the blurred wallpaper, below the windows: the space magnifying out of its thumbnail.
    let zoomed = zoom_wallpaper.len();
    if zoomed > 0 {
        elements.splice(background_start..background_start, zoom_wallpaper);
        for anchor in &mut anchors {
            for _ in 0..zoomed {
                anchor.index = after_insert(anchor.index, background_start);
            }
        }
    }

    // The glass from the back towards the front: a higher glass sees those below.
    if let Some(programs) = &shaders.glass {
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
                    opacity: region.opacity as f32 * anchor.opacity,
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
            let glass = with_states(&anchor.surface, |states| {
                states.data_map.insert_if_missing(GlassCache::default);
                states.data_map.get::<GlassCache>().map(|cache| {
                    cache.elements(
                        renderer,
                        programs,
                        &regions,
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
    // The spaces bar is Glass/Thick over the wallpaper under it (and only that: no windows, no
    // Dock), just behind the bar's sprite.
    if let Some(panorama) = &context.panorama
        && let Some(id) = &bar_id
        && let Some(programs) = &shaders.glass
        && let Some(at) = elements.iter().position(|element| element.id() == id)
    {
        let width = f64::from(output_size.w) / scale;
        let rect = crate::panorama::bar_material_rect(width, panorama.expansion, panorama.progress)
            .to_physical_precise_round(scale);
        let mut wallpaper: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
        for layer in layer_map.layers_on(Layer::Background).rev() {
            let Some(drawn) = crate::layers::drawn_layer(&layer_map, layer, layers_output) else {
                continue;
            };
            wallpaper.extend(AsRenderElements::<GlesRenderer>::render_elements::<
                WaylandSurfaceRenderElement<GlesRenderer>,
            >(
                layer as &LayerSurface,
                renderer,
                drawn.loc.to_f64().to_physical_precise_round(scale),
                scale.into(),
                1.0,
            ));
        }
        let region = crate::panorama::bar_region(
            rect,
            context.dark,
            crate::panorama::bar_alpha(panorama.progress),
            scale,
        );
        let cache = output.user_data().get_or_insert(BarGlass::default);
        match cache.0.elements(
            renderer,
            programs,
            &[region],
            &wallpaper,
            &wallpaper,
            context.glass_quality,
            Rectangle::from_size(output_size),
            scale,
            context.clear_color,
        ) {
            Ok(glass) => {
                let glass: Vec<OutputElement> =
                    glass.into_iter().rev().map(OutputElement::Glass).collect();
                elements.splice(at + 1..at + 1, glass);
            }
            Err(err) => warn!("spaces bar glass skipped: {err:?}"),
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
    use super::{
        ButtonsLook, MinimizeEffect, MorphKind, after_insert, scale_effect, shadow_active,
    };
    use smithay::utils::{Logical, Physical, Rectangle};
    use std::time::Instant;

    /// Where the fitted wallpaper lands and the share of it that is visible.
    type Fitted = (Rectangle<i32, Physical>, f64);

    /// A layer at output size (like the desktop wallpaper) put in a space thumbnail.
    fn wallpaper_in_thumbnail(
        output: (f64, f64),
        count: usize,
        index: usize,
        scale: f64,
    ) -> (Rectangle<i32, Physical>, Option<Fitted>) {
        use smithay::backend::renderer::element::solid::SolidColorRenderElement;
        use smithay::backend::renderer::element::{Element, Id, Kind};
        use smithay::backend::renderer::utils::CommitCounter;
        use smithay::utils::{Point, Scale, Size};
        let output = Size::<f64, Logical>::from(output);
        let rects = crate::thumbnail_row::centered(count, output);
        let thumb = rects[index];
        let s = thumb.size.w / output.w;
        let cell = thumb.to_physical_precise_round(scale);
        let origin: Point<i32, Physical> = thumb.loc.to_physical_precise_round(scale);
        let full = Rectangle::new(origin, output.to_physical_precise_round(scale));
        let wallpaper = SolidColorRenderElement::new(
            Id::new(),
            full,
            CommitCounter::default(),
            [0.2, 0.3, 0.4, 1.0],
            Kind::Unspecified,
        );
        let whole = wallpaper.src();
        // How much of the wallpaper is visible: all of it, not its top-left corner.
        let fitted = super::fit_into_thumbnail(wallpaper, origin, s, cell, scale).map(|e| {
            let shown = e.src();
            (
                e.geometry(Scale::from(scale)),
                (shown.size.w * shown.size.h) / (whole.size.w * whole.size.h),
            )
        });
        (cell, fitted)
    }

    #[test]
    fn the_wallpaper_of_a_space_shrinks_into_its_thumbnail() {
        // One and several spaces, scale 1 and 2, 16:10 and 16:9 outputs.
        for (output, scale) in [
            ((1920.0, 1200.0), 1.0),
            ((1920.0, 1200.0), 2.0),
            ((2560.0, 1440.0), 1.0),
            ((1280.0, 800.0), 1.5),
        ] {
            for count in [1, 2, 5, 16] {
                for index in 0..count {
                    let (cell, fitted) = wallpaper_in_thumbnail(output, count, index, scale);
                    let Some((fitted, shown)) = fitted else {
                        panic!("{output:?} ×{scale} {index}/{count}: nothing drawn");
                    };
                    assert!(
                        cell.contains_rect(fitted),
                        "{output:?} ×{scale} {index}/{count}: {fitted:?} outside {cell:?}"
                    );
                    // It fills the thumbnail (±1 px of rounding).
                    assert!(
                        (fitted.size.w - cell.size.w).abs() <= 1
                            && (fitted.size.h - cell.size.h).abs() <= 1
                            && fitted.loc == cell.loc,
                        "{output:?} ×{scale} {index}/{count}: {fitted:?} instead of {cell:?}"
                    );
                    assert!(
                        shown > 0.97,
                        "{output:?} ×{scale} {index}/{count}: only {shown:.3} of the wallpaper"
                    );
                }
            }
        }
    }

    #[test]
    fn full_panorama_hides_the_menu_bar_and_the_desktop() {
        use crate::panorama::Mode;
        let alpha = super::layer_alpha_in_panorama;
        for namespace in ["menubar", "desktop-icons:dev.frascella.Mustache"] {
            assert_eq!(alpha(namespace, Mode::Panorama, 0.0), 1.0);
            assert_eq!(alpha(namespace, Mode::Panorama, 0.25), 0.75);
            assert_eq!(alpha(namespace, Mode::Panorama, 1.0), 0.0);
        }
        // The Dock and the wallpaper stay.
        assert_eq!(alpha("dock", Mode::Panorama, 1.0), 1.0);
        assert_eq!(alpha("wallpaper", Mode::Panorama, 1.0), 1.0);
        // App Exposé hides only the menu bar, Show Desktop nothing.
        assert_eq!(alpha("menubar", Mode::App, 1.0), 0.0);
        assert_eq!(alpha("desktop-icons:x", Mode::App, 1.0), 1.0);
        assert_eq!(alpha("menubar", Mode::Desktop, 1.0), 1.0);
    }

    #[test]
    fn the_dragged_window_casts_the_active_shadow() {
        let lifted = ButtonsLook {
            lifted: true,
            ..ButtonsLook::default()
        };
        assert!(shadow_active(false, lifted));
        assert!(shadow_active(true, ButtonsLook::default()));
        assert!(!shadow_active(false, ButtonsLook::default()));
    }

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
