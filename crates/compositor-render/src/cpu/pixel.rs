//! The pixel programs (`shadow.frag`, `border.frag`, `titlebar.frag`) on the CPU: the color
//! at a point of the element's area, painted once into a texture.

use compositor_raster::{CpuRenderer, Image};
use smithay::backend::renderer::gles::Uniform;

use super::{
    Uniforms, clamp01, length, mix3, pack, smoothstep, superellipse_distance as shape, zip_with,
};
use crate::Program;

/// A pixel program with its uniforms read.
#[derive(Debug)]
pub(crate) enum PixelProgram {
    Shadow(Shadow),
    Border(Border),
    Titlebar(Box<Titlebar>),
}

impl PixelProgram {
    pub fn new(program: Program, uniforms: &[Uniform<'static>]) -> Option<Self> {
        let u = Uniforms(uniforms);
        Some(match program {
            Program::Shadow => Self::Shadow(Shadow::new(&u)),
            Program::Border => Self::Border(Border::new(&u)),
            Program::Titlebar => Self::Titlebar(Box::new(Titlebar::new(&u))),
            _ => return None,
        })
    }

    /// The premultiplied color at `p` (points in an area `size` points large), before the
    /// element's alpha.
    fn color(&self, p: [f32; 2], size: [f32; 2]) -> [f32; 4] {
        match self {
            Self::Shadow(s) => s.color(p),
            Self::Border(b) => b.color(p, size),
            Self::Titlebar(t) => t.color(p, size),
        }
    }

    /// Paints the part `src` (x, y, w, h in points) of an area `size` points large into
    /// `image`, at the pixel centres as GLES does.
    pub fn paint(&self, renderer: &CpuRenderer, size: [f32; 2], src: [f32; 4], image: &mut Image) {
        let (w, h) = (image.width(), image.height());
        let (kx, ky) = (src[2] / w as f32, src[3] / h as f32);
        renderer.rows_costly(image.pixels_mut(), w, |y, row| {
            let py = src[1] + (y as f32 + 0.5) * ky;
            for (x, out) in row.iter_mut().enumerate() {
                *out = pack(self.color([src[0] + (x as f32 + 0.5) * kx, py], size));
            }
        });
    }
}

/// erfc with the approximation 7.1.26 of Abramowitz and Stegun, as `shadow.frag`.
fn erfc(x: f32) -> f32 {
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_8 + t * (-1.453_152 + t * 1.061_405_4))));
    let e = poly * (-z * z).exp();
    if x >= 0.0 { e } else { 2.0 - e }
}

#[derive(Debug)]
pub(crate) struct Shadow {
    window: [f32; 4],
    radius: f32,
    exponent: f32,
    offset_y: f32,
    amplitude: f32,
    shift: f32,
    sigma: f32,
    ring_side: f32,
    ring_top: f32,
}

impl Shadow {
    fn new(u: &Uniforms<'_>) -> Self {
        Self {
            window: u.f4("window_rect"),
            radius: u.f1("radius"),
            exponent: u.f1("exponent"),
            offset_y: u.f1("offset_y"),
            amplitude: u.f1("amplitude"),
            shift: u.f1("shift"),
            sigma: u.f1("sigma"),
            ring_side: u.f1("ring_side"),
            ring_top: u.f1("ring_top"),
        }
    }

    fn color(&self, p: [f32; 2]) -> [f32; 4] {
        let body = shape(p, self.window, self.radius, self.exponent);
        let mut moved = self.window;
        moved[1] += self.offset_y;
        let d = shape(p, moved, self.radius, self.exponent) - 0.5;
        let shadow =
            self.amplitude * 0.5 * erfc((d - self.shift) / (std::f32::consts::SQRT_2 * self.sigma));
        let ring = if p[1] < self.window[1] {
            self.ring_top
        } else {
            self.ring_side
        };
        let ring_coverage = clamp01(body + 0.5) * clamp01(1.5 - body);
        let window_coverage = clamp01(0.5 - body);
        let a = shadow.max(ring * ring_coverage) * (1.0 - window_coverage);
        [0.0, 0.0, 0.0, a]
    }
}

#[derive(Debug)]
pub(crate) struct Border {
    radius: f32,
    exponent: f32,
    pixel: f32,
    rim: [f32; 4],
    fill: f32,
}

