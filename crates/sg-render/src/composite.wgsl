// Layer pass: one textured quad per picture, blended in linear light.
struct Layer {
    // 2x2 matrix (rows) from quad corners to clip space.
    matrix: vec4<f32>,
    // xy: centre in clip space, z: opacity.
    offset: vec4<f32>,
    // 2x2 matrix (rows) from quad corners to texture coordinate offsets.
    uv: vec4<f32>,
    uv_centre: vec4<f32>,
    // Movement of the content inside the quad while the shutter is open
    // (transitions): xy shift in quad units and z size factor, at the
    // moment it opens and at the moment it closes.
    motion0: vec4<f32>,
    motion1: vec4<f32>,
    // x: how many times its size the quad is drawn (the content repeats
    // mirrored), y: samples taken along the movement.
    extra: vec4<f32>,
    // Match stage: rgb gain and contrast exponent; then pivot, saturation,
    // number of look modules and a number that changes every frame.
    grade0: vec4<f32>,
    grade1: vec4<f32>,
    // Look chain: per module (kind, mix, -, -) and its four knobs.
    modules: array<vec4<f32>, 16>,
};
@group(0) @binding(0) var<uniform> layer: Layer;
@group(0) @binding(1) var picture: texture_2d<f32>;
@group(0) @binding(2) var picture_sampler: sampler;

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct LayerV {
    @builtin(position) pos: vec4<f32>,
    // Position in quad units: -1..1 across the picture, beyond it for its mirror images.
    @location(0) q: vec2<f32>,
};

@vertex
fn layer_vs(@builtin(vertex_index) i: u32) -> LayerV {
    // Corners of a strip: (-1,-1) (1,-1) (-1,1) (1,1), times the repeat count.
    let corner = vec2<f32>(f32(i & 1u) * 2.0 - 1.0, f32((i >> 1u) & 1u) * 2.0 - 1.0) * max(layer.extra.x, 1.0);
    var out: LayerV;
    let p = vec2<f32>(dot(layer.matrix.xy, corner), dot(layer.matrix.zw, corner));
    out.pos = vec4<f32>(layer.offset.xy + p, 0.0, 1.0);
    out.q = corner;
    return out;
}

// Pictures arrive display-referred (BT.709 video shown as on an sRGB
// screen). Decode with the sRGB curve to get linear light.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn to_display(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// A colour of the given hue (degrees) that does not change brightness
// when multiplied in.
fn hue_tint(degrees: f32) -> vec3<f32> {
    let c = 0.5 + 0.5 * cos(radians(degrees) - vec3<f32>(0.0, 2.0943951, 4.1887902));
    return c / max(luma(c), 0.001);
}

// Match, then the look chain, on one linear-light colour. The sums of the
// match are those of `sg_core::colour::Correction::apply`.
fn grade(colour: vec3<f32>, q: vec2<f32>, pixel: vec2<f32>) -> vec3<f32> {
    var c = colour * layer.grade0.rgb;
    let pivot = max(layer.grade1.x, 0.001);
    let lit = max(luma(c), 1e-6);
    let scaled = pivot * pow(lit / pivot, layer.grade0.w) / lit;
    let level = lit * scaled;
    c = max(vec3<f32>(level) + (c * scaled - vec3<f32>(level)) * layer.grade1.y, vec3<f32>(0.0));
    let count = min(u32(layer.grade1.z), 8u);
    for (var i = 0u; i < count; i++) {
        let head = layer.modules[i * 2u];
        let k = layer.modules[i * 2u + 1u];
        var o = c;
        let l = max(luma(c), 1e-6);
        switch u32(head.x) {
            // Balance: temperature, tint, exposure in stops.
            case 1u: {
                o = c * exp2(k.z) * vec3<f32>(exp(0.35 * k.x), exp(-0.25 * k.y), exp(-0.35 * k.x));
            }
            // Tone: contrast, highlight roll-off, shadows, pivot.
            case 2u: {
                let p = max(k.w, 0.001);
                var t = p * pow(l / p, exp(k.x * 0.7));
                let dark = 1.0 - clamp(t / (2.0 * p), 0.0, 1.0);
                t = max(t + k.z * 0.08 * dark * dark, 0.0);
                // 0 stays 0 and 1 stays 1; what lies between is lifted, what lies above is held back.
                t = t * (1.0 + k.y) / (1.0 + k.y * t);
                o = c * (t / l);
            }
            // Colour: saturation, vibrance (more for the dull colours).
            case 3u: {
                let high = max(c.r, max(c.g, c.b));
                let low = min(c.r, min(c.g, c.b));
                let vivid = (high - low) / max(high, 1e-6);
                let factor = k.x * (1.0 + k.y * (1.0 - vivid));
                o = max(vec3<f32>(l) + (c - vec3<f32>(l)) * factor, vec3<f32>(0.0));
            }
            // Split tone: shadow hue, highlight hue, amount, balance.
            case 4u: {
                let bright = sqrt(clamp(l, 0.0, 1.0));
                let edge = 0.5 + 0.4 * k.w;
                let tint = mix(hue_tint(k.x), hue_tint(k.y), smoothstep(edge - 0.3, edge + 0.3, bright));
                o = mix(c, c * tint, k.z);
            }
            // Vignette: amount, size, softness (on the picture, not the frame).
            case 5u: {
                let far = length(clamp(q, vec2<f32>(-1.0), vec2<f32>(1.0))) / 1.4142135;
                o = c * max(1.0 - k.x * smoothstep(k.y * 0.7, k.y * 0.7 + k.z, far), 0.0);
            }
            // Grain: amount, size in pixels; new every frame.
            case 6u: {
                let cell = floor(pixel / max(k.y, 1.0)) + vec2<f32>(layer.grade1.w * 37.0, layer.grade1.w * 11.0);
                let noise = fract(sin(dot(cell, vec2<f32>(12.9898, 78.233))) * 43758.5453) - 0.5;
                o = max(c + vec3<f32>(noise * k.x * 0.3 * sqrt(l)), vec3<f32>(0.0));
            }
            default: {}
        }
        c = mix(c, o, clamp(head.y, 0.0, 1.0));
    }
    return c;
}

