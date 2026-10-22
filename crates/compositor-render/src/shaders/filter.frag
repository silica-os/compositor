#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
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

// 0: none, 1: invert, 2: grayscale, 3: invert and grayscale.
uniform float mode;
// 1.0 leaves the colours alone; above it they move away from mid grey.
uniform float contrast;

void main() {
    vec4 color = texture2D(tex, v_coords);
    vec3 rgb = color.a > 0.0 ? color.rgb / color.a : color.rgb;
    if (mode >= 2.0) {
        // Rec. 709 luma.
        rgb = vec3(dot(rgb, vec3(0.2126, 0.7152, 0.0722)));
    }
    if (mode == 1.0 || mode == 3.0) {
        rgb = vec3(1.0) - rgb;
    }
    rgb = clamp((rgb - 0.5) * contrast + 0.5, 0.0, 1.0);
    color = vec4(rgb * color.a, color.a) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
