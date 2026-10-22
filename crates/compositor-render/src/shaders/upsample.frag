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

// The size of the reduced texture, in pixels.
uniform vec2 tex_size;

// A reduced blur drawn back at full size: bilinear, but kept half a pixel inside the texture.
// The offscreen textures repeat by default, and the outermost pixels would mix in the
// opposite edge; held inside, they take the edge pixel, as the clamped (and the mirrored)
// blur at full resolution does.
void main() {
    vec2 low = 0.5 / tex_size;
    vec4 color = texture2D(tex, clamp(v_coords, low, vec2(1.0) - low)) * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
