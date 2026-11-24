// Minelib — (v216) pluie : des milliers de gouttes filées par leur vitesse, et leurs
// éclaboussures au sol. Tout est calculé par la carte graphique à partir du numéro de la
// goutte : sa place dans une boîte qui suit la caméra (les gouttes restent fixes dans le
// monde, la boîte se répète autour d'elles), sa chute, et le toit au-dessus d'elle (carte
// des toits, une case par colonne de blocs) : il ne pleut pas sous un abri.
// (v251) Les lumières posées (projecteurs, lumières ponctuelles) éclairent les gouttes et les
// éclaboussures : dynlights.wgsl est ajouté à la suite (liaisons 7 à 13 du groupe 0).

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
    // (v251) Position dans le monde, pour les lumières posées.
    @location(2) world: vec3<f32>,
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
    o.world = vec3<f32>(0.0);
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
    // (v254) Comptés depuis la caméra (matrices du jeu sans sa translation).
    let a = view_proj * vec4<f32>(p - cam, 1.0);
    let b = view_proj * vec4<f32>(low - cam, 1.0);
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
    o.world = select(p, low, at_b);
    return o;
}

/// Ce pixel est-il derrière le monde ? (Profondeur inversée : plus grand = plus près ; un
/// millième de marge, la profondeur lue a le tremblement de l'anticrénelage.)
fn behind_world(ndc: vec2<f32>, z: f32) -> bool {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    return z < textureLoad(scene_depth, vec2<i32>(px), 0) * 0.999;
}

/// (v251) Visibilité (1 éclairé, 0 dans l'ombre) de la lumière `L` au point `p` : sa carte
/// d'ombre, comme pour ses rayons (`vl_shadow_at` de volumetric.wgsl).
fn rain_shadow(L: DynLight, fr: DlFrame, p: vec3<f32>, range: f32) -> f32 {
    let to = p - L.pos_range.xyz;
    let dist = max(length(to), 1e-4);
    let su = dl_shadow_uv(L, fr, to / dist);
    return select(
        1.0,
        textureSampleCompareLevel(shadow_atlas, shadow_cmp, su.xy, dist / range - 0.002),
        su.z > 0.5,
    );
}

