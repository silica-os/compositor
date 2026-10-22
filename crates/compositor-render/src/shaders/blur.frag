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
    vec4 sum = vec4(0.0);
    float total = 0.0;
    // Whole pixels within 3σ, from −reach to +reach: the loop stops past the kernel, so a
    // small σ costs only its own taps.
    float reach = floor(min(3.0 * sigma, 256.0));
    // Clamp to edge: the offscreen textures repeat by default, and a capture cut by the
    // output's edge would bring in the opposite side of the capture.
    vec2 low = 0.5 * abs(step);
    vec2 high = vec2(1.0) - low;
    for (int i = 0; i <= 512; i++) {
        float x = float(i) - reach;
        if (x > reach) {
            break;
        }
        float weight = exp(-(x * x) / (2.0 * sigma * sigma));
        sum += texture2D(tex, clamp(v_coords + step * x, low, high)) * weight;
        total += weight;
    }
    vec4 color = sum / max(total, 1e-6) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
