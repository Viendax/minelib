// Minelib — (v172) les autres joueurs (foule, partie FPS, construction), dessinés comme le
// modèle du joueur local en F5 : les mêmes pixels du skin (cuits avec leur grain dans une
// image par skin), le même ombrage des faces et la lumière reçue là où ils sont, lumières
// dynamiques comprises (albédo et surface pour l'éclairage différé). Les images des skins
// sont en couleurs linéaires (demi-flottants). Un seul modèle (six
// pavés) envoyé une fois, et une instance par joueur : sa pose (pieds, regard, marche,
// accroupi) est appliquée ici, comme `player_model::parts` le fait pour le joueur local.
// Ajouté à la suite d'entity.wgsl (caméra, normale des faces, sorties de l'anticrénelage).

@group(1) @binding(0) var skins: texture_2d_array<f32>;
@group(1) @binding(1) var skin_sampler: sampler;

struct PlayerIn {
    // Sommet du modèle : position (pixels, repère de sa partie), coordonnées dans l'image
    // du skin, partie (0 jambe gauche, 1 jambe droite, 2 corps, 3 bras gauche, 4 bras
    // droit, 5 tête) et normale de sa face (repère de la partie).
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) part: u32,
    @location(3) normal: vec3<f32>,
    // Instance : pieds, (lacet, tangage), (phase de la marche, ampleur, accroupi), lumière
    // reçue (0..2), couche du skin (bits 0..15) et objet tenu (bit 16 : bras droit levé).
    @location(4) feet: vec3<f32>,
    @location(5) look: vec2<f32>,
    @location(6) motion: vec3<f32>,
    /// (v187) Sa pose à l'image d'avant (anticrénelage temporel) : pieds, regard, marche.
    @location(10) prev_feet: vec3<f32>,
    @location(11) prev_look: vec2<f32>,
    @location(12) prev_motion: vec3<f32>,
    @location(7) light: vec3<f32>,
    @location(8) skin: u32,
    // Rectangle de la face dans l'image du skin (0..1 : u0, v0, u1, v1) : la lecture y
    // reste (sur les arêtes, l'interpolation déborde à peine sur la face voisine).
    @location(9) rect: vec4<f32>,
};

struct PlayerOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: i32,
    @location(2) world: vec3<f32>,
    @location(3) light: vec3<f32>,
    @location(4) shade: f32,
    @location(5) @interpolate(flat) rect: vec4<f32>,
    /// Le même point à l'image d'avant (le joueur a bougé entre les deux).
    @location(6) prev_world: vec3<f32>,
    /// (v254) Place depuis la caméra, exacte (normale de la face, vecteurs de mouvement).
    @location(7) rel: vec3<f32>,
    @location(8) prev_rel: vec3<f32>,
};

fn m_trans(t: vec3<f32>) -> mat4x4<f32> {
    return mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(t, 1.0),
    );
}

