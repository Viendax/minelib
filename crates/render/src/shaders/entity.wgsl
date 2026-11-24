// Minelib — entités dans le monde (modèle du joueur) : position monde, couleur unie, et
// l'éclairage du lieu donné par le jeu. Avec l'anticrénelage, elles laissent aussi albédo et
// normale (tirée des dérivées de la position : les modèles sont faits de faces planes) :
// les lumières dynamiques leur sont ajoutées après coup (dyn_deferred.wgsl). gbuffer.wgsl
// et dynlights_off.wgsl sont ajoutés à la suite.

// Début des uniformes du monde : la caméra (avec le tremblement du TAA), puis les
// matrices sans tremblement (image courante et précédente) pour les vecteurs de mouvement.
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
    // (v190) Passe d'un miroir : seulement ce qui est devant son plan (voir world.wgsl).
    clip: vec4<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;

/// (v198) Ce qui dépasse derrière un miroir de moins que ça (blocs) reste dans son reflet :
/// c'est dans le bloc miroir lui-même (plein), donc forcément un joueur ou un objet qui s'y
/// enfonce un peu. Plus loin, c'est de l'autre côté du bloc : coupé.
const MIRROR_SLACK: f32 = 0.9;
@group(0) @binding(1) var blocks: texture_2d_array<f32>;
@group(0) @binding(2) var block_sampler: sampler;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) layer: i32,
    @location(4) light: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) layer: i32,
    @location(3) world: vec3<f32>,
    @location(4) light: vec4<f32>,
    // (v254) Place depuis la caméra, exacte : pour les dérivées (normale de la face).
    @location(5) rel: vec3<f32>,
};

/// Couleur du pixel sans l'éclairage : couleur du sommet, texturée pour un bloc tenu
/// (pixels transparents jetés).
fn texel(i: VOut) -> vec3<f32> {
    // Flamme d'une torche tenue (couche −2) : la même animation que la torche à la
    // première personne (viewmodel.wgsl) ; uv.y = hauteur dans le bloc de la torche.
    if i.layer == -2 {
        let y = i.uv.y;
        let c = i.color;
        let wobble = 0.85 + 0.15 * sin(g.cam_pos.w * 11.0 + y * 20.0);
        let core = clamp(1.0 - (y - 0.5) * 3.2, 0.0, 1.0);
        return c * 1.3 * wobble + c * core * 0.5 + vec3<f32>(0.25, 0.22, 0.18) * core;
    }
    if i.layer < 0 {
        return i.color;
    }
    let t = textureSampleLevel(blocks, block_sampler, i.uv, i.layer, 0.0);
    if t.a < 0.5 {
        discard;
    }
    return i.color * t.rgb;
}

/// Couleur éclairée par la lumière du lieu.
fn shade(i: VOut) -> vec3<f32> {
    return texel(i) * i.light.rgb * 2.0;
}

/// (v215) Brouillard au point `world`, comme celui des blocs : rien de près, tout au bout de
/// la vue, où l'entité (un joueur, ce qu'il tient) s'efface avec le relief au lieu de
/// rester dessinée dans le brouillard. Distance à la caméra, hauteur comprise.
fn entity_fog(world: vec3<f32>) -> f32 {
    return smoothstep(g.fog.x, g.fog.y, distance(world, g.cam_pos.xyz));
}

/// (v215) `c` dans le brouillard `fog` : vers le ciel derrière, comme les blocs (`sky_shade`,
/// ajouté à l'assemblage : voir `entity_wgsl` ; (v216) le ciel entier, nuages compris).
fn fogged(c: vec3<f32>, world: vec3<f32>, fog: f32) -> vec3<f32> {
    if fog <= 0.0 {
        return c;
    }
    return mix(c, sky_shade(normalize(world - g.cam_pos.xyz)).rgb, fog);
}

/// Normale de la face (vers la caméra), d'après les dérivées de la position : à calculer
/// avant tout branchement. (v254) La position depuis la caméra (`rel`) : celle du monde,
/// arrondie à 4 mm à 40 000 blocs, donnait des normales au hasard loin du centre du monde.
fn face_normal(rel: vec3<f32>) -> vec3<f32> {
    var n = cross(dpdx(rel), dpdy(rel));
    let len = length(n);
    n = select(vec3<f32>(0.0, 1.0, 0.0), n / max(len, 1e-12), len > 1e-12);
    return select(n, -n, dot(n, -rel) < 0.0);
}