/// (v251) « Je dois voir la pluie tomber sous un projecteur, et les gouttes sur le sol malgré la
/// lumière » : lumière des lumières posées (celles de la tuile d'écran du pixel) renvoyée vers
/// l'œil par l'eau en `p`, vue au point `ndc` de l'écran. Une goutte la renvoie surtout vers
/// l'avant (diffusion de Henyey-Greenstein, `g_hg`) : vue face à la lumière, elle brille ; une
/// éclaboussure (`splash`), lame d'eau sur le bloc, la renvoie de partout, et en reflet vers le
/// haut. Fenêtre de portée, cône, motif et ombres de chaque lumière : ceux des surfaces.
fn rain_lights(p: vec3<f32>, ndc: vec2<f32>, g_hg: f32, splash: bool) -> vec3<f32> {
    let count = dh.counts.x;
    if count == 0u {
        return vec3<f32>(0.0);
    }
    let size = g.screen.xy;
    let frag = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * size;
    let base = dl_tile_base(frag, size);
    let words = dl_tile_words(count);
    let v = normalize(g.cam_pos.xyz - p);
    let shadows_on = (dh.vl_c.w & DL_SHADOWS) != 0u;
    let gg = g_hg * g_hg;
    var acc = vec3<f32>(0.0);
    for (var w = 0u; w < words; w++) {
        var bits = dtiles[base + w];
        loop {
            if bits == 0u {
                break;
            }
            let b = firstTrailingBit(bits);
            bits = bits & (bits - 1u);
            let L = dlights[min(w * 32u + b, count - 1u)];
            // (« Entités seulement » : pas la pluie.)
            if u32(L.cone.z + 0.5) == 1u {
                continue;
            }
            let to_l = L.pos_range.xyz - p;
            let d2 = dot(to_l, to_l);
            let range = L.pos_range.w;
            if d2 >= range * range || d2 < 1e-8 {
                continue;
            }
            let d = sqrt(d2);
            let l = to_l / d;
            let dr = d / range;
            let dr2 = dr * dr;
            let fall = max(1.0 - dr2 * dr2, 0.0);
            var att = fall * fall;
            let fr = dl_frame(L);
            if L.dir_type.w > 0.5 {
                let theta = dot(-l, L.dir_type.xyz);
                att *= clamp((theta - L.cone.x) / max(L.cone.y - L.cone.x, 1e-6), 0.0, 1.0);
                if att > 0.0 {
                    att *= dl_cookie(p, L, fr);
                }
            }
            if att <= 0.0 {
                continue;
            }
            if shadows_on && L.shadow.x > 0.5 {
                att *= rain_shadow(L, fr, p, range);
                if att <= 0.0 {
                    continue;
                }
            }
            // Angle entre la lumière qui arrive (−l) et celle qui repart vers l'œil (v).
            let cos_t = dot(-l, v);
            let den = 1.0 + gg - 2.0 * g_hg * cos_t;
            var phase = (1.0 - gg) / (den * sqrt(max(den, 1e-6)));
            if splash {
                // Reflet de la lame d'eau (normale vers le haut), en plus.
                let h = normalize(l + v);
                phase += 6.0 * pow(max(h.y, 0.0), 64.0) * step(0.0, l.y);
            }
            // (v251) Passée par du verre : de sa couleur.
            acc += L.color.rgb * dl_tint(L, fr, -l, d, range, false) * (att * phase);
        }
    }
    return acc * dh.surf.x;
}

fn drop_color(i: VOut) -> vec4<f32> {
    // Fine au centre, estompée aux deux bouts.
    let across = 1.0 - i.uv.x * i.uv.x;
    let along = 1.0 - i.uv.y * i.uv.y * i.uv.y * i.uv.y;
    let alpha = r.color.a * i.uv.z * across * along;
    // (v251) Et la lumière des lumières posées, renvoyée vers l'avant.
    let lit = rain_lights(i.world, i.ndc, 0.55, false) * RAIN_LIT;
    return vec4<f32>((r.color.rgb + lit) * alpha, alpha);
}

/// (v251) Part de la lumière d'une lumière posée que renvoie une goutte (ou une éclaboussure)
/// vers l'œil : une goutte filée couvre peu de son pixel, mais elle brille sous un projecteur.
const RAIN_LIT: f32 = 0.06;
const SPLASH_LIT: f32 = 0.09;

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
    o.world = vec3<f32>(0.0);
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
    o.clip = view_proj * vec4<f32>(w - cam, 1.0);
    let edge = 1.0 - smoothstep(0.75, 1.0, max(abs(rel.x), abs(rel.y)) / (0.5 * GROUND));
    o.uv = vec4<f32>(corner, edge, age);
    o.ndc = o.clip.xy / max(o.clip.w, 1e-4);
    o.world = w;
    return o;
}

fn splash_color(i: VOut) -> vec4<f32> {
    // Une couronne qui s'élargit et s'efface.
    let d = length(i.uv.xy);
    let age = i.uv.w;
    let radius = 0.18 + 0.7 * age;
    let ring = exp(-(d - radius) * (d - radius) * 90.0);
    let alpha = r.color.a * 1.3 * i.uv.z * ring * (1.0 - age) * (1.0 - age) * (1.0 - smoothstep(0.9, 1.0, d));
    // (v251) Sous une lumière posée, la couronne d'eau la renvoie (et la reflète vers le haut) :
    // on la voit sur le sol éclairé.
    let lit = rain_lights(i.world, i.ndc, 0.2, true) * SPLASH_LIT;
    return vec4<f32>((r.color.rgb + lit) * alpha, alpha);
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
