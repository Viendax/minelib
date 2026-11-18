
// ---------------------------------------------------------------- lumières dynamiques
// Lumières placées par le joueur (ponctuelles et projecteurs) : éclairage des surfaces,
// ombres (atlas calculé dans la grille de blocs, voir lights.rs et shadow_bake.wgsl) et
// motifs projetés (gobos). Ce fichier est ajouté à la suite de world.wgsl, entity.wgsl et
// volumetric.wgsl (liaisons 7 à 13 du groupe 0, les mêmes partout).
// Modèle d'éclairage d'après IRLights (MIT, qualet), réécrit pour Minelib.

struct DynLight {
    // xyz = position (monde), w = portée (blocs).
    pos_range: vec4<f32>,
    // rgb = couleur linéaire × intensité, w = intensité.
    color: vec4<f32>,
    // xyz = direction du projecteur (unitaire), w = 0 ponctuelle, 1 projecteur.
    dir_type: vec4<f32>,
    // x = cos du demi-angle extérieur, y = cos du demi-angle intérieur, z = masque (0 tout,
    // 1 entités seulement, 2 blocs seulement), w = taille de la source (douceur des ombres ;
    // 0 : réglage général).
    cone: vec4<f32>,
    // x = anisotropie (g de Henyey-Greenstein), y = densité, z = force des rayons,
    // w = première case d'ombre dans l'atlas (-1 : aucune).
    vol: vec4<f32>,
    // x = couche du gobo (-1 : aucun), y = rotation (rad), z = échelle, w = 1 si inversé.
    cookie: vec4<f32>,
    // x = carte d'ombre (0 aucune, 1 projecteur, 2 cube de 6 cases), y = tangente du
    // demi-angle de la carte (1 pour un cube).
    shadow: vec4<f32>,
    // x, y = boîtes des joueurs à portée (bits 0..63) : leurs ombres.
    boxes: vec4<u32>,
};

struct DynHeader {
    // x = nombre de lumières, y = tuiles en largeur, z = en hauteur, w = mots par tuile.
    counts: vec4<u32>,
    // Rayons : x = intensité, y = distance max, z = lueur à la source, w = rayon de la lueur.
    vl_a: vec4<f32>,
    // Bruit des rayons : x = part, y = taille (blocs), z = vitesse, w = temps (s).
    vl_b: vec4<f32>,
    // x = pas max, y = pas entre deux tests d'ombre, z = pas entre deux bruits, w = drapeaux
    // (1 ombres des rayons, 2 bruit, 4 tramage tournant, 8 ombres des surfaces, 16 bandes,
    // 32 reflets, 64 rayons, 128 ombres des joueurs).
    vl_c: vec4<u32>,
    // x = douceur des ombres par défaut, y = taille de l'atlas (px), z = taille d'une case,
    // w = cases par ligne.
    sh: vec4<f32>,
    // x = intensité générale, y = force des reflets, z = nombre de bandes, w = douceur.
    surf: vec4<f32>,
    // x = numéro de l'image (tramage), y = métamorphose du bruit, z = nombre de boîtes.
    misc: vec4<f32>,
    box_min: array<vec4<f32>, 48>,
    box_max: array<vec4<f32>, 48>,
};

@group(0) @binding(7) var<uniform> dh: DynHeader;
@group(0) @binding(8) var<storage, read> dlights: array<DynLight>;
@group(0) @binding(9) var<storage, read> dtiles: array<u32>;
@group(0) @binding(10) var shadow_atlas: texture_depth_2d;
@group(0) @binding(11) var shadow_cmp: sampler_comparison;
@group(0) @binding(12) var gobos: texture_2d_array<f32>;
@group(0) @binding(13) var gobo_sampler: sampler;

const DL_VL_SHADOWS: u32 = 1u;
const DL_VL_NOISE: u32 = 2u;
const DL_DITHER: u32 = 4u;
const DL_SHADOWS: u32 = 8u;
const DL_TOON: u32 = 16u;
const DL_SPECULAR: u32 = 32u;
const DL_VOLUMETRIC: u32 = 64u;
const DL_BOX_SHADOWS: u32 = 128u;

