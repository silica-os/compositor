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

// From gl_FragCoord to the position in the window, in physical pixels (x row and y row of an
// affine transform).
uniform vec3 frag_to_local_x;
uniform vec3 frag_to_local_y;
// Window size, radius and exponent of the corner superellipse.
uniform vec2 geo_size;
uniform float radius;
uniform float exponent;

// Corner coverage: |x/r|^n + |y/r|^n <= 1 in the r × r box of every corner.
// Outside the window (popups, client shadows) and far from the corners it does not cut.
float corner_coverage(vec2 p) {
    if (p.x < 0.0 || p.y < 0.0 || p.x > geo_size.x || p.y > geo_size.y) {
        return 1.0;
    }
    vec2 from_edge = min(p, geo_size - p);
    if (from_edge.x >= radius || from_edge.y >= radius) {
        return 1.0;
    }
    vec2 q = (radius - from_edge) / radius;
    float f = pow(pow(q.x, exponent) + pow(q.y, exponent), 1.0 / exponent);
    return clamp((1.0 - f) * radius + 0.5, 0.0, 1.0);
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    vec3 frag = vec3(gl_FragCoord.xy, 1.0);
    color *= corner_coverage(vec2(dot(frag_to_local_x, frag), dot(frag_to_local_y, frag)));

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
