// Minelib — shaders du monde (blocs), du ciel et du réticule.
// Les couleurs sont écrites en sRGB puis converties en linéaire ; la surface est sRGB.
// dynlights.wgsl (lumières dynamiques) est ajouté à la suite.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,   // xyz = position caméra, w = temps (s)
    fog: vec4<f32>,       // x = début, y = fin, z = ratio largeur/hauteur
    sun: vec4<f32>,       // xyz = direction vers le soleil, w = lumière ambiante
    sel_min: vec4<f32>,   // boîte du bloc visé : xyz = coin min, w = 1 si un bloc est visé
    sel_max: vec4<f32>,   // xyz = coin max
    light: vec4<f32>,     // x = lumière du jour (0 nuit .. 1 jour), y = force des torches
    // Anticrénelage temporel : matrices sans décalage sous-pixel (image courante et
    // précédente) pour les vecteurs de mouvement.
    cur_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    screen: vec4<f32>,    // xy = taille de l'image (px)
    // (v190) Passe d'un miroir (voir planar.rs) : on ne garde que ce qui est devant son
    // plan, dot(xyz, p) + w >= 0. Zéro hors de ces passes.
    clip: vec4<f32>,
    // Torche tenue en main : xyz = position, w = portée (0 = aucune) ; couleur (linéaire).
    held_pos: vec4<f32>,
    held_color: vec4<f32>,
    rt: vec4<f32>,        // x = raytracing activé, yzw = coin bas de la grille de blocs
    // Sources de lumière proches (nombre dans light.z) : xyz = position, w = rayon de la
    // flamme (0 = bloc lumineux) ; couleur linéaire, w = portée.
    lights_pos: array<vec4<f32>, 32>,
    lights_col: array<vec4<f32>, 32>,
    // Images des sources derrière les miroirs (nombre dans light.w) : position, couleur
    // (w = portée), plan du miroir (x = axe, y = coordonnée, z = côté éclairé ±1).
    mirror_pos: array<vec4<f32>, 16>,
    mirror_col: array<vec4<f32>, 16>,
    mirror_plane: array<vec4<f32>, 16>,
    // Reflets calculés à part (voir fs_reflect) : vue-projection et position de la caméra
    // de leur dernier calcul (w = 1 si l'image des reflets est valable).
    refl_vp: mat4x4<f32>,
    refl_cam: vec4<f32>,
    // Le joueur vu dans les reflets : x = nombre de pavés, yzw = lumière reçue (linéaire) ;
    // pour chaque pavé, monde → repère du pavé (pixels du skin), coins min (w = sorte :
    // 0 tête, 1 corps, 2 bras, 3 jambe, 4 bloc tenu, 5 objet tenu, 6 lumineux, 7 bloc lumineux) et max (w = couche de
    // texture du bloc, ou couleur 0xRRGGBB de l'objet).
    ray_info: vec4<f32>,
    ray_inv: array<mat4x4<f32>, 8>,
    ray_min: array<vec4<f32>, 8>,
    ray_max: array<vec4<f32>, 8>,
    // Rectangle de texture (u0, v0, u1, v1) d'un morceau de bloc tenu (sortes 4 et 7).
    ray_uv: array<vec4<f32>, 8>,
    // Ombres des joueurs : x = nombre de boîtes, coins min et max (monde). min.w : source de
    // lumière dont la boîte fait l'ombre (partie entière : −1 aucune, −2 la torche tenue),
    // plus 0,5 si le soleil n'atteint pas la boîte (pas d'ombre du soleil).
    shadow_info: vec4<f32>,
    shadow_min: array<vec4<f32>, 64>,
    shadow_max: array<vec4<f32>, 64>,
    // (v190) Miroirs redessinés à part (voir planar.rs), nombre dans mirror_count.x : pour
    // chacun, la vue-projection de son image et son plan (x = axe, y = coordonnée, z = côté
    // vers lequel il regarde ±1, w = couche de `mirror_tex`). (v200) mirror_count.z = 1 :
    // l'eau au niveau mirror_count.y a l'image de ses reflets (`water_tex`).
    mirror_vp: array<mat4x4<f32>, 4>,
    mirror_info: array<vec4<f32>, 4>,
    mirror_count: vec4<f32>,
    // (v201) Torches en lumières ponctuelles (réglage vidéo) : centre (xyz) et rayon (w) de
    // la sphère où chaque bloc lumineux a sa lumière dynamique ; w = 0 : aucune.
    torch_swap: vec4<f32>,
    // (v209) Boîtes d'ombre par groupes de voisines (un joueur, en général) : boule qui
    // contient le groupe (centre xyz, rayon w) et ses boîtes [x, y[ ; shadow_spans[0].z :
    // nombre de groupes.
    shadow_groups: array<vec4<f32>, 16>,
    shadow_spans: array<vec4<u32>, 16>,
    // (v217) Carte d'ombre du soleil des joueurs (voir sunmap.rs) : vue-projection, et x = 1
    // quand elle est à lire, y = son côté (texels), z = marge de profondeur.
    psm_vp: mat4x4<f32>,
    psm_info: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;
@group(0) @binding(3) var voxels: texture_3d<u32>;
@group(0) @binding(4) var palette: texture_1d<u32>;
// Dernière image des reflets : rgb = reflet, a = distance caméra → surface (0 = rien).
@group(0) @binding(5) var refl_tex: texture_2d<f32>;
/// Skin du joueur (64 × 64), pour les reflets.
@group(0) @binding(6) var skin_tex: texture_2d<f32>;
// (v190) Images des miroirs redessinés (une couche par plan, voir planar.rs).
@group(0) @binding(14) var mirror_tex: texture_2d_array<f32>;
// (v200) Joueurs et objets reflétés dans l'eau (voir water_mirror.rs), image retournée de
// gauche à droite ; a = 0 là où rien ne s'y reflète.
@group(0) @binding(15) var water_tex: texture_2d<f32>;
// (v217) Carte d'ombre du soleil des joueurs (profondeur vue du soleil, 1 : personne),
// après les liaisons du ciel (16 à 21).
@group(0) @binding(22) var psm: texture_depth_2d;

fn lin(c: vec3<f32>) -> vec3<f32> {
    return pow(c, vec3<f32>(2.2));
}

fn face_normal(f: u32) -> vec3<f32> {
    switch f {
        case 0u: { return vec3<f32>(1.0, 0.0, 0.0); }
        case 1u: { return vec3<f32>(-1.0, 0.0, 0.0); }
        case 2u: { return vec3<f32>(0.0, 1.0, 0.0); }
        case 3u: { return vec3<f32>(0.0, -1.0, 0.0); }
        case 4u: { return vec3<f32>(0.0, 0.0, 1.0); }
        default: { return vec3<f32>(0.0, 0.0, -1.0); }
    }
}

fn hash3(v: vec3<i32>) -> f32 {
    var h = (u32(v.x) * 73856093u) ^ (u32(v.y) * 19349663u) ^ (u32(v.z) * 83492791u);
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffu) / 65535.0;
}

/// (v201) Part de la lumière des blocs (torches, blocs lumineux) gardée en `p` : avec les
/// torches en lumières ponctuelles, rien au cœur de la sphère où chacune a sa lumière
/// dynamique, puis tout vers son bord. Ses 16 derniers blocs servent de marge : un bloc y
/// est éclairé par des torches un peu plus loin, qui n'ont plus la leur.
fn torch_keep(p: vec3<f32>) -> f32 {
    let r = g.torch_swap.w;
    if r <= 0.0 {
        return 1.0;
    }
    let d = distance(p, g.torch_swap.xyz);
    let inner = max(r - 16.0, 0.0);
    return smoothstep(inner, max(r - 6.0, inner + 2.0), d);
}

// ---------------------------------------------------------------- ciel
// CIEL : DÉBUT (ce bloc est recopié tel quel dans les shaders des entités et des joueurs,
// voir `entity::sky_wgsl` : leur brouillard va vers le même ciel que celui des blocs. Il ne
// lit de `g` que son début : `sun`, `cam_pos`.)

// (v216) Ciel (voir sky.rs) : ses uniformes, à part de ceux du monde.
struct Sky {
    // Soleil : rgb = couleur de son disque, w = part de sa
    // lumière qui arrive (0 : couché).
    sun: vec4<f32>,
    // xyz = direction vers la lune, w = son éclat.
    moon: vec4<f32>,
    // x = étoiles (0 jour … 1 nuit), y = angle du ciel étoilé (rad), z = voile de nuages
    // hauts (halo de 22° autour du soleil et de la lune), w = lueur d'un éclair.
    misc: vec4<f32>,
    // rgb = couleur du ciel couvert, w = part de ciel couvert (brouillard).
    fog: vec4<f32>,
    // Les trois cartes des nuages : xy = chemin fait depuis leur instantané (vent et
    // caméra, blocs), z = rayon de la caméra à l'instantané, w = poids (0 : pas affichée ;
    // deux au plus le sont, en fondu continu de l'une vers l'autre).
    cloud_a: vec4<f32>,
    cloud_b: vec4<f32>,
    cloud_c: vec4<f32>,
    // x = rayon de la caméra, y = rayon du milieu de la couche, z = 1 / angle du bord des
    // cartes, w = épaisseur de la couche (blocs).
    cloud_geo: vec4<f32>,
    // xy = direction (xz) de l'éclair vu de la caméra. (v237) z = sinus de sa hauteur dans
    // le ciel, w = 0 : tout ce côté du ciel s'allume … 1 : une tache autour de lui (voir
    // `flash_side`).
    flash: vec4<f32>,
    // (v217) Sol lointain, sous l'horizon : rgb = sa couleur (sans éclairage), w = hauteur
    // de l'œil au-dessus de lui (blocs).
    ground: vec4<f32>,
    // x = 1 / distance de brume du sol lointain.
    haze: vec4<f32>,
};

// Atmosphère (image cube calculée sur le processeur), les trois cartes des nuages (tout le
// ciel en œil de poisson ; rgb = lumière, a = ce qui passe derrière).
@group(0) @binding(16) var sky_cube: texture_cube<f32>;
@group(0) @binding(17) var cloud_x: texture_2d<f32>;
@group(0) @binding(18) var cloud_y: texture_2d<f32>;
@group(0) @binding(19) var cloud_z: texture_2d<f32>;
@group(0) @binding(20) var sky_samp: sampler;
@group(0) @binding(21) var<uniform> sky: Sky;

/// Tirage (0 … 1) d'une case de l'espace.
fn sky_hash(v: vec3<i32>) -> f32 {
    var h = (u32(v.x) * 73856093u) ^ (u32(v.y) * 19349663u) ^ (u32(v.z) * 83492791u);
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffu) / 65535.0;
}

/// (v216) Couleur du ciel sans nuages ni astres dans la direction `dir` : l'atmosphère (image
/// cube calculée sur le processeur, voir sky.rs), grisée sous un ciel couvert. Le brouillard
/// des blocs lointains s'y fond. Une seule lecture (avant : un dégradé calculé par pixel).
fn sky_color(dir: vec3<f32>) -> vec3<f32> {
    let c = textureSampleLevel(sky_cube, sky_samp, dir, 0.0).rgb;
    // (v235) Un ciel couvert laisse passer un peu de la couleur du ciel derrière lui, du
    // côté où l'on regarde : un horizon un peu plus clair que les nuages en plein jour, une
    // lueur du côté du couchant le soir. (Tout gris, l'horizon était le même partout.)
    return mix(c, sky.fog.rgb, sky.fog.w * 0.86);
}

/// Halo du soleil, et son disque si `disc` : l'auréole serrée autour du disque, la grande
/// lueur, et le halo de 22° (cristaux de glace) quand un voile de nuages hauts s'installe
/// avant la pluie. Couleur du soleil tel qu'il arrive ici : blanc en haut du ciel, orangé
/// puis rouge à l'horizon (`sky_sun`).
fn sun_glow(dir: vec3<f32>, disc: bool) -> vec3<f32> {
    let s = dot(dir, normalize(g.sun.xyz));
    if s < 0.55 || sky.sun.w <= 0.0 {
        return vec3<f32>(0.0);
    }
    // Angle au soleil (rad) sans acos : exact à 1 % près sous 35°.
    let ang = sqrt(max(2.0 - 2.0 * s, 0.0));
    let col = sky.sun.rgb;
    var c = col * (0.42 * exp(-ang * 34.0) + 0.045 * exp(-ang * 7.0));
    if disc {
        // Disque de 0,9° de rayon, un peu plus sombre au bord.
        let x = ang / 0.0157;
        c += col * (1.0 - smoothstep(0.9, 1.0, x)) * (0.6 + 0.4 * sqrt(max(1.0 - x * x, 0.0)));
    }
    let veil = sky.misc.z;
    if veil > 0.0 {
        // Anneau à 22° : bord intérieur net et rougeâtre, extérieur qui s'étale, bleuté.
        let h = (ang - 0.384) * select(55.0, 16.0, ang > 0.384);
        let ring = exp(-h * h);
        let tint = mix(vec3<f32>(1.0, 0.72, 0.55), vec3<f32>(0.85, 0.92, 1.0), smoothstep(0.37, 0.43, ang));
        c += col * tint * ring * 0.02 * veil;
    }
    return c;
}

/// Bruit lissé (3D) à partir de `hash3` : 8 tirages.
fn value_noise(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(sky_hash(i), sky_hash(i + vec3<i32>(1, 0, 0)), u.x);
    let b = mix(sky_hash(i + vec3<i32>(0, 1, 0)), sky_hash(i + vec3<i32>(1, 1, 0)), u.x);
    let c = mix(sky_hash(i + vec3<i32>(0, 0, 1)), sky_hash(i + vec3<i32>(1, 0, 1)), u.x);
    let d = mix(sky_hash(i + vec3<i32>(0, 1, 1)), sky_hash(i + vec3<i32>(1, 1, 1)), u.x);
    return mix(mix(a, b, u.y), mix(c, d, u.y), u.z);
}

