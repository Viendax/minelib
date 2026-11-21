
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
    // demi-angle de la carte (1 pour un cube), z = 1 : arrêt net par les murs seulement
    // (ombres douces décochées), w = 1 (v202) : lumière portée, bords en pente douce.
    shadow: vec4<f32>,
    // x, y = boîtes des joueurs à portée (bits 0..63) : leurs ombres ; z = distance (bits
    // d'un f32) en deçà de laquelle aucune ne peut cacher la lumière (voir `box_sphere`) ;
    // w = 1 (v220) : des joueurs à portée dans l'atlas des ombres des joueurs.
    boxes: vec4<u32>,
    // Calculés une fois pour toutes par le jeu (pas à chaque pixel) : repère du projecteur
    // (xyz : droite, haut ; right.w = 1 / tan du demi-angle, up.w et rect.w = cos et sin de
    // la rotation du gobo), case de sa carte d'ombre (rect.xyz : coin et taille en uv),
    // cône depuis la lumière qui contient les boîtes des joueurs (box_sphere : axe xyz,
    // cosinus du demi-angle en w ; −2 si la lumière est dans leur sphère).
    right: vec4<f32>,
    up: vec4<f32>,
    rect: vec4<f32>,
    box_sphere: vec4<f32>,
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
    // 32 reflets, 64 rayons, 128 ombres des joueurs, 256 éclairage en demi-résolution).
    vl_c: vec4<u32>,
    // x = douceur des ombres par défaut, y = taille de l'atlas (px), z = taille d'une case,
    // w = cases par ligne.
    sh: vec4<f32>,
    // x = intensité générale, y = force des reflets, z = nombre de bandes, w = douceur.
    surf: vec4<f32>,
    // x = numéro de l'image (tramage), y = métamorphose du bruit, z = nombre de boîtes,
    // w = pixels de l'image par pixel des rayons, de côté (2 ou 4).
    misc: vec4<f32>,
    // Boîtes exactes des joueurs (tournées avec eux) : lignes de la matrice qui ramène le
    // monde au cube unité de chaque boîte.
    box_x: array<vec4<f32>, 48>,
    box_y: array<vec4<f32>, 48>,
    box_z: array<vec4<f32>, 48>,
    // Sphère de chaque boîte (centre, rayon), élargie de quelques centièmes : premier tri.
    box_s: array<vec4<f32>, 48>,
};

@group(0) @binding(7) var<uniform> dh: DynHeader;
@group(0) @binding(8) var<storage, read> dlights: array<DynLight>;
@group(0) @binding(9) var<storage, read> dtiles: array<u32>;
@group(0) @binding(10) var shadow_atlas: texture_depth_2d;
@group(0) @binding(11) var shadow_cmp: sampler_comparison;
@group(0) @binding(12) var gobos: texture_2d_array<f32>;
@group(0) @binding(13) var gobo_sampler: sampler;
// (v220) Atlas des ombres des joueurs : leur distance à chaque lumière (rapportée à sa
// portée ; 1 : personne), dans les mêmes cases que les cartes des blocs (voir `EntJob`).
@group(0) @binding(23) var ent_atlas: texture_depth_2d;

const DL_VL_SHADOWS: u32 = 1u;
const DL_VL_NOISE: u32 = 2u;
const DL_DITHER: u32 = 4u;
const DL_SHADOWS: u32 = 8u;
const DL_TOON: u32 = 16u;
const DL_SPECULAR: u32 = 32u;
const DL_VOLUMETRIC: u32 = 64u;
const DL_BOX_SHADOWS: u32 = 128u;
// Éclairage différé en demi-résolution, exact aux bords (dyn_deferred.wgsl).
const DL_HALF_LIGHT: u32 = 256u;

// Variante compilée à part : sans ombres douces (aucune lumière n'en a), tout le code des
// pénombres disparaît du shader, qui tient alors dans moins de registres (plus de pixels
// traités en même temps par la carte). Même image.
override DL_SOFT_SHADOWS: bool = true;

/// Mots du masque des lumières à lire par tuile : seulement ceux qui peuvent en contenir
/// (`count` lumières, 32 par mot), pas les 8 à chaque pixel.
fn dl_tile_words(count: u32) -> u32 {
    return min(min(dh.counts.w, 8u), (count + 31u) / 32u);
}