@fragment
fn layer_fs(v: LayerV) -> @location(0) vec4<f32> {
    let taps = max(u32(layer.extra.y), 1u);
    var sum = vec4<f32>(0.0);
    // Each pixel takes its samples at slightly different instants: the steps
    // between samples then show as fine grain instead of as repeated edges.
    let jitter = select(0.5, fract(52.9829189 * fract(dot(v.pos.xy, vec2<f32>(0.06711056, 0.00583715)))), taps > 1u);
    for (var i = 0u; i < taps; i++) {
        // The content as it lay at this instant of the open shutter.
        let m = mix(layer.motion0, layer.motion1, (f32(i) + jitter) / f32(taps));
        var q = (v.q + m.xy) / max(m.z, 0.01);
        // Beyond its edge the picture continues mirrored: no black comes into view.
        let w = q - 1.0;
        q = abs(w - 4.0 * floor(w / 4.0) - 2.0) - 1.0;
        // Texture space has y down; quad space has y up.
        let t = vec2<f32>(q.x, -q.y);
        let uv = layer.uv_centre.xy + vec2<f32>(dot(layer.uv.xy, t), dot(layer.uv.zw, t));
        let texel = textureSampleLevel(picture, picture_sampler, uv, 0.0);
        // Averaged in linear light and premultiplied, as light adds up on a sensor.
        sum += vec4<f32>(to_linear(texel.rgb) * texel.a, texel.a);
    }
    let mean = sum / f32(taps);
    // Colour is worked on unmultiplied, then weighted by coverage again.
    let graded = grade(mean.rgb / max(mean.a, 1e-5), v.q, v.pos.xy);
    return vec4<f32>(graded * mean.a, mean.a) * layer.offset.z;
}

// Output pass: the linear composite, encoded for the display.
@vertex
fn output_vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: V;
    out.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn output_fs(v: V) -> @location(0) vec4<f32> {
    let c = textureSample(picture, picture_sampler, v.uv);
    return vec4<f32>(to_display(c.rgb), 1.0);
}

// Delivery passes for export: the frame as limited-range BT.709 Y'CbCr,
// written as the two planes of NV12 so the encoder gets what it wants
// without a conversion on the CPU.
fn display_rgb(uv: vec2<f32>) -> vec3<f32> {
    return to_display(textureSample(picture, picture_sampler, uv).rgb);
}

@fragment
fn luma_fs(v: V) -> @location(0) vec4<f32> {
    let c = display_rgb(v.uv);
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    return vec4<f32>((16.0 + 219.0 * y) / 255.0, 0.0, 0.0, 1.0);
}

@fragment
fn chroma_fs(v: V) -> @location(0) vec4<f32> {
    // Rendered at half size with a linear sampler: each texel is the mean
    // of the four pixels it covers.
    let c = display_rgb(v.uv);
    let y = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let cb = (c.b - y) / 1.8556;
    let cr = (c.r - y) / 1.5748;
    return vec4<f32>((128.0 + 224.0 * cb) / 255.0, (128.0 + 224.0 * cr) / 255.0, 0.0, 1.0);
}