/// (v220) Une mer de la lune : la tache sombre autour du point `c` de sa face, de rayon `r`.
fn moon_sea(q: vec3<f32>, c: vec2<f32>, r: f32) -> f32 {
    let d = q - vec3<f32>(c, sqrt(max(1.0 - dot(c, c), 0.0)));
    return exp(-dot(d, d) / (r * r));
}

/// (v220) Un cratère jeune de la lune : sa tache claire en `c`, et les traînées claires qui
/// en partent en étoile jusqu'à `reach`.
fn moon_crater(q: vec3<f32>, c: vec2<f32>, r: f32, reach: f32) -> f32 {
    let v = q.xy - c;
    let d2 = dot(v, v);
    if d2 > reach * reach * 9.0 {
        return 0.0;
    }
    // (Bruit lu sur un cercle autour du cratère : une traînée par bosse, sans couture.)
    let u = v * inverseSqrt(d2 + 1e-6);
    var streak = value_noise(vec3<f32>(u * 6.0, c.x * 40.0));
    streak *= streak;
    let d = sqrt(d2);
    return 0.5 * exp(-d2 / (r * r)) + 0.4 * streak * streak * exp(-d / reach) * smoothstep(r * 0.5, r * 2.0, d);
}

/// (v220) Les cratères de la lune de taille `1 / f` : ce que leur relief ajoute ou retire à
/// la lumière quand le soleil (`sun`, sa direction sur la face) rase le sol. Un cratère par
/// case, au plus : le bord de sa cuvette tourné vers le soleil est clair, l'autre à l'ombre.
fn moon_relief(p: vec2<f32>, f: f32, sun: vec2<f32>) -> f32 {
    let u = p * f;
    let cell = vec3<i32>(i32(floor(u.x)), i32(floor(u.y)), i32(f));
    let h = sky_hash(cell);
    if h < 0.3 {
        return 0.0;
    }
    let c = 0.5 + (vec2<f32>(sky_hash(cell + vec3<i32>(7, 0, 0)), sky_hash(cell + vec3<i32>(0, 13, 0))) - 0.5) * 0.4;
    let v = (fract(u) - c) / (0.1 + 0.2 * h);
    let r = length(v);
    if r > 1.3 || r < 0.001 {
        return 0.0;
    }
    // Pente : elle monte vers le bord dans la cuvette, redescend dehors.
    let slope = select(-0.5 * sin((r - 1.0) * 10.47), sin(r * 3.1416), r < 1.0);
    return -dot(v / r, sun) * slope;
}

/// La lune : une sphère éclairée par le vrai soleil (ses phases en découlent), et sa lueur.
/// Elle montre toujours la même face. (v220) Trois fois plus grande (3° de rayon), et sa
/// face est celle de la vraie : ses mers à leur place, ses grands cratères clairs et leurs
/// traînées, son relief qui ressort le long de la limite du jour, et un sol de poussière
/// (aussi clair au bord qu'au centre à la pleine lune, pas une boule de plâtre). Basse sur
/// l'horizon, l'air la rougit. Rend sa lumière (rgb) et la part du ciel qu'elle cache (a :
/// les étoiles ne brillent pas à travers son côté sombre).
fn moon_shade(dir: vec3<f32>) -> vec4<f32> {
    let k = sky.moon.w;
    let m = sky.moon.xyz;
    let s = dot(dir, m);
    if k <= 0.0 || s < 0.9 {
        return vec4<f32>(0.0);
    }
    let ang = sqrt(max(2.0 - 2.0 * s, 0.0));
    let rm = 0.0524;
    var cover = 0.0;
    let low = 1.0 - smoothstep(0.0, 0.34, m.y);
    let tint = mix(vec3<f32>(1.0, 0.975, 0.93), vec3<f32>(1.0, 0.64, 0.34), low * low) * (1.0 - 0.3 * low * low);
    var c = mix(vec3<f32>(0.55, 0.66, 0.9), tint, 0.3) * (0.035 * exp(-ang * 20.0) + 0.006 * exp(-ang * 7.0)) * k;
    if ang < rm {
        let right = normalize(cross(vec3<f32>(0.0, 0.0, 1.0), m));
        let up = cross(m, right);
        let x = dot(dir, right) / rm;
        let y = dot(dir, up) / rm;
        let z = sqrt(max(1.0 - x * x - y * y, 0.0));
        // Point de la face (x à droite, y en haut, z vers nous), et sa normale dans le monde.
        let q = vec3<f32>(x, y, z);
        let n = right * x + up * y - m * z;
        let sun = normalize(g.sun.xyz);
        let mu = max(dot(n, sun), 0.0);
        // Sol de poussière : il renvoie la lumière vers d'où elle vient.
        var lit = min(2.0 * mu / (mu + z + 0.04), 1.1);
        // Mers : les plaines sombres, à leur place sur la face, aux bords irréguliers.
        let w = q + (vec3<f32>(value_noise(q * 3.3 + 7.0), value_noise(q * 3.3 + 19.0), 0.5) - 0.5) * 0.26
            + (vec3<f32>(value_noise(q * 9.0 + 5.0), value_noise(q * 9.0 + 23.0), 0.5) - 0.5) * 0.08;
        let sea = moon_sea(w, vec2<f32>(-0.60, 0.20), 0.36)
            + moon_sea(w, vec2<f32>(-0.24, 0.52), 0.27)
            + moon_sea(w, vec2<f32>(0.27, 0.46), 0.17)
            + moon_sea(w, vec2<f32>(0.47, 0.13), 0.20)
            + moon_sea(w, vec2<f32>(0.79, 0.30), 0.10)
            + moon_sea(w, vec2<f32>(0.71, -0.09), 0.14)
            + moon_sea(w, vec2<f32>(0.50, -0.28), 0.085)
            + moon_sea(w, vec2<f32>(-0.25, -0.36), 0.19)
            + moon_sea(w, vec2<f32>(-0.58, -0.40), 0.10)
            + moon_sea(w, vec2<f32>(-0.05, 0.80), 0.12)
            + moon_sea(w, vec2<f32>(-0.36, 0.75), 0.10);
        // Grain du sol (hautes terres claires, mouchetées).
        let grain = value_noise(q * 13.0 + 3.0) * 0.6 + value_noise(q * 31.0) * 0.4;
        let mare = smoothstep(0.27, 0.62, sea + (grain - 0.5) * 0.3);
        var albedo = mix(1.0, 0.47, mare) * (0.75 + 0.3 * grain);
        albedo += moon_crater(q, vec2<f32>(-0.10, -0.69), 0.04, 0.4)
            + moon_crater(q, vec2<f32>(-0.33, 0.17), 0.032, 0.15)
            + moon_crater(q, vec2<f32>(-0.61, 0.14), 0.022, 0.09)
            + moon_crater(q, vec2<f32>(-0.68, 0.40), 0.024, 0.06);
        // Relief : on ne le voit que là où la lumière rase le sol, près de la limite du
        // jour, et pas à la pleine lune (on la regarde alors d'où vient la lumière : aucune
        // ombre visible). Les mers sont plus lisses que les hautes terres.
        let sl = vec3<f32>(dot(sun, right), dot(sun, up), -dot(sun, m));
        let graze = (1.0 - smoothstep(0.0, 0.5, mu)) * (1.0 - smoothstep(0.55, 0.96, sl.z)) * (1.0 - 0.55 * mare);
        if graze > 0.01 {
            let sd = sl.xy * inverseSqrt(max(dot(sl.xy, sl.xy), 1e-6));
            let relief = moon_relief(q.xy, 4.3, sd) + 0.8 * moon_relief(q.xy + 0.37, 8.7, sd);
            lit *= 1.0 + clamp(relief * 1.1, -0.85, 0.9) * graze;
        }
        let edge = 1.0 - smoothstep(0.955, 1.0, ang / rm);
        // (Côté nuit à peine visible : la lumière cendrée, bleutée.)
        let disc = (tint * lit * 0.8 + vec3<f32>(0.6, 0.75, 1.0) * 0.013) * albedo * k;
        c = mix(c, disc, edge);
        cover = edge;
    }
    return vec4<f32>(c, cover);
}

/// Étoiles (elles tournent avec l'heure autour de l'axe du monde, se lèvent et se
/// couchent) et Voie lactée. Le ciel est découpé en cases : une étoile dans quelques-unes,
/// sa place, son éclat (beaucoup de faibles, peu de brillantes) et sa teinte tirés au sort.
fn stars(dir: vec3<f32>) -> vec3<f32> {
    let a = sky.misc.y;
    let ca = cos(a);
    let sa = sin(a);
    let d = vec3<f32>(dir.x * ca + dir.y * sa, -dir.x * sa + dir.y * ca, dir.z);
    let p = d * 110.0;
    let cell = vec3<i32>(floor(p));
    let h = sky_hash(cell);
    var c = vec3<f32>(0.0);
    if h > 0.984 {
        let o = vec3<f32>(
            sky_hash(cell + vec3<i32>(17, 0, 0)),
            sky_hash(cell + vec3<i32>(0, 31, 0)),
            sky_hash(cell + vec3<i32>(0, 0, 47)),
        ) - 0.5;
        var dd = fract(p) - 0.5 - o * 0.5;
        // (Écart mesuré sur la voûte seulement : l'étoile est un point rond.)
        dd -= d * dot(dd, d);
        let mag = (h - 0.984) / 0.016;
        let bright = mag * mag * mag * mag;
        let size = 0.0045 + 0.012 * bright;
        let twinkle = 0.8 + 0.2 * sin(g.cam_pos.w * (1.7 + 4.0 * mag) + h * 900.0);
        let tint = mix(vec3<f32>(1.0, 0.82, 0.62), vec3<f32>(0.72, 0.84, 1.0), sky_hash(cell + vec3<i32>(5, 5, 5)));
        c = tint * (0.22 + 2.6 * bright) * exp(-dot(dd, dd) / size) * twinkle;
    }
    // Voie lactée : une bande pâle et inégale en travers du ciel.
    let across = dot(d, vec3<f32>(0.37, 0.18, 0.91));
    let band = exp(-across * across * 14.0);
    if band > 0.02 {
        let dust = value_noise(d * 7.0) * 0.6 + value_noise(d * 19.0) * 0.4;
        c += vec3<f32>(0.5, 0.56, 0.75) * band * smoothstep(0.25, 0.75, dust) * 0.045;
    }
    return c;
}

/// Où tombe, dans une carte des nuages, le point `q` vu de la caméra de son instantané
/// (œil de poisson : le zénith au centre, l'angle depuis le zénith en rayon).
fn cloud_uv(q: vec3<f32>) -> vec2<f32> {
    let d = normalize(q);
    let th = acos(clamp(d.y, -1.0, 1.0));
    let h = d.xz * inverseSqrt(max(dot(d.xz, d.xz), 1e-12));
    return 0.5 + 0.5 * th * sky.cloud_geo.z * h;
}

/// Nuages dans la direction `dir` : rgb = leur lumière, a = part du ciel qui passe
/// derrière. Une lecture par carte affichée (deux en fondu, le plus souvent), au point de
/// la couche que vise `dir`, déplacé du vent et du chemin de la caméra depuis l'instantané
/// de la carte.
fn clouds(dir: vec3<f32>) -> vec4<f32> {
    let wa = sky.cloud_a.w;
    let wb = sky.cloud_b.w;
    let wc = sky.cloud_c.w;
    let total = wa + wb + wc;
    if total <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let rc = sky.cloud_geo.x;
    let rs = sky.cloud_geo.y;
    let b = dir.y * rc;
    let s = -b + sqrt(max(b * b - rc * rc + rs * rs, 0.0));
    // Point visé, depuis le centre de la planète des nuages.
    let p = vec3<f32>(dir.x * s, dir.y * s + rc, dir.z * s);
    var c = vec4<f32>(0.0);
    if wa > 0.0 {
        let q = p + vec3<f32>(sky.cloud_a.x, -sky.cloud_a.z, sky.cloud_a.y);
        c += textureSampleLevel(cloud_x, sky_samp, cloud_uv(q), 0.0) * wa;
    }
    if wb > 0.0 {
        let q = p + vec3<f32>(sky.cloud_b.x, -sky.cloud_b.z, sky.cloud_b.y);
        c += textureSampleLevel(cloud_y, sky_samp, cloud_uv(q), 0.0) * wb;
    }
    if wc > 0.0 {
        let q = p + vec3<f32>(sky.cloud_c.x, -sky.cloud_c.z, sky.cloud_c.y);
        c += textureSampleLevel(cloud_z, sky_samp, cloud_uv(q), 0.0) * wc;
    }
    // (Première carte en train d'apparaître : le reste du poids est du ciel pur.)
    c.a += 1.0 - total;
    return c;
}

/// Ce que reflètent l'eau et les miroirs quand rien n'est devant : le ciel, ses nuages et
/// le soleil.
fn sky_env(dir: vec3<f32>) -> vec3<f32> {
    var c = sky_color(dir) + sun_glow(dir, true);
    if dir.y > -0.03 {
        let cl = clouds(dir);
        c = c * cl.a + cl.rgb;
    }
    return c;
}

/// (v237) Part de la lueur d'un éclair que reçoit le ciel du côté `dir` (unitaire). La
/// foudre qui tombe allume tout son côté du ciel, du bas jusqu'au-dessus de la tête (de
/// loin, on ne voit pas d'où vient la lueur). Celle qu'on lance vers le ciel allume une
/// tache autour de l'endroit où elle entre dans les nuages : avant, les nuages
/// s'allumaient aussi fort au-dessus de soi que là où l'on visait.
fn flash_side(dir: vec3<f32>) -> f32 {
    let f = sky.flash;
    let flat = dir.xz * inverseSqrt(max(dot(dir.xz, dir.xz), 1e-12));
    let side = max(dot(flat, f.xy), 0.0);
    let wide = 0.3 + 0.7 * side * side;
    if f.w <= 0.0 {
        return wide;
    }
    let level = sqrt(max(1.0 - f.z * f.z, 0.0));
    let at = vec3<f32>(f.x * level, f.z, f.y * level);
    let k = max(dot(normalize(dir), at), 0.0);
    let k4 = k * k * k * k;
    return mix(wide, 0.04 + 0.96 * k4 * k4, f.w);
}

