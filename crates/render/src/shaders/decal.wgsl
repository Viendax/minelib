// Minelib — (v249) traces laissées sur les blocs en mode FPS : sang, impacts de balles,
// entailles du couteau, suie des grenades, sol brûlé par le molotov. Elles restent jusqu'à
// la fin de la manche.
//
// Chaque trace est une boîte mince posée sur la surface touchée. Avec l'anticrénelage, elle
// est dessinée après lui, sur l'image finale : le shader lit la profondeur du monde, retrouve
// le point de la scène sous chaque pixel et le replace dans le repère de la trace. La trace
// épouse donc ce qu'il y a dessous (une marche, un coin de bloc), ne dépasse jamais dans le
// vide, et ce qui passe devant elle la cache. Sans anticrénelage : un rectangle posé à plat
// sur la surface, dans la passe du monde.
//
// Une trace ne peint pas une couleur : elle multiplie la couleur de l'image (une tache
// assombrit et teinte ce qu'elle couvre). Elle garde ainsi l'éclairage du lieu, de jour
// comme de nuit, à l'ombre comme sous une lampe, sans rien recalculer. Le dessin de chaque
// sorte est calculé ici (pas d'image) : chaque trace a sa forme, tirée de sa graine.

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

struct DecalGlobals {
    // x = horloge des traces (s) : leur âge est `now.x − born`
    now: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var<uniform> du: DecalGlobals;
@group(2) @binding(0) var scene_depth: texture_depth_2d;

struct Decal {
    // xyz = milieu de la trace, sur la surface (monde) ; w = sorte (`KIND_…`, plus les côtés
    // voisins pour le sol brûlé : voir `burn`)
    @location(0) at: vec4<f32>,
    // xyz = normale de la surface ; w = graine
    @location(1) nrm: vec4<f32>,
    // xyz = axe u de la trace, dans la surface (v = normale × u) ; w = heure de naissance
    @location(2) tan: vec4<f32>,
    // x, y = demi-côtés le long de u et de v (blocs) ; z = demi-épaisseur de la boîte ;
    // w = force (0 … 1)
    @location(3) size: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) at: vec4<f32>,
    @location(2) @interpolate(flat) nrm: vec4<f32>,
    @location(3) @interpolate(flat) tan: vec4<f32>,
    @location(4) @interpolate(flat) size: vec4<f32>,
    // Position à l'écran (−1 … 1), pour lire la profondeur du monde.
    @location(5) @interpolate(linear) ndc: vec2<f32>,
    // Place dans la trace (−1 … 1), pour le rectangle posé à plat.
    @location(6) uv: vec2<f32>,
    // 1 de près … 0 au bout de la distance où on les dessine.
    @location(7) @interpolate(flat) fade: f32,
    // (v254) Place depuis la caméra, exacte loin du centre du monde (voir world.wgsl).
    @location(8) rel: vec3<f32>,
    // (v264) 1 : rectangle posé à plat même après l'anticrénelage (impact dans une vitre : la
    // vitre n'est pas dans la profondeur du monde, rien à quoi coller la boîte).
    @location(9) @interpolate(flat) plat: f32,
};

const KIND_SPLAT: f32 = 1.0;
const KIND_SPRAY: f32 = 2.0;
const KIND_DRIP: f32 = 3.0;
const KIND_HOLE: f32 = 4.0;
const KIND_SLASH: f32 = 5.0;
const KIND_SCORCH: f32 = 6.0;
const KIND_BURN: f32 = 7.0;

/// On les dessine jusqu'à cette distance (blocs) ; elles s'effacent sur le dernier quart.
const FAR: f32 = 110.0;

fn hidden() -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
    return o;
}