/// Premier mot du masque des lumières de la tuile qui contient le pixel `frag` (pixels,
/// origine en haut à gauche) d'une image de `size` pixels.
fn dl_tile_base(frag: vec2<f32>, size: vec2<f32>) -> u32 {
    let gx = dh.counts.y;
    let gy = dh.counts.z;
    let tx = min(u32(max(frag.x, 0.0) / max(size.x, 1.0) * f32(gx)), gx - 1u);
    let ty = min(u32(max(frag.y, 0.0) / max(size.y, 1.0) * f32(gy)), gy - 1u);
    return (ty * gx + tx) * dh.counts.w;
}

/// Une lumière au moins dans la tuile du pixel `frag` ?
fn dl_tile_any(frag: vec2<f32>, size: vec2<f32>) -> bool {
    if dh.counts.x == 0u {
        return false;
    }
    let base = dl_tile_base(frag, size);
    let words = dl_tile_words(dh.counts.x);
    var any = 0u;
    for (var w = 0u; w < words; w++) {
        any = any | dtiles[base + w];
    }
    return any != 0u;
}

/// Coin `vi` (0..6 : deux triangles) du rectangle `r` (NDC : x0, y0, x1, y1) : les passes des
/// lumières ne dessinent que les cases d'écran éclairées.
fn dl_rect_corner(vi: u32, r: vec4<f32>) -> vec4<f32> {
    // Coins (0,0) (1,0) (0,1) (0,1) (1,0) (1,1) : bits de x = 110010, de y = 101100.
    let cx = f32((50u >> vi) & 1u);
    let cy = f32((44u >> vi) & 1u);
    return vec4<f32>(mix(r.x, r.z, cx), mix(r.y, r.w, cy), 0.0, 1.0);
}

// PIÈGE (ciel en v108, projecteurs en v175) : ne jamais retrouver une direction ou une
// position du monde avec `inv_view_proj` (un point « proche » ou « lointain » dé-projeté,
// moins la caméra). La projection n'a pas de plan lointain (Z inversé, à l'infini) et les
// coordonnées du monde sont grandes : les arrondis de l'inversion et de la soustraction
// faussent la direction, et l'image tourne ou saute quand on bouge la vue. Ces deux
// fonctions partent des lignes de `view_proj`, sans inversion ni soustraction (même calcul
// que `sky_dir` du ciel) ; le test `nothing_rebuilds_the_view_with_the_inverse_matrix`
// refuse tout retour de `inv_view_proj`.

/// Rayon de la caméra qui passe par le point `ndc` de l'écran, à l'échelle d'une unité de
/// profondeur de vue (sa composante vers l'avant vaut 1). `m` : la matrice qui a dessiné
/// l'image (avec le décalage du TAA).
fn dl_view_ray(m: mat4x4<f32>, ndc: vec2<f32>) -> vec3<f32> {
    let rx = vec3<f32>(m[0][0], m[1][0], m[2][0]);
    let ry = vec3<f32>(m[0][1], m[1][1], m[2][1]);
    let rw = vec3<f32>(m[0][3], m[1][3], m[2][3]);
    // clip.x = rx·d, clip.y = ry·d, clip.w = rw·d (la caméra se projette en (0, 0, proche,
    // 0)) ; on veut clip.xy = ndc × clip.w, puis clip.w = 1 (devant la caméra).
    let d = cross(rx - ndc.x * rw, ry - ndc.y * rw);
    return d / dot(rw, d);
}

/// Écart entre la caméra et le point de l'écran `ndc` à la profondeur `depth` (> 0) :
/// Z inversé sans plan lointain, profondeur = proche / distance de vue, avec
/// proche = m[3][2]. Pour la position du monde : caméra + cet écart.
fn dl_view_offset(m: mat4x4<f32>, ndc: vec2<f32>, depth: f32) -> vec3<f32> {
    return dl_view_ray(m, ndc) * (m[3][2] / depth);
}