/// (v217) La brume de l'horizon du côté `flat` (x, z, unitaire) : le bas du ciel, sans le
/// dessin des nuages. Le ciel s'y fond dans ses derniers degrés (davantage sous la pluie :
/// l'horizon est bouché), et sous l'horizon le sol lointain s'y perd : les deux se
/// rejoignent sans couture. La lueur d'un éclair l'éclaire aussi.
fn horizon_haze(flat: vec2<f32>) -> vec3<f32> {
    let hd = normalize(vec3<f32>(flat.x, 0.012, flat.y));
    var c = sky_color(hd) + sun_glow(hd, false);
    if sky.misc.w > 0.0 {
        c += vec3<f32>(0.72, 0.8, 1.0) * sky.misc.w * (0.07 + 0.55 * sky.fog.w) * flash_side(hd);
    }
    return c;
}

/// Le ciel au-dessus de l'horizon : l'atmosphère, les étoiles, la lune et le soleil
/// (`bodies` : avec leurs disques), puis les nuages devant eux, la lueur des éclairs, et
/// la brume de l'horizon.
fn sky_above(dir: vec3<f32>, bodies: bool) -> vec3<f32> {
    var c = sky_color(dir);
    if bodies {
        let moon = moon_shade(dir);
        if sky.misc.x > 0.01 && dir.y > 0.0 && moon.a < 1.0 {
            c += stars(dir) * sky.misc.x * smoothstep(0.0, 0.2, dir.y) * (1.0 - moon.a);
        }
        c += moon.rgb;
    }
    let glow = sun_glow(dir, false);
    c += glow;
    let cl = clouds(dir);
    c = c * cl.a + cl.rgb;
    if sky.misc.w > 0.0 {
        // Éclair : les nuages s'illuminent de l'intérieur, surtout du côté où il tombe.
        c += vec3<f32>(0.72, 0.8, 1.0) * sky.misc.w * ((1.0 - cl.a) * 0.85 + 0.07) * flash_side(dir);
    }
    // Les derniers degrés au-dessus de l'horizon se perdent dans sa brume (3° par beau
    // temps, une dizaine sous un ciel couvert).
    let near = 1.0 - smoothstep(0.0, 0.05 + 0.13 * sky.fog.w, dir.y);
    if near > 0.0 {
        let flat = dir.xz * inverseSqrt(max(dot(dir.xz, dir.xz), 1e-12));
        c = mix(c, horizon_haze(flat), near * near);
    }
    if bodies {
        // Le disque du soleil, par-dessus la brume (on le voit se coucher), derrière les
        // nuages.
        c += (sun_glow(dir, true) - glow) * cl.a;
    }
    return c;
}

/// Le ciel dans la direction `dir`, et ce qu'on voit sous l'horizon au-delà du monde
/// chargé : (v217) le sol qui continue, de la couleur du relief lointain, éclairé comme
/// lui (les éclairs compris), et qui se perd peu à peu dans la brume de l'air jusqu'à
/// l'horizon, où il rejoint le bas du ciel sans couture. Avant, une bande unie restait
/// sous l'horizon : coupée net du ciel couvert, et noire pendant un éclair. Le brouillard
/// du bout de la vue se fond dans cette même image : le relief s'y perd comme le sol.
fn sky_shade(dir: vec3<f32>) -> vec4<f32> {
    if dir.y >= 0.0 {
        return vec4<f32>(sky_above(dir, true), 1.0);
    }
    let flat = dir.xz * inverseSqrt(max(dot(dir.xz, dir.xz), 1e-12));
    let low = horizon_haze(flat);
    // Distance au sol lointain le long du regard, et ce que la brume en laisse.
    let t = sky.ground.w / max(-dir.y, 1e-4);
    let haze = 1.0 - exp(-t * sky.haze.x);
    // Éclairé comme le dessus des blocs : soleil et ciel selon l'heure, à peine la nuit.
    let day = g.light.x;
    let sun = max(normalize(g.sun.xyz).y, 0.0);
    let lit = ((g.sun.w + (1.0 - g.sun.w) * sun) * day + 0.04 * (1.0 - day)) * 0.82 + 0.012;
    // De larges taches, à peine : un sol uni se verrait.
    let at = g.cam_pos.xz + dir.xz * t;
    let patches = 0.86 + 0.28 * value_noise(vec3<f32>(at.x, 0.0, at.y) / 96.0);
    let ground = sky.ground.rgb * lit * mix(patches, 1.0, haze);
    return vec4<f32>(mix(ground, low, haze), 1.0);
}

// CIEL : FIN

// ---------------------------------------------------------------- blocs

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) data: u32,
    @location(2) tex: u32,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) @interpolate(flat) block: u32,
    @location(2) @interpolate(flat) face: u32,
    @location(3) ao: f32,
    // Métal (fer, or, diamant) : reflet court (4 blocs) et flou.
    @location(4) @interpolate(flat) short: u32,
    // Blocs : lumière colorée des torches (0..1 par canal). Flamme : couleur émise.
    @location(5) light: vec3<f32>,
    // Couche du tableau de textures, bit 12 = émissif.
    @location(6) @interpolate(flat) tex: u32,
    // Lumière du ciel reçue (0 = grotte fermée, 1 = ciel ouvert).
    @location(7) sky: f32,
    // Reflet du bloc (0 mat … 1 miroir).
    @location(8) reflect: f32,
    // Coordonnées de texture du sommet (faces penchées, voir TEX_LOCAL_UV).
    @location(9) uv: vec2<f32>,
};

@vertex
fn vs_block(v: VIn) -> VOut {
    var o: VOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.world = v.pos;
    o.block = 0u;
    o.sky = f32(v.data & 15u) / 15.0;
    o.reflect = f32((v.data >> 4u) & 15u) / 15.0;
    o.face = (v.data >> 8u) & 7u;
    o.ao = f32((v.data >> 11u) & 3u) / 3.0;
    o.short = (v.data >> 13u) & 1u;
    o.tex = v.tex;
    o.uv = vec2<f32>(f32((v.tex >> 16u) & 255u), f32((v.tex >> 24u) & 255u)) / 128.0;
    let rgb = vec3<f32>(
        f32((v.data >> 14u) & 63u),
        f32((v.data >> 20u) & 63u),
        f32((v.data >> 26u) & 63u),
    ) / 63.0;
    o.light = rgb;
    return o;
}

@fragment
fn fs_block_fast(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.3) {
        discard;
    }
    let c = shade_with(i, false, false, false);
    return vec4<f32>(c.rgb, 1.0);
}

/// Face d'un bloc vue de dos, à travers les trous de sa texture (verre, feuillage,
/// fenêtre d'une porte) : pas dessinée, comme dans Minecraft (sinon les bords du dessous
/// d'une vitre se superposent à ceux du dessus). Les plantes en croix (face 6) se voient
/// des deux côtés.
fn from_behind(i: VOut) -> bool {
    // (v215) La vraie normale de la face (dérivées de la position, à prendre avant tout
    // branchement), dans le sens de celle de son axe : une torche murale est penchée de
    // 22,5°, ses faces ne regardent pas droit le long de leur axe. Jugée avec l'axe, une face
    // qu'on voyait de face était jetée sous certains angles (de haut, ou juste sous le haut
    // de la torche) et on voyait au travers. Les faces droites gardent leur axe.
    let gn = cross(dpdx(i.world), dpdy(i.world));
    let axis = face_normal(i.face);
    let along = dot(gn, axis);
    let n = select(axis, gn * sign(along), along * along > 0.5 * dot(gn, gn) && dot(gn, gn) > 1e-24);
    return i.face < 6u && (i.tex & 32768u) == 0u
        && dot(n, g.cam_pos.xyz - i.world) < 0.0;
}

/// (v209) Ce pixel sera-t-il jeté (trou de la texture sous `cut`, face vue de dos) ? Testé
/// avant l'ombrage, avec la texture lue comme dans `shade_rt` (même échantillon, mêmes
/// dérivées) : mêmes pixels gardés, mais ceux qui partent (feuillage, grillages, vitres
/// claires) ne paient plus l'ombrage entier (soleil, ombres des joueurs, brouillard).
fn cut_out(i: VOut, cut: f32) -> bool {
    let raw = block_uv(i);
    let dx = dpdx(raw);
    let dy = dpdy(raw);
    if from_behind(i) {
        return true;
    }
    // Flamme : toujours opaque (voir `shade_rt`).
    if (i.tex & 4096u) != 0u {
        return false;
    }
    let sharp = select(1.0, 0.0, (i.tex & 16384u) != 0u);
    let a = textureSampleGrad(blocks, block_sampler, fract(raw), i.tex & 4095u, dx * sharp, dy * sharp).a;
    return a < cut;
}

@fragment
fn fs_block(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.3) {
        discard; // pixels transparents (feuillage, torche, verre clair)
    }
    let c = shade(i);
    return vec4<f32>(c.rgb, 1.0);
}

/// Passe translucide (eau, glace, verre teinté) : mélange selon l'alpha de la texture.
@fragment
fn fs_block_translucent(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.02) {
        discard;
    }
    let c = shade_with(i, true, true, true);
    return vec4<f32>(c.rgb, max(c.a, 0.25));
}

/// (v190) Passe d'un miroir : ce qui est derrière son plan (le mur qui le porte, l'autre
/// côté) n'apparaît pas dans son reflet.
fn behind_mirror(p: vec3<f32>) -> bool {
    return dot(g.clip.xyz, p) + g.clip.w < 0.0;
}

/// (v190) Blocs vus dans un miroir (opaques et ceux qui reflètent, sans leurs reflets).
@fragment
fn fs_block_mirror(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.3) || behind_mirror(i.world) {
        discard;
    }
    let c = shade_with(i, false, false, false);
    return vec4<f32>(c.rgb, 1.0);
}

/// (v190) Miroirs et métaux vus dans un miroir : un miroir y montre son image s'il
/// en a une (miroir dans le miroir), sinon il est sombre ; un métal, son reflet par rayon
/// si le raytracing est activé.
@fragment
fn fs_block_mirror_reflect(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.3) || behind_mirror(i.world) {
        discard;
    }
    let c = shade_with(i, true, false, false);
    return vec4<f32>(c.rgb, 1.0);
}

/// (v190) Eau, glace, verre vus dans un miroir.
@fragment
fn fs_block_translucent_mirror(i: VOut) -> @location(0) vec4<f32> {
    if cut_out(i, 0.02) || behind_mirror(i.world) {
        discard;
    }
    let c = shade_with(i, true, true, true);
    return vec4<f32>(c.rgb, max(c.a, 0.25));
}

/// (v190) Reflet d'une face de miroir redessiné à part (voir planar.rs) : si la face en `p`
/// (normale `n`) est sur l'un des plans redessinés à cette image, la scène vue de l'autre
/// côté, lue dans son image (a = 1) ; sinon a = 0 (raytracing ou ciel, comme avant).
fn planar_mirror(p: vec3<f32>, n: vec3<f32>) -> vec4<f32> {
    let count = u32(g.mirror_count.x);
    for (var k = 0u; k < count; k = k + 1u) {
        let m = g.mirror_info[k];
        let axis = u32(m.x);
        // Même plan : même orientation, et la face dessus (à un quart de bloc près : deux
        // plans parallèles sont au moins à un bloc l'un de l'autre).
        if n[axis] * m.z < 0.5 || abs(p[axis] - m.y) > 0.25 {
            continue;
        }
        let clip = g.mirror_vp[k] * vec4<f32>(p, 1.0);
        if clip.w <= 0.0 {
            continue;
        }
        let uv = clip.xy / clip.w * vec2<f32>(0.5, -0.5) + 0.5;
        let dims = vec2<i32>(textureDimensions(mirror_tex).xy);
        let px = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - 1);
        return vec4<f32>(textureLoad(mirror_tex, px, i32(m.w), 0).rgb, 1.0);
    }
    return vec4<f32>(0.0);
}

/// (v200) Joueurs et objets reflétés dans l'eau au point `p` de sa surface : lus dans
/// l'image faite à part (water_mirror.rs), là où `p` est à l'écran, retourné de gauche à
/// droite (même vue-projection, sans le tremblement du TAA) ; a = 0 : rien ne s'y reflète.
fn water_mirror(p: vec3<f32>) -> vec4<f32> {
    let c = g.cur_view_proj * vec4<f32>(p, 1.0);
    if c.w <= 0.0 {
        return vec4<f32>(0.0);
    }
    let uv = c.xy / c.w * vec2<f32>(-0.5, -0.5) + 0.5;
    let dims = vec2<i32>(textureDimensions(water_tex));
    let px = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - 1);
    return textureLoad(water_tex, px, 0);
}

/// (v200) Image des reflets dans l'eau : les blocs n'y laissent que leur profondeur (ils
/// cachent les joueurs derrière eux), coupés au plan de l'eau ; leurs pixels transparents
/// (feuillage, vitres) laissent voir au travers.
@fragment
fn fs_block_depth(i: VOut) -> @location(0) vec4<f32> {
    // (v215) `from_behind` prend des dérivées : appelée avant tout branchement.
    let back = from_behind(i);
    if behind_mirror(i.world) || back {
        discard;
    }
    let a = textureSampleLevel(blocks, block_sampler, fract(block_uv(i)), i.tex & 4095u, 0.0).a;
    if a < 0.3 {
        discard;
    }
    return vec4<f32>(0.0);
}

