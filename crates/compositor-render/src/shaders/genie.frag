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

// From gl_FragCoord to the position in the effect area, in physical pixels.
uniform vec3 frag_to_local_x;
uniform vec3 frag_to_local_y;
// The window frame and the destination thumbnail: x0, y0, x1, y1 in the area.
uniform vec4 window_rect;
uniform vec4 target_rect;
// Funnel bend, descent of the top edge and of the bottom edge (0–1).
uniform vec3 motion;
// Where the frame is in the snapshot: origin and size in texture coordinates.
uniform vec4 frame_uv;

float ease(float x) {
    x = clamp(x, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

void main() {
    vec3 frag = vec3(gl_FragCoord.xy, 1.0);
    vec2 p = vec2(dot(frag_to_local_x, frag), dot(frag_to_local_y, frag));

    // The content lies between the top edge and the bottom edge, which each come down with
    // their own curve towards the thumbnail.
    float top = mix(window_rect.y, target_rect.y, motion.y);
    float bottom = mix(window_rect.w, target_rect.w, motion.z);

    // The funnel: on every screen row the edges go from the window width to the thumbnail
    // width with a smoothstep between the top of the window and the top of the thumbnail,
    // scaled by the bend.
    float span = target_rect.y - window_rect.y;
    float u = abs(span) < 1.0 ? 1.0 : (p.y - window_rect.y) / span;
    float k = motion.x * ease(u);
    float left = mix(window_rect.x, target_rect.x, k);
    float right = mix(window_rect.z, target_rect.z, k);

    float coverage = clamp(min(p.x - left, right - p.x) + 0.5, 0.0, 1.0)
        * clamp(min(p.y - top, bottom - p.y) + 0.5, 0.0, 1.0);
    if (coverage <= 0.0) {
        gl_FragColor = vec4(0.0);
        return;
    }
    vec2 q = clamp(vec2((p.x - left) / max(right - left, 0.001),
                        (p.y - top) / max(bottom - top, 0.001)), 0.0, 1.0);
    vec4 color = texture2D(tex, frame_uv.xy + q * frame_uv.zw);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    color = color * alpha * coverage;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
