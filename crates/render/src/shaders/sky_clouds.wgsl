// Minelib — (v216) carte des nuages : tout le ciel vu en œil de poisson (le zénith au
// centre, l'horizon au bord), dessiné quelques rangées par image (voir sky.rs).
//
// Chaque pixel de la carte lance un rayon à travers la couche de nuages (une coquille
// autour d'une planète de 20 000 blocs de rayon : les nuages de l'horizon sont à une
// distance finie) et y avance pas à pas dans un volume de bruit. La lumière du soleil ou de
// la lune y est suivie sur quelques pas vers la source (ombres propres), avec la diffusion
// vers l'avant qui fait briller les bords à contre-jour.
// Sortie : rgb = lumière des nuages (déjà pesée par leur opacité), a = part du ciel qui
// passe derrière eux.

struct CloudParams {
    // xz = caméra dans le repère des nuages (blocs, modulo 8192), y = horloge du ciel (s)
    origin: vec4<f32>,
    // Direction vers la lumière (soleil ou lune)
    light_dir: vec4<f32>,
    // Lumière directe au niveau des nuages
    light_col: vec4<f32>,
    // Lumière du ciel au-dessus et en dessous
    amb_top: vec4<f32>,
    amb_bot: vec4<f32>,
    // x = seuil du bruit (au-dessus : nuage), y = densité (par bloc), z = épaisseur de la
    // couche (blocs), w = orage (0 … 1)
    shape: vec4<f32>,
    // x = rayon de la caméra, y = rayon de la base des nuages, z = angle du bord de la
    // carte (rad), w = côté de la carte (px)
    geo: vec4<f32>,
    // rgb = gris du ciel couvert (la brume des nuages lointains y tend), w = ciel couvert
    // (0 beau temps … 1 couvert)
    haze: vec4<f32>,
};

@group(0) @binding(0) var<uniform> c: CloudParams;
@group(0) @binding(1) var noise: texture_3d<f32>;
@group(0) @binding(2) var noise_s: sampler;
@group(0) @binding(3) var sky_cube: texture_cube<f32>;
@group(0) @binding(4) var sky_s: sampler;

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

const STEPS: i32 = 56;
const LIGHT_STEPS: i32 = 5;

/// Formes des nuages en `q` (xz : blocs dans le repère des nuages, y : hauteur au-dessus
/// de la base), à la hauteur `h` dans la couche (0 base … 1 sommet). 0 : pas de nuage.
/// `wide` : avec les grandes régions plus ou moins chargées (une lecture de plus).
fn shape(q: vec3<f32>, h: f32, wide: bool) -> f32 {
    var thr = c.shape.x;
    var load = 0.5;
    if wide {
        load = textureSampleLevel(noise, noise_s, vec3<f32>(q.x, 2500.0, q.z) / 8192.0, 0.0).a;
        thr -= (load - 0.5) * 0.3;
    }
    // Les formes « bouillonnent » lentement : le bruit glisse vers le haut avec le temps.
    let evo = c.origin.y * 0.0011;
    let w = textureSampleLevel(noise, noise_s, vec3<f32>(q.x, q.y * 1.3, q.z) / 2048.0 + vec3<f32>(0.0, evo, 0.0), 0.0);
    // Profil : base plate et nette, sommet arrondi, plus haut là où le ciel est chargé
    // (tours des orages).
    let top = mix(0.42 + 0.55 * w.a, 1.0, max(c.shape.w, smoothstep(0.55, 0.8, load) * 0.5));
    // Ciel couvert : le dessous de la couche n'est pas un plafond lisse (il remonte là où
    // le bruit est faible), et la couche reste plus ou moins dense selon le bruit : des
    // masses sombres, des trouées plus claires.
    // (v217) Ce dessous ondule doucement et s'effiloche : il n'a ni marches ni parois (elles
    // découpaient le ciel d'orage en aplats gris aux bords nets).
    let over = c.haze.w;
    let lo = over * 0.13 * clamp(1.2 - 1.4 * w.r, 0.0, 1.0);
    let profile = smoothstep(lo, lo + mix(0.06, 0.24, over), h) * (1.0 - smoothstep(top * 0.5, top, h));
    let base = clamp((w.r * profile - thr) * 3.0, 0.0, 1.0);
    return base * mix(1.0, 0.35 + 1.1 * smoothstep(0.45, 0.95, w.r), over);
}