/// (v190) La face en `p` (normale `n`) est-elle sur un des plans redessinés à cette image ?
fn has_planar(p: vec3<f32>, n: vec3<f32>) -> bool {
    let count = u32(g.mirror_count.x);
    for (var k = 0u; k < count; k = k + 1u) {
        let m = g.mirror_info[k];
        let axis = u32(m.x);
        if n[axis] * m.z >= 0.5 && abs(p[axis] - m.y) <= 0.25 {
            return true;
        }
    }
    return false;
}

// ---------------------------------------------------------------- vecteurs de mouvement (TAA)

struct MvOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
};

/// Déplacement à l'écran (pixels, y vers le bas) entre cette image et la précédente :
/// position précédente − position courante.
fn motion_of(p: vec4<f32>) -> vec2<f32> {
    let cur = g.cur_view_proj * p;
    let prev = g.prev_view_proj * p;
    let d = prev.xy / prev.w - cur.xy / cur.w;
    return d * vec2<f32>(0.5, -0.5) * g.screen.xy;
}

/// Monde opaque avec l'anticrénelage : couleur, mouvement, et ce qu'il faut pour ajouter
/// les lumières dynamiques après coup (éclairage différé, voir gbuffer.wgsl).
struct GbOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
    @location(2) albedo: vec4<f32>,
    @location(3) surf: vec4<f32>,
};

fn gb_out(i: VOut, s: Shaded) -> GbOut {
    var o: GbOut;
    o.color = vec4<f32>(s.color.rgb, 1.0);
    o.motion = motion_of(vec4<f32>(i.world, 1.0));
    // (v217) Dans l'ombre d'un joueur : un mouvement d'un cinquantième de pixel (rien pour
    // la reprojection), que l'anticrénelage prend pour un objet qui bouge tout près (voir
    // `near_motion` dans taa.wgsl) : bornes serrées, et l'endroit marqué pour les images
    // suivantes (`was_moving`). Une ombre qui avance ne laisse plus de traînée derrière elle
    // (« rémanence »), ni là où elle était.
    if s.pshadow > 0.004 {
        o.motion.x += 0.02;
    }
    o.albedo = vec4<f32>(s.albedo, clamp(i.reflect, 0.0, 1.0) * 0.99);
    o.surf = vec4<f32>(
        gb_oct_encode(face_normal(i.face)),
        clamp(s.wd, 0.0, 1.0),
        gb_pack_spec(s.ws, i.short != 0u),
    );
    return o;
}

@fragment
fn fs_block_fast_mv(i: VOut) -> GbOut {
    if cut_out(i, 0.3) {
        discard;
    }
    let s = shade_full(i, false, false, false);
    return gb_out(i, s);
}

@fragment
fn fs_block_mv(i: VOut) -> GbOut {
    if cut_out(i, 0.3) {
        discard;
    }
    // Reflets lus dans l'image des reflets (calculée à part, à son propre rythme).
    let s = shade_full(i, true, true, false);
    return gb_out(i, s);
}

@fragment
fn fs_block_translucent_mv(i: VOut) -> TrOut {
    if cut_out(i, 0.02) {
        discard;
    }
    let s = shade_full(i, true, true, true);
    return translucent_out(i, s);
}

/// (v202) La même chose sans le code du raytracing (servi quand il est coupé, image
/// identique) : la carte garde moins de registres pour ce gros shader, et en fait tourner
/// plus à la fois sur l'eau.
@fragment
fn fs_block_translucent_flat_mv(i: VOut) -> TrOut {
    if cut_out(i, 0.02) {
        discard;
    }
    let s = shade_rt(i, true, true, true, false);
    return translucent_out(i, s);
}

/// Sorties d'une face translucide ombrée `s` (voir `TrOut`).
fn translucent_out(i: VOut, s: Shaded) -> TrOut {
    let c = s.color;
    let a = max(c.a, 0.25);
    var o: TrOut;
    o.color = vec4<f32>(c.rgb, a);
    o.motion = vec2<f32>(0.0);
    // Comme `gb_out`, poids multipliés par l'opacité de la face (mélange : face × a).
    // Albédo en racine carrée (8 bits, plus de nuances dans le sombre).
    o.water = vec4<u32>(
        bitcast<u32>(i.clip.z),
        pack4x8unorm(vec4<f32>(sqrt(clamp(s.albedo, vec3<f32>(0.0), vec3<f32>(1.0))), clamp(i.reflect, 0.0, 1.0) * 0.99)),
        pack4x8unorm(vec4<f32>(
            gb_oct_encode(face_normal(i.face)),
            clamp(s.wd * a, 0.0, 1.0),
            gb_pack_spec(s.ws * a, i.short != 0u),
        )),
        0u,
    );
    return o;
}

/// (v199) Translucides avec l'anticrénelage : couleur, mouvement (celui de ce qui est
/// derrière, gardé), et la face pour l'éclairage différé des lumières dynamiques (`fs_water`
/// de dyn_deferred.wgsl : eau, verre, glace). La dernière face dessinée d'un pixel
/// l'emporte (les translucides vont de l'arrière vers l'avant, chunk par chunk).
struct TrOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
    @location(2) water: vec4<u32>,
};

fn shade(i: VOut) -> vec4<f32> {
    return shade_with(i, true, false, false);
}

/// Reflet lu dans la dernière image des reflets (calculée à son propre rythme) : la surface
/// y est retrouvée par sa position dans le monde, donc juste même si la caméra a bougé
/// depuis. a = 0 si elle n'y figurait pas (cachée, hors champ) : on lance alors le rayon.
fn cached_reflection(p: vec3<f32>) -> vec4<f32> {
    let clip = g.refl_vp * vec4<f32>(p, 1.0);
    let ndc = clip.xy / max(clip.w, 1e-6);
    let uv = ndc * vec2<f32>(0.5, -0.5) + 0.5;
    let dims = vec2<i32>(textureDimensions(refl_tex));
    let px = clamp(vec2<i32>(uv * vec2<f32>(dims)), vec2<i32>(0), dims - 1);
    let t = textureLoad(refl_tex, px, 0);
    let expected = length(p - g.refl_cam.xyz);
    let inside = clip.w > 0.0 && all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    let ok = g.refl_cam.w > 0.5 && inside && t.a > 0.0 && abs(t.a - expected) < expected * 0.02 + 0.05;
    return vec4<f32>(t.rgb, select(0.0, 1.0, ok));
}

/// Passe des reflets (à son propre rythme) : rayon depuis chaque surface réfléchissante
/// visible ; a = distance à la caméra pour la retrouver ensuite.
@fragment
fn fs_reflect(i: VOut) -> @location(0) vec4<f32> {
    let n = face_normal(i.face);
    // (v190) Miroir redessiné à part : son image sert, pas de rayon. (v223) Métal : jamais
    // de rayon (l'image de son plan, de près).
    if (i.reflect > 0.99 && i.short == 0u && has_planar(i.world, n)) || i.short != 0u {
        return vec4<f32>(0.0);
    }
    let to_frag = i.world - g.cam_pos.xyz;
    let dist = length(to_frag);
    let r = reflect(to_frag / max(dist, 1e-4), n);
    let env = trace(i.world + n * 0.02, r).rgb;
    return vec4<f32>(env, dist);
}

/// Couleur d'un pixel du monde, et de quoi y ajouter les lumières dynamiques après coup :
/// albédo, poids de leur lumière diffuse (`wd` : occlusion, bloc visé, reflets, brouillard)
/// et de leurs reflets (`ws` : brouillard). Zéro : pas de lumière (flamme).
struct Shaded {
    color: vec4<f32>,
    albedo: vec3<f32>,
    wd: f32,
    ws: f32,
    // (v217) Ombre d'un joueur sur ce pixel (soleil ou torches, 0 à 1), pour l'anticrénelage
    // (voir `gb_out`).
    pshadow: f32,
};

fn shade_with(i: VOut, reflections: bool, cached: bool, cheap: bool) -> vec4<f32> {
    return shade_full(i, reflections, cached, cheap).color;
}

/// Coordonnées de texture continues d'une face (1 bloc = 1 texture), sauf faces penchées
/// (`TEX_LOCAL_UV`) : les leurs.
fn block_uv(i: VOut) -> vec2<f32> {
    let an = abs(face_normal(i.face));
    return select(
        select(
            select(vec2<f32>(i.world.x, -i.world.y), vec2<f32>(i.world.x, i.world.z), an.y > 0.5),
            vec2<f32>(i.world.z, -i.world.y),
            an.x > 0.5,
        ),
        i.uv,
        (i.tex & 8192u) != 0u,
    );
}

/// `reflections` = false : blocs sans reflet (la grande majorité) ; le compilateur retire
/// tout le code des reflets, le shader est bien plus léger et la carte va plus vite.
fn shade_full(i: VOut, reflections: bool, cached: bool, cheap: bool) -> Shaded {
    return shade_rt(i, reflections, cached, cheap, true);
}

