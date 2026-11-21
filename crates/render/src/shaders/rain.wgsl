// Minelib — (v216) pluie : des milliers de gouttes filées par leur vitesse, et leurs
// éclaboussures au sol. Tout est calculé par la carte graphique à partir du numéro de la
// goutte : sa place dans une boîte qui suit la caméra (les gouttes restent fixes dans le
// monde, la boîte se répète autour d'elles), sa chute, et le toit au-dessus d'elle (carte
// des toits, une case par colonne de blocs) : il ne pleut pas sous un abri.

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

struct Rain {
    // xy = coin (x, z) de la carte des toits (blocs), z = côté de la carte, w = temps (s)
    origin: vec4<f32>,
    // Chute cumulée des quatre groupes de gouttes (blocs, modulo la boîte)
    fall: array<vec4<f32>, 4>,
    // xyz = sens de la chute (unitaire), w = force de la pluie (0 … 1)
    dir: vec4<f32>,
    // rgb = couleur des gouttes (lumière du ciel), a = opacité
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var<uniform> r: Rain;
@group(1) @binding(1) var tops: texture_2d<f32>;
// Avec l'anticrénelage, la pluie est dessinée après lui, sur l'image finale (des gouttes
// nettes à toutes les cadences, sans traînée) : la profondeur du monde de l'image dit ce
// qui la cache.
@group(2) @binding(0) var scene_depth: texture_depth_2d;

/// Boîte des gouttes autour de la caméra (blocs), et celle des éclaboussures (au sol).
const BOX: vec3<f32> = vec3<f32>(40.0, 32.0, 40.0);
const GROUND: f32 = 30.0;
/// Durée d'une éclaboussure (s).
const SPLASH_S: f32 = 0.32;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // x, y = position dans le rectangle (−1 … 1), z = opacité, w = âge (éclaboussure)
    @location(0) uv: vec4<f32>,
    // Position à l'écran (−1 … 1), pour lire la profondeur du monde.
    @location(1) @interpolate(linear) ndc: vec2<f32>,
};

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn unit(v: u32) -> f32 {
    return f32(v >> 8u) * (1.0 / 16777216.0);
}

/// Dessus du plus haut bloc de la colonne de `p` (−1e9 : rien, ou hors de la carte).
fn roof(p: vec3<f32>) -> f32 {
    let cell = vec2<i32>(floor(p.xz)) - vec2<i32>(r.origin.xy);
    let n = i32(r.origin.z);
    if cell.x < 0 || cell.y < 0 || cell.x >= n || cell.y >= n {
        return -1e9;
    }
    return textureLoad(tops, cell, 0).r;
}

const HIDDEN: vec4<f32> = vec4<f32>(2.0, 2.0, 2.0, 1.0);

@vertex
fn vs_drop(@builtin(vertex_index) vi: u32, @builtin(instance_index) id: u32) -> VOut {
    return drop_vertex(vi, id, g.view_proj);
}

// Après l'anticrénelage : sans son tremblement.
@vertex
fn vs_drop_post(@builtin(vertex_index) vi: u32, @builtin(instance_index) id: u32) -> VOut {
    return drop_vertex(vi, id, g.cur_view_proj);
}

fn drop_vertex(vi: u32, id: u32, view_proj: mat4x4<f32>) -> VOut {
    var o: VOut;
    o.clip = HIDDEN;
    o.uv = vec4<f32>(0.0);
    o.ndc = vec2<f32>(0.0);
    let h1 = pcg(id * 4u + 1u);
    let h2 = pcg(h1);
    let h3 = pcg(h2);
    let h4 = pcg(h3);
    let cam = g.cam_pos.xyz;
    // Place de la goutte : fixe dans le monde, la boîte se répète autour de la caméra.
    var p = vec3<f32>(unit(h1), unit(h2), unit(h3)) * BOX + r.fall[id & 3u].xyz;
    p = cam + (fract((p - cam) / BOX) - 0.5) * BOX;
    let len = mix(0.5, 1.15, unit(h4));
    let low = p + r.dir.xyz * len;
    // Sous un toit : pas de pluie.
    if low.y < roof(low) {
        return o;
    }
    let a = view_proj * vec4<f32>(p, 1.0);
    let b = view_proj * vec4<f32>(low, 1.0);
    if a.w < 0.05 || b.w < 0.05 {
        return o;
    }
    // Largeur d'une goutte filée : 2 cm ; jamais moins d'un demi-pixel (plus pâle alors).
    let scale = length(vec3<f32>(view_proj[0][1], view_proj[1][1], view_proj[2][1]));
    let min_r = 1.0 / max(g.screen.y, 1.0);
    let ra = 0.011 * scale / a.w;
    let rb = 0.011 * scale / b.w;
    let thin = clamp(min(ra, rb) / min_r, 0.0, 1.0);
    let aspect = g.screen.x / max(g.screen.y, 1.0);
    var d = (b.xy / b.w - a.xy / a.w) * vec2<f32>(aspect, 1.0);
    let dl = length(d);
    if dl < 1e-6 {
        return o;
    }
    d = d / dl;
    let n = vec2<f32>(-d.y, d.x);
    let at_b = vi == 2u || vi == 3u || vi == 5u;
    let side = select(-1.0, 1.0, vi == 1u || vi == 4u || vi == 5u);
    let q = select(a, b, at_b);
    let rr = max(select(ra, rb, at_b), min_r);
    o.clip = vec4<f32>(q.xy + n * side * rr / vec2<f32>(aspect, 1.0) * q.w, q.z, q.w);
    // Opacité : s'efface au bord de la boîte (pas de saut quand une goutte y repasse), et
    // tout près de l'œil.
    let off = abs(p - cam);
    let edge = 1.0 - smoothstep(0.72, 1.0, max(off.x, off.z) / (0.5 * BOX.x));
    let near = smoothstep(0.25, 1.6, a.w);
    // (Une goutte sur quatre plus claire : la pluie n'est pas un voile uniforme.)
    let shine = mix(0.55, 1.0, f32((id >> 2u) & 1u)) * mix(0.7, 1.0, unit(pcg(h4)));
    o.uv = vec4<f32>(side, select(-1.0, 1.0, at_b), edge * near * thin * shine, 0.0);
    o.ndc = o.clip.xy / o.clip.w;
    return o;
}