/// Bords des nuages rongés par un bruit plus fin : effilochés en bas (on creuse le cœur
/// des cellules du bruit, il reste des filaments), bourgeonnants en haut (on creuse entre
/// les cellules, il reste des bosses rondes : la « choucroute » des cumulus).
fn erode(q: vec3<f32>, h: f32, base: f32) -> f32 {
    let evo = c.origin.y * 0.004;
    let a = textureSampleLevel(noise, noise_s, q / 420.0 + vec3<f32>(0.0, evo, 0.0), 0.0).g;
    let b = textureSampleLevel(noise, noise_s, q / 150.0 - vec3<f32>(0.0, evo * 2.0, 0.0), 0.0).b;
    let fine = a * 0.65 + b * 0.35;
    let bite = mix(fine, 1.0 - fine, clamp(h * 3.5, 0.0, 1.0)) * 0.62;
    // (La pente raide après coup : le bruit déplace le bord du nuage, puis le bord est net.)
    return clamp((base - bite) / (1.0 - bite) * 3.5, 0.0, 1.0);
}

/// Phase de Henyey-Greenstein × 4π (1 = lumière renvoyée également partout).
fn hg(cos_t: f32, g: f32) -> f32 {
    let k = 1.0 + g * g - 2.0 * g * cos_t;
    return (1.0 - g * g) / (k * sqrt(k));
}

/// Deux lobes : surtout vers l'avant (bords brillants à contre-jour), un peu vers l'arrière.
fn phase(cos_t: f32, k: f32) -> f32 {
    return mix(hg(cos_t, 0.62 * k), hg(cos_t, -0.38 * k), 0.32);
}

