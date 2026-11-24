// Minelib — (v216) flammes du molotov : un liquide qui brûle au sol, comme de l'alcool.
// Une flamme d'alcool est presque transparente : bleue à sa base et sur ses bords (là où
// le gaz brûle net), d'un jaune pâle dans son corps, orangée à sa pointe, sans fumée ; elle
// ondule mollement et se déchire en langues.
// (v217) Un vrai volume : chaque case en feu est une boîte que le regard traverse pas à
// pas dans un champ de flammes posé dans le monde (le même volume de bruit que les
// nuages). On tourne autour, on le voit de haut, les langues passent les unes devant les
// autres : avant, des panneaux plats tournés vers l'œil, et ça se voyait. Au sol, le film
// bleu du liquide qui brûle couvre toute la surface en feu. Le bleu s'ajoute à l'image (on
// voit à travers) ; le corps jaune de la flamme la couvre en partie, sinon on ne verrait
// pas le feu sur un sol clair en plein jour.

// Début des uniformes du monde.
struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    // xyz = caméra, w = temps (s)
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
@group(1) @binding(0) var noise: texture_3d<f32>;
@group(1) @binding(1) var noise_s: sampler;
// Avec l'anticrénelage, les flammes sont dessinées après lui, sur l'image finale : la
// profondeur du monde de l'image dit ce qui les cache.
@group(2) @binding(0) var scene_depth: texture_depth_2d;

struct Flame {
    // xyz = milieu de la case en feu, au sol ; w = hauteur des plus hautes flammes (blocs)
    @location(0) at: vec4<f32>,
    // x = graine, y = force (0 éteint … 1), z = côtés où la case voisine brûle aussi (bits :
    // 1 +x, 2 −x, 4 +z, 8 −z ; ailleurs le feu s'arrête en s'amincissant)
    @location(1) misc: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) at: vec4<f32>,
    @location(2) @interpolate(flat) misc: vec4<f32>,
    // Position à l'écran (−1 … 1), pour lire la profondeur du monde.
    @location(3) @interpolate(linear) ndc: vec2<f32>,
};

/// Les flammes ne dépassent pas leur case de plus haut que ça (× leur hauteur).
const TOP: f32 = 1.12;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, f: Flame) -> VOut {
    return place(vi, f, g.view_proj);
}

// Après l'anticrénelage : sans son tremblement.
@vertex
fn vs_post(@builtin(vertex_index) vi: u32, f: Flame) -> VOut {
    return place(vi, f, g.cur_view_proj);
}

/// Boîte de la case : 6 faces de 2 triangles, tournées vers l'extérieur.
fn place(vi: u32, f: Flame, view_proj: mat4x4<f32>) -> VOut {
    let face = vi / 6u;
    let k = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u)[vi % 6u];
    let sg = select(-1.0, 1.0, face % 2u == 0u);
    var n = vec3<f32>(0.0);
    var u = vec3<f32>(0.0);
    var v = vec3<f32>(0.0);
    if face < 2u {
        n = vec3<f32>(sg, 0.0, 0.0);
        u = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
        v = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
    } else if face < 4u {
        n = vec3<f32>(0.0, sg, 0.0);
        u = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
        v = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
    } else {
        n = vec3<f32>(0.0, 0.0, sg);
        u = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
        v = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
    }
    let c = array<vec2<f32>, 4>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0))[k];
    let unit = 0.5 + 0.5 * (n + c.x * u + c.y * v);
    let lo = f.at.xyz - vec3<f32>(0.5, 0.0, 0.5);
    let size = vec3<f32>(1.0, f.at.w * TOP, 1.0);
    var o: VOut;
    o.world = lo + unit * size;
    // (v254) Compté depuis la caméra (matrices du jeu sans sa translation).
    o.clip = view_proj * vec4<f32>(o.world - g.cam_pos.xyz, 1.0);
    o.at = f.at;
    o.misc = f.misc;
    o.ndc = o.clip.xy / max(o.clip.w, 1e-4);
    return o;
}

