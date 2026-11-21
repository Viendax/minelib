// Minelib — (v216) éclairs : la foudre, l'arc tenu en main et ses étincelles sont des
// segments lumineux. Chaque segment est un rectangle tourné vers l'écran, large de son
// halo ; le pixel en tire un cœur blanc très fin et un halo bleuté qui s'éteint au bord.
// La lumière s'ajoute à l'image (rien n'est assombri), et les murs devant cachent l'éclair.

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
    // xy = taille de l'image (px)
    screen: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
// Avec l'anticrénelage, les éclairs sont dessinés après lui, sur l'image finale (nets dès
// la première image, sans traînée) : la profondeur du monde de l'image dit ce qui les
// cache.
@group(1) @binding(0) var scene_depth: texture_depth_2d;

struct Seg {
    // Extrémités : dans le monde (xyz, w ignoré) ou déjà à l'écran (clip space) selon
    // `misc.w`.
    @location(0) a: vec4<f32>,
    @location(1) b: vec4<f32>,
    // rgb = teinte du halo, a = force à l'extrémité a.
    @location(2) color: vec4<f32>,
    // x = force à l'extrémité b ; y, z = demi-largeur du halo en a et en b (blocs dans le
    // monde ; part de la hauteur de l'écran sinon) ; w = 1 : extrémités déjà à l'écran.
    @location(3) misc: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // Position dans le repère du segment, en demi-largeurs du halo : x le long (0 en a),
    // y en travers (−1 … 1).
    @location(0) @interpolate(linear) at: vec2<f32>,
    @location(1) @interpolate(flat) color: vec3<f32>,
    // x = longueur du segment (en demi-largeurs), y et z = force en a et en b.
    @location(2) @interpolate(flat) seg: vec3<f32>,
    // Position à l'écran (−1 … 1), pour lire la profondeur du monde.
    @location(3) @interpolate(linear) ndc: vec2<f32>,
};

/// Plus près que ça de l'œil (w), un bout de segment est ramené devant.
const NEAR_W: f32 = 0.02;
/// Demi-largeur minimale à l'écran (px) : un éclair lointain reste un trait visible.
const MIN_PX: f32 = 1.3;
/// Le rectangle dépasse d'autant les deux bouts du segment (en demi-largeurs) : le halo
/// d'un bout de segment s'arrondit au-delà de lui.
const CAP: f32 = 1.0;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, s: Seg) -> VOut {
    return place(vi, s, g.view_proj);
}

// Après l'anticrénelage : sans son tremblement.
@vertex
fn vs_post(@builtin(vertex_index) vi: u32, s: Seg) -> VOut {
    return place(vi, s, g.cur_view_proj);
}

