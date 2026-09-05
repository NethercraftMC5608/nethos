#pragma once
// The compositor supplies the scene underneath this client, not a wallpaper
// copy. Only the background coordinates bend: client text is sampled once.
static const char *nethos_glass_fragment_shader = R"GLSL(
#version 100
@builtin_ext@
precision highp float;
@builtin@
uniform sampler2D bg_texture;
uniform mat4 background_uv_matrix;
uniform vec2 view_size;
uniform vec2 background_texel;
uniform float sat;
uniform float refraction;
uniform float rim_strength;
varying mediump vec2 uvpos[2];

float coverage(vec2 uv) {
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) return 0.0;
    // Text and icons are not material boundaries. Only a transition into
    // nearly transparent pixels counts as the outside of a glass shape.
    return smoothstep(0.02, 0.18, get_pixel(uv).a);
}
void main() {
    vec4 client = get_pixel(uvpos[0]);
    if (client.a < 0.002 || client.a > 0.998) {
        gl_FragColor = client;
        return;
    }
    vec2 step_uv = vec2(6.0) / view_size;
    vec2 normal = vec2(
        coverage(uvpos[0] + vec2(step_uv.x, 0.0)) - coverage(uvpos[0] - vec2(step_uv.x, 0.0)),
        coverage(uvpos[0] + vec2(0.0, step_uv.y)) - coverage(uvpos[0] - vec2(0.0, step_uv.y)));
    float edge = min(length(normal), 1.0);
    normal /= max(length(normal), 0.001);
    vec2 delta = normal * refraction * edge / view_size;
    // Transform the displacement with the same matrix as the underlying
    // framebuffer: rotated/scaled outputs must bend in the same direction.
    vec2 bend = (background_uv_matrix * vec4(delta, 0.0, 0.0)).xy;
    vec2 uv = clamp(uvpos[1] + bend, background_texel * 0.5,
                    vec2(1.0) - background_texel * 0.5);
    vec4 backdrop = texture2D(bg_texture, uv);
    float luminance = dot(backdrop.rgb, vec3(0.2126, 0.7152, 0.0722));
    backdrop.rgb = mix(vec3(luminance), backdrop.rgb, sat);
    // A quiet rim lit from above; no time uniform, noise or idle animation.
    float light = max(-normal.y, 0.0) * edge * rim_strength;
    backdrop.rgb = mix(backdrop.rgb, vec3(backdrop.a), light);
    gl_FragColor = client + (1.0 - client.a) * clamp(4.0 * client.a, 0.0, 1.0) * backdrop;
}
)GLSL";