/// `shade_full`, avec (`rt`) ou sans le code du raytracing : constante à la compilation,
/// comme les autres choix (le compilateur de DirectX retire ce qui ne sert pas).
fn shade_rt(i: VOut, reflections: bool, cached: bool, cheap: bool, rt: bool) -> Shaded {
    let n = face_normal(i.face);
    let p = i.world - n * 0.001;            // légèrement à l'intérieur du bloc
    let cell = vec3<i32>(floor(p));
    let to_frag = i.world - g.cam_pos.xyz;
    let dist = length(to_frag);
    let fog = smoothstep(g.fog.x, g.fog.y, dist);

    // Texture du pack : coordonnées tirées de la position monde (1 bloc = 1 texture),
    // dérivées calculées avant tout branchement, sur les coordonnées continues
    // (pas de couture de mipmap aux bords des blocs).
    let raw = block_uv(i);
    let layer = i.tex & 4095u;
    // Torche (TEX_SHARP, bit 14) : toujours en pleine résolution, sinon elle s'efface de loin.
    let sharp = select(1.0, 0.0, (i.tex & 16384u) != 0u);
    let tex = textureSampleGrad(blocks, block_sampler, fract(raw), layer, dpdx(raw) * sharp, dpdy(raw) * sharp);

    // Flamme : émissive, cœur plus clair, légère ondulation.
    if (i.tex & 4096u) != 0u {
        let t = g.cam_pos.w;
        let wobble = 0.85 + 0.15 * sin(t * 11.0 + i.world.y * 20.0 + f32(cell.x * 7 + cell.z * 3));
        let core = clamp(1.0 - (fract(i.world.y) - 0.5) * 3.2, 0.0, 1.0);
        let flame = lin(i.light) * 1.3 * wobble + lin(i.light) * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
        var fc = flame;
        if fog > 0.0 {
            fc = mix(flame, sky_color(to_frag / dist), fog * 0.6);
        }
        return Shaded(vec4<f32>(fc, 1.0), vec3<f32>(0.0), 0.0, 0.0, 0.0);
    }
    let alpha = tex.a;
    let albedo = tex.rgb;

    // Éclairage : soleil diffus + ambiant (selon l'heure), modulé par l'occlusion ambiante.
    let sun = normalize(g.sun.xyz);
    var diffuse = max(dot(n, sun), 0.0);
    // Ombres des joueurs : soleil caché par un joueur, torches cachées (plus bas). Le
    // soleil ne fait d'ombre que là où il arrive : sous le ciel ouvert (lumière du ciel
    // pleine, 15). Sous des blocs (toit, surplomb, grotte), il n'y a pas d'ombre du joueur.
    let shadow_from = i.world + n * 0.02;
    let shadows = g.shadow_info.x > 0.5;
    let open_sky = smoothstep(0.94, 0.995, i.sky);
    var pshadow = 0.0;
    // (v217) Aussi sans aucune boîte exacte : la carte du soleil peut avoir des joueurs.
    if (shadows || g.psm_info.x > 0.5) && diffuse > 0.0 && open_sky > 0.0 {
        // (v217) Les joueurs au-delà des boîtes exactes : par la carte du soleil.
        let ps = max(player_shadow(shadow_from, sun, 64.0), psm_shadow(shadow_from));
        diffuse *= 1.0 - 0.85 * open_sky * ps;
        pshadow = ps * open_sky;
    }
    let ao = mix(0.42, 1.0, i.ao * i.ao);
    let day = g.light.x;
    // Lumière du ciel atténuée sous les toits et dans les grottes (courbe ~ quadratique,
    // comme une perception de luminosité), plus un minimum pour ne pas être tout noir.
    let sky_amount = i.sky * i.sky;
    let sky_light = ((g.sun.w + (1.0 - g.sun.w) * diffuse) * day + 0.04 * (1.0 - day)) * sky_amount + 0.012;
    // Lumière colorée des torches, avec un léger scintillement par bloc.
    let t = g.cam_pos.w;
    let flicker = 0.93 + 0.07 * sin(t * 9.0 + hash3(cell / vec3<i32>(4)) * 6.28);
    var torch = pow(i.light, vec3<f32>(2.0)) * g.light.y * flicker * mix(0.55, 1.0, i.ao);
    torch *= torch_keep(i.world);
    let lit = max(i.light.x, max(i.light.y, i.light.z)) > 0.05;
    var ts = vec4<f32>(0.0);
    if shadows && (lit || g.held_pos.w > 0.0) {
        ts = torch_shadow(shadow_from);
        pshadow = max(pshadow, max(max(ts.r, ts.g), max(ts.b, ts.a)));
    }
    if lit {
        torch = max(torch - ts.rgb * flicker, torch * 0.35);
    }
    // Lumière de la torche tenue en main (dynamique) : décroît avec la distance, plus forte
    // sur les faces tournées vers le joueur ; un joueur entre elle et la face y fait son
    // ombre, comme pour les torches posées.
    if g.held_pos.w > 0.0 {
        let to_l = g.held_pos.xyz - i.world;
        let d = length(to_l);
        let f = clamp(1.0 - d / g.held_pos.w, 0.0, 1.0);
        let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
        let held = g.held_color.rgb * f * f * facing * flicker * mix(0.6, 1.0, i.ao);
        torch = max(torch, held * select(1.0, 0.35, ts.a > 0.5));
    }
    // Lumière des torches renvoyée par les miroirs (raytracing).
    if rt && g.rt.x > 0.5 && g.light.w > 0.0 {
        torch = max(torch, mirror_light(i.world + n * 0.01, n) * flicker * mix(0.55, 1.0, i.ao));
    }
    var color = albedo * (sky_light * ao + torch);
    // Lumières dynamiques (éditeur de lumières, voir dynlights.wgsl) : lumière diffuse,
    // et leurs reflets ajoutés après ceux du raytracing (un métal les prend à sa couleur).
    // Éclairage différé (opaques) : la même chose, ajoutée après coup avec ces poids.
    var dyn_spec = vec3<f32>(0.0);
    var wd = mix(0.6, 1.0, ao);
    if dyn_active() {
        // (v198) Dans l'image d'un miroir (passe coupée à son plan), les tuiles de l'écran
        // ne disent rien : toutes les lumières, vues de l'œil reflété.
        var dl: DynOut;
        if any(g.clip.xyz != vec3<f32>(0.0)) {
            dl = dyn_light_all(i.world, n, g.cam_pos.xyz, i.reflect, false, i.clip.xy);
        } else {
            dl = dyn_light(i.world, n, g.cam_pos.xyz, i.reflect, false, i.clip.xy, g.screen.xy);
        }
        color += albedo * dl.diffuse * wd;
        dyn_spec = select(dl.specular, dl.specular * albedo * 1.6, i.short != 0u);
    }

    // Bloc visé légèrement éclairci (le contour est dessiné à part, en lignes).
    // Seulement ses propres faces (point pris juste derrière la face), pas celles des
    // voisins qui le touchent et qu'on voit à travers du verre.
    let eps = vec3<f32>(0.0005);
    if g.sel_min.w > 0.5 && all(p >= g.sel_min.xyz - eps) && all(p <= g.sel_max.xyz + eps) {
        color = color * 1.25 + 0.02;
        wd *= 1.25;
    }

    // Reflets : miroir, métal, glace, eau. Avec le raytracing, un rayon traverse la grille de
    // blocs autour du joueur ; sans, seul le ciel se reflète.
    // Au loin, le reflet s'efface doucement, comme le brouillard de fin de vue : de 60 % à
    // 100 % de 4 chunks (≈ 38 → 64 blocs) ; au-delà la surface est opaque (et aucun rayon
    // n'est lancé : plus de FPS).
    let rt_fade = 1.0 - smoothstep(refl_range() * 0.6, refl_range(), dist);
    // (v190) Miroir redessiné à part (sans rayon) : jusqu'à la pleine portée, quelle que
    // soit la qualité du raytracing.
    let mirror_fade = 1.0 - smoothstep(REFLECT_RANGE * 0.6, REFLECT_RANGE, dist);
    // (v190) Dans l'image d'un miroir (passe coupée à son plan), un miroir sans image à lui
    // (au fond d'un couloir de miroirs) est sombre, sans rayon.
    let in_mirror = any(g.clip.xyz != vec3<f32>(0.0));
    let mirror_drawn = i.reflect > 0.99 && i.short == 0u && (g.mirror_count.x > 0.5 || in_mirror);
    if reflections && i.reflect > 0.0 && max(rt_fade, select(0.0, mirror_fade, mirror_drawn)) > 0.0 {
        let v = to_frag / max(dist, 1e-4);
        let r = reflect(v, n);
        var env: vec3<f32>;
        var cover = 1.0;
        // Métal (`short`, fer, or, diamant). (v223) Un vrai miroir, de près seulement
        // (« le reflet complet, comme un miroir ; 2 blocs, il commence à disparaître à 1,75 ») :
        // l'image redessinée de son plan (planar.rs, `METAL_RANGE`), teintée de sa couleur,
        // qui s'efface de `METAL_FADE` à `METAL_GONE` blocs de la caméra ; au-delà, rien (le
        // reflet des objets proches lancé en rayon, jusqu'à la v222, n'existe plus).
        let short = i.short != 0u;
        let metal_fade = 1.0 - smoothstep(METAL_FADE, METAL_GONE, dist);
        // (v190) Miroir redessiné à part : son image, sans lancer de rayon.
        var planar = vec4<f32>(0.0);
        if (i.reflect > 0.99 && !short) || (short && metal_fade > 0.0) {
            planar = planar_mirror(i.world, n);
        }
        let deepest = in_mirror && i.reflect > 0.99 && !short && planar.a < 0.5;
        var reflect_fade = select(rt_fade, mirror_fade, planar.a > 0.5 || deepest);
        if short {
            reflect_fade = metal_fade;
        }
        if short {
            env = planar.rgb;
            cover = planar.a;
        } else if planar.a > 0.5 {
            env = planar.rgb;
        } else if deepest {
            env = vec3<f32>(0.0);
        } else if rt_fade <= 0.0 {
            // Miroir pas redessiné, trop loin pour un rayon : opaque.
            env = vec3<f32>(0.0);
            cover = 0.0;
        } else if rt && g.rt.x > 0.5 {
            // Un seul appel à `trace` (le compilateur de DirectX recopie tout à chaque appel).
            var found = false;
            if cached {
                let c = cached_reflection(i.world + n * 0.02);
                env = c.rgb;
                found = c.a > 0.5;
            }
            // Verre et glace (passe translucide, souvent plusieurs épaisseurs l'une
            // derrière l'autre) : pas de rayon par pixel et par épaisseur (des images de
            // 100 ms devant de grandes verrières) ; le ciel, selon ce que la face en voit.
            // L'eau (face du dessus) garde son vrai reflet.
            if !found && cheap && i.face != 2u {
                env = sky_env(r);
                cover = i.sky * i.sky;
                found = true;
            }
            if !found {
                let e = trace(i.world + n * 0.02, r);
                env = e.rgb;
                cover = e.a;
            }
        } else {
            env = sky_env(r);
        }
        // (v200) Joueurs et objets reflétés dans l'eau (image faite à part, sans rayon) :
        // par-dessus le reste du reflet, sur le dessus de l'eau de cette nappe. (v202) Toute
        // la case de la nappe : une eau qui s'étale y a toutes les hauteurs (1/9 à 8/9, coins
        // en pente), son reflet pris au plan le plus vu.
        let water_cell = floor(g.mirror_count.y - 0.001);
        if g.mirror_count.z > 0.5 && i.face == 2u && i.world.y > water_cell - 0.12
            && i.world.y < water_cell + 0.95 {
            let w = water_mirror(i.world);
            env = mix(env, w.rgb, w.a);
            cover = max(cover, w.a);
        }
        let fresnel = pow(1.0 - max(dot(-v, n), 0.0), 5.0);
        var k = select(i.reflect * (0.55 + 0.45 * fresnel), 0.96, i.reflect > 0.99) * reflect_fade * cover;
        if short {
            // (v223) Métal poli : presque tout le reflet, comme un miroir.
            k = METAL_MIRROR * reflect_fade * cover;
        }
        // Métal : le reflet prend la couleur du métal (fer gris, or jaune, diamant bleu) au
        // lieu de la remplacer : le bloc garde sa teinte et ne paraît pas transparent.
        let tinted = select(env, env * albedo * 1.6, i.short != 0u);
        color = mix(color, tinted, k);
        wd *= 1.0 - k;
        // Points lumineux : chaque source proche se reflète (couleur de sa flamme), net
        // sur un miroir, plus étalé sur le métal ; avec ou sans raytracing. (Pas sur un
        // miroir redessiné : les flammes sont déjà dans son image.)
        if (g.rt.x < 0.5 || i.reflect < 0.99) && planar.a < 0.5 && !deepest {
            color += specular(i.world, r, i.reflect, i.light, rt) * flicker * reflect_fade;
        }
    }

    color += dyn_spec;

    // Brouillard : le lointain se fond dans le ciel tel qu'on le voit derrière lui dans la
    // direction du regard (calculé seulement là où il y a du brouillard : les pixels proches
    // n'en ont pas). (v216) Le ciel entier, avec le soleil, la lune, les étoiles et les
    // nuages : avant, une montagne lointaine, déjà fondue dans le brouillard donc invisible,
    // gardait la couleur du ciel vide et cachait encore le soleil (il « passait derrière »
    // un relief qu'on ne voyait pas), et elle aurait découpé sa silhouette dans les nuages.
    if fog > 0.0 {
        color = mix(color, sky_shade(to_frag / dist).rgb, fog);
    }
    return Shaded(vec4<f32>(color, alpha), albedo, wd * (1.0 - fog), 1.0 - fog, pshadow);
}

// ---------------------------------------------------------------- ciel

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) vi: u32) -> SkyOut {
    // Triangle plein écran.
    let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    let ndc = uv * 2.0 - 1.0;
    var o: SkyOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.ndc = ndc;
    return o;
}

/// Direction du regard pour ce point de l'écran. Tirée des lignes de la matrice de vue
/// (la direction `d` telle que sa projection tombe sur `ndc`) : ni inversion de matrice ni
/// soustraction de la position de la caméra, dont les erreurs d'arrondi faisaient parfois
/// tourner ou sauter le ciel selon l'endroit et l'angle.
fn sky_dir(ndc: vec2<f32>) -> vec3<f32> {
    let m = g.view_proj;
    let rx = vec3<f32>(m[0][0], m[1][0], m[2][0]);
    let ry = vec3<f32>(m[0][1], m[1][1], m[2][1]);
    let rw = vec3<f32>(m[0][3], m[1][3], m[2][3]);
    // clip.x = rx·d, clip.y = ry·d, clip.w = rw·d ; on veut clip.xy = ndc × clip.w.
    let d = cross(rx - ndc.x * rw, ry - ndc.y * rw);
    // Du bon côté : devant la caméra (clip.w > 0).
    return normalize(d * select(-1.0, 1.0, dot(rw, d) >= 0.0));
}

@fragment
fn fs_sky_mv(i: SkyOut) -> MvOut {
    let dir = sky_dir(i.ndc);
    // Point à l'infini : seule la rotation de la caméra le déplace.
    return MvOut(sky_shade(dir), motion_of(vec4<f32>(dir, 0.0)));
}

@fragment
fn fs_sky(i: SkyOut) -> @location(0) vec4<f32> {
    return sky_shade(sky_dir(i.ndc));
}

// ---------------------------------------------------------------- contour du bloc visé

/// Les 12 arêtes de la boîte visée (24 sommets, lignes), un rien plus grande que le bloc.
@vertex
fn vs_outline(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Coin : bit 0 = x, bit 1 = y, bit 2 = z. Arêtes 0-3 selon x, 4-7 selon y, 8-11 selon z.
    let edge = vi / 2u;
    let end = vi % 2u;
    var c: u32;
    if edge < 4u {
        // selon x : (y, z) fixés
        let yz = edge;
        c = select(0u, 1u, end == 1u) | ((yz & 1u) << 1u) | ((yz >> 1u) << 2u);
    } else if edge < 8u {
        let xz = edge - 4u;
        c = (xz & 1u) | (select(0u, 1u, end == 1u) << 1u) | ((xz >> 1u) << 2u);
    } else {
        let xy = edge - 8u;
        c = (xy & 1u) | ((xy >> 1u) << 1u) | (select(0u, 1u, end == 1u) << 2u);
    }
    let lo = g.sel_min.xyz - vec3<f32>(0.002);
    let hi = g.sel_max.xyz + vec3<f32>(0.002);
    let pos = vec3<f32>(
        select(lo.x, hi.x, (c & 1u) != 0u),
        select(lo.y, hi.y, (c & 2u) != 0u),
        select(lo.z, hi.z, (c & 4u) != 0u),
    );
    return g.view_proj * vec4<f32>(pos, 1.0);
}

@fragment
fn fs_outline() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.7);
}

@fragment
fn fs_outline_mv() -> MvOut {
    return MvOut(vec4<f32>(0.0, 0.0, 0.0, 0.7), vec2<f32>(0.0));
}

// ---------------------------------------------------------------- raytracing (reflets)

const VOX: vec3<i32> = vec3<i32>(128, 64, 128);

/// Case de la grille : bits 0..11 = bloc (0 = vide), bits 12..15 = lumière du ciel
/// (0..15) ; 0xFFFF = hors de la grille.
fn voxel(c: vec3<i32>) -> u32 {
    // Sans retour anticipé (compilateur DirectX).
    let l = c - vec3<i32>(g.rt.yzw);
    let inside = all(l >= vec3<i32>(0)) && all(l < VOX);
    let raw = textureLoad(voxels, clamp(l, vec3<i32>(0), VOX - vec3<i32>(1)), 0).r;
    return select(0xffffu, raw, inside);
}

