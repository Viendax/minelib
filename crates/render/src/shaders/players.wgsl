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

/// Repère (pixels → monde) d'une partie du modèle dans la pose de l'instance : le calcul de
/// `player_model::parts`.
fn part_matrix(part: u32, v: PlayerIn) -> mat4x4<f32> {
    let px = 1.8 / 32.0;
    let base = m_trans(v.feet) * m_rot_y(-v.look.x) * m_scale(px);
    let swing = sin(v.motion.x) * 0.75 * v.motion.y;
    let crouch = clamp(v.motion.z, 0.0, 1.0);
    let lean = 0.45 * crouch;
    let drop = 0.3 / px * crouch * 0.6;
    let pitch = clamp(v.look.y, -1.4, 1.4);
    // (v189) Jambe d'un seul morceau : accroupi, elle se tasse de `drop` au lieu de se
    // faire recouvrir au quart par le bas du corps (pli visible à la hanche). Pieds au sol.
    let shrink = m_scale_y((12.0 - drop) / 12.0);
    if part == 0u {
        return base * m_trans(vec3<f32>(-2.0, 12.0 - drop, 0.0)) * m_rot_x(swing) * shrink;
    }
    if part == 1u {
        return base * m_trans(vec3<f32>(2.0, 12.0 - drop, 0.0)) * m_rot_x(-swing) * shrink;
    }
    let upper = base * m_trans(vec3<f32>(0.0, 12.0 - drop, 0.0)) * m_rot_x(-lean);
    if part == 2u {
        return upper;
    }
    // (v177) Arme à feu (bit 17) : les deux bras tendus vers le regard, un peu tournés
    // vers l'arme (`AIM_TURN_RIGHT`, `AIM_TURN_LEFT` de player_model.rs).
    if (part == 3u || part == 4u) && (v.skin & 0x20000u) != 0u {
        var side = -6.0;
        var turn = -0.5;
        var sign = -1.0;
        if part == 4u {
            side = 6.0;
            turn = 0.15;
            sign = 1.0;
        }
        return upper * m_trans(vec3<f32>(side, 10.0, 0.0)) * m_rot_y(turn)
            * m_rot_x(1.5707964 + pitch + lean + swing * sign * 0.1);
    }
    if part == 3u {
        return upper * m_trans(vec3<f32>(-6.0, 10.0, 0.0)) * m_rot_x(-swing + lean * 0.6);
    }
    if part == 4u {
        var raise = 0.0;
        var k = 1.0;
        if (v.skin & 0x10000u) != 0u {
            raise = clamp(0.65 + 0.6 * pitch, 0.0, 2.0);
            if raise != 0.0 {
                k = 0.3;
            }
        }
        return upper * m_trans(vec3<f32>(6.0, 10.0, 0.0)) * m_rot_x(swing * k + lean * 0.6 + raise);
    }
    return base * m_trans(vec3<f32>(0.0, 24.0 - drop * 1.6, -lean * 4.0)) * m_rot_x(pitch);
}

@vertex
fn vs_player(v: PlayerIn) -> PlayerOut {
    let xf = part_matrix(v.part, v);
    let world = (xf * vec4<f32>(v.pos, 1.0)).xyz;
    let n = normalize((xf * vec4<f32>(v.normal, 0.0)).xyz);
    var o: PlayerOut;
    o.clip = g.view_proj * vec4<f32>(world, 1.0);
    // (v187) Où ce point était à l'image d'avant : la même partie du corps, dans la pose
    // de l'image d'avant (déplacement, demi-tour et balancement des membres compris).
    var p = v;
    p.feet = v.prev_feet;
    p.look = v.prev_look;
    p.motion = v.prev_motion;
    o.prev_world = (part_matrix(v.part, p) * vec4<f32>(v.pos, 1.0)).xyz;
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
    if dot(g.clip.xyz, i.world) + g.clip.w < 0.0 {
        discard;
    }
    return vec4<f32>(player_albedo(i) * i.light, 1.0);
}

// Avec l'anticrénelage : comme les entités fixes dans le monde (`fs_taa_world`).
@fragment
fn fs_player_taa(i: PlayerOut) -> TaaOut {
    let n = face_normal(i.world);
    let cur = g.cur_view_proj * vec4<f32>(i.world, 1.0);
    // (v187) Depuis où il était à l'image d'avant, et pas depuis le même point du monde :
    // sinon un joueur qui marche devant une caméra immobile n'aurait aucun mouvement, et
    // l'anticrénelage le ferait traîner.
    let prev = g.prev_view_proj * vec4<f32>(i.prev_world, 1.0);
    let d = prev.xy / prev.w - cur.xy / cur.w;
    let base = player_albedo(i);
    var o: TaaOut;
    o.color = vec4<f32>(base * i.light, 1.0);
    o.motion = d * vec2<f32>(0.5, -0.5) * g.screen.xy;
    o.albedo = vec4<f32>(base, 1.0);
    o.surf = vec4<f32>(gb_oct_encode(n), 1.0, gb_pack_spec(0.5, false));
    return o;
}