/// Tassement d'une jambe accroupie (voir `part_matrix`).
fn m_scale_y(k: f32) -> mat4x4<f32> {
    return mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, k, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

fn m_scale(s: f32) -> mat4x4<f32> {
    return mat4x4<f32>(
        vec4<f32>(s, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, s, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, s, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

// Mêmes rotations que glam (`Mat4::from_rotation_x`, `from_rotation_y`).
fn m_rot_x(a: f32) -> mat4x4<f32> {
    let c = cos(a);
    let s = sin(a);
    return mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, c, s, 0.0),
        vec4<f32>(0.0, -s, c, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

fn m_rot_y(a: f32) -> mat4x4<f32> {
    let c = cos(a);
    let s = sin(a);
    return mat4x4<f32>(
        vec4<f32>(c, 0.0, -s, 0.0),
        vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(s, 0.0, c, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

// (v225) Proportions d'un personnage de CS2 : mêmes valeurs que `player_model` (LEG, LEG_X,
// SHOULDER_Y, ARM_X, BODY_H, en pixels de 1,8/32 bloc).
// (v234) Tête agrandie de 7,5 % (4,8375 pixels), le reste du corps réduit d'autant pour
// garder 32 pixels de haut : les valeurs de la v225 fois 27,1625 / 27,5
// (`player_model::BODY_K`).
const LEG: f32 = 14.815909;
const LEG_X: f32 = 1.728523;
const SHOULDER_Y: f32 = 10.618068;
const ARM_X: f32 = 4.815170;
const BODY_H: f32 = 12.346591;
// (v225) Foudre de Zeus : le bras droit levé (radians), `player_model::ZEUS_RAISE`.
const ZEUS_RAISE: f32 = 1.15;
// (v239) Objet tenu en construction : le bras droit pointé plus bas que le regard (radians),
// `player_model::ITEM_AIM_DROP`.
const ITEM_AIM_DROP: f32 = 0.367;

/// Repère (pixels → monde) d'une partie du modèle dans la pose de l'instance : le calcul de
/// `player_model::parts`.
fn part_matrix(part: u32, v: PlayerIn) -> mat4x4<f32> {
    let px = 1.8 / 32.0;
    let base = m_trans(v.feet) * m_rot_y(-v.look.x) * m_scale(px);
    let swing = sin(v.motion.x) * 0.75 * v.motion.y;
    let crouch = clamp(v.motion.z, 0.0, 1.0);
    // `CROUCH_LEAN`, `CROUCH_HIP_DROP` de player_model.rs (v247 : hanches tassées de 4,85
    // pixels, la tête accroupie reste dans la boîte de 1,5 bloc).
    let lean = 0.45 * crouch;
    let drop = 4.85 * crouch;
    let pitch = clamp(v.look.y, -1.4, 1.4);
    // (v189) Jambe d'un seul morceau : accroupi, elle se tasse de `drop` au lieu de se
    // faire recouvrir au quart par le bas du corps (pli visible à la hanche). Pieds au sol.
    let shrink = m_scale_y((LEG - drop) / LEG);
    if part == 0u {
        return base * m_trans(vec3<f32>(-LEG_X, LEG - drop, 0.0)) * m_rot_x(swing) * shrink;
    }
    if part == 1u {
        return base * m_trans(vec3<f32>(LEG_X, LEG - drop, 0.0)) * m_rot_x(-swing) * shrink;
    }
    let upper = base * m_trans(vec3<f32>(0.0, LEG - drop, 0.0)) * m_rot_x(-lean);
    if part == 2u {
        return upper;
    }
    // (v177) Arme à feu (bit 17) : les deux bras tendus vers le regard, un peu tournés
    // vers l'arme (`AIM_TURN_RIGHT`, `AIM_TURN_LEFT` de player_model.rs).
    // (v215) Lampe torche (bit 18) : le bras droit seul, tendu de même. (v239) Objet tenu en
    // construction (bits 18 et 20) : tendu de même, pointé plus bas (`ITEM_AIM_DROP`).
    if ((part == 3u || part == 4u) && (v.skin & 0x20000u) != 0u)
        || (part == 4u && (v.skin & 0x40000u) != 0u) {
        var side = -ARM_X;
        var turn = -0.5;
        var sign = -1.0;
        var down = 0.0;
        if part == 4u {
            side = ARM_X;
            turn = 0.15;
            sign = 1.0;
            if (v.skin & 0x100000u) != 0u {
                down = ITEM_AIM_DROP;
            }
        }
        return upper * m_trans(vec3<f32>(side, SHOULDER_Y, 0.0)) * m_rot_y(turn)
            * m_rot_x(1.5707964 + max(pitch - down, -1.4) + lean + swing * sign * 0.1);
    }
    if part == 3u {
        return upper * m_trans(vec3<f32>(-ARM_X, SHOULDER_Y, 0.0)) * m_rot_x(-swing + lean * 0.6);
    }
    if part == 4u {
        var raise = 0.0;
        var k = 1.0;
        // (v225) Foudre de Zeus (bit 19) : le bras levé devant soi, quel que soit le regard
        // (`player_model::parts`).
        // (v251) Bras levé : balancement de 0,1 (`RAISED_SWING`), comme le bras tendu.
        if (v.skin & 0x80000u) != 0u {
            raise = ZEUS_RAISE;
            k = 0.1;
        } else if (v.skin & 0x10000u) != 0u {
            raise = clamp(0.65 + 0.6 * pitch, 0.0, 2.0);
            if raise != 0.0 {
                k = 0.1;
            }
        }
        return upper * m_trans(vec3<f32>(ARM_X, SHOULDER_Y, 0.0)) * m_rot_x(swing * k + lean * 0.6 + raise);
    }
    // (v234) Tête posée au milieu du haut du corps (le cou), même penché : elle avance avec
    // lui (`player_model::neck`).
    let neck = vec3<f32>(0.0, LEG - drop + BODY_H * cos(lean), -BODY_H * sin(lean));
    return base * m_trans(neck) * m_rot_x(pitch);
}

/// (v200) Portée des reflets dans l'eau (blocs, à l'horizontale, `water_mirror::RANGE`) : les
/// blocs plus loin n'y sont pas, ils ne cacheraient pas un joueur derrière eux.
const WATER_MIRROR_RANGE: f32 = 64.0;

/// (v202) Le joueur (pieds en `feet`) est-il tout entier hors de l'image de cette passe ?
/// Test prudent sur une sphère autour de son corps, en coordonnées de découpe : déplacer un
/// point de `r` dans le monde change x, y et w d'au plus `r` fois la longueur de leur ligne
/// de la matrice. Dans une foule, la plupart sont derrière ou à côté de la caméra : leurs
/// 216 sommets n'ont plus à calculer deux poses chacun.
fn player_off_view(feet: vec3<f32>) -> bool {
    let m = g.view_proj;
    let c = m * vec4<f32>(feet + vec3<f32>(0.0, 0.9, 0.0) - g.cam_pos.xyz, 1.0);
    let r = 1.8;
    let lx = length(vec3<f32>(m[0][0], m[1][0], m[2][0]));
    let ly = length(vec3<f32>(m[0][1], m[1][1], m[2][1]));
    let lw = length(vec3<f32>(m[0][3], m[1][3], m[2][3]));
    let ex = r * (lx + lw);
    let ey = r * (ly + lw);
    return c.w + r * lw < 0.0 || c.x + c.w < -ex || c.x - c.w > ex || c.y + c.w < -ey
        || c.y - c.w > ey;
}

@vertex
fn vs_player(v: PlayerIn) -> PlayerOut {
    // (v200) Image des reflets dans l'eau (sa coupe au plan est multipliée par 1 000, celle
    // d'un miroir a une normale unitaire, voir water_mirror.rs) : seulement les joueurs à
    // portée, les autres hors de l'image. (v202) Et dans toute passe, ceux qui sont hors de
    // son image.
    if (g.clip.y > 500.0 && distance(v.feet.xz, g.cam_pos.xz) > WATER_MIRROR_RANGE)
        || player_off_view(v.feet) {
        var out: PlayerOut;
        out.clip = vec4<f32>(0.0, 0.0, -1.0, 1.0);
        return out;
    }
    // (v254) Le modèle est posé depuis la caméra (ses pieds moins la caméra : deux grands
    // nombres voisins, exact), puis compté comme tel : loin du centre du monde, ses sommets
    // ne tremblent plus les uns par rapport aux autres. Sa place dans le monde en est tirée.
    let cam = g.cam_pos.xyz;
    var vr = v;
    vr.feet = v.feet - cam;
    let xf = part_matrix(v.part, vr);
    let rel = (xf * vec4<f32>(v.pos, 1.0)).xyz;
    let world = rel + cam;
    let n = normalize((xf * vec4<f32>(v.normal, 0.0)).xyz);
    var o: PlayerOut;
    o.clip = g.view_proj * vec4<f32>(rel, 1.0);
    // (v187) Où ce point était à l'image d'avant : la même partie du corps, dans la pose
    // de l'image d'avant (déplacement, demi-tour et balancement des membres compris).
    var p = v;
    p.feet = v.prev_feet - cam;
    p.look = v.prev_look;
    p.motion = v.prev_motion;
    o.prev_rel = (part_matrix(v.part, p) * vec4<f32>(v.pos, 1.0)).xyz;
    o.prev_world = o.prev_rel + cam;
    o.rel = rel;
    o.uv = v.uv;
    o.layer = i32(v.skin & 0xffffu);
    o.world = world;
    o.light = v.light;
    // Centres des pixels du bord de la face (64 pixels de côté).
    o.rect = v.rect + vec4<f32>(0.5, 0.5, -0.5, -0.5) / 64.0;
    // `player_model::face_shade`.
    o.shade = clamp(0.78 + 0.22 * n.y + 0.06 * n.x, 0.45, 1.0);
    return o;
}

/// Couleur du pixel sans l'éclairage du lieu : le skin cuit, ombré selon la face.
fn player_albedo(i: PlayerOut) -> vec3<f32> {
    let uv = clamp(i.uv, i.rect.xy, i.rect.zw);
    return textureSampleLevel(skins, skin_sampler, uv, i.layer, 0.0).rgb * i.shade;
}

@fragment
fn fs_player(i: PlayerOut) -> @location(0) vec4<f32> {
    // (v190) Dans un miroir : rien de ce qui est derrière lui.
    // (v198) Moins d'un bloc derrière : un joueur collé au miroir, gardé entier.
    if dot(g.clip.xyz, i.world) + g.clip.w < -MIRROR_SLACK {
        discard;
    }
    // (v215) Dans le brouillard comme les blocs (`entity_fog`).
    return vec4<f32>(fogged(player_albedo(i) * i.light, i.world, entity_fog(i.world)), 1.0);
}

// Avec l'anticrénelage : comme les entités fixes dans le monde (`fs_taa_world`).
@fragment
fn fs_player_taa(i: PlayerOut) -> TaaOut {
    let n = face_normal(i.rel);
    let cur = g.cur_view_proj * vec4<f32>(i.rel, 1.0);
    // (v187) Depuis où il était à l'image d'avant, et pas depuis le même point du monde :
    // sinon un joueur qui marche devant une caméra immobile n'aurait aucun mouvement, et
    // l'anticrénelage le ferait traîner.
    let prev = g.prev_view_proj * vec4<f32>(i.prev_rel, 1.0);
    let d = prev.xy / prev.w - cur.xy / cur.w;
    let base = player_albedo(i);
    // (v215) Dans le brouillard comme les blocs, lumières dynamiques comprises.
    let fog = entity_fog(i.world);
    var o: TaaOut;
    o.color = vec4<f32>(fogged(base * i.light, i.world, fog), 1.0);
    o.motion = d * vec2<f32>(0.5, -0.5) * g.screen.xy;
    o.albedo = vec4<f32>(base, 1.0);
    o.surf = vec4<f32>(gb_oct_encode(n), 1.0 - fog, gb_pack_spec(0.5 * (1.0 - fog), false));
    return o;
}

// ---------------------------------------------------------------- (v220) ombres des lumières
// Les joueurs dans l'atlas des ombres des joueurs (voir `EntAtlas` dans lights.rs) : une case
// par projecteur, six par lumière ponctuelle, mêmes places et même repère que l'atlas des
// blocs. Ce qui y est écrit : la distance à la lumière, rapportée à sa portée (comme les
// cartes des blocs, shadow_bake.wgsl).

struct LightJob {
    // xyz = position de la lumière, w = portée.
    pos_range: vec4<f32>,
    // Repère de la case : droite, haut, avant (w = tangente du demi-angle ; 1 pour une face
    // de cube).
    right: vec4<f32>,
    up: vec4<f32>,
    fwd: vec4<f32>,
};

// (Liaison 24 : après celles du ciel, 16 à 21, et des cartes d'ombre, 22 et 23.)
@group(0) @binding(24) var<uniform> lj: LightJob;

struct LjOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
};

@vertex
fn vs_player_light(v: PlayerIn) -> LjOut {
    let xf = part_matrix(v.part, v);
    let world = (xf * vec4<f32>(v.pos, 1.0)).xyz;
    let to = world - lj.pos_range.xyz;
    let c = vec3<f32>(dot(lj.right.xyz, to), dot(lj.up.xyz, to), dot(lj.fwd.xyz, to));
    let t = max(lj.fwd.w, 1e-4);
    var o: LjOut;
    // Même projection que `dl_shadow_uv` : (x, y) / (z · tan). La profondeur vient du
    // fragment (distance) : ici, seulement de quoi ne jamais couper devant ni derrière.
    o.clip = vec4<f32>(c.x / t, c.y / t, c.z * 0.5, c.z);
    o.world = world;
    return o;
}

@fragment
fn fs_player_light(i: LjOut) -> @builtin(frag_depth) f32 {
    let d = distance(i.world, lj.pos_range.xyz);
    // Collé à la lumière (la main qui tient une lampe) : pas d'ombre.
    if d < 0.3 {
        discard;
    }
    return clamp(d / max(lj.pos_range.w, 1e-3), 0.0, 1.0);
}