impl Border {
    fn new(u: &Uniforms<'_>) -> Self {
        Self {
            radius: u.f1("radius"),
            exponent: u.f1("exponent"),
            pixel: u.f1("pixel"),
            rim: u.f4("rim"),
            fill: u.f1("fill"),
        }
    }

    fn color(&self, p: [f32; 2], size: [f32; 2]) -> [f32; 4] {
        let d = shape(p, [0.0, 0.0, size[0], size[1]], self.radius, self.exponent);
        let cover = |d: f32| clamp01(0.5 - d / self.pixel);
        let band = if self.fill > 0.5 {
            cover(d)
        } else {
            cover(d) - cover(d + 1.0)
        };
        let [r, g, b, a] = self.rim;
        [r * a * band, g * a * band, b * a * band, a * band]
    }
}

/// The colors of one window button: centre, top, bottom and the outline at the sides, top and
/// bottom.
#[derive(Debug, Clone, Copy)]
struct Disc {
    c: [f32; 3],
    t: [f32; 3],
    b: [f32; 3],
    o: [f32; 3],
    ot: [f32; 3],
    ob: [f32; 3],
}

impl Disc {
    fn new(t: [f32; 3], b: [f32; 3], o: [f32; 3], ot: [f32; 3], ob: [f32; 3]) -> Self {
        Self {
            c: mix3(t, b, 0.5),
            t,
            b,
            o,
            ot,
            ob,
        }
    }

    /// A dark disc: one rim color all around.
    fn dark(t: [f32; 3], b: [f32; 3], o: [f32; 3]) -> Self {
        Self::new(t, b, o, o, o)
    }

    /// A light disc: the outline weaker (`weak`) at the top and bottom.
    fn light(t: [f32; 3], b: [f32; 3], o: [f32; 3], weak: f32) -> Self {
        Self::new(t, b, o, mix3(t, o, weak), mix3(b, o, weak))
    }
}

/// One button to draw: its disc, the pressed shift, grey or lit.
#[derive(Debug, Clone, Copy)]
struct Button {
    center: [f32; 2],
    disc: Disc,
    shift: [f32; 3],
    grey: bool,
    /// The symbol's color and kind when the symbols show.
    symbol: Option<([f32; 3], Symbol)>,
}

#[derive(Debug, Clone, Copy)]
enum Symbol {
    Close,
    Minimize,
    Plus,
    Zoom,
}

#[derive(Debug)]
pub(crate) struct Titlebar {
    frame_height: f32,
    radius: f32,
    exponent: f32,
    pixel: f32,
    background: [f32; 3],
    separator: [f32; 4],
    symbols: f32,
    bar: f32,
    dark: bool,
    edge_fade: f32,
    shadow: f32,
    buttons: [Button; 3],
}

const fn rgb(r: f32, g: f32, b: f32) -> [f32; 3] {
    [r, g, b]
}

