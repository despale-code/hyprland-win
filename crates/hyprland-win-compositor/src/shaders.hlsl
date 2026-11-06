// ============================================================================
// Hyprland-Win Direct3D 11 HLSL Shader Pipeline
// Implements:
// 1. Window Surface Pass with Signed-Distance-Field (SDF) Rounded Corners
// 2. Rotated Dual-Stop Gradient Border (e.g. col.active_border = rgba(33ccffee) rgba(00ff99ee) 45deg)
// 3. DirectComposition Premultiplied-Alpha Border Overlay Pass
// 4. Dual-Kawase Downsample & Upsample Blur Passes
// ============================================================================

cbuffer WindowUniforms : register(b0) {
    float4 rect_px;          // (x, y, width, height) in screen pixels
    float4 screen_info;      // (screen_w, screen_h, rounding_px, border_size_px)
    float4 border_color_a;   // Primary border RGBA
    float4 border_color_b;   // Secondary border RGBA (gradient stop 2)
    float4 extra_params;     // (border_angle_rad, opacity, is_active, _pad)
};

Texture2D<float4> WindowTex : register(t0);
SamplerState LinearClamp : register(s0);

struct VSOut {
    float4 pos : SV_Position;
    float2 uv  : TEXCOORD0;
    float2 local_px : TEXCOORD1;
};

VSOut vs_main(uint vid : SV_VertexID) {
    // Full-quad triangle strip from 4 vertex IDs (0..3)
    float2 uv = float2((vid << 1) & 2, vid & 2);
    uv.x = saturate(uv.x);
    uv.y = saturate(uv.y);

    // Expand the quad bounds outward by pad so the outer border ring and
    // subtle active glow around rect_px are never clipped by the quad edges
    float pad = screen_info.w + 6.0;
    float2 expanded_pos = rect_px.xy - pad;
    float2 expanded_size = rect_px.zw + pad * 2.0;

    // local_px is (0,0) at window top-left and rect_px.zw at window bottom-right
    float2 local_px = uv * expanded_size - pad;
    float2 screen_px = rect_px.xy + local_px;

    // Convert pixel coordinates to NDC [-1, 1]
    float2 ndc;
    ndc.x = (screen_px.x / screen_info.x) * 2.0 - 1.0;
    ndc.y = 1.0 - (screen_px.y / screen_info.y) * 2.0;

    VSOut o;
    o.pos = float4(ndc, 0.0, 1.0);
    o.uv = uv;
    o.local_px = local_px;
    return o;
}

