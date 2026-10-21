// Jwala UI shaders.
//
// `vs_quad`/`fs_quad` draw every 2D primitive as an instanced quad; `kind` selects the shape.
// `fs_blur` and `fs_shadow` are full-screen passes for the soft shadows.

struct Globals {
    // logical viewport size (the quad coordinates are logical pixels)
    size: vec2<f32>,
    // physical pixels per logical pixel
    scale: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var img: texture_2d<f32>;
@group(1) @binding(1) var clamp_smp: sampler;
@group(1) @binding(2) var repeat_smp: sampler;
@group(2) @binding(0) var glyphs: texture_2d<f32>;

struct Inst {
    @location(0) rect: vec4<f32>,   // x, y, w, h
    @location(1) uv: vec4<f32>,     // u0, v0, u1, v1
    @location(2) color: vec4<f32>,  // straight alpha
    @location(3) color2: vec4<f32>,
    @location(4) p: vec4<f32>,      // kind, radius, stroke width (0 = fill), extra (skew / focus)
    @location(5) q: vec4<f32>,      // kind-specific (line endpoints, gradient span)
    @location(6) clip: vec4<f32>,   // x0, y0, x1, y1
};

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,  // logical px relative to the rect origin
    @location(1) uv: vec2<f32>,
    @location(2) world: vec2<f32>,
    @location(3) @interpolate(flat) rect: vec4<f32>,
    @location(4) @interpolate(flat) color: vec4<f32>,
    @location(5) @interpolate(flat) color2: vec4<f32>,
    @location(6) @interpolate(flat) p: vec4<f32>,
    @location(7) @interpolate(flat) q: vec4<f32>,
    @location(8) @interpolate(flat) clip: vec4<f32>,
};

// Each pipeline is specialised for one primitive kind (a pipeline-override constant), so the shader
// compiler drops every other branch. GPUs barely notice; software rendering (WARP) otherwise pays
// for all branches on every pixel.
override KIND: f32 = 0.0;

const K_RRECT: f32 = 0.0;
const K_IMAGE: f32 = 1.0;
const K_GLYPH: f32 = 2.0;
const K_ELLIPSE: f32 = 3.0;
const K_GLOW: f32 = 4.0;
const K_GRAD: f32 = 5.0;
const K_LINE: f32 = 6.0;
const K_TILE: f32 = 7.0;

@vertex
fn vs_quad(@builtin(vertex_index) vi: u32, i: Inst) -> VOut {
    var corners = array<vec2<f32>, 6>(vec2(0., 0.), vec2(1., 0.), vec2(0., 1.), vec2(1., 0.), vec2(1., 1.), vec2(0., 1.));
    let c = corners[vi];
    // shapes with analytic edges get a pixel of margin for antialiasing
    var pad = 1.5;
    if (KIND == K_GLYPH || KIND == K_IMAGE || KIND == K_TILE) && i.p.y == 0.0 { pad = 0.0; }
    let local = mix(vec2(-pad), i.rect.zw + vec2(pad), c);
    var world = i.rect.xy + local;
    // horizontal skew for italic text: shift by -skew * (y - bottom)
    world.x -= i.p.w * (local.y - i.rect.w) * select(0.0, 1.0, KIND == K_GLYPH);
    var o: VOut;
    o.pos = vec4(world.x / g.size.x * 2.0 - 1.0, 1.0 - world.y / g.size.y * 2.0, 0.0, 1.0);
    o.local = local;
    let t = local / max(i.rect.zw, vec2(1e-4));
    o.uv = mix(i.uv.xy, i.uv.zw, t);
    o.world = world;
    o.rect = i.rect;
    o.color = i.color;
    o.color2 = i.color2;
    o.p = i.p;
    o.q = i.q;
    o.clip = i.clip;
    return o;
}