/// Bruit par pixel (interleaved gradient noise), décalé à chaque image : le TAA lisse.
fn dl_ign(frag: vec2<f32>) -> f32 {
    let f = frag + vec2<f32>(5.588238, 5.588238) * (dh.misc.x % 64.0);
    return fract(52.9829189 * fract(0.06711056 * f.x + 0.00583715 * f.y));
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

/// Direction `d` dans le repère (droite, haut, avant) de la face `f` du cube, le même que
/// celui du calcul des cartes (shadow_bake.wgsl) : +X (−z, y, x), +Y (x, −z, y),
/// +Z (x, y, z) ; une face négative retourne l'avant et un des deux autres axes.
fn dl_face_coords(f: u32, d: vec3<f32>) -> vec3<f32> {
    let ax = f / 2u;
    var c = select(
        select(d, vec3<f32>(d.x, -d.z, d.y), ax == 1u),
        vec3<f32>(-d.z, d.y, d.x),
        ax == 0u,
    );
    if (f & 1u) == 1u {
        c = select(vec3<f32>(-c.x, c.y, -c.z), vec3<f32>(c.x, -c.y, -c.z), ax == 1u);
    }
    return c;
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

/// Ce qu'il faut d'une lumière pour ses ombres et son gobo (calculé par le jeu) : repère du
/// projecteur, case de sa carte, 1 / tan du demi-angle, rotation du gobo.
struct DlFrame {
    b: mat3x3<f32>,
    rect: vec3<f32>,
    fy: f32,
    rot: vec2<f32>,
};

fn dl_frame(L: DynLight) -> DlFrame {
    var f: DlFrame;
    f.b = mat3x3<f32>(L.right.xyz, L.up.xyz, L.dir_type.xyz);
    f.rect = L.rect.xyz;
    f.fy = L.right.w;
    f.rot = vec2<f32>(L.up.w, L.rect.w);
    return f;
}

/// Gobo : part de la lumière qui passe le motif projeté (1 sans gobo). Le motif occupe le
/// cône du projecteur (échelle 1), haut de l'image vers le haut du repère ; hors de l'image :
/// noir (blanc s'il est inversé).
fn dl_cookie(p: vec3<f32>, L: DynLight, fr: DlFrame) -> f32 {
    let layer = L.cookie.x;
    if layer < 0.0 {
        return 1.0;
    }
    let to = p - L.pos_range.xyz;
    let z = dot(fr.b[2], to);
    if z < 0.001 {
        return 1.0;
    }
    var q = vec2<f32>(dot(fr.b[0], to), dot(fr.b[1], to)) * (fr.fy / z) / max(L.cookie.z, 1e-3);
    q = vec2<f32>(fr.rot.x * q.x - fr.rot.y * q.y, fr.rot.y * q.x + fr.rot.x * q.y);
    let uv = vec2<f32>(q.x * 0.5 + 0.5, 0.5 - q.y * 0.5);
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    var m = textureSampleLevel(gobos, gobo_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), i32(layer), 0.0).r;
    m = select(0.0, m, inside);
    return select(m, 1.0 - m, L.cookie.w > 0.5);
}

/// Position dans l'atlas (uv) du texel qui voit la direction `d` depuis la lumière ;
/// z = 1 si la direction est dans la carte (un projecteur ne voit que son cône).
fn dl_shadow_uv(L: DynLight, fr: DlFrame, d: vec3<f32>) -> vec3<f32> {
    var c: vec3<f32>;
    var rect = fr.rect;
    if L.shadow.x > 1.5 {
        let f = dl_cube_face(d);
        c = dl_face_coords(f, d);
        rect = dl_tile_rect(L.vol.w + f32(f));
    } else {
        c = vec3<f32>(dot(fr.b[0], d), dot(fr.b[1], d), dot(fr.b[2], d));
    }
    let ndc = c.xy / max(c.z * L.shadow.y, 1e-6);
    let ok = c.z > 1e-5 && all(abs(ndc) <= vec2<f32>(1.0));
    // Un demi-texel à l'intérieur de la case : le filtrage ne lit jamais la case voisine.
    let px = 1.0 / max(dh.sh.y, 1.0);
    let local = clamp(
        vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * rect.z,
        vec2<f32>(px * 0.5),
        vec2<f32>(rect.z - px * 0.5),
    );
    return vec3<f32>(rect.xy + local, select(0.0, 1.0, ok));
}

/// Rotation du disque de Vogel d'un échantillon au suivant (angle d'or).
const DL_GOLDEN = vec2<f32>(-0.73736887, 0.67549029);

/// Ombre nette lissée : filtre en tente sur trois texels de côté (quatre lectures bilinéaires
/// comparées, aux places et poids du filtre de The Witness), chaque lecture comparée à la
/// distance du plan de la face dans sa propre direction (pas d'acné sur une face rasante),
/// puis bord resserré. Bords arrondis et nets, sans marches d'escalier, stables (pas de
/// bruit). `dir` : direction lumière → point, `ndt` : n · (point − lumière), `dist` :
/// distance, `eps` : marge.
fn dl_shadow_tent(L: DynLight, fr: DlFrame, dir: vec3<f32>, n: vec3<f32>, ndt: f32, dist: f32, range: f32, eps: f32) -> f32 {
    var rect = fr.rect;
    var c: vec3<f32>;
    var nb: vec3<f32>;
    if L.shadow.x > 1.5 {
        let f = dl_cube_face(dir);
        c = dl_face_coords(f, dir);
        nb = dl_face_coords(f, n);
        rect = dl_tile_rect(L.vol.w + f32(f));
    } else {
        c = vec3<f32>(dot(fr.b[0], dir), dot(fr.b[1], dir), dot(fr.b[2], dir));
        nb = vec3<f32>(dot(fr.b[0], n), dot(fr.b[1], n), dot(fr.b[2], n));
    }
    let th = L.shadow.y;
    let tile = max(dh.sh.z, 1.0);
    let atlas = max(dh.sh.y, 1.0);
    let ndc = c.xy / max(c.z * th, 1e-6);
    // Place dans la case, en texels ; coin de texel le plus proche et part au-delà.
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5) * tile;
    let corner = floor(uv + 0.5);
    let st = uv + 0.5 - corner;
    let base = corner - 0.5;
    let w0 = 3.0 - 2.0 * st;
    let w1 = 1.0 + 2.0 * st;
    let o0 = (2.0 - st) / w0 - 1.0;
    let o1 = st / w1 + 1.0;
    var sum = 0.0;
    for (var k = 0u; k < 4u; k++) {
        let hx = (k & 1u) == 1u;
        let hy = k >= 2u;
        let w = select(w0.x, w1.x, hx) * select(w0.y, w1.y, hy);
        let pt = clamp(base + vec2<f32>(select(o0.x, o1.x, hx), select(o0.y, o1.y, hy)), vec2<f32>(0.5), vec2<f32>(tile - 0.5));
        // Direction de cette lecture dans le repère de la carte (même longueur dans le monde) :
        // n · direction = nb · ck / |ck|.
        let ck = vec3<f32>(vec2<f32>(pt.x / tile * 2.0 - 1.0, 1.0 - pt.y / tile * 2.0) * th, 1.0);
        let nd = dot(nb, ck) / length(ck);
        let tk = select(dist, clamp(ndt / nd, 0.0, range * 2.0), nd < -1e-3);
        sum += w * textureSampleCompareLevel(shadow_atlas, shadow_cmp, rect.xy + pt / atlas, tk / range - eps);
    }
    // Bord resserré autour de la moitié : la courbe à mi-ombre de la tente est lisse (pas de
    // marches), le bord reste net au lieu d'un flou de trois texels. (v202) Sauf pour une
    // lumière portée (shadow.w) : elle bouge sans cesse, sa carte est refaite à chaque pas
    // et chaque bord y saute d'un texel ; en pente douce, il glisse.
    let v = sum * (1.0 / 16.0);
    return select(smoothstep(0.35, 0.65, v), v, L.shadow.w > 0.5);
}

