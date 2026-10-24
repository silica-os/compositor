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

// One texture pixel step along the axis of the pass (x or y, in 0–1 coordinates).
uniform vec2 step;
// σ of the Gaussian in texture pixels; the kernel reaches 3σ (at most 256 pixels).
uniform float sigma;

void main() {
    // Whole pixels within 3σ, from −reach to +reach: the loop stops past the kernel, so a
    // small σ costs only its own taps.
    float reach = floor(min(3.0 * sigma, 256.0));
    // Clamp to edge: the offscreen textures repeat by default, and a capture cut by the
    // output's edge would bring in the opposite side of the capture. Held at the centre of the
    // edge pixel, a pair of taps across the edge reads that pixel for both, as the clamp
    // would for each.
    vec2 low = 0.5 * abs(step);
    vec2 high = vec2(1.0) - low;
    vec4 sum = texture2D(tex, v_coords);
    float total = 1.0;
    // The weights g(x) = exp(−x² / 2σ²) by recurrence (no exp per tap): g(x + 1) = g(x) · r(x),
    // r(x + 1) = r(x) · q, with r(0) = exp(−1 / 2σ²) and q = r(0)².
    float r = exp(-0.5 / (sigma * sigma));
    float q = r * r;
    float g = r;
    r *= q;
    // Pixels a and a + 1 (a odd) in one bilinear read, between them in proportion to their
    // weights, on both sides: half the reads of one tap per pixel, the same sum.
    for (int i = 0; i <= 128; i++) {
        float a = float(2 * i + 1);
        if (a > reach) {
            break;
        }
        float ga = g;
        g *= r;
        r *= q;
        float gb = a + 1.0 > reach ? 0.0 : g;
        g *= r;
        r *= q;
        float w = ga + gb;
        vec2 offset = step * (a + gb / w);
        sum += (texture2D(tex, clamp(v_coords + offset, low, high))
            + texture2D(tex, clamp(v_coords - offset, low, high))) * w;
        total += 2.0 * w;
    }
    vec4 color = sum / max(total, 1e-6) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
