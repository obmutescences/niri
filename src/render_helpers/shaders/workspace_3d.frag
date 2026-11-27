precision highp float;

uniform float niri_alpha;
uniform float niri_scale;
uniform sampler2D tex;
uniform vec2 view_size;
uniform vec2 view_origin;
uniform vec2 tex_size;
uniform vec2 dest_size;
// Workspace center in screen-center coordinates (logical px; +y down).
uniform float center_y;
// Workspace center depth offset in logical px (+z recedes away from camera).
uniform float z_offset;
// 3D rotation around horizontal X axis in radians.
// Negative tilts top edge away (+Z); positive tilts bottom edge away (+Z).
uniform float rotate_x;
// Virtual camera focal length in logical px: (H / 2) / tan(fov / 2).
uniform float focal;
// Additional 2D scale applied to the workspace plane.
uniform vec2 ws_scale;
// Dynamic corner radius in logical px (0 at rest).
uniform float corner_radius;
// Depth/tilt darkening amount in [0, 1].
uniform float dim_amount;
// Subtle diagonal specular glass sheen intensity in [0, 1].
uniform float sheen_strength;

varying vec2 niri_v_coords;

float sd_rounded_box(vec2 p, vec2 b, float r) {
    float cr = min(r, min(b.x, b.y));
    vec2 q = abs(p) - b + vec2(cr);
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - cr;
}

void main() {
    // Screen coordinates centered at output center (+x right, +y down).
    vec2 s = (niri_v_coords - 0.5) * dest_size;

    float c = cos(rotate_x);
    float sn = sin(rotate_x);

    // Exact ray-plane intersection from camera O = (0, 0, -focal) along ray D = (s.x, s.y, focal)
    // with plane passing through P_c = (0, center_y, z_offset) and normal N = (0, -sn, c).
    float denom = focal * c - s.y * sn;
    if (denom <= 0.001) {
        discard;
    }

    float numer = (focal + z_offset) * c - center_y * sn;
    float t = numer / denom;
    if (t <= 0.001) {
        discard;
    }

    // 3D intersection point in camera space.
    float x3 = t * s.x;
    float y3 = t * s.y;
    float z3 = (t - 1.0) * focal;

    // Project onto workspace plane local coordinates centered at P_c.
    float lx = x3;
    float ly = (y3 - center_y) * c + (z3 - z_offset) * sn;

    vec2 half_plane = (view_size * 0.5) * ws_scale;
    float min_ws_scale = max(min(ws_scale.x, ws_scale.y), 0.01);
    // Perspective-adaptive 1 physical pixel anti-aliasing width.
    float aa_px = max(t / max(niri_scale * min_ws_scale, 0.5), 0.5 / max(niri_scale, 1.0));

    float dist = sd_rounded_box(vec2(lx, ly), half_plane, corner_radius * min_ws_scale);
    float mask = 1.0 - smoothstep(-aa_px * 0.5, aa_px * 0.5, dist);
    if (mask <= 0.001) {
        discard;
    }

    // Map local plane coordinates back into exact viewport-locked offscreen texture UV.
    vec2 local_px = vec2(lx / max(ws_scale.x, 0.001), ly / max(ws_scale.y, 0.001)) + view_size * 0.5;
    vec2 uv = (local_px + view_origin) / max(tex_size, vec2(1.0));
    vec4 color = texture2D(tex, clamp(uv, vec2(0.0), vec2(1.0)));

    // Depth & tilt darkening (Lambertian cosine shading + explicit dim).
    float shade = clamp(1.0 - dim_amount, 0.05, 1.0);
    color.rgb *= shade;

    // Subtle moving diagonal glass reflection sweep tied to 3D tilt angle.
    if (sheen_strength > 0.001 && color.a > 0.001) {
        vec2 local_uv = local_px / max(view_size, vec2(1.0));
        vec2 centered = (local_uv - 0.5) * 2.0;
        float sweep_bias = clamp(rotate_x * 1.4, -1.2, 1.2);
        float d_beam = dot(centered - vec2(0.0, sweep_bias * 0.55), vec2(0.45, 0.89));
        float beam = exp(-(d_beam * d_beam) * 5.5);
        color.rgb = min(
            color.rgb + vec3(0.92, 0.96, 1.0) * (beam * sheen_strength * 0.24 * color.a),
            vec3(color.a)
        );
    }

    gl_FragColor = color * (niri_alpha * mask);
}
