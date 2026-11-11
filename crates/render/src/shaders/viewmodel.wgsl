// Minelib — main et objet tenu (première personne). Sommets déjà en coordonnées écran
// (calculés par le jeu), textures du tableau des blocs.

// Début des uniformes du monde : seule l'horloge (cam_pos.w) sert ici.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VIn {
    @location(0) clip: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) layer: u32,
    @location(3) color: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
    @location(2) color: vec4<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    o.clip = v.clip;
    o.uv = v.uv;
    o.layer = v.layer;
    o.color = v.color;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    // Couche 0xFFFFFFFF : couleur unie (peau, flamme).
    let tex = textureSampleLevel(blocks, block_sampler, i.uv, i32(min(i.layer, 4095u)), 0.0);
    // Couche 0xFFFFFFFE : flamme, animée comme celle des torches posées (uv.y = hauteur
    // dans le bloc, couleur sRGB de la flamme).
    if i.layer == 0xfffffffeu {
        let y = i.uv.y;
        let t = g.cam_pos.w;
        let c = pow(i.color.rgb, vec3<f32>(2.2));
        let wobble = 0.85 + 0.15 * sin(t * 11.0 + y * 20.0);
        let core = clamp(1.0 - (y - 0.5) * 3.2, 0.0, 1.0);
        return vec4<f32>(c * 1.3 * wobble + c * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core, 1.0);
    }
    if i.layer == 0xffffffffu {
        // Alpha < 1 (fumée) : tramage animé, lissé par l'anticrénelage temporel.
        if i.color.a < 0.999 {
            let ign = fract(52.9829189 * fract(dot(i.clip.xy, vec2<f32>(0.06711056, 0.00583715))));
            let n = fract(ign + g.cam_pos.w * 7.31);
            if n > i.color.a {
                discard;
            }
        }
        return vec4<f32>(i.color.rgb, 1.0);
    }
    if tex.a < 0.5 {
        discard;
    }
    return vec4<f32>(tex.rgb * i.color.rgb, 1.0);
}