/// Premier mot du masque des lumières de la tuile qui contient le pixel `frag` (pixels,
/// origine en haut à gauche) d'une image de `size` pixels.
fn dl_tile_base(frag: vec2<f32>, size: vec2<f32>) -> u32 {
    let gx = dh.counts.y;
    let gy = dh.counts.z;
    let tx = min(u32(max(frag.x, 0.0) / max(size.x, 1.0) * f32(gx)), gx - 1u);
    let ty = min(u32(max(frag.y, 0.0) / max(size.y, 1.0) * f32(gy)), gy - 1u);
    return (ty * gx + tx) * dh.counts.w;
}

/// Bruit par pixel (interleaved gradient noise), décalé à chaque image : le TAA lisse.
fn dl_ign(frag: vec2<f32>) -> f32 {
    let f = frag + vec2<f32>(5.588238, 5.588238) * (dh.misc.x % 64.0);
    return fract(52.9829189 * fract(0.06711056 * f.x + 0.00583715 * f.y));
}

/// Point `i` sur `n` d'un disque de Vogel (rayon 1), tourné de `phi`.
fn dl_vogel(i: u32, n: u32, phi: f32) -> vec2<f32> {
    let r = sqrt((f32(i) + 0.5) / f32(n));
    let t = f32(i) * 2.39996323 + phi;
    return vec2<f32>(cos(t), sin(t)) * r;
}

/// Repère d'un projecteur dirigé selon `d` : (droite, haut, avant). Le même pour l'ombre,
/// le gobo et le calcul de la carte d'ombre.
fn dl_spot_basis(d: vec3<f32>) -> mat3x3<f32> {
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(d.y) > 0.99);
    let s = normalize(cross(d, up));
    let u = cross(s, d);
    return mat3x3<f32>(s, u, d);
}

/// Face du cube (0 +X, 1 −X, 2 +Y, 3 −Y, 4 +Z, 5 −Z) que regarde la direction `d`.
fn dl_cube_face(d: vec3<f32>) -> u32 {
    let a = abs(d);
    if a.x >= a.y && a.x >= a.z {
        return select(1u, 0u, d.x > 0.0);
    }
    if a.y >= a.z {
        return select(3u, 2u, d.y > 0.0);
    }
    return select(5u, 4u, d.z > 0.0);
}

/// Repère d'une face du cube : (droite, haut, avant).
fn dl_face_basis(f: u32) -> mat3x3<f32> {
    switch f {
        case 0u: { return mat3x3<f32>(vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0)); }
        case 1u: { return mat3x3<f32>(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(-1.0, 0.0, 0.0)); }
        case 2u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, -1.0), vec3<f32>(0.0, 1.0, 0.0)); }
        case 3u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, -1.0, 0.0)); }
        case 4u: { return mat3x3<f32>(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, 1.0)); }
        default: { return mat3x3<f32>(vec3<f32>(-1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(0.0, 0.0, -1.0)); }
    }
}

/// Gobo : part de la lumière qui passe le motif projeté (1 sans gobo). Le motif occupe le
/// cône du projecteur (échelle 1), haut de l'image vers le haut du repère ; hors de l'image :
/// noir (blanc s'il est inversé).
fn dl_cookie(p: vec3<f32>, L: DynLight) -> f32 {
    let layer = L.cookie.x;
    if layer < 0.0 {
        return 1.0;
    }
    let b = dl_spot_basis(normalize(L.dir_type.xyz));
    let to = p - L.pos_range.xyz;
    let z = dot(b[2], to);
    if z < 0.001 {
        return 1.0;
    }
    // 1 / tan(demi-angle) : le bord du cône tombe au bord de l'image.
    let c = clamp(L.cone.x, -1.0, 1.0);
    let fy = min(c * inverseSqrt(max(1.0 - c * c, 1e-12)), 114.58865);
    var q = vec2<f32>(dot(b[0], to), dot(b[1], to)) * (fy / z) / max(L.cookie.z, 1e-3);
    let cr = cos(L.cookie.y);
    let sr = sin(L.cookie.y);
    q = vec2<f32>(cr * q.x - sr * q.y, sr * q.x + cr * q.y);
    let uv = vec2<f32>(q.x * 0.5 + 0.5, 0.5 - q.y * 0.5);
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    var m = textureSampleLevel(gobos, gobo_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), i32(layer), 0.0).r;
    m = select(0.0, m, inside);
    return select(m, 1.0 - m, L.cookie.w > 0.5);
}

