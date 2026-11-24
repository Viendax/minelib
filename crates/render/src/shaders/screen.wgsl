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
    // (v206) Apparition des boutons, de 0 à 1 (voir `screen.rs` côté jeu) ; (v212) une
    // seule pour tous, en `x` ; (v213) en `y`, les pages où aller : précédente (1),
    // suivante (2) — sans elles, retour et suivant sont grisés.
    @location(2) reveal: vec2<f32>,
    // (v243) Curseur (x, y, 0 aucun / 1 / 2 clavier dans la page) et onde d'un appui
    // (x, y, âge 0..1, négatif : aucune), dessinés au-dessus des boutons.
    @location(3) cursor: vec4<f32>,
    @location(4) ripple: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec3<f32>,
    @location(2) @interpolate(flat) reveal: vec2<f32>,
    @location(3) @interpolate(flat) cursor: vec4<f32>,
    @location(4) @interpolate(flat) ripple: vec4<f32>,
};

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    // (v254) Compté depuis la caméra (matrices du jeu sans sa translation).
    o.clip = g.view_proj * vec4<f32>(v.pos - g.cam_pos.xyz, 1.0);
    o.uv = v.uv;
    o.world = v.pos;
    o.reveal = v.reveal;
    o.cursor = v.cursor;
    o.ripple = v.ripple;
    return o;
}

fn texel(i: VOut) -> vec4<f32> {
    return vec4<f32>(textureSampleLevel(vid, vid_s, i.uv, 0.0).rgb, 1.0);
}

// Les boutons des écrans, **en haut à gauche** (v211) : un rond gris foncé avec une loupe
// (Google), et à sa droite un rond avec une vidéo (la recherche de YouTube), puis (v213)
// une flèche à gauche (page précédente) et une flèche à droite (page suivante). Ils
// apparaissent tous ensemble, du même fondu (`reveal.x`). Dessinés ici
// même (aucune image, aucune police, donc aucune licence ; la vidéo est un cadre arrondi
// et un triangle de lecture blancs, pas le logo de YouTube). Le jeu reconnaît les clics
// avec `screen::on_button`, qui garde les mêmes mesures (`BOUTONS`, `RAYON`).
//
// Les pages des écrans font 1280 × 720 : une longueur donnée en `u` vaut 16/9 de la même
// en `v`, c'est ce qui rend le rond rond. Centres à 38 px des bords, rayon 28 px, 12 px
// entre les deux.
const BTN_ASPECT = 16.0 / 9.0;
const BTN_C = vec2<f32>(0.030, 0.053);
const BTN2_C = vec2<f32>(0.0828, 0.053);
const BTN3_C = vec2<f32>(0.1356, 0.053);
const BTN4_C = vec2<f32>(0.1884, 0.053);
// Les flèches (en mesures de `u`) : demi-longueur du trait, et la pointe.
const FLECHE_L = 0.0078;
const POINTE = 0.0052;
const BTN_R = 0.022;
// La loupe : son anneau un peu en haut à gauche du centre du rond.
const LOUPE_D = vec2<f32>(-0.0035, -0.0035);
const LOUPE_R = 0.0092;
const TRAIT = 0.0026;
// La vidéo (en mesures de `u`) : demi-largeur et demi-hauteur du cadre, et son arrondi.
const CADRE = vec2<f32>(0.0112, 0.0080);
const CADRE_R = 0.0028;

// Distance d'un point à un segment (pour le manche de la loupe).
fn dist_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-9), 0.0, 1.0);
    return length(p - a - ab * t);
}

// Distance signée à un rectangle arrondi centré en 0.
fn dist_cadre(q: vec2<f32>, demi: vec2<f32>, r: f32) -> f32 {
    let d = abs(q) - demi + vec2<f32>(r, r);
    return length(max(d, vec2<f32>(0.0, 0.0))) + min(max(d.x, d.y), 0.0) - r;
}