@vertex
fn vs_main(v: VIn) -> VOut {
    var o: VOut;
    // (v254) Les matrices du jeu sont comptées depuis la caméra (sans sa translation) : le
    // point aussi. Loin du centre du monde (40 000 blocs), deux grands nombres voisins qui se
    // soustraient d'abord restent exacts ; multipliés par la matrice puis soustraits, ils
    // perdaient des millimètres et l'image tremblait.
    o.clip = g.view_proj * vec4<f32>(v.pos - g.cam_pos.xyz, 1.0);
    o.color = v.color;
    o.uv = v.uv;
    o.layer = v.layer;
    o.world = v.pos;
    o.rel = v.pos - g.cam_pos.xyz;
    o.light = v.light;
    return o;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    // (v190) Dans un miroir : rien de ce qui est derrière lui (zéro ailleurs : tout passe).
    // (v198) Sauf à moins d'un bloc : c'est encore dans le bloc miroir, donc un joueur
    // collé au miroir (sa tête, son bras, ce qu'il tient) ; coupé, son reflet avait des
    // trous.
    if dot(g.clip.xyz, i.world) + g.clip.w < -MIRROR_SLACK {
        discard;
    }
    return vec4<f32>(fogged(shade(i), i.world, entity_fog(i.world)), 1.0);
}

/// Avec l'anticrénelage : couleur, mouvement, et de quoi y ajouter les lumières dynamiques
/// (voir gbuffer.wgsl ; albédo.a = 1 : entité).
struct TaaOut {
    @location(0) color: vec4<f32>,
    @location(1) motion: vec2<f32>,
    @location(2) albedo: vec4<f32>,
    @location(3) surf: vec4<f32>,
};

fn taa_out(i: VOut, n: vec3<f32>, motion: vec2<f32>) -> TaaOut {
    let base = texel(i);
    // (v215) Dans le brouillard, les lumières dynamiques s'effacent avec lui (comme sur
    // les blocs) : `light.a` à 0, aucune sur cette entité.
    let fog = entity_fog(i.world);
    let lit = select(0.0, 1.0, i.light.a > 0.5) * (1.0 - fog);
    var o: TaaOut;
    o.color = vec4<f32>(fogged(base * i.light.rgb * 2.0, i.world, fog), 1.0);
    o.motion = motion;
    o.albedo = vec4<f32>(base, 1.0);
    o.surf = vec4<f32>(gb_oct_encode(n), lit, gb_pack_spec(0.5 * lit, false));
    return o;
}

// Avec l'anticrénelage temporel, le modèle du joueur local (il suit la caméra) : pas de
// vecteur de mouvement.
@fragment
fn fs_taa(i: VOut) -> TaaOut {
    let n = face_normal(i.rel);
    return taa_out(i, n, vec2<f32>(0.0));
}

// Le reste (têtes des cartes, autres joueurs, bombe, armes au sol) : fixé dans le monde,
// il bouge à l'écran comme les blocs (sinon l'anticrénelage le fait traîner). (v218) Plus un
// cinquantième de pixel (rien pour la reprojection) : ces modèles bougent souvent eux-mêmes
// (objets lâchés qui tombent, flottent et tournent), sans vecteur de mouvement à eux ;
// l'anticrénelage les traite comme près d'un objet qui bouge (bornes serrées, et l'endroit
// marqué pour les images suivantes) : plus de traînée derrière un objet lâché.
@fragment
fn fs_taa_world(i: VOut) -> TaaOut {
    let n = face_normal(i.rel);
    // (v254) Comptés depuis la caméra (voir `vs_main`).
    let p = vec4<f32>(i.rel, 1.0);
    let cur = g.cur_view_proj * p;
    let prev = g.prev_view_proj * p;
    let d = prev.xy / prev.w - cur.xy / cur.w;
    return taa_out(i, n, d * vec2<f32>(0.5, -0.5) * g.screen.xy + vec2<f32>(0.02, 0.0));
}
