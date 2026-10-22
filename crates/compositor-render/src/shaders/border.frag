precision highp float;

uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// In logical points; the element covers the window frame.
uniform float radius;
uniform float exponent;
// Points per physical pixel, for antialiasing.
uniform float pixel;
// The rim colour (straight alpha): `Line/Border` of the Amalfi tokens.
uniform vec4 rim;
// 1: the whole inside is filled (the veil over a window that has a sheet), 0: the rim.
uniform float fill;

// Signed distance (positive outside) from the window's superellipse outline.
float shape_distance(vec2 p) {
    vec2 half_size = size * 0.5;
    vec2 d = abs(p - half_size) - (half_size - vec2(radius));
    if (d.x > 0.0 && d.y > 0.0) {
        vec2 q = d / radius;
        float f = pow(pow(q.x, exponent) + pow(q.y, exponent), 1.0 / exponent);
        return (f - 1.0) * radius;
    }
    return max(d.x, d.y) - radius;
}

float cover(float distance) {
    return clamp(0.5 - distance / pixel, 0.0, 1.0);
}

void main() {
    vec2 p = v_coords * size;
    float d = shape_distance(p);
    // The 1 pt band just inside the outline.
    float band = fill > 0.5 ? cover(d) : cover(d) - cover(d + 1.0);
    vec4 result = vec4(rim.rgb * rim.a, rim.a) * band * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif
    gl_FragColor = result;
}