/// Visibilité (0 dans l'ombre … 1 éclairé) de la lumière `L` en `p`, face de normale `n`
/// tournée vers elle. Ombres douces (PCSS) : recherche des bloqueurs, puis filtrage dont la
/// largeur grandit avec la taille de la source et la distance au bloqueur. Chaque échantillon
/// est comparé à la distance du plan de la face dans sa direction (pas d'acné sur les faces
/// des blocs, même rasantes). `noise` : rotation du disque (lissée par le TAA) ; `soft` :
/// faux pour un apport trop faible pour que la pénombre se voie (un seul échantillon).
/// Les points du disque de Vogel (rayon √((k + ½) / n), angle k × angle d'or + rotation)
/// sont obtenus en tournant le précédent : ni sinus ni cosinus par échantillon.
fn dl_shadow(L: DynLight, fr: DlFrame, p: vec3<f32>, n: vec3<f32>, noise: f32, soft: bool, d0: f32) -> f32 {
    let range = max(L.pos_range.w, 0.01);
    let lp = L.pos_range.xyz;
    // Taille d'un texel de la carte à cette distance (blocs).
    let texel = 2.0 * L.shadow.y * d0 / max(dh.sh.z, 1.0);
    let q = p + n * (0.01 + 1.2 * texel);
    let to = q - lp;
    let dist = max(length(to), 1e-4);
    let dir = to / dist;
    let ndt = dot(n, to);
    let center = dl_shadow_uv(L, fr, dir);
    if center.z < 0.5 {
        return 1.0;
    }
    let eps = 2e-4;
    // Apport trop faible pour qu'un bord se voie : une lecture.
    if !soft {
        return textureSampleCompareLevel(shadow_atlas, shadow_cmp, center.xy, dist / range - eps);
    }
    // Ombre nette (réglage, ou source sans taille) : bords lissés.
    let bulb = select(dh.sh.x, L.cone.w, L.cone.w > 0.0);
    if !DL_SOFT_SHADOWS || bulb < 0.01 || L.shadow.z > 0.5 {
        return dl_shadow_tent(L, fr, dir, n, ndt, dist, range, eps);
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
    var a = vec2<f32>(cos(phi), sin(phi));
    for (var k = 0u; k < 8u; k++) {
        let o = a * (sqrt((f32(k) + 0.5) / 8.0) * search);
        a = vec2<f32>(a.x * DL_GOLDEN.x - a.y * DL_GOLDEN.y, a.x * DL_GOLDEN.y + a.y * DL_GOLDEN.x);
        let dk = normalize(dir + t1 * o.x + t2 * o.y);
        let s = dl_shadow_uv(L, fr, dk);
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
    a = vec2<f32>(cos(phi + 1.7), sin(phi + 1.7));
    for (var k = 0u; k < 12u; k++) {
        let o = a * (sqrt((f32(k) + 0.5) / 12.0) * filt);
        a = vec2<f32>(a.x * DL_GOLDEN.x - a.y * DL_GOLDEN.y, a.x * DL_GOLDEN.y + a.y * DL_GOLDEN.x);
        let dk = normalize(dir + t1 * o.x + t2 * o.y);
        let s = dl_shadow_uv(L, fr, dk);
        let nd = dot(n, dk);
        let tk = select(dist, clamp(ndt / nd, 0.0, range * 2.0), nd < -1e-3);
        let c = textureSampleCompareLevel(shadow_atlas, shadow_cmp, s.xy, tk / range - eps);
        vis += select(1.0, c, s.z > 0.5);
    }
    return vis / 12.0;
}

/// Ombre d'un joueur (boîtes à portée de la lumière) sur le trajet `p` → lumière : 1 si
/// caché, 0 sinon. `entity` : `p` est sur un joueur (sa propre boîte ne le cache pas).
/// `l`, `d` : direction (unitaire) et distance de la lumière vues du point de la surface,
/// à quelques centièmes de `p` (les sphères des tris sont élargies d'autant).
fn dl_box_shadow(L: DynLight, p: vec3<f32>, entity: bool, l: vec3<f32>, d: f32) -> f32 {
    // Le trajet passe loin de tous les joueurs (presque toujours) : rien à essayer. Il
    // sort du cône (vu de la lumière) qui contient leur sphère, ou s'arrête avant elle.
    // (Deux produits, avec ce que `dl_one` a déjà calculé.)
    if dot(-l, L.box_sphere.xyz) < L.box_sphere.w || d < bitcast<f32>(L.boxes.z) {
        return 0.0;
    }
    let seg = L.pos_range.xyz - p;
    let inv_seg2 = 1.0 / max(dot(seg, seg), 1e-8);
    let p4 = vec4<f32>(p, 1.0);
    let l4 = vec4<f32>(L.pos_range.xyz, 1.0);
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
            // Premier tri : le trajet passe loin de la sphère de cette boîte.
            let sp = dh.box_s[k];
            let v = sp.xyz - p;
            let off = seg * clamp(dot(v, seg) * inv_seg2, 0.0, 1.0) - v;
            if dot(off, off) > sp.w * sp.w {
                continue;
            }
            // Segment point → lumière dans le cube unité de la boîte (même paramètre t :
            // la matrice est affine). La boîte tourne avec le morceau de joueur : l'ombre
            // de la tête tombe juste sur les épaules, pas en triangle sur le torse.
            let rx = dh.box_x[k];
            let ry = dh.box_y[k];
            let rz = dh.box_z[k];
            let a = vec3<f32>(dot(rx, p4), dot(ry, p4), dot(rz, p4));
            let d = vec3<f32>(dot(rx, l4), dot(ry, l4), dot(rz, l4)) - a;
            let inv = 1.0 / select(d, vec3<f32>(1e-6), abs(d) < vec3<f32>(1e-6));
            // Point dans la boîte : sur le joueur lui-même, pas d'ombre de sa propre boîte ;
            // sur le monde (le sol sous les pieds posés), dans l'ombre comme autour.
            let inside = entity && all(a > vec3<f32>(0.0)) && all(a < vec3<f32>(1.0));
            let t0 = -a * inv;
            let t1 = (vec3<f32>(1.0) - a) * inv;
            let near = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
            let far = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
            if !inside && far >= max(near, 0.0) && near < 1.0 {
                hit = 1.0;
            }
        }
    }
    return hit;
}