/// Coin (xy) et taille (z) en uv d'une case de l'atlas des ombres.
fn dl_tile_rect(tile: f32) -> vec3<f32> {
    let per = max(dh.sh.w, 1.0);
    let t = floor(tile + 0.5);
    let row = floor(t / per);
    let col = t - row * per;
    let size = dh.sh.z / max(dh.sh.y, 1.0);
    return vec3<f32>(col * size, row * size, size);
}

/// Position dans l'atlas (uv) du texel qui voit la direction `d` depuis la lumière ;
/// z = 1 si la direction est dans la carte (un projecteur ne voit que son cône).
fn dl_shadow_uv(L: DynLight, d: vec3<f32>) -> vec3<f32> {
    var b: mat3x3<f32>;
    var tile = L.vol.w;
    if L.shadow.x > 1.5 {
        let f = dl_cube_face(d);
        b = dl_face_basis(f);
        tile = tile + f32(f);
    } else {
        b = dl_spot_basis(normalize(L.dir_type.xyz));
    }
    let fz = dot(b[2], d);
    let ndc = vec2<f32>(dot(b[0], d), dot(b[1], d)) / max(fz * L.shadow.y, 1e-6);
    let ok = fz > 1e-5 && all(abs(ndc) <= vec2<f32>(1.0));
    let rect = dl_tile_rect(tile);
    // Un demi-texel à l'intérieur de la case : le filtrage ne lit jamais la case voisine.
    let px = 1.0 / max(dh.sh.y, 1.0);
    let local = clamp(
        vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * rect.z,
        vec2<f32>(px * 0.5),
        vec2<f32>(rect.z - px * 0.5),
    );
    return vec3<f32>(rect.xy + local, select(0.0, 1.0, ok));
}