// Signed Distance Function for a 2D box with rounded corners
float sd_rounded_box(float2 p, float2 b, float r) {
    float2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

float4 ps_main(VSOut input) : SV_Target {
    float2 half_size = rect_px.zw * 0.5;
    float2 center_rel = input.local_px - half_size;

    float radius = min(screen_info.z, min(half_size.x, half_size.y));
    float border_w = screen_info.w;

    // Outer SDF (window + border outer edge)
    float dist_outer = sd_rounded_box(center_rel, half_size, radius);
    // Inner SDF (content area inside border)
    float dist_inner = sd_rounded_box(center_rel, half_size - border_w, max(radius - border_w, 0.0));

    // Smooth 1px anti-aliased alpha coverage
    float outer_alpha = 1.0 - smoothstep(-0.5, 0.5, dist_outer);
    if (outer_alpha <= 0.001) {
        discard;
    }

    float inner_alpha = 1.0 - smoothstep(-0.5, 0.5, dist_inner);

    // Sample the zero-copy DWM shared window texture
    float4 tex_color = WindowTex.Sample(LinearClamp, input.uv);
    tex_color.a *= extra_params.y; // Apply active/inactive window opacity

    // Compute rotated gradient factor along border_angle_rad
    float angle = extra_params.x;
    float2 dir = float2(cos(angle), sin(angle));
    float2 norm_pos = center_rel / max(half_size, float2(1.0, 1.0));
    float grad_t = saturate(dot(norm_pos, dir) * 0.5 + 0.5);
    float4 border_col = lerp(border_color_a, border_color_b, grad_t);

    // Blend inner window texture with anti-aliased gradient border ring
    float4 final_rgb = lerp(border_col, tex_color, inner_alpha);
    final_rgb.a *= outer_alpha;
    return final_rgb;
}

// Dedicated pixel shader entry point for DirectComposition click-through border overlay swapchain
// Output: Premultiplied-alpha (DXGI_ALPHA_MODE_PREMULTIPLIED)
float4 ps_border_overlay(VSOut input) : SV_Target {
    float2 half_size = rect_px.zw * 0.5;
    float2 center_rel = input.local_px - half_size;
    float border_w = max(screen_info.w, 1.5);

    // Compute rotated dual-stop gradient color along angle = extra_params.x
    float angle = extra_params.x;
    float2 dir = float2(cos(angle), sin(angle));
    float2 norm_pos = center_rel / max(half_size, float2(1.0, 1.0));
    float grad_t = saturate(dot(norm_pos, dir) * 0.5 + 0.5);
    float4 border_col = lerp(border_color_a, border_color_b, grad_t);

    // Subtle top-edge specular sheen
    float top_sheen = saturate(-center_rel.y / max(half_size.y, 1.0)) * 0.10;
    border_col.rgb = saturate(border_col.rgb + top_sheen);

    if (extra_params.w > 0.5) {
        // Tiled window: exact 22px corner radius matching the inward radius of monitor edge borders!
        float inner_radius = clamp(screen_info.z, 8.0, 32.0);
        float dist = sd_rounded_box(center_rel, half_size, inner_radius);

        float inner_edge = -border_w + 0.5;
        float outer_edge = 1.5;
        float inner_cov = 1.0 - smoothstep(inner_edge - 0.6, inner_edge + 0.6, dist);
        float outer_cov = 1.0 - smoothstep(outer_edge - 0.6, outer_edge + 0.6, dist);
        float ring_cov = saturate(outer_cov - inner_cov);

        // Mask the corner crescent between the 22px rounded border and DWM's 8px native corner
        // (including uDWM's 1px outer frame border out to +2.5px) using the live wallpaper texture!
        float dist_dwm8 = sd_rounded_box(center_rel, half_size, 6.0);
        float dwm8_cov = 1.0 - smoothstep(1.8, 3.0, dist_dwm8);
        float mask_alpha = (1.0 - outer_cov) * dwm8_cov;

        float2 screen_px = rect_px.xy + input.local_px;
        float2 screen_uv = saturate(screen_px / max(screen_info.xy, float2(1.0, 1.0)));
        float3 wall_rgb = WindowTex.Sample(LinearClamp, screen_uv).rgb;

        float border_op = extra_params.y;
        float3 ring_rgb = lerp(wall_rgb, border_col.rgb, border_op);
        float ring_eff_a = ring_cov * max(border_op, dwm8_cov);

        float total_a = saturate(ring_eff_a + mask_alpha);
        if (total_a <= 0.002) {
            discard;
        }
        float3 premul_rgb = ring_rgb * ring_eff_a + wall_rgb * mask_alpha;
        return float4(premul_rgb, total_a);
    } else {
        // Floating window (above overlay): match native DWM 8px corner radius
        float inner_radius = 8.0;
        float dist = sd_rounded_box(center_rel, half_size, inner_radius);

        float inner_edge = -3.0;
        float outer_edge = border_w;
        float ring_center = (inner_edge + outer_edge) * 0.5;
        float ring_half_w = (outer_edge - inner_edge) * 0.5;
        float ring_dist = abs(dist - ring_center) - ring_half_w;
        float ring_alpha = 1.0 - smoothstep(-0.6, 0.6, ring_dist);

        float alpha = ring_alpha * extra_params.y;
        if (alpha <= 0.002) {
            discard;
        }
        return float4(border_col.rgb * alpha, alpha);
    }
}