/// Reflet des sources de lumière dans la direction réfléchie `r` depuis `p`.
/// `face_light` : lumière des torches reçue par la face (propagée case par case, donc
/// arrêtée par les murs) : une source dont la lumière n'arrive pas jusqu'ici est cachée
/// derrière un mur, son reflet n'apparaît pas.
fn specular(p: vec3<f32>, r: vec3<f32>, k: f32, face_light: vec3<f32>, rt: bool) -> vec3<f32> {
    let m = max(face_light.x, max(face_light.y, face_light.z));
    let lit2 = m * m;
    let shin = exp2(mix(5.0, 13.0, k));
    let gain = mix(0.6, 6.0, k * k);
    var c = vec3<f32>(0.0);
    // (v165) Le point le plus fort, vérifié dans la grille de blocs (raytracing) : une
    // lampe derrière un mur ne s'y reflète plus. Les autres gardent l'estimation par la
    // lumière reçue (un seul rayon par pixel : le compilateur de DirectX).
    var best = vec3<f32>(0.0);
    var best_at = vec3<f32>(0.0);
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let lp = g.lights_pos[j];
        let to_l = lp.xyz - p;
        let d = length(to_l);
        let reach = g.lights_col[j].w;
        let f = clamp(1.0 - d / reach, 0.0, 1.0);
        if f <= 0.0 {
            continue;
        }
        // Source étendue : on élargit le lobe selon sa taille apparente.
        let size = max(lp.w, 0.25) / max(d, 0.3);
        let cosr = max(dot(r, to_l / d), 0.0);
        // (v199) Hors du lobe (cosr^e < 2⁻¹⁶, invisible même × gain × couleur) : rien à
        // ajouter, ni la suite. Même image, la plupart des pixels s'arrêtent là.
        let lg = min(shin, 2.0 / (size * size)) * log2(max(cosr, 1e-30));
        if lg < -16.0 {
            continue;
        }
        let s = exp2(lg);
        // Sans obstacle, la lumière de cette source (−25 % par case, en ligne brisée : au
        // plus √3 fois la distance) arrive ici à au moins ~0,75^(1,75 (d + 0,7)) (au carré
        // ici) ; bien moins : elle contourne un mur, la source est cachée.
        let direct = pow(0.75, 3.5 * (d + 0.7));
        let seen = smoothstep(0.15, 0.6, lit2 / max(direct, 1e-6));
        let add = g.lights_col[j].rgb * s * f * f * gain * seen;
        c += add;
        if dot(add, vec3<f32>(1.0)) > dot(best, vec3<f32>(1.0)) {
            best = add;
            best_at = lp.xyz;
        }
    }
    if rt && g.rt.x > 0.5 && dot(best, vec3<f32>(1.0)) > 0.003 && !light_clear(p, best_at) {
        c = max(c - best, vec3<f32>(0.0));
    }
    if g.held_pos.w > 0.0 {
        let to_l = g.held_pos.xyz - p;
        let d = length(to_l);
        let f = clamp(1.0 - d / g.held_pos.w, 0.0, 1.0);
        let size = 0.1 / max(d, 0.3);
        let s = pow(max(dot(r, to_l / max(d, 1e-3)), 0.0), min(shin, 2.0 / (size * size)));
        c += g.held_color.rgb * s * f * f * gain;
    }
    return c;
}

/// Lumière colorée reçue en `p` (normale `n`) des sources proches, sans ombres, plus celle
/// que les miroirs renvoient. (v165) Une source derrière la face (de l'autre côté de son
/// plan : derrière le mur dont c'est la face) ne l'éclaire pas : dans les reflets, les
/// murs ne s'allument plus des lampes qui sont de l'autre côté.
fn point_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    var c = vec3<f32>(0.0);
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let to_l = g.lights_pos[j].xyz - p;
        let d = length(to_l);
        let facing = dot(n, to_l / max(d, 1e-3));
        let front = select(0.0, 1.0, facing > -0.05);
        c = max(c, g.lights_col[j].rgb * torch_falloff(d) * (max(facing, 0.0) * 0.25 + 0.75) * front);
    }
    return max(c, mirror_light(p, n));
}

/// (v165) Rien de plein entre `p` et la source en `l` (grille de blocs du raytracing) :
/// seulement les cubes opaques arrêtent la lumière (pas les dalles, plantes, vitres,
/// feuillages ni torches). Hors de la grille : visible.
fn light_clear(p: vec3<f32>, l: vec3<f32>) -> bool {
    let to_l = l - p;
    let dist = length(to_l);
    let d = to_l / max(dist, 1e-4);
    var cell = vec3<i32>(floor(p));
    let goal = vec3<i32>(floor(l));
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - p) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var clear = true;
    // Borne qui dépend de la distance : le compilateur ne déroule pas la boucle.
    let steps = i32(min(dist * 1.8, 48.0)) + 2;
    for (var i = 0; i < steps; i++) {
        if t_max.x < t_max.y && t_max.x < t_max.z {
            cell.x += step.x;
            t_max.x += inv.x;
        } else if t_max.y < t_max.z {
            cell.y += step.y;
            t_max.y += inv.y;
        } else {
            cell.z += step.z;
            t_max.z += inv.z;
        }
        if all(cell == goal) || min(t_max.x, min(t_max.y, t_max.z)) > dist + 1.0 {
            break;
        }
        let raw = voxel(cell);
        let id = raw & 4095u;
        if raw != 0xffffu && id != 0u {
            let shp = textureLoad(palette, i32(id) + 4096, 0);
            let pal = textureLoad(palette, i32(id), 0);
            // Cube (forme 0), sans trous dans sa texture (bit 4), pas une torche.
            let cube = (shp.w & 3u) == 0u && (shp.w & 16u) == 0u && ((pal.w >> 8u) & 7u) == 0u;
            if cube {
                clear = false;
                break;
            }
        }
    }
    return clear;
}

/// Lumière de la torche tenue en main en `p` (même formule que sur les faces du monde) ;
/// sert aussi dans les reflets : le monde vu dans le miroir est éclairé par la torche.
/// Sans branchement (compilateur DirectX).
fn held_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let to_l = g.held_pos.xyz - p;
    let d = length(to_l);
    let f = clamp(1.0 - d / max(g.held_pos.w, 1e-3), 0.0, 1.0) * select(0.0, 1.0, g.held_pos.w > 0.0);
    let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
    return g.held_color.rgb * (f * f * facing);
}

/// Atténuation de la lumière des torches comme dans le monde : −25 % par case parcourue
/// (case par case, donc un peu plus loin qu'à vol d'oiseau), au carré dans le shader.
fn torch_falloff(d: f32) -> f32 {
    return select(0.0, pow(0.5625, d * 1.25), d < 15.0);
}

/// Palette : reflet du bloc (0 mat … 1 miroir).
fn block_reflect(id: u32) -> f32 {
    return f32((textureLoad(palette, i32(id), 0).w >> 4u) & 15u) / 15.0;
}

/// Lumière des torches renvoyée par les miroirs : chaque source a une image symétrique
/// derrière le plan d'un miroir ; elle n'éclaire `p` que si le trajet vers cette image
/// traverse vraiment une face de miroir (vérifié dans la grille de blocs).
fn mirror_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    // Sans retour anticipé ni `continue` : le compilateur DirectX (FXC) part en récursion
    // infinie sur cette fonction sinon.
    var c = vec3<f32>(0.0);
    let count = select(0u, u32(g.light.w), g.rt.x > 0.5);
    for (var j = 0u; j < count; j++) {
        let plane = g.mirror_plane[j];
        let to_l = g.mirror_pos[j].xyz - p;
        let d = length(to_l);
        let f = torch_falloff(d);
        let dir = select(select(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), plane.x > 0.5 && plane.x < 1.5), vec3<f32>(1.0, 0.0, 0.0), plane.x < 0.5);
        let pa = dot(p, dir);
        let la = dot(to_l, dir);
        // `p` du côté éclairé du miroir, segment vers l'image qui croise son plan sur une
        // face de miroir (vérifiée dans la grille de blocs).
        let t = (plane.y - pa) / select(la, 1e-5, abs(la) < 1e-5);
        // Tests bon marché d'abord : les lectures de la grille (2 par image) seulement pour
        // les pixels assez proches de l'image, du bon côté du miroir. Au-delà de 10 cases la
        // lumière renvoyée est invisible (< 0,2 %).
        var ok = d < 10.0 && (pa - plane.y) * plane.z > 0.001 && t > 0.0 && t < 1.0;
        if ok {
            let hit = p + to_l * t;
            let l = vec3<i32>(floor(hit - dir * plane.z * 0.5)) - vec3<i32>(g.rt.yzw);
            let inside = all(l >= vec3<i32>(0)) && all(l < VOX);
            let id = textureLoad(voxels, clamp(l, vec3<i32>(0), VOX - vec3<i32>(1)), 0).r & 4095u;
            let refl = (textureLoad(palette, i32(id), 0).w >> 4u) & 15u;
            ok = inside && id != 0u && refl == 15u;
        }
        let facing = max(dot(n, to_l / max(d, 1e-3)), 0.0) * 0.6 + 0.4;
        c = max(c, g.mirror_col[j].rgb * (f * facing * 0.9 * f32(ok)));
    }
    return c;
}

/// Torche murale : point (ou direction si `dir`) de la case vers le repère du modèle
/// droit (inverse de `wall_torch_xf` côté jeu). `code` : 1 droite, 2..5 murale.
fn torch_local(q: vec3<f32>, code: u32, dir: bool) -> vec3<f32> {
    if code == 1u {
        return q;
    }
    let k = select(1.0, 0.0, dir);
    var x = q.x;
    var z = q.z;
    if code == 2u {
        z = q.x;
        x = k - q.z;
    } else if code == 3u {
        x = q.z;
        z = k - q.x;
    } else if code == 5u {
        x = k - q.x;
        z = k - q.z;
    }
    let c = 0.9238795;
    let s = 0.3826834;
    let yp = q.y - 0.2 * k;
    let zp = z - 0.9375 * k;
    return vec3<f32>(x, yp * c - zp * s, yp * s + zp * c + 0.5 * k);
}

/// Rayon / boîte : (entrée, sortie, axe de la face d'entrée).
fn box_hit(o: vec3<f32>, d: vec3<f32>, mn: vec3<f32>, mx: vec3<f32>) -> vec3<f32> {
    let sd = select(vec3<f32>(-1.0), vec3<f32>(1.0), d >= vec3<f32>(0.0));
    let inv = sd / max(abs(d), vec3<f32>(1e-8));
    let a = (mn - o) * inv;
    let b = (mx - o) * inv;
    let t0 = min(a, b);
    let t1 = max(a, b);
    let tn = max(max(t0.x, t0.y), t0.z);
    let tf = min(min(t1.x, t1.y), t1.z);
    var axis = 0.0;
    if t0.y >= t0.x && t0.y >= t0.z {
        axis = 1.0;
    } else if t0.z >= t0.x && t0.z >= t0.y {
        axis = 2.0;
    }
    return vec3<f32>(tn, select(-1.0, tf, tf >= max(tn, 0.0)), axis);
}

/// Couleur (sRGB 0xRRGGBB) d'un pixel du skin du joueur : même dessin que
/// `player_model::skin` (sorte de pavé, face 0 devant 1 derrière 2 côté 3 dessus 4 dessous,
/// pixel u, v avec v = 0 en haut, largeur n de la face).
fn skin_color(kind: u32, face: u32, u: u32, v: u32, n: u32) -> u32 {
    let SKIN = 0xd6a684u;
    let SKIN_DARK = 0xc08f6du;
    let HAIR = 0x241c18u;
    let SHIRT = 0xb5523au;
    let SHIRT_DARK = 0x8f3f2cu;
    let PANTS = 0x5b6b3au;
    let SHOES = 0x5a3a22u;
    if kind == 0u {
        if face == 3u {
            return HAIR;
        }
        if face == 4u {
            return SKIN;
        }
        if face == 1u {
            return select(SKIN, HAIR, v <= 6u);
        }
        if face == 2u {
            return select(SKIN, HAIR, v <= 1u || (v <= 4u && (u <= 1u || u + 2u >= n)));
        }
        if v <= 1u || ((u == 0u || u == 7u) && v == 2u) {
            return HAIR;
        }
        if v == 4u && (u == 1u || u == 6u) {
            return 0xf2f2f2u;
        }
        if v == 4u && (u == 2u || u == 5u) {
            return 0x5a3a1eu;
        }
        if (u == 3u || u == 4u) && v == 5u {
            return SKIN_DARK;
        }
        if (u == 3u || u == 4u) && v == 6u {
            return 0x8a5a48u;
        }
        return SKIN;
    }
    if kind == 1u {
        if face == 0u && v == 0u && (u == 3u || u == 4u) {
            return SKIN;
        }
        return select(SHIRT, SHIRT_DARK, face <= 1u && v == 11u);
    }
    if kind == 2u {
        if face == 3u {
            return SHIRT;
        }
        if face == 4u {
            return SKIN;
        }
        return select(select(SHIRT, SHIRT_DARK, v == 3u), SKIN, v > 3u);
    }
    if face == 3u {
        return PANTS;
    }
    return select(PANTS, SHOES, face == 4u || v >= 9u);
}

/// Pixel du vrai skin (même calcul que `skin::Skin::color`) : partie 0 jambe gauche,
/// 1 jambe droite, 2 corps, 3 bras gauche, 4 bras droit, 5 tête ; face 0 devant,
/// 1 derrière, 2 côté droit, 3 côté gauche, 4 dessus, 5 dessous ; taille w × d. Couleur
/// sRGB et opacité (a < 0,5 : pixel vide, on garde le dessin par défaut).
fn skin_pixel(part: u32, face: u32, u: u32, v: u32, w: u32, d: u32) -> vec4<f32> {
    var base = vec2<u32>(0u, 0u);
    var over = vec2<u32>(32u, 0u);
    switch part {
        case 0u: { base = vec2<u32>(16u, 48u); over = vec2<u32>(0u, 48u); }
        case 1u: { base = vec2<u32>(0u, 16u); over = vec2<u32>(0u, 32u); }
        case 2u: { base = vec2<u32>(16u, 16u); over = vec2<u32>(16u, 32u); }
        case 3u: { base = vec2<u32>(32u, 48u); over = vec2<u32>(48u, 48u); }
        case 4u: { base = vec2<u32>(40u, 16u); over = vec2<u32>(40u, 32u); }
        default: {}
    }
    let ru = w - 1u - min(u, w - 1u);
    var l = vec2<u32>(d + u, d + v);
    switch face {
        case 1u: { l = vec2<u32>(2u * d + w + u, d + v); }
        case 2u: { l = vec2<u32>(u, d + v); }
        case 3u: { l = vec2<u32>(d + w + u, d + v); }
        case 4u: { l = vec2<u32>(d + ru, v); }
        case 5u: { l = vec2<u32>(d + w + ru, v); }
        default: {}
    }
    let top = textureLoad(skin_tex, min(over + l, vec2<u32>(63u)), 0);
    if top.a >= 0.5 {
        return top;
    }
    return textureLoad(skin_tex, min(base + l, vec2<u32>(63u)), 0);
}

