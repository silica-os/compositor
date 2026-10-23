#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Size of the texture (background already blurred) in physical pixels, and the regions inside
// the texture: up to 8 rectangles (one column per rectangle: x, y, width, height), their
// radii, how many there are. The regions merge with a smooth union within `merge` pixels,
// like drops. Corner exponent: 2, the measured glasses have circular arcs.
uniform vec2 tex_size;
uniform mat4 shapes[2];
uniform mat4 radii;
// Each region's rotation about the centre of its rectangle (set_rotation), as (cos, sin), two
// per column: region 2j in xy, 2j + 1 in zw. The rectangles are the ones before the rotation.
uniform mat4 rotations;
// The tips' outlines (TIP-01), one column per region: half base of the sharp flanks, tan of the
// flank angle, apex radius, fillet radius (pixels).
uniform mat4 tips[2];
uniform float shape_count;
uniform float merge;
uniform float exponent;
// Material color on the encoded sRGB values: out = M · in + b + q2 · l² + q3 · l³ (rows of M,
// l the mean of the input channels).
uniform vec3 row_r;
uniform vec3 row_g;
uniform vec3 row_b;
uniform vec3 offset;
uniform vec3 curve2;
uniform vec3 curve3;
// Physical pixels per point.
uniform float scale_px;
// Edge lens: per side (0 sides, 1 top, 2 bottom) the (t, s) points in pixels: a pixel at
// distance t from the edge samples the background at distance s (negative: outside the edge).
// Columns of every mat4: t0, s0, t1, s1. lens_reach: how far the lens reaches.
uniform mat4 lens[3];
uniform float lens_reach;
// Edge, measured: per orientation (0 top, 1 sides, 2 bottom) the offsets inside the edge,
// one per point (0–11, levels 0–1 relative to the fill on gray), and the alpha of black on
// the first two points outside (slots 12, 13). Lights (> 0) add less over lighter colors,
// shadows (< 0) multiply. `fill_gray`: the fill over the reference gray.
uniform mat4 edges[3];
uniform float fill_gray;
// How fast an orientation's edge fades around a curve: its weight is the matching component of
// the normal to this power. On a capsule's end the measured dark side line is gone by ~45°
// from the side and the light rows by ~45° from the top (fit on the Control Center modules):
// the square of the component spread the side line over the whole end, a black crescent.
uniform float edge_power;
// The profile of the surface kind (glass_profile.rs). The lens band's width per region (one
// slot per region, like the radii: the profile's falloff narrowed to the region's thickness),
// the lens strength, the edge light and dark, saturation and tint over the material, and the
// shadow outside the shape (black alpha, σ and downward offset in pixels; 0 alpha: none).
uniform mat4 lens_widths;
uniform float refraction;
// The split of the red and blue samples along the normal at the rim, as a fraction of the
// refraction band (0: none), and the backdrop's brightness (a multiplier before the tint).
uniform float chromatic;
uniform float brightness;
uniform float rim;
uniform float line;
uniform float saturation;
uniform vec4 tint_color;
uniform vec3 shadow;
// Whether the material has an edge of its own (`edges`): the Amalfi materials have none.
uniform float edge_on;
// The Amalfi rims (glass_profile.rs), after the tint: the inside rim, a band `inside_rim_width`
// pixels wide just inside the edge, filled with a color; the rim light, an inner shadow of a
// color: the outside of the shape moved down by `rim_light_shape.x` pixels and blurred by σ
// `rim_light_shape.y`, drawn inside it (it lights the top inner edge).
uniform vec4 inside_rim;
uniform float inside_rim_width;
uniform vec4 rim_light;
uniform vec2 rim_light_shape;
// Without blur (the opaque quality level, `fallback.a` 1): the tint covers `fallback.rgb`
// (Surface/Window) instead of the blurred backdrop.
uniform vec4 fallback;

// The rotation of region i, (cos, sin).
vec2 rotation_of(int i) {
    vec4 pair = rotations[i / 2];
    return (i - (i / 2) * 2 == 0) ? pair.xy : pair.zw;
}

// A screen vector in the frame of a region rotated by cs: turned back by the rotation.
vec2 to_frame(vec2 v, vec2 cs) {
    return vec2(cs.x * v.x + cs.y * v.y, -cs.y * v.x + cs.x * v.y);
}