/// Ce pixel est-il derrière le monde ? (Profondeur inversée : plus grand = plus près ; un
/// millième de marge, la profondeur lue a le tremblement de l'anticrénelage.)
fn behind_world(ndc: vec2<f32>, z: f32) -> bool {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    return z < textureLoad(scene_depth, vec2<i32>(px), 0) * 0.999;
}

fn drop_color(i: VOut) -> vec4<f32> {
    // Fine au centre, estompée aux deux bouts.
    let across = 1.0 - i.uv.x * i.uv.x;
    let along = 1.0 - i.uv.y * i.uv.y * i.uv.y * i.uv.y;
    let alpha = r.color.a * i.uv.z * across * along;
    return vec4<f32>(r.color.rgb * alpha, alpha);
}

@fragment
fn fs_drop_post(i: VOut) -> @location(0) vec4<f32> {
    if behind_world(i.ndc, i.clip.z) {
        discard;
    }
    return drop_color(i);
}

@fragment
fn fs_drop(i: VOut) -> @location(0) vec4<f32> {
    return drop_color(i);
}

@vertex
fn vs_splash(@builtin(vertex_index) vi: u32, @builtin(instance_index) id: u32) -> VOut {
    return splash_vertex(vi, id, g.view_proj);
}

@vertex
fn vs_splash_post(@builtin(vertex_index) vi: u32, @builtin(instance_index) id: u32) -> VOut {
    return splash_vertex(vi, id, g.cur_view_proj);
}

fn splash_vertex(vi: u32, id: u32, view_proj: mat4x4<f32>) -> VOut {
    var o: VOut;
    o.clip = HIDDEN;
    o.uv = vec4<f32>(0.0);
    o.ndc = vec2<f32>(0.0);
    // Chaque numéro donne une éclaboussure après l'autre, à une place tirée au sort.
    let phase = unit(pcg(id * 2u + 77u));
    let t = r.origin.w / SPLASH_S + phase;
    let turn = u32(floor(t));
    let age = fract(t);
    let h1 = pcg(id * 31u + turn * 7919u + 5u);
    let h2 = pcg(h1);
    let cam = g.cam_pos.xyz;
    var p = vec3<f32>(unit(h1) * GROUND, 0.0, unit(h2) * GROUND);
    let rel = (fract((p.xz - cam.xz) / GROUND) - 0.5) * GROUND;
    p = vec3<f32>(cam.x + rel.x, cam.y, cam.z + rel.y);
    let top = roof(p);
    // Sur le dessus du bloc ; rien s'il est loin en dessous ou au-dessus (hors de vue).
    if top < -1e8 || abs(top - cam.y) > 24.0 {
        return o;
    }
    let corner = vec2<f32>(
        select(-1.0, 1.0, vi == 1u || vi == 4u || vi == 5u),
        select(-1.0, 1.0, vi == 2u || vi == 3u || vi == 5u),
    );
    let size = 0.2;
    let w = vec3<f32>(p.x + corner.x * size, top + 0.02, p.z + corner.y * size);
    o.clip = view_proj * vec4<f32>(w, 1.0);
    let edge = 1.0 - smoothstep(0.75, 1.0, max(abs(rel.x), abs(rel.y)) / (0.5 * GROUND));
    o.uv = vec4<f32>(corner, edge, age);
    o.ndc = o.clip.xy / max(o.clip.w, 1e-4);
    return o;
}

fn splash_color(i: VOut) -> vec4<f32> {
    // Une couronne qui s'élargit et s'efface.
    let d = length(i.uv.xy);
    let age = i.uv.w;
    let radius = 0.18 + 0.7 * age;
    let ring = exp(-(d - radius) * (d - radius) * 90.0);
    let alpha = r.color.a * 1.3 * i.uv.z * ring * (1.0 - age) * (1.0 - age) * (1.0 - smoothstep(0.9, 1.0, d));
    return vec4<f32>(r.color.rgb * alpha, alpha);
}

@fragment
fn fs_splash(i: VOut) -> @location(0) vec4<f32> {
    return splash_color(i);
}

@fragment
fn fs_splash_post(i: VOut) -> @location(0) vec4<f32> {
    if behind_world(i.ndc, i.clip.z) {
        discard;
    }
    return splash_color(i);
}