fn srgb_hex(c: u32) -> vec3<f32> {
    return lin(vec3<f32>(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0);
}

/// Le joueur dans un reflet : pavé le plus proche touché avant `max_t`. Couleur finale
/// (déjà éclairée) en rgb, distance en w (< 0 : rien).
fn player_hit(o: vec3<f32>, d: vec3<f32>, max_t: f32) -> vec4<f32> {
    var best = vec4<f32>(0.0, 0.0, 0.0, -1.0);
    var bt = max_t;
    let count = i32(g.ray_info.x);
    for (var k = 0; k < 8; k++) {
        if k >= count {
            break;
        }
        let m = g.ray_inv[k];
        let lo = (m * vec4<f32>(o, 1.0)).xyz;
        let ld = (m * vec4<f32>(d, 0.0)).xyz;
        let mn = g.ray_min[k].xyz;
        let mx = g.ray_max[k].xyz;
        let h = box_hit(lo, ld, mn, mx);
        // Rayon parti de l'intérieur (pied dans l'eau…) : ignoré.
        if h.y < 0.0 || h.x <= 1e-3 || h.x >= bt {
            continue;
        }
        bt = h.x;
        let p = lo + ld * h.x;
        let ax = i32(h.z);
        let dax = select(select(ld.z, ld.y, ax == 1), ld.x, ax == 0);
        let s = dax < 0.0; // face du côté + du pavé
        let kind = u32(g.ray_min[k].w);
        var face = 0u;
        var u = 0.0;
        var v = 0.0;
        var n = 1.0;
        if ax == 2 {
            face = select(0u, 1u, s);
            u = select(mx.x - p.x, p.x - mn.x, s);
            v = mx.y - p.y;
            n = mx.x - mn.x;
        } else if ax == 0 {
            face = 2u;
            u = select(p.z - mn.z, mx.z - p.z, s);
            v = mx.y - p.y;
            n = mx.z - mn.z;
        } else {
            face = select(4u, 3u, s);
            u = p.x - mn.x;
            v = select(p.z - mn.z, mx.z - p.z, s);
            n = mx.x - mn.x;
        }
        var albedo: vec3<f32>;
        if kind == 4u || kind == 7u {
            let size = vec2<f32>(n, select(mx.y - mn.y, mx.z - mn.z, ax == 1));
            let layer = i32(g.ray_max[k].w);
            let rect = g.ray_uv[k];
            let st = clamp(vec2<f32>(u, v) / size, vec2<f32>(0.0), vec2<f32>(0.999));
            albedo = textureSampleLevel(blocks, block_sampler, mix(rect.xy, rect.zw, st), layer, 0.0).rgb;
        } else if kind >= 5u {
            albedo = srgb_hex(u32(g.ray_max[k].w));
        } else {
            let iu = u32(clamp(u, 0.0, n - 0.5));
            let iv = u32(max(v, 0.0));
            albedo = srgb_hex(skin_color(kind, face, iu, iv, u32(n + 0.5)));
            // Vrai skin du joueur : face numérotée comme `player_model::emit_box`.
            let part = u32(g.ray_max[k].w);
            if part > 0u {
                var sf = select(0u, 1u, s);
                if ax == 0 {
                    sf = select(3u, 2u, s);
                } else if ax == 1 {
                    sf = select(5u, 4u, s);
                }
                let w = u32(mx.x - mn.x + 0.5);
                let dd = u32(mx.z - mn.z + 0.5);
                let px = skin_pixel(part - 1u, sf, iu, iv, w, dd);
                if px.a >= 0.5 {
                    albedo = lin(px.rgb);
                }
            }
        }
        // Ombrage selon l'orientation de la face dans le monde, comme le modèle.
        let sg = select(-1.0, 1.0, s);
        let ln = select(select(vec3<f32>(0.0, 0.0, sg), vec3<f32>(0.0, sg, 0.0), ax == 1), vec3<f32>(sg, 0.0, 0.0), ax == 0);
        let wn = normalize((transpose(m) * vec4<f32>(ln, 0.0)).xyz);
        let shade = clamp(0.78 + 0.22 * wn.y + 0.06 * wn.x, 0.45, 1.0);
        // Flamme et bâton d'une torche (6) : leur propre lumière.
        let lit = select(g.ray_info.yzw, vec3<f32>(1.0), kind >= 6u);
        best = vec4<f32>(albedo * shade * lit, h.x);
    }
    return best;
}

/// Boîte d'un bloc non plein (voir `rt::shape_texel`) : coin bas et coin haut dans la case.
fn box_lo(b: u32) -> vec3<f32> {
    return vec3<f32>(f32(b & 15u), f32((b >> 4u) & 15u), f32((b >> 8u) & 15u)) / 16.0;
}
fn box_hi(b: u32) -> vec3<f32> {
    return (vec3<f32>(f32((b >> 12u) & 15u), f32((b >> 16u) & 15u), f32((b >> 20u) & 15u)) + 1.0) / 16.0;
}

/// Côté horizontal `s` d'un poteau : 0 +X, 1 −X, 2 +Z, 3 −Z.
fn side_dir(s: i32) -> vec3<i32> {
    return select(select(select(vec3<i32>(0, 0, -1), vec3<i32>(0, 0, 1), s == 2), vec3<i32>(-1, 0, 0), s == 1), vec3<i32>(1, 0, 0), s == 0);
}

/// Bras vers +X (`lo`, `hi`) tourné vers le côté `s`.
fn arm_lo(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(lo.z, lo.y, 1.0 - hi.x), vec3<f32>(lo.z, lo.y, lo.x), s == 2), vec3<f32>(1.0 - hi.x, lo.y, lo.z), s == 1), lo, s == 0);
}
fn arm_hi(lo: vec3<f32>, hi: vec3<f32>, s: i32) -> vec3<f32> {
    return select(select(select(vec3<f32>(hi.z, hi.y, 1.0 - lo.x), vec3<f32>(hi.z, hi.y, hi.x), s == 2), vec3<f32>(1.0 - lo.x, hi.y, hi.z), s == 1), hi, s == 0);
}

/// Un poteau se relie-t-il à la case `c` ? (bloc plein ou autre poteau, comme le maillage)
fn post_link(c: vec3<i32>) -> bool {
    let raw = voxel(c);
    let id = raw & 4095u;
    let kind = textureLoad(palette, i32(id) + 4096, 0).w & 3u;
    let torch = (textureLoad(palette, i32(id), 0).w >> 8u) & 7u;
    return raw != 0xffffu && id != 0u && torch == 0u && (kind == 0u || kind == 2u);
}

/// Boîte la plus proche touchée par le rayon dans un bloc non plein (dalle, escalier,
/// clôture, muret, vitre, panneau…) : x = distance (< 0 : rien), y = axe de la face.
fn shape_hit(o: vec3<f32>, d: vec3<f32>, cell: vec3<i32>, shp: vec4<u32>, t_in: f32) -> vec2<f32> {
    let lo = o - vec3<f32>(cell);
    let b0 = shp.x | ((shp.y & 255u) << 16u);
    let b1 = (shp.y >> 8u) | (shp.z << 8u);
    let kind = shp.w & 3u;
    let post = (shp.w >> 2u) & 3u;
    // Poteau : voisins auxquels il se relie (vitre isolée : en croix).
    var link = vec4<bool>(false);
    if kind == 2u {
        link = vec4<bool>(post_link(cell + side_dir(0)), post_link(cell + side_dir(1)), post_link(cell + side_dir(2)), post_link(cell + side_dir(3)));
        if post == 2u && !any(link) {
            link = vec4<bool>(true);
        }
    }
    var best = vec2<f32>(-1.0, 0.0);
    var bt = 1e9;
    for (var k = 0; k < 10; k++) {
        var mn = box_lo(b0);
        var mx = box_hi(b0);
        // (Genre 3 : plante en croix, dans la grille pour les ombres des lumières ; les
        // reflets la traversent.)
        var used = k == 0 && kind != 3u;
        if k == 1 {
            mn = box_lo(b1);
            mx = box_hi(b1);
            used = kind == 1u && b1 != 0u;
        } else if k >= 2 {
            let side = (k - 2) / 2;
            let second = (k - 2) % 2 == 1;
            // Clôture : deuxième traverse 6/16 plus haut ; muret et vitre : un seul bras.
            let lift = vec3<f32>(0.0, select(0.0, 0.375, second), 0.0);
            mn = arm_lo(box_lo(b1), box_hi(b1), side) + lift;
            mx = arm_hi(box_lo(b1), box_hi(b1), side) + lift;
            used = kind == 2u && link[side] && (!second || post == 0u);
        }
        if used {
            let h = box_hit(lo, d, mn, mx);
            if h.y >= 0.0 && h.x >= t_in - 1e-3 && h.x < bt {
                bt = h.x;
                best = vec2<f32>(h.x, h.z);
            }
        }
    }
    return best;
}

/// Flamme animée, comme celle des torches posées (`y` : hauteur dans le bloc).
fn flame_color(light: vec3<f32>, y: f32, cell: vec3<i32>) -> vec3<f32> {
    let t = g.cam_pos.w;
    let wobble = 0.85 + 0.15 * sin(t * 11.0 + y * 20.0 + f32(cell.x * 7 + cell.z * 3));
    let core = clamp(1.0 - (fract(y) - 0.5) * 3.2, 0.0, 1.0);
    return light * 1.3 * wobble + light * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
}

/// Couleur de flamme de la torche en `cell` (source la plus proche), en linéaire.
fn torch_flame(cell: vec3<i32>) -> vec3<f32> {
    let center = vec3<f32>(cell) + 0.5;
    var best = 1.5;
    var c = pow(vec3<f32>(1.0, 0.588, 0.216), vec3<f32>(2.2));
    let count = u32(g.light.z);
    for (var j = 0u; j < count; j++) {
        let d = length(g.lights_pos[j].xyz - center);
        if d < best && g.lights_pos[j].w > 0.0 {
            best = d;
            c = g.lights_col[j].rgb / 1.25;
        }
    }
    return c;
}

/// Surface touchée par un rayon (l'éclairage est calculé ensuite, une seule fois).
struct RayHit {
    // Couleur de la texture, ou couleur finale si `shade` < 0 (flamme, bloc lumineux, ciel).
    color: vec3<f32>,
    t: f32,
    normal: vec3<f32>,
    // Part renvoyée plus loin (miroir), 0 sinon.
    k: f32,
    // Ombrage selon la face (< 0 : couleur déjà finale).
    shade: f32,
    // Lumière propre ajoutée (bâton d'une torche éclairé par sa flamme).
    glow: vec3<f32>,
    // Lumière du ciel reçue (0 grotte … 1 ciel ouvert), comme les faces du monde.
    sky: f32,
};

/// Un segment de rayon case par case (DDA) depuis `o` dans la direction `d`.
/// Portée des reflets (blocs) : 4 chunks. Le rayon réfléchi ne va pas plus loin (chemin
/// total, rebonds compris), avec le même fondu vers le ciel que la fin de la vue ; et les
/// surfaces plus loin que ça de la caméra ne reflètent plus.
const REFLECT_RANGE: f32 = 64.0;
/// Portée des reflets réduite quand le GPU est à la peine (`screen.w`, 0 = pleine portée).
fn refl_range() -> f32 {
    return select(REFLECT_RANGE, clamp(g.screen.w, 4.0, REFLECT_RANGE), g.screen.w > 0.0);
}
/// (v223) Reflets des métaux : l'image redessinée de leur plan (comme un miroir), entière
/// jusqu'à `METAL_FADE` blocs de la caméra, effacée à `METAL_GONE` (aussi dans planar.rs :
/// `METAL_RANGE`, `METAL_FADE`) ; part du reflet sur le métal.
const METAL_FADE: f32 = 1.75;
const METAL_GONE: f32 = 2.0;
const METAL_MIRROR: f32 = 0.85;

