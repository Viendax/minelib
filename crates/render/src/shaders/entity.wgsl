// Minelib — entités dans le monde (modèle du joueur) : position monde, couleur unie, et
// l'éclairage du lieu donné par le jeu. Avec l'anticrénelage, elles laissent aussi albédo et
// normale (tirée des dérivées de la position : les modèles sont faits de faces planes) :
// les lumières dynamiques leur sont ajoutées après coup (dyn_deferred.wgsl). gbuffer.wgsl
// et dynlights_off.wgsl sont ajoutés à la suite.

// Début des uniformes du monde : la caméra (avec le tremblement du TAA), puis les
// matrices sans tremblement (image courante et précédente) pour les vecteurs de mouvement.
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
    // (v190) Passe d'un miroir : seulement ce qui est devant son plan (voir world.wgsl).
    clip: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) layer: i32,
    @location(4) light: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: i32,
    @location(3) world: vec3<f32>,
    @location(4) light: vec4<f32>,
};

/// Couleur du pixel sans l'éclairage : couleur du sommet, texturée pour un bloc tenu
/// (pixels transparents jetés).
fn texel(i: VOut) -> vec3<f32> {
    // Flamme d'une torche tenue (couche −2) : la même animation que la torche à la
    // première personne (viewmodel.wgsl) ; uv.y = hauteur dans le bloc de la torche.
    if i.layer == -2 {
        let y = i.uv.y;
        let c = i.color;
        let wobble = 0.85 + 0.15 * sin(g.cam_pos.w * 11.0 + y * 20.0);
        let core = clamp(1.0 - (y - 0.5) * 3.2, 0.0, 1.0);
        return c * 1.3 * wobble + c * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
    }
    if i.layer < 0 {
        return i.color;
    }
    let t = textureSampleLevel(blocks, block_sampler, i.uv, i.layer, 0.0);
    if t.a < 0.5 {
        discard;
    }
    return i.color * t.rgb;
}

/// Couleur éclairée par la lumière du lieu.
fn shade(i: VOut) -> vec3<f32> {
    return texel(i) * i.light.rgb * 2.0;
}

/// Normale de la face (vers la caméra), d'après les dérivées de la position : à calculer
/// avant tout branchement.
fn face_normal(world: vec3<f32>) -> vec3<f32> {
    var n = cross(dpdx(world), dpdy(world));
    let len = length(n);
    n = select(vec3<f32>(0.0, 1.0, 0.0), n / max(len, 1e-12), len > 1e-12);
    return select(n, -n, dot(n, g.cam_pos.xyz - world) < 0.0);
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    o.uv = v.uv;
    o.layer = v.layer;
    o.world = v.pos;
    o.light = v.light;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    // (v190) Dans un miroir : rien de ce qui est derrière lui (zéro ailleurs : tout passe).
    if dot(g.clip.xyz, i.world) + g.clip.w < 0.0 {
        discard;
    }
    return vec4<f32>(shade(i), 1.0);
}

/// Avec l'anticrénelage : couleur, mouvement, et de quoi y ajouter les lumières dynamiques
/// (voir gbuffer.wgsl ; albédo.a = 1 : entité).
struct TaaOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
    @location(2) albedo: vec4<f32>,
    @location(3) surf: vec4<f32>,
};

fn taa_out(i: VOut, n: vec3<f32>, motion: vec2<f32>) -> TaaOut {
    let base = texel(i);
    // `light.a` à 0 : pas de lumières dynamiques sur cette entité.
    let lit = select(0.0, 1.0, i.light.a > 0.5);
    var o: TaaOut;
    o.color = vec4<f32>(base * i.light.rgb * 2.0, 1.0);
    o.motion = motion;
    o.albedo = vec4<f32>(base, 1.0);
    o.surf = vec4<f32>(gb_oct_encode(n), lit, gb_pack_spec(0.5 * lit, false));
    return o;
}

// Avec l'anticrénelage temporel, le modèle du joueur local (il suit la caméra) : pas de
// vecteur de mouvement.
@fragment
fn fs_taa(i: VOut) -> TaaOut {
    let n = face_normal(i.world);
    return taa_out(i, n, vec2<f32>(0.0));
}

// Le reste (têtes des cartes, autres joueurs, bombe, armes au sol) : fixé dans le monde,
// il bouge à l'écran comme les blocs (sinon l'anticrénelage le fait traîner).
@fragment
fn fs_taa_world(i: VOut) -> TaaOut {
    let n = face_normal(i.world);
    let p = vec4<f32>(i.world, 1.0);
    let cur = g.cur_view_proj * p;
    let prev = g.prev_view_proj * p;
    let d = prev.xy / prev.w - cur.xy / cur.w;
    return taa_out(i, n, d * vec2<f32>(0.5, -0.5) * g.screen.xy);
}
