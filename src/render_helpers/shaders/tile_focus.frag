precision highp float;

uniform float niri_alpha;
uniform float niri_scale;
uniform sampler2D tex;
uniform vec2 src_size;
uniform vec2 tex_size;
uniform vec2 src_pivot;
uniform vec2 dest_size;
uniform vec2 dest_center;
uniform vec2 persp_half_size;
uniform vec2 scale_xy;
// 3D perspective tilt in [-1, 1] (x = yaw / horizontal press, y = pitch / vertical press)
uniform vec2 tilt;
// Subtle activation brightness pulse (0..0.5)
uniform float brightness_pulse;

varying vec2 niri_v_coords;

void main() {
    // Exact coordinate relative to the tile's transform pivot.
    vec2 p = (niri_v_coords - 0.5) * dest_size + dest_center;

    // 3D perspective foreshortening for directional swivel / pointer-press tilt.
    vec2 norm_p = p / max(persp_half_size, vec2(1.0));
    float persp = max(1.0 + dot(norm_p, tilt) * 0.22, 0.42);
    vec2 p3d = p / persp;

    // Inverse anisotropic (jelly) scale around src_pivot.
    vec2 inv_scale = vec2(
        1.0 / max(scale_xy.x, 0.001),
        1.0 / max(scale_xy.y, 0.001)
    );
    vec2 q = p3d * inv_scale;
    vec2 px = src_pivot + q;
    vec2 uv = px / max(tex_size, vec2(1.0));

    // Smooth 1-physical-pixel edge anti-aliasing.
    float min_scale = min(scale_xy.x, scale_xy.y);
    float aa_px = 1.0 / max(niri_scale * min_scale, 0.5);
    vec2 edge_d = min(px, src_size - px);
    float edge_alpha = smoothstep(0.0, aa_px, min(edge_d.x, edge_d.y));
    if (edge_alpha <= 0.001) {
        discard;
    }

    vec4 color = texture2D(tex, clamp(uv, vec2(0.0), vec2(1.0)));

    // Subtle activation brightness lift & directional tilt sheen (premultiplied-safe).
    if (color.a > 0.001) {
        float tilt_mag = length(tilt);
        float sheen = 0.0;
        if (tilt_mag > 0.001) {
            vec2 local_uv = px / max(src_size, vec2(1.0));
            vec2 centered = (local_uv - 0.5) * 2.0;
            float d_beam = dot(centered - tilt * 0.5, normalize(tilt + vec2(0.0001, 0.0002)));
            sheen = exp(-(d_beam * d_beam) * 5.0) * 0.08 * min(tilt_mag, 1.0);
        }
        float boost = clamp(brightness_pulse + sheen, 0.0, 0.55);
        if (boost > 0.0001) {
            color.rgb = min(color.rgb * (1.0 + boost) + vec3(boost * 0.12 * color.a), vec3(color.a));
        }
    }

    gl_FragColor = color * (niri_alpha * edge_alpha);
}