/// (v220) Ombre des joueurs de l'atlas des joueurs sur le trajet `p` → lumière (0 : caché,
/// 1 : éclairé ; filtrage bilinéaire comparé, bord doux d'un texel). `l`, `d` : direction
/// (unitaire) et distance de la lumière vues de `p`.
fn dl_ent_shadow(L: DynLight, fr: DlFrame, p: vec3<f32>, l: vec3<f32>, d: f32) -> f32 {
    let s = dl_shadow_uv(L, fr, -l);
    if s.z < 0.5 {
        return 1.0;
    }
    // Un demi-texel de cet atlas (moins fin que celui des blocs en Haute et Ultra) à
    // l'intérieur de la case : le filtrage ne lit jamais les joueurs de la case voisine.
    var rect = fr.rect;
    if L.shadow.x > 1.5 {
        rect = dl_tile_rect(L.vol.w + f32(dl_cube_face(-l)));
    }
    let px = 0.5 / f32(max(textureDimensions(ent_atlas).x, 1u));
    let uv = clamp(s.xy, rect.xy + vec2<f32>(px), rect.xy + vec2<f32>(rect.z - px));
    let range = max(L.pos_range.w, 1e-3);
    // Marge : 5 centièmes de bloc (le sol sous les pieds n'est pas dans leur ombre propre).
    return textureSampleCompareLevel(ent_atlas, shadow_cmp, uv, (d - 0.05) / range);
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

/// Une lumière `L` en `p` (normale `n`, `v` : vers la caméra) : diffus et reflets, sans
/// l'intensité générale. `boxes` : avec les ombres des joueurs.
fn dl_one(
    L: DynLight,
    p: vec3<f32>,
    n: vec3<f32>,
    v: vec3<f32>,
    gloss: f32,
    entity: bool,
    noise: f32,
    boxes: bool,
) -> DynOut {
    var o = DynOut(vec3<f32>(0.0), vec3<f32>(0.0));
    let flags = dh.vl_c.w;
    let shadows_on = (flags & DL_SHADOWS) != 0u;
    let boxes_on = boxes && (flags & DL_BOX_SHADOWS) != 0u;
    let toon = (flags & DL_TOON) != 0u;
    let spec_k = select(0.0, dh.surf.y, (flags & DL_SPECULAR) != 0u);
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
        let fr = dl_frame(L);
        if L.dir_type.w > 0.5 {
            let theta = dot(-l, L.dir_type.xyz);
            att *= clamp((theta - L.cone.x) / max(L.cone.y - L.cone.x, 1e-6), 0.0, 1.0);
            if att > 0.0 {
                att *= dl_cookie(p, L, fr);
            }
        }
        if att > 0.0 && ndl > 0.0 {
            if shadows_on && L.shadow.x > 0.5 {
                att *= dl_shadow(L, fr, p, n, noise, att * L.color.w > 0.003, d);
            }
            if boxes_on && att > 0.0 && (L.boxes.x | L.boxes.y) != 0u {
                att *= 1.0 - dl_box_shadow(L, p + n * 0.02, entity, l, d);
            }
            // (v220) Les autres joueurs à portée, dans l'atlas des joueurs (sur le monde : un
            // joueur garde l'ombre des boîtes exactes, sans s'ombrer lui-même).
            if boxes_on && !entity && att > 0.0 && L.boxes.w != 0u {
                att *= dl_ent_shadow(L, fr, p + n * 0.02, l, d);
            }
            var diff = att * ndl;
            if toon {
                diff = dl_toon(diff, dh.surf.z, dh.surf.w);
            }
            o.diffuse = L.color.rgb * diff;
            if spec_k > 0.0 {
                // Blinn-Phong normalisé : tache nette sur un miroir, large et faible
                // sur un bloc mat.
                let h = normalize(l + v);
                let shin = mix(24.0, 1024.0, gloss * gloss);
                let s = pow(max(dot(n, h), 0.0), shin) * (shin + 8.0) / 25.1327;
                o.specular = L.color.rgb * (att * ndl * s * mix(0.02, 1.0, gloss) * spec_k);
            }
        }
    }
    return o;
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
    let words = dl_tile_words(count);
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
            let c = dl_one(dlights[min(w * 32u + b, count - 1u)], p, n, v, gloss, entity, noise, true);
            o.diffuse += c.diffuse;
            o.specular += c.specular;
        }
    }
    o.diffuse *= dh.surf.x;
    o.specular *= dh.surf.x;
    return o;
}