/// Le feu s'amincit vers les bords de la surface qui brûle (les côtés sans voisine en
/// feu) : 1 au cœur, 0 au bord.
fn inside(u: vec2<f32>, sides: u32) -> f32 {
    var m = 1.0;
    if (sides & 1u) == 0u {
        m *= 1.0 - smoothstep(0.08, 0.5, u.x);
    }
    if (sides & 2u) == 0u {
        m *= 1.0 - smoothstep(0.08, 0.5, -u.x);
    }
    if (sides & 4u) == 0u {
        m *= 1.0 - smoothstep(0.08, 0.5, u.y);
    }
    if (sides & 8u) == 0u {
        m *= 1.0 - smoothstep(0.08, 0.5, -u.y);
    }
    return m;
}

/// Le feu au point `p` (monde), `y` blocs au-dessus du sol : rgb = lumière émise par bloc
/// traversé, a = ce qu'il cache par bloc traversé.
fn fire(p: vec3<f32>, y: f32, hmax: f32, edge: f32) -> vec4<f32> {
    let t = g.cam_pos.w;
    // Chaque langue a sa hauteur, qui vacille.
    let wave = sin(p.x * 2.1 + 1.3 * sin(p.z * 1.7 + t * 0.6) + t * 1.1) * sin(p.z * 2.4 - t * 0.8);
    let high = hmax * (0.62 + 0.3 * wave + 0.08 * sin(t * 7.3 + p.x * 3.0));
    let h = y / max(high, 0.05);
    var out = vec4<f32>(0.0);
    if h < 1.15 {
        // Les langues penchent et se tordent en montant.
        let sway = h * h * 0.2 * vec2<f32>(sin(t * 2.3 + p.x * 1.3 + p.z * 0.7), cos(t * 1.9 + p.z * 1.1 - p.x * 0.5));
        let q = p.xz + sway;
        // Carte des langues (1 au cœur de chacune) : elles se resserrent en pointe.
        let w = textureSampleLevel(noise, noise_s, vec3<f32>(q.x / 2.4, t * 0.05, q.y / 2.4), 0.0).g;
        // Turbulence qui monte : elle déchire leurs bords.
        let lick = textureSampleLevel(noise, noise_s, vec3<f32>(p.x / 1.6, (y - t * 1.9) / 1.6, p.z / 1.6), 0.0).a;
        let hc = clamp(h, 0.0, 1.0);
        let thr = mix(0.2, 0.9, pow(hc, 0.8)) + (0.5 - lick) * (0.25 + 0.5 * hc);
        let body = smoothstep(thr, thr + 0.2, w) * (1.0 - smoothstep(0.92, 1.15, h));
        if body > 0.0 {
            // Couleurs d'une flamme d'alcool : bleue au pied et sur ses bords minces, jaune
            // pâle dans son corps, orangée à la pointe.
            let core = smoothstep(thr + 0.03, thr + 0.24, w);
            let hot = smoothstep(0.06, 0.34, hc) * core;
            let blue = vec3<f32>(0.1, 0.27, 1.0);
            let yellow = vec3<f32>(1.0, 0.7, 0.26) * 1.15;
            let tip = vec3<f32>(1.0, 0.34, 0.06) * 0.95;
            let col = mix(blue, mix(yellow, tip, smoothstep(0.5, 1.0, hc)), hot);
            let d = body * edge;
            out = vec4<f32>(col * d * 2.7, d * (0.45 + 2.8 * hot));
        }
    }
    // Le film bleu : le liquide brûle sur toute la surface, au ras du sol, en courtes
    // flammes qui courent dessus.
    if y < 0.2 {
        let run = textureSampleLevel(noise, noise_s, vec3<f32>(p.x / 1.1 + t * 0.07, t * 0.3, p.z / 1.1 - t * 0.05), 0.0).g;
        let film = (1.0 - y / 0.2) * (0.35 + 0.9 * run) * min(edge * 3.0, 1.0);
        out += vec4<f32>(vec3<f32>(0.13, 0.33, 1.0) * film * 1.2, film * 0.2);
    }
    return out;
}

/// Tirage par pixel : le départ des pas est décalé d'un pixel à l'autre (pas de bandes).
fn dither(px: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(px, vec2<f32>(0.06711056, 0.00583715))));
}

