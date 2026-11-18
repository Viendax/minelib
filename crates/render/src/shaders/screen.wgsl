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

// L'image d'un écran change sans que l'écran bouge : l'anticrénelage temporel, qui garde
// longtemps l'historique d'un pixel immobile, laisserait des images rémanentes. Un
// mouvement « hors de l'écran » lui fait prendre l'image courante seule (aucun autre
// passage ne lit les mouvements). Le décalage sous-pixel de l'anticrénelage (différence
// entre `view_proj` et `cur_view_proj`) est annulé en lisant la vidéo : sans historique
// pour le moyenner, elle tremblerait.
const NO_HISTORY = vec2<f32>(30000.0, 30000.0);

@fragment
fn fs_taa_world(i: VOut) -> TaaOut {
    var o: TaaOut;
    let p = vec4<f32>(i.world, 1.0);
    let jit = g.view_proj * p;
    let cur = g.cur_view_proj * p;
    let off = (jit.xy / jit.w - cur.xy / cur.w) * vec2<f32>(0.5, -0.5) * g.screen.xy;
    let uv = i.uv + dpdx(i.uv) * off.x + dpdy(i.uv) * off.y;
    o.color = vec4<f32>(textureSampleLevel(vid, vid_s, uv, 0.0).rgb, 1.0);
    o.motion = NO_HISTORY;
    return o;
}