/// (v220) Un joueur fait-il de l'ombre en `p` (normale `n`) à une des lumières de sa tuile
/// d'écran (boîtes exactes, ou atlas des joueurs) ? Pour l'anticrénelage : voir
/// `fs_shadow_motion` (dyn_deferred.wgsl).
fn dl_player_shadowed(p: vec3<f32>, n: vec3<f32>, entity: bool, frag: vec2<f32>, size: vec2<f32>) -> bool {
    let count = dh.counts.x;
    if count == 0u || (dh.vl_c.w & DL_BOX_SHADOWS) == 0u {
        return false;
    }
    let base = dl_tile_base(frag, size);
    let words = dl_tile_words(count);
    let q = p + n * 0.02;
    for (var w = 0u; w < words; w++) {
        var bits = dtiles[base + w];
        loop {
            if bits == 0u {
                break;
            }
            let b = firstTrailingBit(bits);
            bits = bits & (bits - 1u);
            let L = dlights[min(w * 32u + b, count - 1u)];
            if (L.boxes.x | L.boxes.y | L.boxes.w) == 0u {
                continue;
            }
            let to_l = L.pos_range.xyz - p;
            let d2 = dot(to_l, to_l);
            let r = L.pos_range.w;
            if d2 >= r * r || d2 <= 1e-8 {
                continue;
            }
            let d = sqrt(d2);
            let l = to_l / d;
            if dot(n, l) <= 0.0 || (L.dir_type.w > 0.5 && dot(-l, L.dir_type.xyz) < L.cone.x) {
                continue;
            }
            if (L.boxes.x | L.boxes.y) != 0u && dl_box_shadow(L, q, entity, l, d) > 0.5 {
                return true;
            }
            if !entity && L.boxes.w != 0u && dl_ent_shadow(L, dl_frame(L), q, l, d) < 0.5 {
                return true;
            }
        }
    }
    return false;
}

/// Comme `dyn_light`, avec toutes les lumières (pas seulement celles d'une tuile de
/// l'écran) et sans les ombres des joueurs : pour le bras à la première personne, dessiné
/// à part, qui n'est pas à sa place dans l'image du monde.
fn dyn_light_all(p: vec3<f32>, n: vec3<f32>, cam: vec3<f32>, gloss: f32, entity: bool, frag: vec2<f32>) -> DynOut {
    var o = DynOut(vec3<f32>(0.0), vec3<f32>(0.0));
    let count = dh.counts.x;
    let v = normalize(cam - p);
    let noise = dl_ign(frag);
    for (var i = 0u; i < count; i++) {
        let c = dl_one(dlights[i], p, n, v, gloss, entity, noise, false);
        o.diffuse += c.diffuse;
        o.specular += c.specular;
    }
    o.diffuse *= dh.surf.x;
    o.specular *= dh.surf.x;
    return o;
}