fn sd_rrect(p: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half + vec2(r);
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn sd_segment(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    return length(pa - ba * h);
}

// coverage of a signed distance (logical px) with ~1 physical pixel of softness
fn cover(d: f32) -> f32 {
    let aa = 0.75 / g.scale;
    return clamp(0.5 - d / (2.0 * aa), 0.0, 1.0);
}

// rounded-rect mask for images and tiles; radius 0 = no mask
fn rect_mask(v: VOut) -> f32 {
    if v.p.y <= 0.0 { return 1.0; }
    let half = v.rect.zw * 0.5;
    return cover(sd_rrect(v.local - half, half, min(v.p.y, min(half.x, half.y))));
}

@fragment
fn fs_quad(v: VOut) -> @location(0) vec4<f32> {
    // derivatives in uniform control flow, so the branches below can sample with mipmaps
    let dx = dpdx(v.uv);
    let dy = dpdy(v.uv);
    if v.world.x < v.clip.x || v.world.y < v.clip.y || v.world.x > v.clip.z || v.world.y > v.clip.w { discard; }
    let kind = KIND;
    var col = v.color;
    var a = 1.0;
    let half = v.rect.zw * 0.5;
    let pc = v.local - half;

    if kind == K_RRECT || kind == K_GRAD {
        let d = sd_rrect(pc, half, min(v.p.y, min(half.x, half.y)));
        if v.p.z > 0.0 { a = cover(abs(d + v.p.z * 0.5) - v.p.z * 0.5); } else { a = cover(d); }
        if kind == K_GRAD {
            let t = clamp((v.world.y - v.q.x) / max(v.q.y - v.q.x, 1e-4), 0.0, 1.0);
            col = mix(v.color, v.color2, t);
        }
    } else if kind == K_IMAGE {
        let s = textureSampleGrad(img, clamp_smp, v.uv, dx, dy);   // premultiplied
        return s * v.color * rect_mask(v);
    } else if kind == K_TILE {
        let s = textureSampleGrad(img, repeat_smp, v.uv, dx, dy);
        return s * v.color * rect_mask(v);
    } else if kind == K_GLYPH {
        a = textureSampleLevel(glyphs, clamp_smp, v.uv, 0.0).r;
        if v.q.y > v.q.x {
            let t = clamp((v.world.y - v.q.x) / (v.q.y - v.q.x), 0.0, 1.0);
            col = mix(v.color, v.color2, t);
        }
    } else if kind == K_ELLIPSE {
        let r = max(half, vec2(1e-3));
        let k = length(pc / r);
        let d = (k - 1.0) * min(r.x, r.y);
        if v.p.z > 0.0 { a = cover(abs(d + v.p.z * 0.5) - v.p.z * 0.5); } else { a = cover(d); }
    } else if kind == K_GLOW {
        // radial falloff from color (centre) to transparent (edge); p.w = plateau ("focus scale")
        let r = max(half, vec2(1e-3));
        let k = length(pc / r);
        let t = clamp((k - v.p.w) / max(1.0 - v.p.w, 1e-3), 0.0, 1.0);
        col = mix(v.color, vec4(v.color2.rgb, v.color2.a), t);
        a = select(1.0, 0.0, k > 1.0);
    } else if kind == K_LINE {
        let d = sd_segment(v.world, v.q.xy, v.q.zw) - v.p.z * 0.5;
        a = cover(d);
    }
    let alpha = col.a * a;
    return vec4(col.rgb * alpha, alpha);
}

// ------------------------------------------------------------------ shadows

struct FsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> FsOut {
    let x = f32((vi << 1u) & 2u);
    let y = f32(vi & 2u);
    var o: FsOut;
    o.pos = vec4(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2(x, y);
    return o;
}

struct Blur {
    dir: vec2<f32>,     // texel step (1/width, 0) or (0, 1/height)
    sigma: f32,         // in texels
    _pad: f32,
};

@group(0) @binding(0) var<uniform> blur: Blur;
@group(0) @binding(1) var blur_src: texture_2d<f32>;
@group(0) @binding(2) var blur_smp: sampler;

// separable Gaussian on the alpha channel, using linear filtering to take two taps per fetch
@fragment
fn fs_blur(v: FsOut) -> @location(0) vec4<f32> {
    let s = max(blur.sigma, 0.01);
    let radius = i32(ceil(s * 3.0));
    var acc = textureSample(blur_src, blur_smp, v.uv).a;
    var wsum = 1.0;
    for (var i = 1; i <= radius; i += 2) {
        let x0 = f32(i);
        let x1 = f32(i + 1);
        let w0 = exp(-x0 * x0 / (2.0 * s * s));
        let w1 = exp(-x1 * x1 / (2.0 * s * s));
        let w = w0 + w1;
        let o = (x0 * w0 + x1 * w1) / max(w, 1e-8);
        acc += (textureSample(blur_src, blur_smp, v.uv + blur.dir * o).a + textureSample(blur_src, blur_smp, v.uv - blur.dir * o).a) * w;
        wsum += 2.0 * w;
    }
    let r = acc / wsum;
    return vec4(r, r, r, r);
}

struct Shadow {
    // per term: offset in uv (xy), strength (z)
    t0: vec4<f32>,
    t1: vec4<f32>,
    t2: vec4<f32>,
    // 1 if a receiver mask is bound
    use_receiver: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sh: Shadow;
@group(0) @binding(1) var tex0: texture_2d<f32>;
@group(0) @binding(2) var tex1: texture_2d<f32>;
@group(0) @binding(3) var tex2: texture_2d<f32>;
@group(0) @binding(4) var receiver: texture_2d<f32>;
@group(0) @binding(5) var sh_smp: sampler;

// each term darkens by strength * blurred occluder (shifted); terms multiply like stacked lights
@fragment
fn fs_shadow(v: FsOut) -> @location(0) vec4<f32> {
    var light = 1.0;
    light *= 1.0 - sh.t0.z * textureSample(tex0, sh_smp, v.uv - sh.t0.xy).a;
    light *= 1.0 - sh.t1.z * textureSample(tex1, sh_smp, v.uv - sh.t1.xy).a;
    light *= 1.0 - sh.t2.z * textureSample(tex2, sh_smp, v.uv - sh.t2.xy).a;
    var a = 1.0 - light;
    if sh.use_receiver.x > 0.5 { a *= textureSample(receiver, sh_smp, v.uv).a; }
    return vec4(0.0, 0.0, 0.0, a);   // premultiplied black
}