/// Le feu de la case le long du regard : rgb = sa lumière, a = ce qu'il cache de ce qui est
/// derrière. `depth` : profondeur du monde à ce pixel (0 : rien ne le cache).
fn flame(i: VOut, view_proj: mat4x4<f32>, depth: f32) -> vec4<f32> {
    let force = i.misc.y;
    if force <= 0.001 {
        return vec4<f32>(0.0);
    }
    let o = g.cam_pos.xyz;
    let d = normalize(i.world - o);
    let lo = i.at.xyz - vec3<f32>(0.5, 0.0, 0.5);
    let hi = i.at.xyz + vec3<f32>(0.5, i.at.w * TOP, 0.5);
    let inv = 1.0 / select(d, vec3<f32>(1e-6), abs(d) < vec3<f32>(1e-6));
    let ta = (lo - o) * inv;
    let tb = (hi - o) * inv;
    let t0 = max(max(max(min(ta.x, tb.x), min(ta.y, tb.y)), min(ta.z, tb.z)), 0.03);
    let t1 = min(min(max(ta.x, tb.x), max(ta.y, tb.y)), max(ta.z, tb.z));
    if t1 <= t0 {
        return vec4<f32>(0.0);
    }
    let sides = u32(i.misc.z + 0.5);
    let hmax = i.at.w * (0.35 + 0.65 * force);
    let n = clamp(ceil((t1 - t0) / 0.1), 3.0, 18.0);
    let ds = (t1 - t0) / n;
    // Profondeur à l'écran le long du rayon (elle varie droit avec la distance).
    // (v254) Depuis la caméra (`o`), comme les matrices.
    let c0 = view_proj * vec4<f32>(d * t0, 1.0);
    let c1 = view_proj * vec4<f32>(d * t1, 1.0);
    var t = t0 + ds * dither(i.clip.xy);
    var lum = vec3<f32>(0.0);
    var through = 1.0;
    for (var k = 0; k < 18; k++) {
        if t >= t1 || through < 0.03 {
            break;
        }
        let c = mix(c0, c1, (t - t0) / (t1 - t0));
        // Derrière le monde (profondeur inversée : plus grand = plus près) : fini.
        if c.z < depth * 0.999 * c.w {
            break;
        }
        let p = o + d * t;
        let edge = inside(p.xz - i.at.xz, sides);
        if edge > 0.002 {
            // Tout contre l'œil, le feu s'efface (on ne se retrouve pas dans un mur de feu).
            let f = fire(p, p.y - i.at.y, hmax, edge) * (force * smoothstep(0.05, 0.6, t));
            let a = 1.0 - exp(-f.a * ds);
            lum += through * f.rgb * ds;
            through *= 1.0 - a;
        }
        t += ds;
    }
    // Le regard sort par le sol de la case : la lueur du feu sur ce sol, chaude sous les
    // flammes, bleutée partout où le liquide brûle.
    if d.y < -1e-4 && through > 0.03 {
        let tg = (i.at.y - o.y) / d.y;
        if tg > t0 - 1e-3 && tg < t1 + 1e-3 {
            let cg = mix(c0, c1, clamp((tg - t0) / (t1 - t0), 0.0, 1.0));
            if cg.z >= depth * 0.97 * cg.w {
                let p = o + d * tg;
                let edge = inside(p.xz - i.at.xz, sides);
                let tt = g.cam_pos.w;
                let run = textureSampleLevel(noise, noise_s, vec3<f32>(p.x / 1.1 + tt * 0.07, tt * 0.3, p.z / 1.1 - tt * 0.05), 0.0).g;
                let warm = vec3<f32>(1.0, 0.5, 0.16) * 0.12 * (0.8 + 0.2 * sin(tt * 9.0 + i.misc.x * 7.0));
                let blue = vec3<f32>(0.12, 0.3, 1.0) * (0.12 + 0.3 * run);
                lum += through * (warm + blue) * min(edge * 2.5, 1.0) * force * smoothstep(0.05, 0.6, tg);
            }
        }
    }
    // Plusieurs langues l'une derrière l'autre : leur lumière s'ajoute sans virer au blanc
    // (la couleur est gardée, c'est son éclat qui plafonne).
    let top = max(lum.r, max(lum.g, lum.b));
    lum *= (1.0 - exp(-top * 1.3)) / max(top, 1e-4);
    return vec4<f32>(lum, 1.0 - through);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    // Dans la passe du monde : les faces avant de la boîte, cachées par ce qui est devant.
    return flame(i, g.view_proj, 0.0);
}

@fragment
fn fs_post(i: VOut) -> @location(0) vec4<f32> {
    // Après l'anticrénelage : la profondeur du monde dit où le regard s'arrête.
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((i.ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    return flame(i, g.cur_view_proj, textureLoad(scene_depth, vec2<i32>(px), 0));
}
