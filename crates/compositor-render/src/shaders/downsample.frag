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

// The source size in pixels, the size drawn and the factor (1–8). The output starts `pad`
// output pixels before the source on every side: the border the blur reads past the edge.
uniform vec2 src_size;
uniform vec2 out_size;
uniform float factor;
uniform float pad;
// 1: past the source's edge the pixels are mirrored (the spaces bar); 0: the edge repeats.
uniform float mirror;

// One source coordinate (pixels, the centre of a pixel) kept inside the source.
float inside(float p, float size) {
    if (mirror > 0.5) {
        // Reflected at the edge: −0.5 → 0.5, size + 0.5 → size − 0.5.
        p = abs(p);
        p = size - abs(size - p);
    }
    return clamp(p, 0.5, size - 0.5);
}

// Each output pixel is the plain mean of the factor × factor source pixels it covers: a box
// filter, so that downsampling does not alias (the blur's prefilter). Past the source's edge
// the source is extended as the full-resolution blur extends it (the edge pixel repeated, or
// mirrored), so a border cell holds exactly what the full-resolution blur reads there.
void main() {
    vec2 cell = floor(v_coords * out_size) - vec2(pad);
    vec2 origin = cell * factor;
    vec4 sum = vec4(0.0);
    for (int j = 0; j < 8; j++) {
        if (float(j) >= factor) {
            break;
        }
        float y = inside(origin.y + float(j) + 0.5, src_size.y);
        for (int i = 0; i < 8; i++) {
            if (float(i) >= factor) {
                break;
            }
            float x = inside(origin.x + float(i) + 0.5, src_size.x);
            sum += texture2D(tex, vec2(x, y) / src_size);
        }
    }
    vec4 color = sum / (factor * factor) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