// Distance (approchée) à un triangle de lecture pointant à droite, centré en 0.
fn dist_lecture(q: vec2<f32>) -> f32 {
    let a = vec2<f32>(-0.0030, -0.0040);
    let b = vec2<f32>(-0.0030, 0.0040);
    let c = vec2<f32>(0.0042, 0.0);
    let n1 = normalize(vec2<f32>(-1.0, 0.0));
    let n2 = normalize(vec2<f32>(b.y - c.y, c.x - b.x));
    let n3 = normalize(vec2<f32>(c.y - a.y, a.x - c.x));
    return max(dot(q - a, n1), max(dot(q - b, n2), dot(q - c, n3)));
}

// Distance à une flèche horizontale centrée en 0, vers la gauche (`sens` = -1) ou vers la
// droite (`sens` = 1) : un trait et deux branches de pointe.
fn dist_fleche(q: vec2<f32>, sens: f32) -> f32 {
    let bout = vec2<f32>(FLECHE_L * sens, 0.0);
    let d_trait = dist_seg(q, vec2<f32>(-FLECHE_L * sens, 0.0), bout);
    let haut = bout + vec2<f32>(-POINTE * sens, -POINTE);
    let bas = bout + vec2<f32>(-POINTE * sens, POINTE);
    let d_pointe = min(dist_seg(q, bout, haut), dist_seg(q, bout, bas));
    return min(d_trait, d_pointe) - TRAIT * 0.55;
}

// `uv` ramené en mesures rondes (celles de `u`).
fn btn_space(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x, uv.y / BTN_ASPECT);
}

// (v213) Un bouton dont le dessin est gris s'il n'a nulle part où aller (`actif` faux).
fn bouton_actif(p: vec2<f32>, c: vec2<f32>, d_icone: f32, reveal: f32, aa: f32, actif: bool, dessous: vec3<f32>) -> vec3<f32> {
    let d_rond = length(p - c) - BTN_R;
    let rond = 1.0 - smoothstep(-aa, aa, d_rond);
    let icone = 1.0 - smoothstep(-aa, aa, d_icone);
    let encre = select(vec3<f32>(0.36, 0.37, 0.39), vec3<f32>(0.92, 0.93, 0.95), actif);
    let fond = mix(dessous, vec3<f32>(0.11, 0.115, 0.125), rond * 0.93 * reveal);
    return mix(fond, encre, icone * rond * reveal);
}

// Un bouton : rond gris foncé, et son dessin (distance signée `d_icone`) presque blanc.
fn bouton(p: vec2<f32>, c: vec2<f32>, d_icone: f32, reveal: f32, aa: f32, dessous: vec3<f32>) -> vec3<f32> {
    let d_rond = length(p - c) - BTN_R;
    let rond = 1.0 - smoothstep(-aa, aa, d_rond);
    let icone = 1.0 - smoothstep(-aa, aa, d_icone);
    // Le tout apparaît et disparaît en fondu avec `reveal`.
    let fond = mix(dessous, vec3<f32>(0.11, 0.115, 0.125), rond * 0.93 * reveal);
    return mix(fond, vec3<f32>(0.92, 0.93, 0.95), icone * rond * reveal);
}

