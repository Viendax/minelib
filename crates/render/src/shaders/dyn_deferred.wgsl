// Minelib — lumières dynamiques (éditeur de lumières) ajoutées après coup à l'image du monde
// opaque : éclairage différé. Le monde et les entités laissent albédo, normale et poids dans
// deux images (gbuffer.wgsl) ; ce shader plein écran ajoute la lumière de chaque pixel
// visible une seule fois (pas sur les faces cachées derrière d'autres), sans alourdir le
// shader du monde. Même calcul que l'éclairage direct (dyn_light, dynlights.wgsl) :
//   couleur += albédo × diffus × poids diffus + reflets × poids des reflets
// (reflets teintés par l'albédo sur le métal). gbuffer.wgsl et dynlights.wgsl sont ajoutés
// à la suite.

// Début des uniformes du monde.
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
@group(1) @binding(0) var scene_depth: texture_depth_2d;
@group(1) @binding(1) var gb_albedo: texture_2d<f32>;
@group(1) @binding(2) var gb_surf: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_deferred(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    // Aucune lumière dans la tuile (la plus grande partie de l'écran, souvent) : rien à lire.
    if !dl_tile_any(frag.xy, g.screen.xy) {
        return vec4<f32>(0.0);
    }
    let px = vec2<i32>(frag.xy);
    let s = textureLoad(gb_surf, px, 0);
    let spec = gb_unpack_spec(s.a);
    if s.b <= 0.0 && spec.x <= 0.0 {
        return vec4<f32>(0.0);
    }
    let a = textureLoad(gb_albedo, px, 0);
    // Position du pixel : sa profondeur, à travers la même matrice (avec le décalage du
    // TAA) que celle qui l'a dessiné.
    let depth = textureLoad(scene_depth, px, 0);
    let ndc = vec2<f32>(frag.x / g.screen.x * 2.0 - 1.0, 1.0 - frag.y / g.screen.y * 2.0);
    let w = g.inv_view_proj * vec4<f32>(ndc, depth, 1.0);
    let p = w.xyz / w.w;
    let n = gb_oct_decode(s.rg);
    let entity = a.a > 0.995;
    let gloss = select(a.a / 0.99, 0.15, entity);
    let d = dyn_light(p, n, g.cam_pos.xyz, gloss, entity, frag.xy, g.screen.xy);
    let sp = select(d.specular, d.specular * a.rgb * 1.6, spec.y > 0.5);
    return vec4<f32>(a.rgb * d.diffuse * s.b + sp * spec.x, 0.0);
}