impl Titlebar {
    // The palette: 0.318 is a green, not 1/π.
    #[allow(clippy::approx_constant)]
    fn new(u: &Uniforms<'_>) -> Self {
        let active = u.f1("active");
        let symbols = u.f1("symbols");
        let pressed = u.f1("pressed");
        let alt = u.f1("alt");
        let disabled = u.f1("disabled");
        let dark = u.f1("dark") > 0.5;
        let weak = u.f1("edge_weak");
        let lit = active > 0.5 || symbols > 0.0;
        let (close, minimize, zoom, grey) = if dark {
            let grey_t = rgb(0.302, 0.302, 0.302);
            let grey_b = rgb(0.333, 0.333, 0.333);
            let grey_c = mix3(grey_t, grey_b, 0.5);
            let light = rgb(0.600, 0.600, 0.600);
            (
                Disc::dark(
                    rgb(0.973, 0.388, 0.353),
                    rgb(0.933, 0.400, 0.376),
                    rgb(0.933, 0.333, 0.286),
                ),
                Disc::dark(
                    rgb(0.980, 0.710, 0.000),
                    rgb(0.992, 0.804, 0.161),
                    rgb(0.973, 0.725, 0.016),
                ),
                Disc::dark(
                    rgb(0.004, 0.737, 0.000),
                    rgb(0.294, 0.757, 0.200),
                    rgb(0.035, 0.725, 0.004),
                ),
                Disc::new(
                    grey_t,
                    grey_b,
                    mix3(grey_c, light, 0.351),
                    mix3(grey_t, light, 0.351),
                    mix3(grey_b, light, 0.351),
                ),
            )
        } else {
            (
                Disc::light(
                    rgb(0.996, 0.431, 0.400),
                    rgb(0.973, 0.557, 0.525),
                    rgb(0.871, 0.306, 0.282),
                    weak,
                ),
                Disc::light(
                    rgb(0.992, 0.725, 0.184),
                    rgb(0.996, 0.827, 0.282),
                    rgb(0.816, 0.553, 0.043),
                    weak,
                ),
                Disc::light(
                    rgb(0.384, 0.816, 0.180),
                    rgb(0.604, 0.855, 0.486),
                    rgb(0.318, 0.620, 0.196),
                    weak,
                ),
                Disc::light(
                    rgb(0.957, 0.957, 0.969),
                    rgb(0.918, 0.922, 0.925),
                    rgb(0.765, 0.765, 0.780),
                    0.5,
                ),
            )
        };
        let (close, minimize, zoom) = if lit {
            (close, minimize, zoom)
        } else {
            (grey, grey, grey)
        };
        let zoom_off = disabled > 0.5;
        let min_off = disabled > 1.5;
        let shift = |v: [f32; 3]| v.map(|c| c / 255.0);
        let close_p = shift(if dark {
            [14.0, 31.0, 19.0]
        } else {
            [6.0, 35.0, 28.0]
        });
        let min_p = shift(if dark {
            [2.0, 50.0, 18.0]
        } else {
            [0.0, 45.0, 28.0]
        });
        let zoom_p = shift(if dark {
            [4.0, 8.0, 10.0]
        } else {
            [1.0, 6.0, -9.0]
        });
        let none = [0.0; 3];
        let pick = |on: bool, a: [f32; 3], b: [f32; 3]| if on { a } else { b };
        let (close_y, min_y, zoom_y) = if dark {
            (
                pick(pressed == 0.0, rgb(0.733, 0.0, 0.0), rgb(0.549, 0.0, 0.0)),
                pick(
                    pressed == 1.0,
                    rgb(0.867, 0.298, 0.0),
                    rgb(0.647, 0.227, 0.0),
                ),
                pick(
                    pressed == 2.0,
                    rgb(0.200, 0.494, 0.0),
                    rgb(0.176, 0.447, 0.004),
                ),
            )
        } else {
            (
                pick(pressed == 0.0, rgb(0.855, 0.0, 0.0), rgb(0.639, 0.0, 0.0)),
                pick(pressed == 1.0, rgb(1.0, 0.353, 0.0), rgb(0.757, 0.267, 0.0)),
                pick(
                    pressed == 2.0,
                    rgb(0.231, 0.588, 0.0),
                    rgb(0.212, 0.533, 0.0),
                ),
            )
        };
        let show = symbols > 0.0;
        let (bx, by) = (u.f1("button_x"), u.f1("button_y"));
        let buttons = [
            Button {
                center: [bx, by],
                disc: close,
                shift: pick(pressed == 0.0, close_p, none),
                grey: !lit,
                symbol: show.then_some((close_y, Symbol::Close)),
            },
            Button {
                center: [bx + 23.0, by],
                disc: if min_off { grey } else { minimize },
                shift: pick(pressed == 1.0 && !min_off, min_p, none),
                grey: min_off || !lit,
                symbol: (show && !min_off).then_some((min_y, Symbol::Minimize)),
            },
            Button {
                center: [bx + 46.0, by],
                disc: if zoom_off { grey } else { zoom },
                shift: pick(pressed == 2.0 && !zoom_off, zoom_p, none),
                grey: zoom_off || !lit,
                symbol: (show && !zoom_off).then_some((
                    zoom_y,
                    if alt > 0.5 {
                        Symbol::Plus
                    } else {
                        Symbol::Zoom
                    },
                )),
            },
        ];
        Self {
            frame_height: u.f1("frame_height"),
            radius: u.f1("radius"),
            exponent: u.f1("exponent"),
            pixel: u.f1("pixel"),
            background: u.f3("background"),
            separator: u.f4("separator"),
            symbols,
            bar: u.f1("bar"),
            dark,
            edge_fade: u.f1("edge_fade"),
            shadow: u.f1("shadow"),
            buttons,
        }
    }

    fn cover(&self, distance: f32) -> f32 {
        clamp01(0.5 - distance / self.pixel)
    }