// A point in the frame of the region `rect` rotated by cs about its centre, where the
// rectangle is upright again (rotations keep distances, so every shape is measured there).
vec2 to_region(vec2 p, vec4 rect, vec2 cs) {
    vec2 c = rect.xy + rect.zw * 0.5;
    return c + to_frame(p - c, cs);
}

float rounded_distance(vec2 p, vec4 rect, float r) {
    vec2 half_size = rect.zw * 0.5;
    r = min(r, min(half_size.x, half_size.y));
    vec2 d = abs(p - (rect.xy + half_size)) - (half_size - vec2(r));
    if (r > 0.0 && d.x > 0.0 && d.y > 0.0) {
        vec2 q = d / r;
        float f = pow(pow(q.x, exponent) + pow(q.y, exponent), 1.0 / exponent);
        return (f - 1.0) * r;
    }
    return max(d.x, d.y) - max(r, 0.0);
}

// The same with continuous corners: the corner starts r * (1 + s) from the corner point along
// each edge and is a superellipse over that extent (exponent 2 at s = 0, 3.53 at 0.6, the fit
// of the Sketch body path); `continuous_exponent` in glass.rs is the CPU twin.
float continuous_distance(vec2 p, vec4 rect, float r, float s) {
    vec2 half_size = rect.zw * 0.5;
    float a = min(r * (1.0 + s), min(half_size.x, half_size.y));
    vec2 d = abs(p - (rect.xy + half_size)) - (half_size - vec2(a));
    if (a > 0.0 && d.x > 0.0 && d.y > 0.0) {
        float n = 2.0 + 2.55 * s;
        vec2 q = d / a;
        float f = pow(pow(q.x, n) + pow(q.y, n), 1.0 / n);
        float grad = sqrt(pow(q.x, 2.0 * n - 2.0) + pow(q.y, 2.0 * n - 2.0)) / pow(f, n - 1.0);
        return (f - 1.0) * a / max(grad, 1e-3);
    }
    return max(d.x, d.y) - a;
}

// A region's corner: circular, or continuous when its smoothing (the x of its tip column) is
// above 0.
float region_distance(vec2 p, vec4 rect, float r, float s) {
    return s > 0.0 ? continuous_distance(p, rect, r, s) : rounded_distance(p, rect, r);
}

// Distance from the arc of `c`, radius `r`, between the directions `a0` and `a1` (an arc
// under 180°); far away outside that sector (the neighbouring pieces cover its ends).
float arc_distance(vec2 q, vec2 c, float r, vec2 a0, vec2 a1) {
    vec2 v = q - c;
    float turn = a0.x * a1.y - a0.y * a1.x;
    bool inside = (a0.x * v.y - a0.y * v.x) * turn >= 0.0
               && (v.x * a1.y - v.y * a1.x) * turn >= 0.0;
    return inside ? abs(length(v) - r) : 1.0e6;
}

