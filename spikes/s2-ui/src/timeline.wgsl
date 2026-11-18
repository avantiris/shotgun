// Instanced rounded-rect timeline renderer. One instance per visible clip.
struct Globals {
    size_px: vec2<f32>, // size of the timeline viewport in physical pixels
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> g: Globals;

struct Inst {
    @location(0) rect: vec4<f32>,   // x, y, w, h in px, relative to viewport
    @location(1) color: vec4<f32>,  // rgba8 unorm, gamma space, straight alpha
    @location(2) params: vec4<f32>, // corner radius px, border darkening 0..1, unused, unused
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,      // px from rect centre
    @location(1) half: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Inst) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let p = inst.rect.xy + corner * inst.rect.zw;
    var out: VsOut;
    out.pos = vec4<f32>(p.x / g.size_px.x * 2.0 - 1.0, 1.0 - p.y / g.size_px.y * 2.0, 0.0, 1.0);
    out.half = inst.rect.zw * 0.5;
    out.local = (corner - vec2<f32>(0.5)) * inst.rect.zw;
    out.color = inst.color;
    out.params = inst.params;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = min(in.params.x, min(in.half.x, in.half.y));
    let q = abs(in.local) - in.half + vec2<f32>(r);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r; // <0 inside
    let cov = clamp(0.5 - d, 0.0, 1.0);
    // 1px darker border + slight vertical gradient
    let border = clamp(d + 1.5, 0.0, 1.0) * in.params.y;
    let grad = 1.0 - 0.12 * (in.local.y / max(in.half.y, 1.0));
    let rgb = in.color.rgb * grad * (1.0 - 0.45 * border);
    let a = in.color.a * cov;
    return vec4<f32>(rgb * a, a); // premultiplied, same blend as egui
}