@fragment
fn fs_clouds(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let v = (pos.xy / c.geo.w - 0.5) * 2.0;
    let r = length(v);
    let clear = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if r > 1.0 {
        return clear;
    }
    let th = r * c.geo.z;
    let hz = v / max(r, 1e-6);
    let d = vec3<f32>(sin(th) * hz.x, cos(th), sin(th) * hz.y);
    let rc = c.geo.x;
    let r0 = c.geo.y;
    let thick = c.shape.z;
    let r1 = r0 + thick;
    // Entrée et sortie de la couche (la caméra est dessous).
    let b = d.y * rc;
    let disc = b * b - rc * rc;
    let s0 = -b + sqrt(max(disc + r0 * r0, 0.0));
    let s1 = -b + sqrt(max(disc + r1 * r1, 0.0));
    if d.y < -0.03 || s0 > 7500.0 {
        return clear;
    }
    // Près de l'horizon le rayon reste très longtemps dans la couche : on s'arrête après
    // 2 600 blocs (opaque bien avant, ou noyé dans la brume).
    let span = min(s1 - s0, 2600.0);
    let ds = span / f32(STEPS);
    // Départ décalé d'un pixel à l'autre : les pas ne font pas de bandes.
    let jitter = fract(52.9829189 * fract(dot(pos.xy, vec2<f32>(0.06711056, 0.00583715))));
    let light = normalize(c.light_dir.xyz);
    let cos_t = dot(d, light);
    // (v217) Chemin qui reste dans la couche vers la lumière après la marche des ombres,
    // en gros (blocs) : très long quand elle est basse.
    let rest = min(thick * 0.6 / max(abs(light.y), 0.03), 3000.0);
    // Lumière qui arrive par en dessous (soleil ou lune sous l'horizon de la couche) : la
    // planète la cache, elle s'éteint en passant sous l'horizon ; et sous un ciel couvert
    // il n'y a pas de bord par où une lumière basse entrerait.
    let below = smoothstep(-0.03, 0.05, light.y) * (1.0 - c.haze.w * (1.0 - smoothstep(0.05, 0.35, light.y)));
    // Diffusion multiple approchée : trois « octaves », de plus en plus étalées et de moins
    // en moins absorbées.
    let p0 = phase(cos_t, 1.0);
    let p1 = phase(cos_t, 0.5) * 0.45;
    let p2 = phase(cos_t, 0.25) * 0.16;
    var through = 1.0;
    var lum = vec3<f32>(0.0);
    var far = 0.0;
    var weight = 0.0;
    for (var k = 0; k < STEPS; k++) {
        let s = s0 + (f32(k) + jitter) * ds;
        let p = d * s;
        let alt = length(vec3<f32>(p.x, p.y + rc, p.z)) - r0;
        let h = alt / thick;
        if h <= 0.0 || h >= 1.0 {
            continue;
        }
        let q = vec3<f32>(c.origin.x + p.x, alt, c.origin.z + p.z);
        let base = shape(q, h, true);
        if base <= 0.0 {
            continue;
        }
        let e = erode(q, h, base);
        let dens = e * c.shape.y;
        if dens <= 1e-4 {
            continue;
        }
        // Ombre propre : épaisseur de nuage entre ce point et la lumière.
        var od = 0.0;
        var ls = 12.0;
        var lq = q;
        for (var j = 0; j < LIGHT_STEPS; j++) {
            lq += light * ls;
            let lh = lq.y / thick;
            if lh >= 1.0 || lh <= 0.0 {
                break;
            }
            let lb = shape(lq, lh, false);
            if j < 2 {
                // Tout près : avec les bourgeons (chacun fait de l'ombre à son voisin).
                od += erode(lq, lh, lb) * c.shape.y * ls;
            } else {
                // Plus loin : leur creux moyen à la place (deux lectures de moins).
                od += clamp((lb - 0.31) * 5.0, 0.0, 1.0) * c.shape.y * ls;
            }
            ls *= 1.8;
        }
        // (v217) La marche s'arrête à 270 blocs : sous un ciel couvert, la couche continue
        // bien au-delà. Sans ce reste, un soleil bas éclairait de biais le dessous des
        // nuages d'orage (des traînées beiges, en pointillé d'un pixel à l'autre).
        od += c.shape.y * 0.5 * c.haze.w * c.haze.w * rest;
        // Dans les creux et sur les bords (peu denses), moins de lumière a été diffusée
        // vers nous que dans le cœur d'un bourgeon : c'est ce qui dessine la « choucroute ».
        let crease = 0.35 + 0.65 * pow(e, mix(0.4, 1.4, smoothstep(0.15, 0.9, h)));
        // Sous une couche très épaisse, la lumière arrive encore, diffusée des centaines de
        // fois (elle décroît lentement avec l'épaisseur) : le gris des ciels de pluie, plus
        // sombre sous les masses épaisses.
        let deep = 0.22 * (1.0 - exp(-od * 0.5)) / (1.0 + 0.10 * od);
        let direct = c.light_col.rgb * below
            * (crease * (exp(-od) * p0 + exp(-od * 0.35) * p1 + exp(-od * 0.12) * p2) + deep);
        // Lumière du ciel : plus forte en haut du nuage, celle du sol en dessous.
        var amb = mix(c.amb_bot.rgb, c.amb_top.rgb, pow(h, 0.7) * (1.0 - 0.75 * c.haze.w)) * (0.55 + 0.45 * exp(-dens * (1.0 - h) * thick * 0.6));
        // (v217) Sous un ciel couvert, la lumière vient d'en haut à travers la couche : le
        // dessous est plus clair là où elle est mince, sombre sous ses masses épaisses, en
        // dégradés doux (avant : un gris uni).
        let load = textureSampleLevel(noise, noise_s, vec3<f32>(q.x, 900.0, q.z) / 3100.0, 0.0).a;
        amb *= mix(1.0, mix(1.38, 0.5, smoothstep(0.3, 0.72, load * 0.55 + base * 0.45)), c.haze.w);
        let a = 1.0 - exp(-dens * ds);
        lum += through * a * (direct + amb);
        far += through * a * s;
        weight += through * a;
        through *= 1.0 - a;
        if through < 0.01 {
            through = 0.0;
            break;
        }
    }
    // Lumière → couleur affichée : même courbe que le ciel (les parties au soleil tendent
    // vers le blanc sans se brûler, les ombres gardent leur modelé).
    let opacity = 1.0 - through;
    if opacity > 1e-4 {
        let col = lum / opacity;
        // (En partie sur la luminance seule : les roses et les orangés du couchant gardent
        // leur teinte au lieu de blanchir.)
        let y = dot(col, vec3<f32>(0.2126, 0.7152, 0.0722));
        let keep = col * ((1.0 - exp(-y)) / max(y, 1e-5));
        lum = min(mix(1.0 - exp(-col), keep, 0.6), vec3<f32>(1.0)) * opacity;
    }
    // Brume : les nuages lointains prennent la couleur du ciel de l'horizon.
    let dist = select(s0, far / max(weight, 1e-5), weight > 0.0);
    let haze = 1.0 - exp(-dist / 3400.0);
    // (0,86 : comme `sky_color` de world.wgsl, le ciel couvert laisse passer un peu du ciel.)
    let sky = mix(textureSampleLevel(sky_cube, sky_s, d, 0.0).rgb, c.haze.rgb, c.haze.w * 0.86);
    lum = mix(lum, sky * opacity, haze);
    // Par beau temps, les nuages les plus lointains s'effacent en partie dans la brume :
    // l'horizon reste dégagé, on voit le soleil s'y coucher. (Pas sous un ciel couvert :
    // l'horizon y est bouché.)
    let fade = 0.65 * smoothstep(1400.0, 3200.0, dist) * (1.0 - c.haze.w);
    return vec4<f32>(lum * (1.0 - fade), mix(through, 1.0, fade));
}
