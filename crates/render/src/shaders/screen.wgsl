// Minelib — écran vidéo dans le monde : quad texturé par une image dynamique, fixé dans
// le monde. group 0 : uniformes du monde. group 1 : la vidéo.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    fog: vec4<f32>,
    sun: vec4<f32>,
    sel_min: vec4<f32>,
    sel_max: vec4<f32>,
    light: vec4<f32>,
    cur_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    screen: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

@group(1) @binding(0) var vid: texture_2d<f32>;
@group(1) @binding(1) var vid_s: sampler;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.world = v.pos;
    return o;
}

fn texel(i: VOut) -> vec4<f32> {
    return vec4<f32>(textureSampleLevel(vid, vid_s, i.uv, 0.0).rgb, 1.0);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return texel(i);
}

struct TaaOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
};

// Fixé dans le monde : il bouge à l'écran comme les blocs (même calcul que les entités
// fixes), sinon l'anticrénelage le fait traîner.
@fragment
fn fs_taa_world(i: VOut) -> TaaOut {
    var o: TaaOut;
    o.color = texel(i);
    let p = vec4<f32>(i.world, 1.0);
    let cur = g.cur_view_proj * p;
    let prev = g.prev_view_proj * p;
    let d = prev.xy / prev.w - cur.xy / cur.w;
    o.motion = d * vec2<f32>(0.5, -0.5) * g.screen.xy;
    return o;
}