/// Visibilité (0 dans l'ombre … 1 éclairé) de la lumière `L` en `p`, face de normale `n`
/// tournée vers elle. Ombres douces (PCSS) : recherche des bloqueurs, puis filtrage dont la
/// largeur grandit avec la taille de la source et la distance au bloqueur. Chaque échantillon
/// est comparé à la distance du plan de la face dans sa direction (pas d'acné sur les faces
/// des blocs, même rasantes). `noise` : rotation du disque (lissée par le TAA) ; `soft` :
/// faux pour un apport trop faible pour que la pénombre se voie (un seul échantillon).
fn dl_shadow(L: DynLight, p: vec3<f32>, n: vec3<f32>, noise: f32, soft: bool) -> f32 {
    let range = max(L.pos_range.w, 0.01);
    let lp = L.pos_range.xyz;
    let d0 = length(p - lp);
    // Taille d'un texel de la carte à cette distance (blocs).
    let texel = 2.0 * L.shadow.y * d0 / max(dh.sh.z, 1.0);
    let q = p + n * (0.01 + 1.2 * texel);
    let to = q - lp;
    let dist = max(length(to), 1e-4);
    let dir = to / dist;
    let ndt = dot(n, to);
    let center = dl_shadow_uv(L, dir);
    if center.z < 0.5 {
        return 1.0;
    }
    let eps = 2e-4;
    let bulb = select(dh.sh.x, L.cone.w, L.cone.w > 0.0);
    if bulb < 0.01 || !soft {
        return textureSampleCompareLevel(shadow_atlas, shadow_cmp, center.xy, dist / range - eps);
    }
    // Repère autour de la direction centrale (échantillons en angle autour d'elle).
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(dir.y) > 0.9);
    let t1 = normalize(cross(dir, up));
    let t2 = cross(dir, t1);
    let ang_texel = 2.0 * L.shadow.y / max(dh.sh.z, 1.0);
    let phi = noise * 6.2831853;
    // 1. Bloqueurs : distance moyenne de ceux qui cachent la source (bloqueur supposé à
    // mi-chemin pour la zone de recherche).
    let search = clamp(bulb / max(dist, 0.25), 1.5 * ang_texel, 0.45);
    var sum = 0.0;
    var cnt = 0.0;
    for (var k = 0u; k < 8u; k++) {
        let o = dl_vogel(k, 8u, phi) * search;
        let dk = normalize(dir + t1 * o.x + t2 * o.y);
        let s = dl_shadow_uv(L, dk);
        let nd = dot(n, dk);
        let tk = select(dist, clamp(ndt / nd, 0.0, range * 2.0), nd < -1e-3);
        let z = textureLoad(shadow_atlas, vec2<i32>(s.xy * dh.sh.y), 0);
        let blocked = s.z > 0.5 && z < tk / range - eps;
        sum += select(0.0, z, blocked);
        cnt += select(0.0, 1.0, blocked);
    }
    if cnt < 0.5 {
        return 1.0;
    }
    let db = max(sum / cnt * range, 0.05);
    // 2. Pénombre : largeur (blocs) au niveau de la face, ramenée en angle.
    let pen = bulb * max(dist - db, 0.0) / db;
    let filt = clamp(0.5 * pen / dist, ang_texel, 0.45);
    var vis = 0.0;
    for (var k = 0u; k < 12u; k++) {
        let o = dl_vogel(k, 12u, phi + 1.7) * filt;
        let dk = normalize(dir + t1 * o.x + t2 * o.y);
        let s = dl_shadow_uv(L, dk);
        let nd = dot(n, dk);
        let tk = select(dist, clamp(ndt / nd, 0.0, range * 2.0), nd < -1e-3);
        let c = textureSampleCompareLevel(shadow_atlas, shadow_cmp, s.xy, tk / range - eps);
        vis += select(1.0, c, s.z > 0.5);
    }
    return vis / 12.0;
}

/// Ombre d'un joueur (boîtes à portée de la lumière) sur le trajet `p` → lumière : 1 si
/// caché, 0 sinon.
fn dl_box_shadow(L: DynLight, p: vec3<f32>) -> f32 {
    let to_l = L.pos_range.xyz - p;
    let inv = 1.0 / select(to_l, vec3<f32>(1e-6), abs(to_l) < vec3<f32>(1e-6));
    var hit = 0.0;
    for (var w = 0u; w < 2u; w++) {
        var bits = select(L.boxes.y, L.boxes.x, w == 0u);
        loop {
            if bits == 0u || hit > 0.5 {
                break;
            }
            let b = firstTrailingBit(bits);
            bits = bits & (bits - 1u);
            let k = w * 32u + b;
            let mn = dh.box_min[k].xyz;
            let mx = dh.box_max[k].xyz;
            // Point dans la boîte (pieds du joueur) : pas d'ombre sur lui-même.
            let inside = all(p > mn) && all(p < mx);
            let t0 = (mn - p) * inv;
            let t1 = (mx - p) * inv;
            let near = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
            let far = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
            if !inside && far >= max(near, 0.0) && near < 1.0 {
                hit = 1.0;
            }
        }
    }
    return hit;
}

/// Éclairage en bandes (style dessin animé).
fn dl_toon(x: f32, bands: f32, w: f32) -> f32 {
    let v = clamp(x, 0.0, 1.0) * max(bands, 1.0);
    let band = floor(v);
    let fr = v - band;
    let t = select(smoothstep(1.0 - w, 1.0, fr), 0.0, w <= 1e-4);
    return (band + t) / max(bands, 1.0);
}