fn buttons(uv: vec2<f32>, reveal: vec2<f32>, dessous: vec3<f32>) -> vec3<f32> {
    let p = btn_space(uv);
    // Lissage sur la largeur d'un pixel de l'écran, quelle que soit la distance (calculé
    // avant tout test : une dérivée se prend partout pareil).
    let aa = max(length(fwidth(p)) * 0.7071, 1e-6);
    if reveal.x <= 0.001 {
        return dessous;
    }
    var col = dessous;
    // La loupe : un anneau et son manche.
    let c1 = btn_space(BTN_C);
    if reveal.x > 0.001 && length(p - c1) < BTN_R * 1.5 {
        let lc = c1 + LOUPE_D;
        let d_anneau = abs(length(p - lc) - LOUPE_R) - TRAIT * 0.5;
        let a = lc + vec2<f32>(0.7071, 0.7071) * (LOUPE_R + TRAIT * 0.2);
        let b = lc + vec2<f32>(0.7071, 0.7071) * (LOUPE_R + TRAIT * 0.2 + 0.0082);
        let d_manche = dist_seg(p, a, b) - TRAIT * 0.55;
        col = bouton(p, c1, min(d_anneau, d_manche), reveal.x, aa, col);
    }
    // La vidéo : un cadre arrondi et un triangle de lecture.
    let c2 = btn_space(BTN2_C);
    if reveal.x > 0.001 && length(p - c2) < BTN_R * 1.5 {
        let q = p - c2;
        let d_cadre = abs(dist_cadre(q, CADRE, CADRE_R)) - TRAIT * 0.5;
        col = bouton(p, c2, min(d_cadre, dist_lecture(q)), reveal.x, aa, col);
    }
    // Retour et suivant : des flèches, grisées sans page où aller.
    let pages = u32(reveal.y + 0.5);
    let c3 = btn_space(BTN3_C);
    if reveal.x > 0.001 && length(p - c3) < BTN_R * 1.5 {
        col = bouton_actif(p, c3, dist_fleche(p - c3, -1.0), reveal.x, aa, (pages & 1u) != 0u, col);
    }
    let c4 = btn_space(BTN4_C);
    if reveal.x > 0.001 && length(p - c4) < BTN_R * 1.5 {
        col = bouton_actif(p, c4, dist_fleche(p - c4, 1.0), reveal.x, aa, (pages & 2u) != 0u, col);
    }
    return col;
}

// (v243) Le curseur et l'onde d'un appui, en pixels de la page (1280 × 720), au-dessus de
// tout. Avant, le jeu les dessinait dans l'image de la page : sous les boutons, et toute
// l'image repartait à la carte graphique à chaque mouvement du réticule.
const PAGE_PX = vec2<f32>(1280.0, 720.0);

fn anneau(px: vec2<f32>, c: vec2<f32>, rayon: f32, epais: f32, couleur: vec3<f32>, force: f32, aa: f32, dessous: vec3<f32>) -> vec3<f32> {
    let d = abs(length(px - c) - rayon) - epais * 0.5;
    let a = (1.0 - smoothstep(-aa, aa, d)) * force;
    return mix(dessous, couleur, a);
}

fn marks(uv: vec2<f32>, cursor: vec4<f32>, ripple: vec4<f32>, dessous: vec3<f32>) -> vec3<f32> {
    let px = uv * PAGE_PX;
    let aa = max(length(fwidth(px)) * 0.7071, 1e-4);
    var col = dessous;
    // Onde : un cercle qui s'agrandit et s'efface (blanc vers le noir, comme avant).
    if ripple.z >= 0.0 {
        let t = clamp(ripple.z, 0.0, 1.0);
        let gris = pow(1.0 - t, 2.2);
        col = anneau(px, ripple.xy * PAGE_PX, 8.0 + 40.0 * t, 3.0, vec3<f32>(gris), 1.0, aa, col);
    }
    // Curseur : petit anneau blanc, bleu quand le clavier va dans la page (couleurs de la
    // page, sRGB, passées en linéaire comme la texture).
    if cursor.z > 0.5 {
        let bleu = vec3<f32>(0.0452, 0.402, 1.0);
        let c = select(vec3<f32>(1.0), bleu, cursor.z > 1.5);
        col = anneau(px, cursor.xy * PAGE_PX, 7.0, 2.0, c, 1.0, aa, col);
    }
    return col;
}

fn overlay(i: VOut, page: vec3<f32>) -> vec3<f32> {
    return marks(i.uv, i.cursor, i.ripple, buttons(i.uv, i.reveal, page));
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    return vec4<f32>(overlay(i, texel(i).rgb), 1.0);
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
    let p = vec4<f32>(i.world - g.cam_pos.xyz, 1.0);
    let jit = g.view_proj * p;
    let cur = g.cur_view_proj * p;
    let off = (jit.xy / jit.w - cur.xy / cur.w) * vec2<f32>(0.5, -0.5) * g.screen.xy;
    let uv = i.uv + dpdx(i.uv) * off.x + dpdy(i.uv) * off.y;
    o.color = vec4<f32>(overlay(i, textureSampleLevel(vid, vid_s, uv, 0.0).rgb), 1.0);
    o.motion = NO_HISTORY;
    return o;
}