    /// One button's disc over nothing, premultiplied.
    fn button(&self, p: [f32; 2], button: &Button) -> [f32; 4] {
        let (dx, dy) = (p[0] - button.center[0], p[1] - button.center[1]);
        let r = length(dx, dy);
        // Far from the disc and its shadow nothing is drawn: skip the rest.
        if r > 9.0 + 2.0 * self.pixel {
            return [0.0; 4];
        }
        let disc = self.cover(r - 7.0);
        let d = &button.disc;
        let plain = if self.dark {
            mix3(d.t, d.b, clamp01((dy + 7.0) / 14.0))
        } else if dy < 0.0 {
            mix3(d.c, d.t, clamp01(-dy / 6.0))
        } else {
            mix3(d.c, d.b, clamp01(dy / 5.0))
        };
        let fill = zip_with(plain, button.shift, |p, s| clamp01(p + s));
        let theta = dx.abs().atan2(dy.abs().max(0.0001));
        let arc = 1.0 - smoothstep(0.0, self.edge_fade, theta);
        let edge = if dy < 0.0 { d.ot } else { d.ob };
        let outline = mix3(d.o, edge, arc);
        let ring = clamp01(r - 6.0);
        let base = mix3(fill, outline, ring);
        let under = if button.grey || self.shadow <= 0.0 {
            0.0
        } else {
            self.shadow * self.cover(length(dx, dy - 1.0) - 7.0)
        };
        over(
            [base[0] * disc, base[1] * disc, base[2] * disc, disc],
            [0.0, 0.0, 0.0, under],
        )
    }

    fn symbol(&self, d: [f32; 2], symbol: Symbol) -> f32 {
        let (x, y) = (d[0], d[1]);
        match symbol {
            Symbol::Close => {
                let a = (x - y).abs() / std::f32::consts::SQRT_2;
                let b = (x + y).abs() / std::f32::consts::SQRT_2;
                let stroke = a.min(b) - 1.0;
                let frame = x.abs().max(y.abs()) - 4.0;
                self.cover(stroke.max(frame))
            }
            Symbol::Minimize => self.cover((x.abs() - 4.0).max(y.abs() - 1.0)),
            Symbol::Plus => {
                let h = (x.abs() - 4.0).max(y.abs() - 0.8);
                let v = (y.abs() - 4.0).max(x.abs() - 0.8);
                self.cover(h.min(v))
            }
            Symbol::Zoom => {
                let frame = (x.abs() - 3.0).max(y.abs() - 3.0);
                let gap = 0.75 - (x + y).abs() / std::f32::consts::SQRT_2;
                self.cover(frame.max(gap))
            }
        }
    }

    fn color(&self, p: [f32; 2], size: [f32; 2]) -> [f32; 4] {
        let outline = shape(
            p,
            [0.0, 0.0, size[0], self.frame_height],
            self.radius,
            self.exponent,
        );
        let bar = self.bar > 0.5;
        let mut color = if bar {
            let [r, g, b] = self.background;
            [r, g, b, 1.0]
        } else {
            [0.0; 4]
        };
        if bar && self.separator[3] > 0.0 && p[1] > size[1] - 1.0 {
            let [r, g, b] = mix3(
                self.background,
                [self.separator[0], self.separator[1], self.separator[2]],
                self.separator[3],
            );
            color = [r, g, b, 1.0];
        }
        for button in &self.buttons {
            color = over(self.button(p, button), color);
        }
        for button in &self.buttons {
            if let Some((rgb, symbol)) = button.symbol {
                let d = [p[0] - button.center[0], p[1] - button.center[1]];
                let k = self.symbol(d, symbol) * self.symbols;
                color = over([rgb[0] * k, rgb[1] * k, rgb[2] * k, k], color);
            }
        }
        let k = self.cover(outline);
        color.map(|c| c * k)
    }
}

/// Premultiplied `top` over `bottom`.
fn over(top: [f32; 4], bottom: [f32; 4]) -> [f32; 4] {
    let k = 1.0 - top[3];
    zip_with(top, bottom, |t, b| t + b * k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erfc_matches_known_values() {
        assert!((erfc(0.0) - 1.0).abs() < 1e-6);
        assert!((erfc(1.0) - 0.157_299_2).abs() < 1e-5);
        assert!((erfc(-1.0) - 1.842_700_8).abs() < 1e-5);
    }

    #[test]
    fn over_keeps_an_opaque_top() {
        assert_eq!(
            over([0.5, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0]),
            [0.5, 0.0, 0.0, 1.0]
        );
    }
}