/// Boîte de la trace (6 faces de 2 triangles, tournées vers l'extérieur), ou, `flat`, son
/// rectangle posé sur la surface (les 6 premiers sommets).
fn place(vi: u32, d: Decal, view_proj: mat4x4<f32>, flat: bool) -> VOut {
    let cam = g.cam_pos.xyz;
    let dist = length(d.at.xyz - cam);
    if dist > FAR || d.size.w <= 0.0 {
        return hidden();
    }
    let n = d.nrm.xyz;
    let u = d.tan.xyz;
    let v = cross(n, u);
    // (v264) Le rectangle à plat est donné dans les deux sens (sommets 6 à 11 : l'ordre
    // inverse) : la passe d'après l'anticrénelage écarte les faces avant, l'autre aucune.
    var k = array<u32, 6>(0u, 1u, 2u, 0u, 2u, 3u)[vi % 6u];
    if flat && vi >= 6u {
        k = array<u32, 6>(0u, 2u, 1u, 0u, 3u, 2u)[vi % 6u];
    }
    let c = array<vec2<f32>, 4>(vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0))[k];
    var o: VOut;
    o.plat = select(0.0, 1.0, flat);
    if flat {
        if vi >= 12u {
            return hidden();
        }
        let p = d.at.xyz + u * (c.x * d.size.x) + v * (c.y * d.size.y);
        // Un peu vers l'œil, d'autant plus qu'elle est loin : jamais dans la surface.
        o.world = p + normalize(cam - p) * (0.004 + 0.0015 * dist);
        o.uv = c;
    } else {
        let face = vi / 6u;
        let sg = select(-1.0, 1.0, face % 2u == 0u);
        var fn_ = vec3<f32>(0.0);
        var fu = vec3<f32>(0.0);
        var fv = vec3<f32>(0.0);
        if face < 2u {
            fn_ = vec3<f32>(sg, 0.0, 0.0);
            fu = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
            fv = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
        } else if face < 4u {
            fn_ = vec3<f32>(0.0, sg, 0.0);
            fu = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), sg > 0.0);
            fv = select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
        } else {
            fn_ = vec3<f32>(0.0, 0.0, sg);
            fu = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), sg > 0.0);
            fv = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), sg > 0.0);
        }
        // Coin de la boîte dans son repère (u, v, normale), de −1 à 1.
        let l = fn_ + c.x * fu + c.y * fv;
        o.world = d.at.xyz + u * (l.x * d.size.x) + v * (l.y * d.size.y) + n * (l.z * d.size.z);
        o.uv = l.xy;
    }
    // (v254) Les matrices du jeu sont comptées depuis la caméra : le point aussi.
    o.rel = o.world - cam;
    o.clip = view_proj * vec4<f32>(o.rel, 1.0);
    o.at = d.at;
    o.nrm = d.nrm;
    o.tan = d.tan;
    o.size = d.size;
    o.ndc = o.clip.xy / max(o.clip.w, 1e-4);
    o.fade = 1.0 - smoothstep(FAR * 0.75, FAR, dist);
    return o;
}

// Sans anticrénelage : le rectangle, dans la passe du monde.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32, d: Decal) -> VOut {
    return place(vi, d, g.view_proj, true);
}

// Après l'anticrénelage (sans son tremblement) : la boîte.
@vertex
fn vs_post(@builtin(vertex_index) vi: u32, d: Decal) -> VOut {
    // (v264) Impact dans une vitre (sorte 4, matière 3) : à plat sur la vitre.
    let glass = floor(d.at.w + 0.01) == KIND_HOLE && u32(fract(d.at.w) * 16.0 + 0.5) == 3u;
    return place(vi, d, g.cur_view_proj, glass);
}

// ---------------------------------------------------------------- hasard

