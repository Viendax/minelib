// Minelib — entités dans le monde (modèle du joueur) : position monde, couleur unie déjà
// éclairée par le jeu.

// Début des uniformes du monde : la caméra (avec le tremblement du TAA).
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) layer: i32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: i32,
};

/// Couleur du pixel : couleur du sommet, texturée pour un bloc tenu (pixels transparents
/// jetés).
fn texel(i: VOut) -> vec3<f32> {
    if i.layer < 0 {
        return i.color;
    }
    let t = textureSampleLevel(blocks, block_sampler, i.uv, i.layer, 0.0);
    if t.a < 0.5 {
        discard;
    }
    return i.color * t.rgb;
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    o.uv = v.uv;
    o.layer = v.layer;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(texel(i), 1.0);
}

struct TaaOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
};

// Avec l'anticrénelage temporel : pas de vecteur de mouvement (le modèle suit la caméra).
@fragment
fn fs_taa(i: VOut) -> TaaOut {
    var o: TaaOut;
    o.color = vec4<f32>(texel(i), 1.0);
    o.motion = vec2<f32>(0.0);
    return o;
}