struct DynOut {
    diffuse: vec3<f32>,
    specular: vec3<f32>,
};

/// Des lumières à dessiner pour cette image ?
fn dyn_active() -> bool {
    return dh.counts.x > 0u;
}

/// Lumière reçue en `p` (normale `n`) des lumières dynamiques, vue depuis `cam`.
/// `gloss` : 0 mat … 1 miroir ; `entity` : surface d'une entité (masques « entités
/// seulement » / « blocs seulement ») ; `frag`, `size` : pixel et taille de l'image.
fn dyn_light(
    p: vec3<f32>,
    n: vec3<f32>,
    cam: vec3<f32>,
    gloss: f32,
    entity: bool,
    frag: vec2<f32>,
    size: vec2<f32>,
) -> DynOut {
    var o = DynOut(vec3<f32>(0.0), vec3<f32>(0.0));
    let count = dh.counts.x;
    if count == 0u {
        return o;
    }
    let base = dl_tile_base(frag, size);
    let words = min(dh.counts.w, 8u);
    let flags = dh.vl_c.w;
    let shadows_on = (flags & DL_SHADOWS) != 0u;
    let boxes_on = (flags & DL_BOX_SHADOWS) != 0u;
    let toon = (flags & DL_TOON) != 0u;
    let spec_k = select(0.0, dh.surf.y, (flags & DL_SPECULAR) != 0u);
    let v = normalize(cam - p);
    let noise = dl_ign(frag);
    for (var w = 0u; w < words; w++) {
        var bits = dtiles[base + w];
        loop {
            if bits == 0u {
                break;
            }
            let b = firstTrailingBit(bits);
            bits = bits & (bits - 1u);
            let L = dlights[min(w * 32u + b, count - 1u)];
            let to_l = L.pos_range.xyz - p;
            let d2 = dot(to_l, to_l);
            let r = L.pos_range.w;
            let mask = u32(L.cone.z + 0.5);
            let skip = (mask == 1u && !entity) || (mask == 2u && entity);
            if d2 < r * r && d2 > 1e-8 && !skip {
                let d = sqrt(d2);
                let l = to_l / d;
                let ndl = max(dot(n, l), 0.0);
                // Fenêtre radiale de Frostbite : (1 − (d/r)⁴)².
                let dr = d / r;
                let dr2 = dr * dr;
                let fall = max(1.0 - dr2 * dr2, 0.0);
                var att = fall * fall;
                if L.dir_type.w > 0.5 {
                    let theta = dot(-l, L.dir_type.xyz);
                    att *= clamp((theta - L.cone.x) / max(L.cone.y - L.cone.x, 1e-6), 0.0, 1.0);
                    if att > 0.0 {
                        att *= dl_cookie(p, L);
                    }
                }
                if att > 0.0 && ndl > 0.0 {
                    if shadows_on && L.shadow.x > 0.5 {
                        att *= dl_shadow(L, p, n, noise, att * L.color.w > 0.003);
                    }
                    if boxes_on && att > 0.0 && (L.boxes.x | L.boxes.y) != 0u {
                        att *= 1.0 - dl_box_shadow(L, p + n * 0.02);
                    }
                    var diff = att * ndl;
                    if toon {
                        diff = dl_toon(diff, dh.surf.z, dh.surf.w);
                    }
                    o.diffuse += L.color.rgb * diff;
                    if spec_k > 0.0 {
                        // Blinn-Phong normalisé : tache nette sur un miroir, large et faible
                        // sur un bloc mat.
                        let h = normalize(l + v);
                        let shin = mix(24.0, 1024.0, gloss * gloss);
                        let s = pow(max(dot(n, h), 0.0), shin) * (shin + 8.0) / 25.1327;
                        o.specular += L.color.rgb * (att * ndl * s * mix(0.02, 1.0, gloss) * spec_k);
                    }
                }
            }
        }
    }
    o.diffuse *= dh.surf.x;
    o.specular *= dh.surf.x;
    return o;
}
