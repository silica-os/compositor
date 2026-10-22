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

// The gradient below the menu bar (glass.md, "Menu bar · gradient"): the luminance of the
// wallpaper goes through a table that compresses the highlights at the top and fades
// downwards; the shift is the same for the three channels (cyan drops by 68 in every
// channel, dark magenta does not change).
// Table: 7 rows (y 0, 30, 45, 60, 80, 100, 120 pt) by 11 levels (96, 112, …, 240,
// 255), in 5 mat4 read in order.
uniform mat4 tone[5];
// Texture height in pixels and pixels per point.
uniform float tex_height;
uniform float scale_px;

float table(int i) {
    int m = i / 16;
    int c = (i - m * 16) / 4;
    int k = i - m * 16 - c * 4;
    vec4 column = tone[m][c];
    return k == 0 ? column.x : (k == 1 ? column.y : (k == 2 ? column.z : column.w));
}

float row_y(int r) {
    return r == 0 ? 0.0 : (r == 1 ? 30.0 : (r == 2 ? 45.0 : (r == 3 ? 60.0 : (r == 4 ? 80.0 : (r == 5 ? 100.0 : 120.0)))));
}

float level(int c) {
    return c == 10 ? 255.0 : 96.0 + 16.0 * float(c);
}

// The luminance after the table at row y (pt), for a luminance 0–255.
float mapped(float l, float y) {
    if (l <= 96.0 || y >= 120.0) {
        return l;
    }
    int c0 = 0;
    for (int c = 0; c < 10; c++) {
        if (l >= level(c)) {
            c0 = c;
        }
    }
    float fc = clamp((l - level(c0)) / (level(c0 + 1) - level(c0)), 0.0, 1.0);
    int r0 = 0;
    for (int r = 0; r < 6; r++) {
        if (y >= row_y(r)) {
            r0 = r;
        }
    }
    float fr = clamp((y - row_y(r0)) / (row_y(r0 + 1) - row_y(r0)), 0.0, 1.0);
    float a = mix(table(r0 * 11 + c0), table(r0 * 11 + c0 + 1), fc);
    float b = mix(table((r0 + 1) * 11 + c0), table((r0 + 1) * 11 + c0 + 1), fc);
    return mix(a, b, fr);
}

void main() {
    vec3 color = texture2D(tex, v_coords).rgb;
    float y = v_coords.y * tex_height / scale_px;
    float l = dot(color, vec3(0.2126, 0.7152, 0.0722)) * 255.0;
    float shift = (mapped(l, y) - l) / 255.0;
    vec4 result = vec4(clamp(color + vec3(shift), 0.0, 1.0), 1.0) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif
    gl_FragColor = result;
}