fn march(o: vec3<f32>, d: vec3<f32>, max_t: f32) -> RayHit {
    var cell = vec3<i32>(floor(o));
    let step = vec3<i32>(sign(d));
    let inv = 1.0 / max(abs(d), vec3<f32>(1e-6));
    var t_max = (select(vec3<f32>(cell), vec3<f32>(cell) + 1.0, d > vec3<f32>(0.0)) - o) * sign(d) * inv;
    t_max = select(t_max, vec3<f32>(1e9), abs(d) < vec3<f32>(1e-6));
    var axis = 1;
    var t = 0.0;
    var sky_prev = 1.0;
    // Première itération : la case de départ elle-même (torche juste devant un miroir).
    for (var i = 0; i < 97; i++) {
        if i == 0 {
            // rien : on teste la case de départ
        } else if t_max.x < t_max.y && t_max.x < t_max.z {
            t = t_max.x;
            cell.x += step.x;
            t_max.x += inv.x;
            axis = 0;
        } else if t_max.y < t_max.z {
            t = t_max.y;
            cell.y += step.y;
            t_max.y += inv.y;
            axis = 1;
        } else {
            t = t_max.z;
            cell.z += step.z;
            t_max.z += inv.z;
            axis = 2;
        }
        // Au-delà de la portée des reflets : on s'arrête (ciel, fondu dans `trace`).
        if t > max_t {
            break;
        }
        let raw = voxel(cell);
        if raw == 0xffffu {
            break;
        }
        let id = raw & 4095u;
        // Ciel reçu par une face : celui de la case vide d'où arrive le rayon.
        let sky_cell = f32(raw >> 12u) / 15.0;
        let sky_face = select(sky_prev, sky_cell, i == 0);
        sky_prev = sky_cell;
        if id == 0u {
            continue;
        }
        let pal = textureLoad(palette, i32(id), 0);
        let torch = (pal.w >> 8u) & 7u;
        // Case de départ : seules les torches comptent (le rayon part de la surface).
        // (v251) Le verre (bit 11) est dans la grille pour teinter les lumières : les reflets
        // le traversent, comme avant.
        if (i == 0 && torch == 0u) || ((pal.w >> 11u) & 1u) == 1u {
            continue;
        }
        if torch != 0u {
            // Modèle de la torche : bâton (texture) et flamme (animée), droits ou penchés.
            let lo = torch_local(o - vec3<f32>(cell), torch, false);
            let ld = torch_local(d, torch, true);
            let hs = box_hit(lo, ld, vec3<f32>(0.4375, 0.0, 0.4375), vec3<f32>(0.5625, 0.5, 0.5625));
            let hf = box_hit(lo, ld, vec3<f32>(0.40625, 0.5, 0.40625), vec3<f32>(0.59375, 0.8125, 0.59375));
            let ok_s = hs.y >= 0.0 && hs.x >= t - 1e-4;
            let ok_f = hf.y >= 0.0 && hf.x >= t - 1e-4;
            if ok_f && (!ok_s || hf.x <= hs.x) {
                let lp = lo + ld * hf.x;
                return RayHit(flame_color(torch_flame(cell), lp.y, cell), hf.x, -d, 0.0, -1.0, vec3<f32>(0.0), 1.0);
            }
            if ok_s {
                let lp = lo + ld * hs.x;
                var uv = vec2<f32>(lp.x, lp.z);
                var layer = select(pal.z, pal.x, ld.y < 0.0);
                var shade = select(0.5, 1.0, ld.y < 0.0);
                if hs.z == 0.0 {
                    uv = vec2<f32>(lp.z, 1.0 - lp.y);
                    layer = pal.y;
                    shade = 0.62;
                } else if hs.z == 2.0 {
                    uv = vec2<f32>(lp.x, 1.0 - lp.y);
                    layer = pal.y;
                    shade = 0.8;
                }
                let albedo = textureSampleLevel(blocks, block_sampler, uv, i32(layer), 0.0).rgb;
                // Le bâton est éclairé par sa propre flamme.
                return RayHit(albedo, hs.x, -d, 0.0, shade, torch_flame(cell) * 0.9, sky_cell);
            }
            continue;
        }
        // Bloc non plein : la boîte touchée dans la case (sinon le rayon continue).
        let shp = textureLoad(palette, i32(id) + 4096, 0);
        var ht = t;
        var hax = axis;
        var sky_hit = sky_face;
        let partial = (shp.w & 3u) != 0u;
        if partial {
            let sh = shape_hit(o, d, cell, shp, t);
            if sh.x < 0.0 {
                continue;
            }
            ht = sh.x;
            hax = i32(sh.y);
            // Face dans la case (dessus d'une dalle) : éclairée par l'air de la case.
            sky_hit = max(sky_face, sky_cell);
        }
        // Point touché et coordonnées de texture comme pour les faces du monde.
        let p = o + d * ht;
        var uv = vec2<f32>(p.x, p.z);
        var layer = select(pal.z, pal.x, step.y < 0);
        var shade = select(0.5, 1.0, step.y < 0);
        if hax == 0 {
            uv = vec2<f32>(p.z, -p.y);
            layer = pal.y;
            shade = 0.62;
        } else if hax == 2 {
            uv = vec2<f32>(p.x, -p.y);
            layer = pal.y;
            shade = 0.8;
        }
        let fs = -vec3<f32>(step);
        let nrm = select(select(vec3<f32>(0.0, 0.0, fs.z), vec3<f32>(0.0, fs.y, 0.0), hax == 1), vec3<f32>(fs.x, 0.0, 0.0), hax == 0);
        // Miroir : texture nette (pleine résolution jusqu'à ~30 blocs, comme vue en
        // direct).
        let lod = clamp(log2(max(ht * 0.03, 1.0)), 0.0, 6.0);
        let texel = textureSampleLevel(blocks, block_sampler, fract(uv), i32(layer), lod);
        // Pixels transparents d'un bloc fin (rail, échelle) : le rayon passe.
        if partial && texel.a < 0.5 {
            continue;
        }
        let albedo = texel.rgb;
        if (pal.w & 1u) != 0u {
            return RayHit(albedo * 1.2, ht, nrm, 0.0, -1.0, vec3<f32>(0.0), 1.0); // bloc lumineux
        }
        let r = f32((pal.w >> 4u) & 15u) / 15.0;
        return RayHit(albedo, ht, nrm, select(0.0, 0.96, r > 0.99), shade, vec3<f32>(0.0), sky_hit);
    }
    return RayHit(sky_env(d), 1e9, vec3<f32>(0.0), 0.0, -1.0, vec3<f32>(0.0), 1.0);
}

/// Reflet vu depuis `o` dans la direction `d` : jusqu'à 4 rebonds de miroir en miroir.
/// (v223 : plus de reflet court des métaux ici, voir `METAL_FADE`.) Un seul appel à
/// `march` par shader : le compilateur de DirectX (FXC) met bien moins longtemps.
fn trace(o0: vec3<f32>, d0: vec3<f32>) -> vec4<f32> {
    var o = o0;
    var d = d0;
    var acc = vec3<f32>(0.0);
    var thr = 1.0;
    // Miroir dans le miroir : on continue tant que le reflet apporte encore quelque chose
    // (au plus 64 rebonds ; borne lue dans les uniformes pour que le compilateur ne
    // déroule pas la boucle).
    // Qualité adaptative (calculée côté CPU d'après le temps GPU) : nombre de rebonds dans
    // `screen.z`, portée dans `screen.w`.
    let max_bounces = max(i32(g.screen.z), 1);
    var travelled = 0.0;
    let range = refl_range();
    for (var bounce = 0; bounce < max_bounces; bounce++) {
        var h = march(o, d, range - travelled);
        // Le joueur (et ce qu'il tient), devant ce que le rayon a touché.
        let ph = player_hit(o, d, min(h.t, range - travelled));
        if ph.w > 0.0 {
            h = RayHit(ph.rgb, ph.w, -d, 0.0, -1.0, vec3<f32>(0.0), 1.0);
        }
        var color = h.color;
        if h.shade >= 0.0 {
            let p = o + d * h.t;
            // Même éclairage que les faces du monde : soleil et ciel atténués sous les toits
            // et dans les grottes, plus les torches.
            let day = g.light.x;
            let diffuse = max(dot(h.normal, normalize(g.sun.xyz)), 0.0);
            let sky_light = ((g.sun.w + (1.0 - g.sun.w) * diffuse) * day + 0.04 * (1.0 - day)) * h.sky * h.sky + 0.012;
            let lit = max(point_light(p + h.normal * 0.02, h.normal), held_light(p + h.normal * 0.02, h.normal));
            color = h.color * (sky_light * mix(1.0, h.shade, 0.35) + lit * 0.8 + h.glow * 0.5);
        }
        // Brouillard de fin de portée, comme en fin de vue : vers la couleur du ciel.
        let far = travelled + min(h.t, range);
        color = mix(color, sky_color(d), smoothstep(range * 0.6, range, far));
        travelled = far;
        if h.k <= 0.0 || bounce == max_bounces - 1 || thr * h.k < 0.02 || travelled >= range {
            acc += thr * color;
            return vec4<f32>(acc, 1.0);
        }
        acc += thr * (1.0 - h.k) * color;
        thr *= h.k;
        o = o + d * h.t + h.normal * 0.002;
        d = reflect(d, h.normal);
    }
    return vec4<f32>(acc, 1.0);
}


/// (v217) Part de l'ombre des joueurs de la carte du soleil en `p` (0 à 1) : un joueur y est
/// plus près du soleil que `p` le long de son rayon. Quatre texels voisins, pondérés par la
/// place du point (bord en pente sur un texel, qui ne scintille pas).
fn psm_shadow(p: vec3<f32>) -> f32 {
    if g.psm_info.x < 0.5 {
        return 0.0;
    }
    let c = g.psm_vp * vec4<f32>(p, 1.0);
    let uv = vec2<f32>(c.x * 0.5 + 0.5, 0.5 - c.y * 0.5);
    if any(uv <= vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) || c.z <= 0.0 || c.z >= 1.0 {
        return 0.0;
    }
    let size = g.psm_info.y;
    let t = uv * size - 0.5;
    let i0 = vec2<i32>(floor(t));
    let f = t - floor(t);
    let hi = vec2<i32>(i32(size) - 1);
    let z = c.z - g.psm_info.z;
    let s00 = select(0.0, 1.0, textureLoad(psm, clamp(i0, vec2<i32>(0), hi), 0) < z);
    let s10 = select(0.0, 1.0, textureLoad(psm, clamp(i0 + vec2<i32>(1, 0), vec2<i32>(0), hi), 0) < z);
    let s01 = select(0.0, 1.0, textureLoad(psm, clamp(i0 + vec2<i32>(0, 1), vec2<i32>(0), hi), 0) < z);
    let s11 = select(0.0, 1.0, textureLoad(psm, clamp(i0 + vec2<i32>(1, 1), vec2<i32>(0), hi), 0) < z);
    return mix(mix(s00, s10, f.x), mix(s01, s11, f.x), f.y);
}

/// Rayon `o + t d` (0 < t < `tmax`) qui touche une des boîtes des joueurs : 1, sinon 0.
/// Ne regarde que les boîtes à moins de 24 blocs de `o`.
fn player_shadow(o: vec3<f32>, d: vec3<f32>, tmax: f32) -> f32 {
    let n = u32(g.shadow_info.x);
    if n == 0u {
        return 0.0;
    }
    // Le rayon passe-t-il seulement près des boîtes ? (boule qui les contient toutes)
    let bm = g.shadow_info.yzw - o;
    let br = g.shadow_max[0].w;
    let bt = dot(bm, d);
    if bt < -br || dot(bm, bm) - bt * bt > br * br {
        return 0.0;
    }
    let inv = 1.0 / select(d, vec3<f32>(1e-6), abs(d) < vec3<f32>(1e-6));
    // (v209) Groupe par groupe (un joueur, en général) : un rayon qui passe loin de la boule
    // d'un groupe ne touche aucune de ses boîtes. Avant, chaque pixel essayait toutes les
    // boîtes (48 avec les bots) dès que le rayon passait dans la boule de toutes.
    let groups = g.shadow_spans[0].z;
    for (var q = 0u; q < groups; q++) {
        let s = g.shadow_groups[q];
        let oc = s.xyz - o;
        let tc = dot(oc, d);
        let r = s.w + 0.25;
        if tc < -r || dot(oc, oc) - tc * tc > r * r {
            continue;
        }
        let span = g.shadow_spans[q];
        for (var k = span.x; k < span.y; k++) {
            let mn = g.shadow_min[k].xyz;
            let mx = g.shadow_max[k].xyz;
            let c = (mn + mx) * 0.5;
            if dot(c - o, c - o) > 576.0 {
                continue;
            }
            // Des blocs cachent la boîte du soleil : elle n'en fait pas d'ombre. (Un point
            // du monde dans la boîte, le sol sous les pieds posés, est dans son ombre : pas
            // de trou clair à la place des pieds.)
            if fract(g.shadow_min[k].w) > 0.25 {
                continue;
            }
            let t0 = (mn - o) * inv;
            let t1 = (mx - o) * inv;
            let near = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
            let far = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
            if far >= max(near, 0.0) && near < tmax {
                return 1.0;
            }
        }
    }
    return 0.0;
}

/// Lumière d'une source cachée par un joueur : chaque boîte ne fait de l'ombre que pour
/// sa source la plus proche (choisie par le jeu), un seul essai par boîte voisine. rgb :
/// lumière des torches posées retirée ; a = 1 : la torche tenue en main est cachée.
fn torch_shadow(p: vec3<f32>) -> vec4<f32> {
    var c = vec4<f32>(0.0);
    let n = u32(g.shadow_info.x);
    // Trop loin de toutes les boîtes (boule qui les contient) : aucune ombre possible.
    let bm = g.shadow_info.yzw - p;
    let br = g.shadow_max[0].w + 6.0;
    if n == 0u || dot(bm, bm) > br * br {
        return c;
    }
    // (v209) Groupe par groupe : une boîte n'agit qu'à moins de 6 blocs, un groupe plus
    // loin que ça de toutes ses boîtes est sauté d'un seul essai.
    let groups = g.shadow_spans[0].z;
    for (var q = 0u; q < groups; q++) {
        let s = g.shadow_groups[q];
        let gm = s.xyz - p;
        let gr = s.w + 6.25;
        if dot(gm, gm) > gr * gr {
            continue;
        }
        let span = g.shadow_spans[q];
        for (var k = span.x; k < span.y; k++) {
            let j = i32(floor(g.shadow_min[k].w));
            let mn = g.shadow_min[k].xyz;
            let mx = g.shadow_max[k].xyz;
            let m = (mn + mx) * 0.5 - p;
            // (Un point du monde dans la boîte, le sol sous les pieds posés, est dans son
            // ombre : pas de trou clair à la place des pieds.)
            if j == -1 || dot(m, m) > 36.0 {
                continue;
            }
            let held = j == -2;
            let lp = select(g.lights_pos[max(j, 0)].xyz, g.held_pos.xyz, held);
            // Torche tenue dans la boîte (la main qui la tient) : pas d'ombre de celle-ci.
            if held && (g.held_pos.w <= 0.0 || (all(lp > mn) && all(lp < mx))) {
                continue;
            }
            let to_l = lp - p;
            let d = length(to_l);
            if d < 0.3 || d > 14.0 {
                continue;
            }
            let dir = to_l / d;
            let inv = 1.0 / select(dir, vec3<f32>(1e-6), abs(dir) < vec3<f32>(1e-6));
            let t0 = (mn - p) * inv;
            let t1 = (mx - p) * inv;
            let near = max(max(min(t0.x, t1.x), min(t0.y, t1.y)), min(t0.z, t1.z));
            let far = min(min(max(t0.x, t1.x), max(t0.y, t1.y)), max(t0.z, t1.z));
            if far >= max(near, 0.0) && near < d {
                if held {
                    c.a = 1.0;
                } else {
                    c = vec4<f32>(max(c.rgb, g.lights_col[j].rgb * torch_falloff(d)), c.a);
                }
            }
        }
    }
    return c;
}
