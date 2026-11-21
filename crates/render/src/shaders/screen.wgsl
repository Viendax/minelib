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

// Le bouton de recherche, en haut à gauche de l'écran : un rond gris foncé et une loupe,
// dessinés ici même (aucune image, aucune police, donc aucune licence). Un clic dessus
// amène la page sur Google — le jeu le reconnaît avec `screen::on_search_button`, qui
// garde les mêmes mesures que ce qui est dessiné ici. (v205)
//
// Les pages des écrans font 1280 × 720 : une longueur donnée en `u` vaut 16/9 de la même
// en `v`, c'est ce qui rend le rond rond.
const BTN_ASPECT = 16.0 / 9.0;
const BTN_C = vec2<f32>(0.030, 0.053);
const BTN_R = 0.022;
const LOUPE_C = vec2<f32>(0.0265, 0.0455);
const LOUPE_R = 0.0092;
const TRAIT = 0.0026;

// Distance d'un point à un segment (pour le manche de la loupe).
fn dist_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-9), 0.0, 1.0);
    return length(p - a - ab * t);
}

// `uv` ramené en mesures rondes (celles de `u`).
fn btn_space(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x, uv.y / BTN_ASPECT);
}

fn search_button(uv: vec2<f32>, dessous: vec3<f32>) -> vec3<f32> {
    let p = btn_space(uv);
    let c = btn_space(BTN_C);
    let lc = btn_space(LOUPE_C);
    let d_rond = length(p - c) - BTN_R;
    // Lissage sur la largeur d'un pixel de l'écran, quelle que soit la distance.
    let aa = max(fwidth(d_rond), 1e-6);
    let rond = 1.0 - smoothstep(-aa, aa, d_rond);
    if rond <= 0.001 {
        return dessous;
    }
    // La loupe : un anneau et son manche.
    let d_anneau = abs(length(p - lc) - LOUPE_R) - TRAIT * 0.5;
    let a = lc + vec2<f32>(0.7071, 0.7071) * (LOUPE_R + TRAIT * 0.2);
    let b = lc + vec2<f32>(0.7071, 0.7071) * (LOUPE_R + TRAIT * 0.2 + 0.0082);
    let d_manche = dist_seg(p, a, b) - TRAIT * 0.55;
    let d_loupe = min(d_anneau, d_manche);
    let loupe = 1.0 - smoothstep(-aa, aa, d_loupe);
    // Gris foncé, et la loupe presque blanche par-dessus.
    let fond = mix(dessous, vec3<f32>(0.11, 0.115, 0.125), rond * 0.93);
    return mix(fond, vec3<f32>(0.92, 0.93, 0.95), loupe * rond);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(search_button(i.uv, texel(i).rgb), 1.0);
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
    o.color = vec4<f32>(
        search_button(i.uv, textureSampleLevel(vid, vid_s, uv, 0.0).rgb),
        1.0,
    );
    o.motion = NO_HISTORY;
    return o;
}
