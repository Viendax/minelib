// Minelib — (v216) flammes du molotov : un liquide qui brûle au sol, comme de l'alcool.
// Une flamme d'alcool est presque transparente : bleue à sa base et sur ses bords (là où
// le gaz brûle net), d'un jaune pâle dans son corps, orangée à sa pointe, sans fumée ; elle
// ondule mollement et se déchire en langues. Chaque flamme est un rectangle debout, tourné
// vers l'œil ; sa forme vient d'un bruit qui monte (le même volume de bruit que les
// nuages). Au sol, une lueur et de courtes flammes bleues : la flaque. Le bleu s'ajoute à
// l'image (on voit à travers) ; le corps jaune de la flamme la couvre en partie, sinon on
// ne verrait pas le feu sur un sol clair en plein jour.

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
    // xyz = pied de la flamme (monde), w = taille (blocs)
    @location(0) at: vec4<f32>,
    // x = graine (chaque flamme bouge à sa façon), y = force (0 éteinte … 1), z = 0 : une
    // flamme, 1 : la flaque au sol
    @location(1) misc: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // x = de gauche à droite (−1 … 1), y = du pied à la pointe (0 … 1)
    @location(0) uv: vec2<f32>,
    // x = graine, y = force, z = sorte, w = distance à l'œil (blocs)
    @location(1) @interpolate(flat) misc: vec4<f32>,
    // Position à l'écran (−1 … 1), pour lire la profondeur du monde.
    @location(2) @interpolate(linear) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, f: Flame) -> VOut {
    return place(vi, f, g.view_proj);
}

// Après l'anticrénelage : sans son tremblement.
@vertex
fn vs_post(@builtin(vertex_index) vi: u32, f: Flame) -> VOut {
    return place(vi, f, g.cur_view_proj);
}

fn place(vi: u32, f: Flame, view_proj: mat4x4<f32>) -> VOut {
    var o: VOut;
    let c = vec2<f32>(
        select(-1.0, 1.0, vi == 1u || vi == 4u || vi == 5u),
        select(0.0, 1.0, vi == 2u || vi == 3u || vi == 5u),
    );
    let to = g.cam_pos.xyz - f.at.xyz;
    var w = f.at.xyz;
    if f.misc.z < 0.5 {
        // Debout, tournée vers l'œil autour de la verticale.
        let flat = normalize(vec3<f32>(to.x, 0.0, to.z) + vec3<f32>(1e-5, 0.0, 0.0));
        let right = vec3<f32>(flat.z, 0.0, -flat.x);
        w += right * (c.x * f.at.w * 0.6) + vec3<f32>(0.0, c.y * f.at.w * 1.6, 0.0);
    } else {
        // À plat sur le sol.
        w += vec3<f32>(c.x * f.at.w, 0.03, (c.y * 2.0 - 1.0) * f.at.w);
    }
    o.clip = view_proj * vec4<f32>(w, 1.0);
    o.uv = c;
    o.misc = vec4<f32>(f.misc.xyz, length(to));
    o.ndc = o.clip.xy / max(o.clip.w, 1e-4);
    return o;
}

/// Flamme en ce point : rgb = sa lumière, a = ce qu'elle cache de ce qui est derrière.
fn flame(i: VOut) -> vec4<f32> {
    let t = g.cam_pos.w;
    let seed = i.misc.x;
    let x = i.uv.x;
    // Tout contre l'œil : elle s'efface (on ne se retrouve pas dans un mur de feu).
    let force = i.misc.y * smoothstep(0.3, 1.0, i.misc.w);
    if force <= 0.001 {
        return vec4<f32>(0.0);
    }
    if i.misc.z > 0.5 {
        // La flaque : une lueur au sol et de courtes flammes bleues qui courent dessus.
        let q = vec2<f32>(x, i.uv.y * 2.0 - 1.0);
        let d = length(q);
        if d >= 1.0 {
            return vec4<f32>(0.0);
        }
        let n = textureSampleLevel(noise, noise_s, vec3<f32>(q * 0.7 + seed, t * 0.35), 0.0).g;
        let lick = smoothstep(0.35, 0.8, n);
        let fall = (1.0 - d) * (1.0 - d);
        let warm = vec3<f32>(1.0, 0.5, 0.16) * 0.22 * (0.8 + 0.2 * sin(t * 9.0 + seed * 7.0));
        let blue = vec3<f32>(0.12, 0.3, 1.0) * 0.6 * lick;
        return vec4<f32>((warm + blue) * fall * force, lick * fall * force * 0.25);
    }
    // La flamme vacille : sa hauteur change sans cesse.
    let high = 0.74 + 0.26 * sin(t * 7.3 + seed * 11.0) * sin(t * 3.1 + seed * 5.0);
    let y = i.uv.y / high;
    if y >= 1.12 {
        return vec4<f32>(0.0);
    }
    // Turbulence qui monte : un bruit lent (la flamme se penche et se tord, de plus en
    // plus vers le haut) et un bruit plus fin (elle se déchire en langues).
    let slow = textureSampleLevel(noise, noise_s, vec3<f32>(x * 0.3 + seed * 0.37, i.uv.y * 0.45 - t * 0.5, t * 0.1 + seed), 0.0).a;
    let fine = textureSampleLevel(noise, noise_s, vec3<f32>(x * 0.85 + seed, i.uv.y * 1.2 - t * 1.15, t * 0.2), 0.0).g;
    let sway = (slow - 0.5) * 1.5 * y * y + sin(t * 2.3 + seed * 5.0) * 0.08 * y;
    let fx = abs(x - sway);
    // Large au pied, elle se resserre en pointe.
    let width = (1.0 - pow(clamp(y, 0.0, 1.0), 1.5)) * 1.1 * (0.72 + 0.56 * fine);
    let body = 1.0 - smoothstep(width * 0.3, width, fx);
    // Les langues : le bruit ronge la flamme, de plus en plus vers la pointe.
    let lick = smoothstep(0.12 + 0.5 * y, 0.45 + 0.5 * y, fine * 0.7 + slow * 0.6 + (1.0 - y) * 0.35);
    let dens = body * lick * (1.0 - smoothstep(0.85, 1.12, y));
    // Couleurs d'une flamme d'alcool : bleue au pied et sur ses bords minces, jaune pâle
    // dans son corps, orangée à la pointe.
    let inner = body * body;
    let hot = smoothstep(0.1, 0.45, y) * inner;
    let blue = vec3<f32>(0.1, 0.27, 1.0) * 1.0;
    let yellow = vec3<f32>(1.0, 0.7, 0.26) * 1.15;
    let tip = vec3<f32>(1.0, 0.34, 0.06) * 0.95;
    var col = mix(blue, mix(yellow, tip, smoothstep(0.45, 1.0, y)), hot);
    // Le liseré bleu vif du pied.
    col += vec3<f32>(0.22, 0.42, 1.0) * (1.0 - smoothstep(0.0, 0.14, y)) * 0.5;
    let a = dens * force;
    // Le corps jaune couvre en partie ce qui est derrière ; le bleu, presque pas.
    return vec4<f32>(col * a, a * (0.12 + 0.6 * hot));
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return flame(i);
}

@fragment
fn fs_post(i: VOut) -> @location(0) vec4<f32> {
    // Derrière le monde ? (Profondeur inversée : plus grand = plus près ; un millième de
    // marge, la profondeur lue a le tremblement de l'anticrénelage.)
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((i.ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    if i.clip.z < textureLoad(scene_depth, vec2<i32>(px), 0) * 0.999 {
        discard;
    }
    return flame(i);
}