fn hash1(p: vec2<f32>) -> f32 {
    var q = fract(vec3<f32>(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
}

fn hash2(p: vec2<f32>) -> vec2<f32> {
    var q = fract(vec3<f32>(p.xyx) * vec3<f32>(0.1031, 0.1030, 0.0973));
    q += dot(q, q.yzx + 33.33);
    return fract((q.xx + q.yz) * q.zy);
}

/// Bruit lisse (0 … 1).
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (3.0 - 2.0 * f);
    let a = hash1(i);
    let b = hash1(i + vec2<f32>(1.0, 0.0));
    let c = hash1(i + vec2<f32>(0.0, 1.0));
    let d = hash1(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    return 0.5 * vnoise(p) + 0.3 * vnoise(p * 2.03 + 7.1) + 0.2 * vnoise(p * 4.1 + 3.7);
}

/// Décalage du hasard d'une trace : chacune a sa forme.
fn seeded(seed: f32) -> vec2<f32> {
    return vec2<f32>(seed * 17.13, seed * 5.71);
}

// ---------------------------------------------------------------- sang

/// Ce que le sang laisse de la couleur qu'il couvre (un facteur par couleur). Mince, il est
/// d'un rouge vif et laisse voir le support ; épais, d'un rouge presque noir. En séchant (en
/// une minute environ) il brunit et ternit.
fn blood_tint(thick: f32, age: f32) -> vec3<f32> {
    let dry = smoothstep(8.0, 70.0, age);
    let thin = mix(vec3<f32>(0.80, 0.10, 0.07), vec3<f32>(0.60, 0.20, 0.13), dry);
    let deep = mix(vec3<f32>(0.33, 0.012, 0.012), vec3<f32>(0.19, 0.034, 0.024), dry);
    return mix(thin, deep, clamp(thick, 0.0, 1.0));
}

/// Tache ronde aux bords découpés (des lobes, quelques doigts), entourée de gouttes : une
/// goutte tombée, une éclaboussure reçue de face. Rend (part couverte, épaisseur).
fn splat(uv: vec2<f32>, seed: f32) -> vec2<f32> {
    let so = seeded(seed);
    let r = length(uv);
    let dir = uv / max(r, 1e-4);
    // Un bord festonné, et quelques épines fines (pas une étoile).
    let lobes = vnoise(dir * 1.7 + so);
    let scallops = vnoise(dir * 7.0 + so * 2.7);
    let spines = pow(vnoise(dir * 5.5 + so * 1.9), 5.0);
    let edge = 0.38 + 0.14 * lobes + 0.05 * scallops + 0.40 * spines;
    let body = 1.0 - smoothstep(edge - 0.07, edge, r);
    // Gouttes autour, de plus en plus petites et rares en s'éloignant.
    let gp = uv * 4.5 + so;
    let id = floor(gp);
    let f = fract(gp) - 0.5;
    let c = (hash2(id + 31.7) - 0.5) * 0.4;
    let away = length((id + 0.5 + c - so) / 4.5);
    let present = step(hash1(id * 1.31 + 5.0), 0.42 * (1.0 - smoothstep(0.55, 0.95, away)));
    let rad = (0.06 + 0.13 * hash1(id + 77.0)) * (1.15 - min(away, 1.0));
    let drop = present * (1.0 - smoothstep(rad - 0.05, rad, length(f - c)));
    let cov = max(body, drop);
    let thick = max(body * (1.0 - smoothstep(edge * 0.35, edge, r)), drop * 0.6);
    return vec2<f32>(cov, thick);
}

/// Giclée : le sang arrive en biais et s'étale dans le sens où il allait (+u). Une tache
/// là où le gros du jet a touché, puis des gouttes allongées qui s'écartent en éventail, de
/// plus en plus rares. Rend (part couverte, épaisseur).
fn spray(uv: vec2<f32>, seed: f32) -> vec2<f32> {
    let so = seeded(seed);
    let q = uv - vec2<f32>(-0.82, 0.0);
    let d = length(q);
    let ang = atan2(q.y, q.x);
    // Gouttes : une par case (distance, angle), allongée le long du jet.
    let gp = vec2<f32>(d * 5.5, ang * 6.0) + so;
    let id = floor(gp);
    let f = fract(gp) - 0.5;
    let rnd = hash2(id + 11.3);
    let fan = 1.0 - smoothstep(0.30, 0.95, abs(ang));
    let reach = 1.0 - smoothstep(0.8, 1.75, d);
    let present = step(hash1(id * 1.7 + 3.0), 0.66 * fan * reach);
    let len = 0.30 + 0.16 * rnd.x;
    let wid = 0.10 + 0.22 * rnd.y;
    let e = vec2<f32>((f.x - (rnd.y - 0.5) * 0.2) / len, (f.y - (rnd.x - 0.5) * 0.3) / wid);
    let drop = present * (1.0 - smoothstep(0.7, 1.0, length(e)));
    // Le gros de la tache.
    let c0 = q - vec2<f32>(0.42, 0.0);
    let r0 = length(c0 * vec2<f32>(0.8, 1.25));
    let dir0 = c0 / max(length(c0), 1e-4);
    let edge = 0.20 + 0.14 * vnoise(dir0 * 2.0 + so) + 0.20 * pow(vnoise(dir0 * 5.0 + so * 1.3), 3.0);
    let body = 1.0 - smoothstep(edge - 0.05, edge, r0);
    let cov = max(body, drop);
    let thick = max(body * (1.0 - smoothstep(edge * 0.3, edge, r0)), drop * (0.45 + 0.4 * rnd.x));
    return vec2<f32>(cov, thick);
}

/// Coulure le long d'un mur : elle part du haut de sa boîte et descend peu à peu, une
/// goutte ronde à son bout. `aspect` : hauteur de la boîte sur sa largeur.
fn drip(uv: vec2<f32>, seed: f32, age: f32, aspect: f32) -> vec2<f32> {
    let grown = 1.0 - exp(-max(age, 0.0) / 2.4);
    let tip = 1.0 - 2.0 * grown * (1.0 - 1.4 / max(aspect, 2.0));
    let cx = 0.22 * sin(uv.y * 2.3 + seed * 6.0);
    let half = 0.42 + 0.2 * vnoise(vec2<f32>(uv.y * 3.0, seed * 9.0));
    let trail = step(tip, uv.y) * (1.0 - smoothstep(half - 0.25, half, abs(uv.x - cx)));
    // La goutte du bout, ronde dans le monde (la boîte est bien plus haute que large).
    let bead = length(vec2<f32>(uv.x - cx, (uv.y - tip) * aspect));
    let head = (1.0 - smoothstep(0.65, 0.95, bead)) * step(0.001, grown);
    let wet = mix(0.9, 0.45, smoothstep(tip, 1.0, uv.y));
    return vec2<f32>(max(trail * step(0.02, grown), head), max(trail * wet, head));
}

// ---------------------------------------------------------------- impacts et brûlures

/// Fissures fines qui partent du trou : `count` traits, de `start` à au plus `reach`.
fn cracks(uv: vec2<f32>, so: vec2<f32>, seed: f32, count: i32, start: f32, reach: f32, thin: f32) -> f32 {
    var crack = 0.0;
    for (var k = 0; k < count; k++) {
        let h = hash2(vec2<f32>(f32(k) * 7.3, seed * 3.1) + so);
        let a = (f32(k) + h.x) * 6.2831853 / f32(count);
        let cd = vec2<f32>(cos(a), sin(a));
        let along = dot(uv, cd);
        let across = abs(dot(uv, vec2<f32>(-cd.y, cd.x)) + 0.03 * sin(along * 14.0 + h.y * 6.0));
        let far = start + (reach - start) * (0.45 + 0.55 * h.y);
        crack = max(crack, step(start * 0.7, along) * (1.0 - smoothstep(far * 0.7, far, along))
            * (1.0 - smoothstep(thin, thin * 2.4, across)));
    }
    return crack;
}

/// Impact de balle (v264) : facteurs de clarté (1 : rien ; plus : plus clair), selon la matière
/// touchée `mat` (0 pierre, 1 bois, 2 métal, 3 verre, 4 matière meuble). `sun` : d'où vient
/// la lumière, dans le repère de la trace : la paroi d'un creux qui lui fait face est claire,
/// celle d'en face dans l'ombre (le relief). `aspect` : largeur sur hauteur de la trace
/// (celle du bois est allongée le long du fil, son axe u).
fn hole(uv: vec2<f32>, seed: f32, mat: u32, sun: vec2<f32>, aspect: f32) -> vec4<f32> {
    let so = seeded(seed);
    var f = vec3<f32>(1.0);
    var edge = length(uv);
    if mat == 1u {
        // Bois : un petit trou, le bois arraché en fuseau le long du fil (plus clair, en
        // fibres), des fentes qui filent dans le fil.
        let q = vec2<f32>(uv.x * aspect, uv.y);
        let r = length(q);
        let side = step(0.0, q.x);
        let span = aspect * (0.55 + 0.35 * hash1(vec2<f32>(side * 3.7 + 1.0, seed) + so));
        let t = clamp(abs(q.x) / span, 0.0, 1.0);
        let fib = vnoise(vec2<f32>(q.x * 1.6, q.y * 15.0) + so);
        let wide = 0.46 * (1.0 - pow(t, 1.4)) * (0.72 + 0.5 * fib);
        let torn = (1.0 - smoothstep(wide * 0.8, wide + 0.02, abs(q.y))) * step(t, 0.999);
        let pit = 1.0 - smoothstep(0.15, 0.24, r);
        let gaps = step(fib, 0.26) * torn;
        let split = (1.0 - smoothstep(0.012, 0.035, abs(q.y - 0.06 * sin(q.x * 3.0 + seed))))
            * (1.0 - smoothstep(aspect * 0.75, aspect * 0.98, abs(q.x))) * step(span * 0.6, abs(q.x));
        // Relief : le bord du haut ou du bas du fuseau, selon la lumière.
        let lit = -sign(q.y) * sun.y * smoothstep(wide * 0.5, wide, abs(q.y));
        f = mix(f, vec3<f32>(1.30, 1.21, 1.02) * (0.86 + 0.26 * fib) * (1.0 + 0.22 * lit), torn);
        f = mix(f, vec3<f32>(0.52, 0.47, 0.40), gaps * 0.8);
        f = mix(f, vec3<f32>(0.50), split * 0.75);
        f = mix(f, vec3<f32>(0.07), pit);
        edge = max(abs(uv.x), abs(uv.y));
    } else if mat == 2u {
        // Métal : un trou net, son bord mis à nu qui brille du côté de la lumière, la tôle
        // enfoncée autour, noircie près du trou ; pas de fissure.
        let r = length(uv);
        let dir = uv / max(r, 1e-4);
        let lit = -dot(dir, sun);
        let pit = 1.0 - smoothstep(0.17, 0.21, r);
        let bevel = (1.0 - smoothstep(0.28, 0.33, r)) * (1.0 - pit);
        let dent = (1.0 - smoothstep(0.30, 0.70, r)) * (1.0 - pit) * (1.0 - bevel);
        let soot = (1.0 - smoothstep(0.30, 0.52, r)) * (1.0 - pit) * (1.0 - bevel);
        let scratch = step(0.72, vnoise(vec2<f32>(atan2(dir.y, dir.x) * 7.0, r * 3.0) + so)) * dent;
        f = mix(f, vec3<f32>(1.0 + 0.30 * lit), dent);
        f = mix(f, f * 0.74, soot);
        f = mix(f, f * 1.16, scratch * 0.6);
        f = mix(f, vec3<f32>(1.08 + 0.62 * lit), bevel);
        f = mix(f, vec3<f32>(0.04), pit);
    } else if mat == 3u {
        // Verre : un petit trou, le verre broyé autour (dépoli : gris, piqué de points
        // clairs), une étoile de fissures et des arcs entre elles. Une fissure est un trait
        // sombre bordé de clair : elle se voit devant le ciel comme devant un mur sombre.
        let r = length(uv);
        let dir = uv / max(r, 1e-4);
        let pit = 1.0 - smoothstep(0.05, 0.08, r);
        let crush = (1.0 - smoothstep(0.12, 0.24 + 0.08 * vnoise(dir * 4.0 + so), r)) * (1.0 - pit);
        let star = cracks(uv, so, seed, 9, 0.10, 0.98, 0.009);
        let glow = cracks(uv, so, seed, 9, 0.10, 0.98, 0.026);
        let ang = atan2(dir.y, dir.x);
        let sector = hash1(vec2<f32>(floor((ang + 3.1415927) * 9.0 / 6.2831853), seed) + so);
        let ring1 = (1.0 - smoothstep(0.008, 0.022, abs(r - (0.36 + 0.06 * sector)))) * step(0.45, sector);
        let ring2 = (1.0 - smoothstep(0.008, 0.022, abs(r - (0.60 + 0.08 * sector)))) * step(sector, 0.40);
        let arcs = max(ring1, ring2);
        f = mix(f, vec3<f32>(1.32), glow * 0.55);
        f = mix(f, vec3<f32>(0.55), max(star, arcs) * 0.85);
        let grit = vnoise(uv * 24.0 + so);
        f = mix(f, mix(vec3<f32>(0.66), vec3<f32>(1.45), step(0.62, grit)), crush);
        f = mix(f, vec3<f32>(0.38), pit);
    } else if mat == 4u {
        // Terre, sable, laine, feuilles : un creux aux bords mous, quelques grains autour.
        let r = length(uv);
        let dir = uv / max(r, 1e-4);
        let lit = -dot(dir, sun);
        let rim = 0.34 + 0.12 * vnoise(dir * 2.5 + so);
        let bowl = 1.0 - smoothstep(rim * 0.35, rim, r);
        let wall = smoothstep(rim * 0.3, rim * 0.8, r) * (1.0 - smoothstep(rim * 0.8, rim * 1.15, r));
        let grains = step(0.70, vnoise(uv * 13.0 + so)) * (1.0 - smoothstep(rim, 0.9, r)) * (1.0 - bowl);
        f = mix(f, vec3<f32>(0.42 + 0.3 * smoothstep(0.0, rim, r)), bowl);
        f = f * (1.0 + 0.22 * lit * wall);
        f = mix(f, f * 0.84, grains);
    } else {
        // Pierre, béton, brique : un petit trou, l'éclat arraché autour (la matière fraîche,
        // plus claire, en cuvette), quelques éclats partis à côté, des fissures courtes.
        let r = length(uv);
        let dir = uv / max(r, 1e-4);
        let lit = -dot(dir, sun);
        let rp = 0.13 + 0.03 * vnoise(dir * 3.0 + so);
        let pit = 1.0 - smoothstep(rp, rp + 0.05, r);
        let rim = 0.42 + 0.22 * vnoise(dir * 2.5 + so * 1.7);
        let spall = (1.0 - smoothstep(rim - 0.03, rim, r)) * (1.0 - pit);
        let chip = step(0.64, vnoise(uv * 7.0 + so * 3.0)) * (1.0 - smoothstep(rim, rim + 0.26, r))
            * (1.0 - spall) * (1.0 - pit);
        let hair = cracks(uv, so, seed, 3, rim * 0.8, rim + 0.42, 0.011) * (1.0 - spall) * (1.0 - pit);
        // La cuvette : plus sombre au fond, claire au bord ; sa paroi prend la lumière.
        let depth = smoothstep(rp, rim * 0.85, r);
        let fresh = (0.80 + 0.40 * depth) * (0.93 + 0.14 * vnoise(uv * 11.0 + so));
        let wall = smoothstep(rp, rim * 0.5, r) * (1.0 - smoothstep(rim * 0.7, rim, r)) + 0.35;
        f = mix(f, vec3<f32>(1.13, 1.12, 1.09), chip * 0.75);
        f = mix(f, vec3<f32>(0.52), hair * 0.8);
        f = mix(f, vec3<f32>(fresh * 1.02, fresh, fresh * 0.97) * (1.0 + 0.30 * lit * wall), spall);
        f = mix(f, vec3<f32>(0.06), pit);
    }
    return vec4<f32>(f, 1.0 - smoothstep(0.92, 1.0, edge));
}

/// Entaille du couteau : une saignée sombre effilée aux deux bouts, la matière éraflée
/// plus claire de part et d'autre.
fn slash(uv: vec2<f32>, seed: f32) -> vec4<f32> {
    let so = seeded(seed);
    let taper = pow(max(1.0 - uv.x * uv.x, 0.0), 0.7);
    let wob = 0.12 * (vnoise(vec2<f32>(uv.x * 5.0, 1.0) + so) - 0.5);
    let y = abs(uv.y - wob);
    let cut = (1.0 - smoothstep(0.16 * taper, 0.30 * taper + 0.01, y)) * step(0.02, taper);
    let scrape = (1.0 - smoothstep(0.30 * taper, 0.85 * taper + 0.01, y)) * step(0.02, taper) * (1.0 - cut);
    var f = mix(vec3<f32>(1.0), vec3<f32>(1.16, 1.15, 1.12), scrape * 0.7);
    f = mix(f, vec3<f32>(0.10), cut);
    return vec4<f32>(f, 1.0);
}

/// Suie d'une explosion : noire au milieu, en rayons irréguliers jusqu'au bord, un anneau
/// de cendre grise.
fn scorch(uv: vec2<f32>, seed: f32) -> vec4<f32> {
    let so = seeded(seed);
    let r = length(uv);
    let dir = uv / max(r, 1e-4);
    let rays = vnoise(dir * 3.0 + so);
    let reach = 0.62 + 0.33 * rays;
    // Le cœur calciné, puis la suie soufflée en traînées qui s'allongent par endroits.
    let core = 1.0 - smoothstep(0.10, 0.20 + 0.26 * reach, r);
    let streaks = pow(vnoise(dir * 13.0 + so * 3.1), 1.6);
    let blown = (1.0 - smoothstep(0.15, reach, r)) * (0.30 + 0.70 * streaks);
    let grain = 0.80 + 0.20 * fbm(uv * 9.0 + so);
    let soot = clamp(max(core, blown) * grain, 0.0, 1.0);
    // Un liseré de cendre grise autour du cœur.
    let ash = (1.0 - smoothstep(0.0, 0.10, abs(r - (0.22 + 0.26 * reach)))) * 0.10 * fbm(uv * 14.0 + so);
    let f = mix(vec3<f32>(1.0), vec3<f32>(0.09, 0.085, 0.08), soot) + vec3<f32>(ash);
    return vec4<f32>(f, 1.0 - smoothstep(0.9, 1.0, r));
}

/// Le feu s'arrête au bord de la surface qui a brûlé (les côtés sans case voisine brûlée,
/// bits : 1 +x, 2 −x, 4 +z, 8 −z, comme les flammes) : 1 au cœur, 0 au bord. `w` : place
/// dans la case, de −1 à 1 le long de x et de z.
fn inside(w: vec2<f32>, sides: u32) -> f32 {
    var e = 1.0;
    if (sides & 1u) == 0u { e = min(e, 1.0 - w.x); }
    if (sides & 2u) == 0u { e = min(e, 1.0 + w.x); }
    if (sides & 4u) == 0u { e = min(e, 1.0 - w.y); }
    if (sides & 8u) == 0u { e = min(e, 1.0 + w.y); }
    return clamp(e, 0.0, 1.0);
}

/// Sol brûlé par le molotov : il noircit par plaques pendant que le feu brûle (cinq
/// secondes), plus fort au cœur de la surface en feu. Une trace par case, à plat, son axe u
/// le long de x (donc v le long de −z). `at` : le milieu de la case dans le monde (les
/// plaques passent d'une case à sa voisine sans couture).
fn burn(uv: vec2<f32>, at: vec2<f32>, sides: u32, age: f32) -> vec4<f32> {
    let w = vec2<f32>(uv.x, -uv.y);
    let p = at + w * 0.5;
    let edge = inside(w, sides);
    let patches = fbm(p * 2.6);
    let grown = smoothstep(0.0, 5.0, age);
    let char = clamp(smoothstep(0.0, 0.5, edge + (patches - 0.5) * 0.7) * (0.55 + 0.6 * patches), 0.0, 1.0) * grown;
    let f = mix(vec3<f32>(1.0), vec3<f32>(0.11, 0.10, 0.095), char * 0.95);
    return vec4<f32>(f, 1.0);
}

// ---------------------------------------------------------------- dessin

/// Ce que la trace laisse de l'image en `uv` (−1 … 1 dans la trace) : le facteur par
/// couleur (rgb), écrit pour le mélange `image × (rgb + 1 − a)`. `seen` : part visible
/// (0 … 1 : hors de la boîte, surface de travers, loin).
fn trace(i: VOut, uv: vec2<f32>, seen: f32) -> vec4<f32> {
    // Hors de la trace (le pixel montre autre chose, devant ou derrière sa boîte) : l'image
    // reste telle quelle, sans rien calculer.
    if seen <= 0.0 {
        return vec4<f32>(0.0);
    }
    let kind = floor(i.at.w + 0.01);
    let seed = i.nrm.w;
    let age = du.now.x - i.tan.w;
    var f = vec3<f32>(1.0);
    var cov = 0.0;
    if kind == KIND_SPLAT || kind == KIND_SPRAY || kind == KIND_DRIP {
        var b = vec2<f32>(0.0);
        if kind == KIND_SPLAT {
            b = splat(uv, seed);
        } else if kind == KIND_SPRAY {
            b = spray(uv, seed);
        } else {
            b = drip(uv, seed, age, i.size.y / max(i.size.x, 1e-4));
        }
        f = blood_tint(b.y, age);
        // Rien ne touche le bord de la boîte.
        cov = b.x * (0.80 + 0.20 * b.y) * (1.0 - smoothstep(0.9, 1.0, max(abs(uv.x), abs(uv.y))));
    } else if kind == KIND_HOLE {
        // (v264) La matière touchée (en seizièmes après la sorte), et d'où vient la lumière
        // dans le repère de la trace (le soleil, toujours un peu d'en haut : le relief reste
        // lisible la nuit et à l'intérieur).
        let mat = u32(fract(i.at.w) * 16.0 + 0.5);
        let lw = normalize(g.sun.xyz * 0.7 + vec3<f32>(0.0, 0.8, 0.0));
        let t3 = i.tan.xyz;
        let s2 = vec2<f32>(dot(lw, t3), dot(lw, cross(i.nrm.xyz, t3)));
        let h = hole(uv, seed, mat, s2 / max(length(s2), 0.4), i.size.x / max(i.size.y, 1e-4));
        f = h.rgb;
        cov = h.a;
    } else if kind == KIND_SLASH {
        let h = slash(uv, seed);
        f = h.rgb;
        cov = h.a;
    } else if kind == KIND_SCORCH {
        let h = scorch(uv, seed);
        f = h.rgb;
        cov = h.a;
    } else if kind == KIND_BURN {
        let sides = u32(fract(i.at.w) * 16.0 + 0.5);
        let h = burn(uv, i.at.xz, sides, age);
        f = h.rgb;
        cov = h.a;
    }
    // Impacts, suie, brûlures : leurs facteurs sont dits en clarté vue à l'écran (0,1 : dix
    // fois plus sombre à l'œil) ; l'image, elle, est mélangée en lumière (linéaire).
    if kind >= KIND_HOLE {
        f = pow(max(f, vec3<f32>(0.0)), vec3<f32>(2.2));
    }
    let k = clamp(cov * seen * i.size.w * i.fade, 0.0, 1.0);
    let fm = mix(vec3<f32>(1.0), f, k);
    let m = clamp(min(fm.r, min(fm.g, fm.b)), 0.0, 1.0);
    return vec4<f32>(fm - vec3<f32>(m), 1.0 - m);
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return trace(i, i.uv, 1.0);
}

@fragment
fn fs_post(i: VOut) -> @location(0) vec4<f32> {
    // Le point de la scène sous ce pixel : sur le rayon de l'œil au fond de la boîte, à la
    // distance que dit la profondeur du monde (inversée, sans plan lointain : elle vaut
    // l'inverse de la distance en avant, à un facteur près ; aucune matrice inverse).
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp((i.ndc * vec2<f32>(0.5, -0.5) + 0.5) * dims, vec2<f32>(0.0), dims - 1.0);
    let depth = max(textureLoad(scene_depth, vec2<i32>(px), 0), 1e-7);
    // (v264) Rectangle à plat (vitre) : il se voit si rien d'opaque n'est devant lui (la
    // profondeur est inversée : plus grande, plus près).
    if i.plat > 0.5 {
        return trace(i, i.uv, step(depth, i.clip.z * 1.002));
    }
    // (v254) Depuis la caméra (exact loin du centre du monde ; ses dérivées aussi).
    let o = g.cam_pos.xyz;
    let p = i.rel * (i.clip.z / depth);
    // Sa place dans la trace.
    let n = i.nrm.xyz;
    let u = i.tan.xyz;
    let q = p - (i.at.xyz - o);
    let l = vec3<f32>(dot(q, u) / i.size.x, dot(q, cross(n, u)) / i.size.y, dot(q, n) / i.size.z);
    let within = step(max(abs(l.x), max(abs(l.y), abs(l.z))), 1.0);
    // La surface regarde-t-elle du côté de la trace ? (Sinon : le flanc d'une marche, un mur
    // qui traverse la boîte : la trace s'y étirerait.)
    let c = cross(dpdx(p), dpdy(p));
    let len = length(c);
    let facing = select(1.0, abs(dot(c, n)) / max(len, 1e-20), len > 1e-20);
    return trace(i, l.xy, within * smoothstep(0.30, 0.55, facing));
}