fn place(vi: u32, s: Seg, view_proj: mat4x4<f32>) -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    o.at = vec2<f32>(0.0);
    o.color = s.color.rgb;
    o.seg = vec3<f32>(0.0);
    o.ndc = vec2<f32>(0.0);
    let screen = s.misc.w > 0.5;
    var a = s.a;
    var b = s.b;
    var wa = s.misc.y;
    var wb = s.misc.z;
    var ia = s.color.a;
    var ib = s.misc.x;
    if !screen {
        a = view_proj * vec4<f32>(s.a.xyz, 1.0);
        b = view_proj * vec4<f32>(s.b.xyz, 1.0);
    }
    // Derrière l'œil : rien, ou le bout qui dépasse ramené devant.
    if a.w < NEAR_W && b.w < NEAR_W {
        return o;
    }
    if a.w < NEAR_W {
        let t = (NEAR_W - a.w) / (b.w - a.w);
        a = mix(a, b, t);
        wa = mix(wa, wb, t);
        ia = mix(ia, ib, t);
    } else if b.w < NEAR_W {
        let t = (NEAR_W - b.w) / (a.w - b.w);
        b = mix(b, a, t);
        wb = mix(wb, wa, t);
        ib = mix(ib, ia, t);
    }
    // Demi-largeur du halo à l'écran (la demi-hauteur de l'écran vaut 1). Dans le monde :
    // la taille d'un bloc à cette distance (2e ligne de la projection).
    let scale = length(vec3<f32>(view_proj[0][1], view_proj[1][1], view_proj[2][1]));
    var ra = wa * 2.0;
    var rb = wb * 2.0;
    if !screen {
        ra = wa * scale / a.w;
        rb = wb * scale / b.w;
    }
    let r = max(0.5 * (ra + rb), MIN_PX * 2.0 / max(g.screen.y, 1.0));
    // Le segment à l'écran, en pixels carrés (x multiplié par le rapport de l'écran).
    let aspect = g.screen.x / max(g.screen.y, 1.0);
    let k = vec2<f32>(aspect, 1.0);
    let pa = a.xy / a.w * k;
    let pb = b.xy / b.w * k;
    var d = pb - pa;
    let len = length(d);
    if len > 1e-6 {
        d = d / len;
    } else {
        d = vec2<f32>(1.0, 0.0);
    }
    let n = vec2<f32>(-d.y, d.x);
    // 6 sommets : deux triangles (a−, a+, b−) et (b−, a+, b+).
    let at_b = vi == 2u || vi == 3u || vi == 5u;
    let side = select(-1.0, 1.0, vi == 1u || vi == 4u || vi == 5u);
    let along = select(-CAP, len / r + CAP, at_b);
    let p = (pa + d * (along * r) + n * (side * r)) / k;
    // (Déjà divisé par w : les valeurs varient droit à l'écran.)
    o.clip = vec4<f32>(p, select(a.z / a.w, b.z / b.w, at_b), 1.0);
    o.at = vec2<f32>(along, side);
    o.seg = vec3<f32>(len / r, ia, ib);
    o.ndc = p;
    return o;
}

/// Lumière reçue en (`x`, `y`) d'un trait lumineux tendu de 0 à `len` le long de x, dont la
/// lueur s'étale sur `s` autour de lui (tout en demi-largeurs du halo) : 1 sur le trait.
/// C'est la somme de ce qu'envoie chaque point du trait : deux segments bout à bout
/// donnent exactement la lumière d'un seul, sans trou ni surbrillance à la jointure, même
/// en angle.
fn line_light(x: f32, y: f32, len: f32, s: f32) -> f32 {
    let ends = 0.5 * (tanh(1.2 * (len - x) / s) + tanh(1.2 * x / s));
    return exp(-(y * y) / (s * s)) * ends;
}

/// Couleur du pixel : un cœur blanc, étroit et très vif, et un halo coloré large qui
/// s'éteint au bord.
fn glow(i: VOut) -> vec3<f32> {
    let x = i.at.x;
    let y = i.at.y;
    let len = i.seg.x;
    let force = mix(i.seg.y, i.seg.z, clamp(x / max(len, 1e-5), 0.0, 1.0));
    let core = line_light(x, y, len, 0.07);
    let near = line_light(x, y, len, 0.19);
    let halo = line_light(x, y, len, 0.47);
    let edge = (1.0 - y * y) * (1.0 - y * y);
    let c = vec3<f32>(1.0) * (core * 2.6 + near * 0.3) + i.color * (near * 0.55 + halo * 0.3);
    return c * edge * force;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(glow(i), 0.0);
}

/// Ce pixel de l'éclair est-il derrière le monde ? (Profondeur inversée : plus grand =
/// plus près ; un millième de marge, la profondeur lue a le tremblement de l'anticrénelage.)
fn behind_world(ndc: vec2<f32>, z: f32) -> bool {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    return z < textureLoad(scene_depth, vec2<i32>(px), 0) * 0.999;
}

@fragment
fn fs_post(i: VOut) -> @location(0) vec4<f32> {
    if behind_world(i.ndc, i.clip.z) {
        discard;
    }
    return vec4<f32>(glow(i), 0.0);
}