float segment_distance(vec2 q, vec2 a, vec2 b) {
    vec2 ab = b - a;
    float h = clamp(dot(q - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(q - a - ab * h);
}

// The tip measured in TIP-01, in its own frame: the body edge along y = 0 (x from 0 to
// `size.x`, the axis in the middle), the apex towards +y. Not a triangle: from the body edge a
// concave fillet (radius rf) turns into a straight flank at angle a from the edge, which ends
// in a round apex (radius ra) at depth `size.y`. k: half base hb of the sharp flanks, tan(a),
// ra, rf. The shape goes on 2 pt into the body (no seam with it) and stops where its fillets
// leave the edge, so near a corner it does not stick out of the body's side.
float tip_distance(vec2 p, vec2 size, vec4 k) {
    float hb = k.x;
    float ta = k.y;
    float ra = k.z;
    float rf = k.w;
    float ca = inversesqrt(1.0 + ta * ta);
    float sa = ta * ca;
    // tan(a / 2)
    float th = sa / (1.0 + ca);
    vec2 q = vec2(abs(p.x - 0.5 * size.x), p.y);
    // From the sharp base corner b along the flank (u, towards the apex).
    vec2 u = vec2(-ca, sa);
    vec2 b = vec2(hb, 0.0);
    float w = hb + rf * th;
    vec2 t2 = b + rf * th * u;
    vec2 apex = vec2(0.0, hb * ta);
    vec2 t3 = apex - ra * ta * u;
    vec2 cf = vec2(w, rf);
    vec2 cr = vec2(0.0, apex.y - ra / ca);
    float d = q.x >= w ? abs(q.y) : length(q - vec2(w, 0.0));
    d = min(d, arc_distance(q, cf, rf, vec2(0.0, -1.0), normalize(t2 - cf + vec2(1e-6, 0.0))));
    d = min(d, segment_distance(q, t2, t3));
    d = min(d, arc_distance(q, cr, ra, normalize(t3 - cr + vec2(1e-6, 0.0)), vec2(0.0, 1.0)));
    // Inside: above the outline, which is a function of |x|.
    float edge = q.x >= w ? 0.0
               : q.x >= t2.x ? rf - sqrt(max(rf * rf - (q.x - w) * (q.x - w), 0.0))
               : q.x >= t3.x ? (hb - q.x) * ta
               : cr.y + sqrt(max(ra * ra - q.x * q.x, 0.0));
    float sd = q.y < edge ? -d : d;
    float overlap = 2.0 * scale_px;
    return max(sd, max(-p.y - overlap, q.x - w));
}

// Smooth (polynomial) minimum: the bridge between two nearby shapes.
float smooth_min(float a, float b, float k) {
    if (k <= 0.0) {
        return min(a, b);
    }
    float h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

// The lens band's width of the region nearest to p, and that region's rotation (`frame`).
float lens_width_at(vec2 p, out vec2 frame) {
    float best = 1.0e6;
    float width = 1.0;
    frame = vec2(1.0, 0.0);
    for (int i = 0; i < 8; i++) {
        if (float(i) >= shape_count) {
            break;
        }
        vec4 rect = shapes[i / 4][i - (i / 4) * 4];
        float r = max(radii[i / 4][i - (i / 4) * 4], 0.0);
        vec2 cs = rotation_of(i);
        float d = region_distance(to_region(p, rect, cs), rect, r, tips[i / 4][i - (i / 4) * 4].x);
        if (d < best) {
            best = d;
            width = lens_widths[i / 4][i - (i / 4) * 4];
            frame = cs;
        }
    }
    return max(width, 0.05);
}

// Signed distance from the union of the regions, with corners at least `min_radius`; each
// region moved by `shift` in its own frame (the shadow's offset turns with the region).
float union_distance(vec2 screen, float min_radius, vec2 shift) {
    float d = 1.0e6;
    for (int i = 0; i < 8; i++) {
        if (float(i) >= shape_count) {
            break;
        }
        vec4 rect = shapes[i / 4][i - (i / 4) * 4];
        float r = radii[i / 4][i - (i / 4) * 4];
        vec2 p = to_region(screen, rect, rotation_of(i)) - shift;
        // A tip: −1 points down, −2 up, −3 left, −4 right, turned into the downward frame (a
        // reflection or a transposition, which keep distances). Its outline already holds the
        // fillets into the body: a plain union, not the merge distance of separate shapes.
        bool tip = r < 0.0;
        vec4 k = tips[i / 4][i - (i / 4) * 4];
        vec2 near = p - rect.xy;
        vec2 far = rect.xy + rect.zw - p;
        float shape = r > -0.5 ? region_distance(p, rect, max(r, min_radius), k.x)
                    : r > -1.5 ? tip_distance(near, rect.zw, k)
                    : r > -2.5 ? tip_distance(vec2(near.x, far.y), rect.zw, k)
                    : r > -3.5 ? tip_distance(vec2(near.y, far.x), rect.wz, k)
                               : tip_distance(near.yx, rect.wz, k);
        d = tip ? min(d, shape) : smooth_min(d, shape, merge);
    }
    return d;
}

// A measured edge profile at position s (points from the edge, 0 = the first one).
float profile_at(mat4 m, float s) {
    s = max(s, 0.0);
    float i0 = floor(s);
    float a = 0.0;
    float b = 0.0;
    for (int i = 0; i < 12; i++) {
        float v = m[i / 4][i - (i / 4) * 4];
        if (float(i) == i0) {
            a = v;
        }
        if (float(i) == i0 + 1.0) {
            b = v;
        }
    }
    return mix(a, b, s - i0);
}

// The dark outline just outside: alpha at position u (points from the edge, 0 = the first).
float outline_at(mat4 m, float u) {
    u = max(u, 0.0);
    return u < 1.0 ? mix(m[3][0], m[3][1], u) : mix(m[3][1], 0.0, clamp(u - 1.0, 0.0, 1.0));
}

// What the three orientations share, for the part of a curve none of them owns (a corner's
// diagonal): the weakest value if they agree in sign, nothing if they do not.
float shared_value(float a, float b, float c) {
    if (a >= 0.0 && b >= 0.0 && c >= 0.0) {
        return min(a, min(b, c));
    }
    if (a <= 0.0 && b <= 0.0 && c <= 0.0) {
        return max(a, max(b, c));
    }
    return 0.0;
}

// The source distance for a pixel at distance t, interpolating the curve.
float lens_source(mat4 curve, float t) {
    vec2 previous = vec2(-1.0, 0.0);
    for (int i = 0; i < 8; i++) {
        vec4 column = curve[i / 2];
        vec2 q = (i - (i / 2) * 2 == 0) ? column.xy : column.zw;
        if (q.x > 1.0e5) {
            break;
        }
        if (t <= q.x) {
            if (previous.x < 0.0) {
                return q.y;
            }
            return mix(previous.y, q.y, (t - previous.x) / max(q.x - previous.x, 1e-3));
        }
        previous = q;
    }
    return t;
}

void main() {
    vec2 p = v_coords * tex_size;
    vec2 none = vec2(0.0);
    float sd = union_distance(p, 0.0, none);
    float t = -sd;
    vec2 e = vec2(0.5, 0.0);
    // The nearest region's frame: its own top, sides and bottom pick the lens curves and the
    // edge profiles however it is rotated (the displacement itself stays along the screen
    // normal, the background is sampled in screen space).
    vec2 frame;
    float band = lens_width_at(p, frame);

    // Lens: the pixel at distance t samples the background at distance s along the normal
    // (glass.md, "Refraction"). The profile scales the band's width (the curve keeps its
    // shape: evaluated at t / width, its displacement times width) and the displacement.
    // The Amalfi glass (Regular) uses it as a refraction at the edge: the curve
    // pulls the backdrop in over a band along the edge, nothing past it, and `chromatic` splits
    // the red and blue samples a little along the normal.
    vec2 sample_at = p;
    vec2 split = vec2(0.0);
    if (refraction > 0.0) {
        // Outward normal from the distance (central differences), on a shape with corners at
        // least as wide as the refraction band: without it, on the diagonal of a right angle
        // the normal jumps and the displacement leaves a seam.
        vec2 n = vec2(union_distance(p + e.xy, lens_reach, none) - union_distance(p - e.xy, lens_reach, none),
                      union_distance(p + e.yx, lens_reach, none) - union_distance(p - e.yx, lens_reach, none));
        n = length(n) > 0.0 ? normalize(n) : vec2(0.0, -1.0);
        vec2 nf = to_frame(n, frame);
        float vertical = nf.y * nf.y;
        float tb = t / band;
        float measured = mix(lens_source(lens[0], tb),
                             lens_source(nf.y < 0.0 ? lens[1] : lens[2], tb), vertical);
        float source = t + refraction * band * (measured - tb);
        sample_at = p + n * (t - source);
        // The split grows from the band's inner side to the rim.
        float rim_ness = clamp(1.0 - tb / max(lens_reach, 1.0), 0.0, 1.0);
        split = n * (chromatic * band * lens_reach * rim_ness);
    }
    // Clamped to the texture: it repeats by default, and outside it lies the opposite side.
    vec2 half_texel = 0.5 / tex_size;
    vec2 sample_g = clamp(sample_at / tex_size, half_texel, vec2(1.0) - half_texel);

    // Without blur the tint covers the fallback surface, not the backdrop.
    bool opaque = fallback.a > 0.5;
    vec3 background;
    if (opaque) {
        background = fallback.rgb;
    } else if (chromatic > 0.0 && refraction > 0.0) {
        vec2 sample_r = clamp((sample_at + split) / tex_size, half_texel, vec2(1.0) - half_texel);
        vec2 sample_b = clamp((sample_at - split) / tex_size, half_texel, vec2(1.0) - half_texel);
        background = vec3(texture2D(tex, sample_r).r, texture2D(tex, sample_g).g,
                          texture2D(tex, sample_b).b);
    } else {
        background = texture2D(tex, sample_g).rgb;
    }
    vec3 color = vec3(dot(row_r, background), dot(row_g, background), dot(row_b, background)) + offset;
    float luminance = (background.r + background.g + background.b) / 3.0;
    color += (curve2 + curve3 * luminance) * luminance * luminance;
    // The profile over the material: saturation around the luminance (not on the fallback
    // surface), then the tint fill.
    float rec709 = dot(color, vec3(0.2126, 0.7152, 0.0722));
    color = mix(vec3(rec709), color, opaque ? 1.0 : saturation);
    if (!opaque) {
        color = clamp(color * brightness, 0.0, 1.0);
    }
    color = mix(color, tint_color.rgb, tint_color.a);

    // The material's own edge, if any: the profiles, weighted by the orientation of the true
    // shape's normal (top, sides, bottom), so corners, tips and merged bridges blend
    // continuously.
    float outline = 0.0;
    if (edge_on > 0.5) {
        vec2 m = vec2(union_distance(p + e.xy, 0.0, none) - union_distance(p - e.xy, 0.0, none),
                      union_distance(p + e.yx, 0.0, none) - union_distance(p - e.yx, 0.0, none));
        m = length(m) > 0.0 ? to_frame(normalize(m), frame) : vec2(0.0, -1.0);
        float w_top = pow(max(-m.y, 0.0), edge_power);
        float w_bottom = pow(max(m.y, 0.0), edge_power);
        float w_side = pow(abs(m.x), edge_power);
        float w_shared = max(1.0 - w_top - w_bottom - w_side, 0.0);
        float s_in = t / scale_px - 0.5;
        float in_top = profile_at(edges[0], s_in);
        float in_side = profile_at(edges[1], s_in);
        float in_bottom = profile_at(edges[2], s_in);
        float offset_in = w_top * in_top + w_side * in_side + w_bottom * in_bottom
                        + w_shared * shared_value(in_top, in_side, in_bottom);
        offset_in *= offset_in > 0.0 ? rim : line;
        if (offset_in > 0.0) {
            color += offset_in * min(vec3(1.0), (vec3(1.0) - color) / max(1.0 - fill_gray, 0.05));
        } else {
            color *= 1.0 + offset_in / max(fill_gray, 0.05);
        }
        float u_out = sd / scale_px - 0.5;
        float out_top = outline_at(edges[0], u_out);
        float out_side = outline_at(edges[1], u_out);
        float out_bottom = outline_at(edges[2], u_out);
        outline = w_top * out_top + w_side * out_side + w_bottom * out_bottom
                + w_shared * min(out_top, min(out_side, out_bottom));
        outline = clamp(outline * line, 0.0, 1.0);
    }

    // The inside rim: the part of the pixel within `inside_rim_width` of the edge.
    float in_band = clamp(inside_rim_width + 0.5 - t, 0.0, 1.0);
    color = mix(color, inside_rim.rgb, inside_rim.a * in_band);
    // The rim light: the outside of the shape moved down, blurred (the logistic approximation
    // of a Gaussian edge), seen from inside.
    if (rim_light.a > 0.0) {
        float lit_distance = union_distance(p, 0.0, vec2(0.0, rim_light_shape.x));
        float lit = 1.0 / (1.0 + exp(-1.702 * lit_distance / max(rim_light_shape.y, 0.25)));
        color = mix(color, rim_light.rgb, rim_light.a * lit);
    }

    color = clamp(color, 0.0, 1.0);
    float coverage = clamp(0.5 - sd, 0.0, 1.0);
    vec4 result = vec4(color, 1.0) * coverage + vec4(0.0, 0.0, 0.0, outline * (1.0 - coverage));
    // The shadow, under the glass and outside it: the falloff of a Gaussian edge (the logistic
    // approximation of its integral) at the distance from the shape moved down by the offset.
    if (shadow.x > 0.0) {
        float ds = union_distance(p, 0.0, vec2(0.0, shadow.z));
        float a = shadow.x / (1.0 + exp(1.702 * ds / max(shadow.y, 0.5)));
        a *= clamp(sd + 0.5, 0.0, 1.0);
        result += vec4(0.0, 0.0, 0.0, a) * (1.0 - result.a);
    }
    result *= alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif
    gl_FragColor = result;
}
