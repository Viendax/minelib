//! (v269) L'Évidé : un monstre de gelée blanche, sur le modèle du joueur étiré à 3,5 blocs.
//!
//! Demandes de Viendax (23 et 24 novembre) : « basé sur notre modèle de joueur, la même
//! largeur, 3,5 blocs de haut » ; « tout blanc, avec du relief gris clair ou jaunâtre » ; « pas
//! d'yeux, juste leur contour, blanc/gris à cet endroit » ; « une bouche sombre, creuse,
//! effrayante, creusée dedans » ; « il est en gelée, même dedans » ; « il marche, court, rampe
//! (3,5 de long, 1 de large et de haut, à la vitesse du sneak), avec des transitions et des
//! mouvements effrayants » ; « il se déplace comme un joueur ou un PNJ : à la vitesse de la
//! marche quand il n'attaque pas, à celle d'un joueur quand il poursuit » ; « il nous voit si
//! on est sur son "écran", jusqu'à 50 blocs, pas à travers les blocs pleins » ; « il tourne
//! assez régulièrement » ; « il nous suit tant qu'il peut, jusqu'à nous manger ou être mangé » ;
//! « les épées ne lui font rien : il faut le manger vivant, de près ; on l'attrape et on en
//! mange un morceau, qui ne revient jamais » ; « il nous attrape avec ses mains, se penche pour
//! mordre (en rampant, seule sa tête bouge), ouvre grand la bouche et la referme sur nous ».
//!
//! Son corps est celui du joueur (`player_model::rig`), chaque pavé étiré en hauteur, découpé
//! en petits cubes (`grids` : ceux du joueur, deux fois plus de rangées). Ce qu'on lui mange
//! est retiré cube par cube (`flesh::Flesh`, sans repousse) ; sa bouche est un creux de la
//! tête qui s'agrandit et se referme (`mouth`). Il se déplace avec les allures, la pesanteur
//! et le saut du joueur (`player::GAIT`) et les mêmes boîtes de collision ; haut de 3,5 blocs,
//! il rampe partout où le plafond est plus bas.
//!
//! (v272) Dans un monde de construction en ligne, chacun a les siens : ils vivent dans son
//! jeu, les autres joueurs ne les voient pas (la version partagée, que le relais devra
//! valider de bout en bout, reste à faire). Jamais en partie FPS.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use glam::{Mat4, Vec3};
use minelib_render::{EntityVertex, ShadowBox};
use minelib_world::{ChunkPos, World};

use crate::corpse::Liquid;
use crate::flesh::{Flesh, PARTS};

/// Sa taille debout (blocs) et son étirement par rapport au joueur.
pub const HEIGHT: f32 = 3.5;
const TALL: f32 = HEIGHT / crate::player::HEIGHT;
/// Demi-largeur de sa boîte de collision : celle du joueur (« la même largeur »).
const HALF: f32 = crate::player::HALF_WIDTH;
/// Hauteur de sa boîte quand il rampe (« 1 de large et de haut »).
const CRAWL_H: f32 = 0.95;
/// (v277) Sous cette hauteur il est assez à plat pour mordre comme en rampant (la tête
/// seule, nos jambes).
const LOW: f32 = 1.5;
/// En rampant : hauteur de ses hanches, et leur recul derrière sa position (sa boîte est à
/// l'avant, autour de la poitrine et de la tête ; le corps traîne derrière, 3,5 de long).
const CRAWL_HIP: f32 = 0.5;
const CRAWL_BACK: f32 = 1.36;
/// Il nous voit jusque-là (blocs), dans le champ de son « écran » (celui du jeu : 75° de
/// haut, en 16:9).
pub const SEE: f32 = 50.0;
const VIEW_TAN_Y: f32 = 0.767_327;
const VIEW_TAN_X: f32 = VIEW_TAN_Y * 16.0 / 9.0;
/// Tout près (blocs), il nous sent sans nous voir (mais pas à travers un bloc plein).
const FEEL: f32 = 3.0;
/// Sans nous voir, il nous cherche encore ce temps-là (s) avant de reprendre sa ronde.
const GIVE_UP_S: f32 = 10.0;
/// Il se met à mordre quand nos pieds sont à moins de tant de blocs des siens (à plat) :
/// debout, en rampant. Il place ensuite sa bouche sur l'endroit visé (`Prey::spot`).
const HOLD: f32 = 2.3;
const CRAWL_NEAR: f32 = 1.5;
/// En rampant, il s'arrête à cette distance de l'endroit visé (seule sa tête bouge).
const CRAWL_HOLD: f32 = 0.9;
/// (v272) Debout, jusqu'où son buste se plie aux hanches pour amener sa bouche à la hauteur
/// de ce qu'il mord (rad depuis la verticale : plus de π/2, il plonge vers le sol).
const LEAN_MAX: f32 = 3.0;
/// (v272) Sa tête quand il mord debout : le visage tourné vers le bas (rad). (v290) Le plus
/// bas qu'elle se tourne : elle vise le centre de notre corps (`BITE_INWARD`).
const BITE_PITCH: f32 = -1.2;
/// (v290) En mordant, sa tête vise le milieu du membre mordu, à la hauteur de l'endroit mordu
/// ramenée de cette part vers ce milieu.
const BITE_INWARD: f32 = 0.3;
/// (v299) Son cri : tant de secondes à faire sa gueule ronde, dont les dernières à la
/// refermer.
const SCREAM_S: f32 = 1.5;
const MAW_SHUT_S: f32 = 0.2;
// (v312) Les règles que le relais surveille en ligne sont celles d'ici.
const _: () = assert!(LIFE == minelib_net::evide::LIFE);
const _: () = assert!(OUR_BITE_HURTS == minelib_net::evide::EAT_HURTS);
const _: () = assert!(MAX <= minelib_net::evide::MAX_PER_OWNER);
/// (v312) Sur le fil (`Evide::wire`) : sa penche et les angles de sa tête sont dits sur un
/// octet, jusqu'à tant (rad). Un pantin sans nouvelles depuis tant de secondes s'en va.
const WIRE_HUNCH: f32 = 2.0;
const WIRE_HEAD: f32 = 1.6;
const PUPPET_GONE_S: f32 = 3.0;
/// Pantins au plus dans un jeu ; leurs numéros commencent là (ceux de nos Évidés sont petits).
const PUPPETS: usize = 64;
const PUPPET_IDS: u32 = 0x4000_0000;
/// Une morsure : durée du geste (s), instant où la bouche se referme.
const BITE_S: f32 = 1.8;
const BITE_AT: f32 = 1.04;
/// (v290) À partir de ce moment de la morsure (s), il se redresse et lâche prise : « il revient
/// à sa position initiale à la fin de la morsure » (d'où une morsure toutes les 1,8 s au lieu
/// de 1,6 : le temps de se redresser tout à fait avant de se pencher pour la suivante).
const BITE_BACK: f32 = 1.2;
/// On peut viser un morceau de lui à manger de cette distance (blocs, depuis nos yeux) :
/// « de près ». (v272) Il fallait ensuite que notre bouche y arrive en se penchant ; (v290)
/// plus maintenant (« je n'arrive plus à manger le monstre […] que ça fonctionne quand je
/// clique ») : on se penche vers lui autant qu'on peut, et le clic suffit.
pub const EAT_REACH: f32 = 1.5;
/// (v272) « Si sa bouche se décolle avant, ça ne nous mange pas » : au moment où elle se
/// referme, sa bouche doit être à moins de tant de blocs de l'endroit visé.
pub const TOUCH: f32 = 0.3;
/// Côté (cubes) du morceau qu'on lui mange à chaque bouchée.
const OUR_BITE: usize = 4;
/// (v290) « Il doit avoir autant de vie que le joueur […] on le tue en 5 morsures » : sa vie
/// (celle d'un joueur, `save::MAX_HEALTH`), et ce que lui retire chacune de nos bouchées.
/// (Avant : mort quand on avait mangé la moitié de son corps, ou les quatre cinquièmes de sa
/// tête.)
pub const LIFE: u8 = crate::save::MAX_HEALTH;
pub const OUR_BITE_HURTS: u8 = 4;
/// (v299) Sa fin : mort, il tombe, puis fond dès qu'il ne bouge plus (au plus tard après
/// `MELT_WAIT` secondes) : sa chair en `MELT_S` secondes, puis ses veines en `VEIN_S` ; au
/// plus tant de gouttes par image. Vivant, la gelée coule de ce qu'on lui a mangé : gouttes
/// du jet, puis gouttes par seconde au début, qui s'espacent (divisées par e toutes les
/// `OOZE_FADE_S` secondes), pendant `OOZE_S`.
const MELT_WAIT: f32 = 1.6;
const MELT_S: f32 = 5.0;
const VEIN_S: f32 = 2.5;
const MELT_DROPS: usize = 5;
const OOZE_SPURT: u8 = 12;
const OOZE_RATE: f32 = 8.0;
const OOZE_FADE_S: f32 = 3.0;
const OOZE_S: f32 = 10.0;
/// Au plus tant de monstres à la fois ; au plus tant venus tout seuls.
pub const MAX: usize = 16;
const NATURAL_MAX: usize = 2;
/// (v269) Apparition naturelle : la nuit, autour de chaque joueur, entre `SPAWN_NEAR` et
/// `SPAWN_FAR` blocs (« les mobs apparaissent dans les 256 blocs autour de nous »), en moyenne
/// une fois toutes les `SPAWN_EVERY_S` secondes de nuit. « 3 fois plus qu'un zombie avec un
/// lingot de fer » : dans Minecraft un zombie a 2,5 % de chances d'un butin rare, un tiers
/// d'un lingot de fer ; trois fois cela, 2,5 % des apparitions d'un monstre commun, soit à peu
/// près une par vingt minutes de nuit et par joueur. **Ce monstre est la référence des taux
/// d'apparition** : les prochains se régleront par rapport à lui.
pub const SPAWN_EVERY_S: f32 = 1200.0;
const SPAWN_NEAR: f32 = 24.0;
const SPAWN_FAR: f32 = 256.0;
/// Trop loin du joueur (blocs) : il disparaît.
const DESPAWN: f32 = 320.0;

const LEG_L: usize = 0;
const LEG_R: usize = 1;
const CHEST: usize = 2;
const ARM_L: usize = 3;
const ARM_R: usize = 4;
const HEAD: usize = 5;
const BELLY: usize = 6;

// Ses couleurs : blanc, relief gris clair ou jaunâtre, contour des yeux, bouche.
const WHITE: u32 = 0xf3f1ea;
const GREY: u32 = 0xdcdad2;
const YELLOW: u32 = 0xe9e2c4;
const HOLLOW: u32 = 0xcbc9c1;
// (v299) « Les yeux, dissimule-les mieux, en gelée » : leur contour et leur creux ont les
// teintes de sa gelée (`flesh::JELLY`), à peine plus marquées que ses plaques jaunâtres (ils
// étaient gris : 0xa3a29c et 0xd9d8d3).
const OUTLINE: u32 = 0xdcd8bc;
const SOCKET: u32 = 0xe9e5d0;
const MOUTH: [u32; 3] = [0x2a1418, 0x160a0d, 0x070304];
// (v299) Ses dents : l'ivoire de devant, et de côté (dans l'ombre de la bouche).
const TOOTH: [u32; 2] = [0xd9cc98, 0x9c9068];
// (v272) Ses veines, dans la gelée.
const VEIN: [u32; 2] = [0x4d2a68, 0x3a2152];

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Rapproche `v` de `to` d'au plus `step`.
fn approach(v: f32, to: f32, step: f32) -> f32 {
    v + (to - v).clamp(-step, step)
}

/// Écart d'angle ramené entre −π et π.
fn turn(to: f32, from: f32) -> f32 {
    (to - from + PI).rem_euclid(TAU) - PI
}

/// Vers où regarde un corps tourné de `yaw` (le lacet du joueur).
fn forward(yaw: f32) -> Vec3 {
    Vec3::new(yaw.sin(), 0.0, -yaw.cos())
}

/// Grille de cubes de chaque pavé : celle du joueur, deux fois plus de rangées (le corps est
/// étiré de 1,94 : les cubes restent à peu près cubiques).
pub fn grids() -> [[usize; 3]; PARTS] {
    crate::player_model::flesh_grids().map(|[x, y, z]| [x, y * 2, z])
}

/// Sa posture, pour le dessin.
#[derive(Clone, Copy, Debug, Default)]
pub struct Anim {
    /// Phase du pas et son ampleur (0 … 1).
    pub walk: f32,
    pub swing: f32,
    /// 0 debout … 1 à plat ventre.
    pub crawl: f32,
    /// Penche du buste vers l'avant, debout (rad) : voûté, penché sur sa proie.
    pub hunch: f32,
    /// Angle des bras dans le monde (0 : pendants, π/2 : tendus devant).
    pub arms: f32,
    /// 0 … 1 : les bras se referment sur la proie.
    pub grab: f32,
    /// Bouche : −0,5 refermée, 0 au repos (un creux), 1 grande ouverte.
    pub mouth: f32,
    /// Tête : lacet (rad, positif vers sa droite), tangage (positif vers le haut), roulis.
    pub head_yaw: f32,
    pub head_pitch: f32,
    pub head_roll: f32,
    /// 0 … 1 : il s'affaisse (sa fin).
    pub sink: f32,
}

/// (v290) « Ajoute une pliure au niveau des genoux et des coudes de notre perso/npc et de ce
/// monstre […] adapte les animations » : pli du genou de la jambe qui avance, debout ; plis
/// du coude (au repos, en plus quand il tend les bras vers nous, en plus quand le bras part en
/// avant en marchant) ; de combien ses avant-bras se referment sur nous quand il mord ;
/// largeur du bas d'un membre plié (ses côtés ne se confondent pas avec ceux du haut) ; pli à
/// partir duquel un membre fait deux pavés d'ombre.
const KNEE_STEP: f32 = 0.8;
const ELBOW_REST: f32 = 0.25;
const ELBOW_REACH: f32 = 0.3;
const ELBOW_WALK: f32 = 0.4;
const GRAB_BEND: f32 = 0.9;
const LOWER_NARROW: f32 = 0.98;
const SHADOW_BEND: f32 = 0.3;

/// (v290) Son corps dans une posture : ses sept pavés (`parts`), et pour chaque membre le
/// repère de sa moitié du bas (sous le genou, sous le coude) et l'angle dont il est plié.
pub struct Limbs {
    pub parts: [(Mat4, [f32; 3], [f32; 3]); PARTS],
    pub lower: [Option<Mat4>; PARTS],
    pub bend: [f32; PARTS],
}

fn smooth(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Ses pavés dans cette posture : repère (pixels → monde) et coins (pixels), dans l'ordre
/// du modèle du joueur. (v290) Un membre : le repère de sa moitié du haut (voir `limbs`).
pub fn parts(pos: Vec3, yaw: f32, a: &Anim) -> [(Mat4, [f32; 3], [f32; 3]); PARTS] {
    limbs(pos, yaw, a).parts
}

/// (v290) Ses morceaux rigides : (pavé, repère, coins). Un membre en donne deux, au-dessus et
/// au-dessous du genou ou du coude.
pub fn segments(pos: Vec3, yaw: f32, a: &Anim) -> Vec<(usize, Mat4, [f32; 3], [f32; 3])> {
    let l = limbs(pos, yaw, a);
    let mut out = Vec::with_capacity(PARTS + 4);
    for (i, (xf, lo, hi)) in l.parts.into_iter().enumerate() {
        match l.lower[i] {
            Some(low) => {
                let mid = (lo[1] + hi[1]) / 2.0;
                out.push((i, xf, [lo[0], mid, lo[2]], hi));
                out.push((i, low, lo, [hi[0], mid, hi[2]]));
            }
            None => out.push((i, xf, lo, hi)),
        }
    }
    out
}

/// (v290) De combien ses hanches descendent à chaque pas, debout (pixels du modèle) : ses
/// jambes se balancent raides depuis la hanche ; sans cela, au milieu du pas (les deux jambes
/// écartées), ses deux pieds quittaient le sol d'un tiers de bloc. La jambe d'appui (tendue)
/// garde ainsi le pied au sol, l'autre (genou plié) passe au-dessus.
fn step_drop(a: &Anim) -> f32 {
    let r = crate::player_model::rig();
    let (len, half) = (r.leg * TALL, (r.boxes[LEG_L].1[2] - r.boxes[LEG_L].0[2]) / 2.0);
    let angle = (a.walk.sin() * a.swing * 0.75).abs();
    let low = (a.crawl * 10.0).min(1.0);
    (len - (len * angle.cos() + half * angle.sin())).max(0.0) * (1.0 - low)
}

pub fn limbs(pos: Vec3, yaw: f32, a: &Anim) -> Limbs {
    let r = crate::player_model::rig();
    let tall = |i: usize| {
        let (mut lo, mut hi) = r.boxes[i];
        lo[1] *= TALL;
        hi[1] *= TALL;
        (lo, hi)
    };
    let c = a.crawl;
    // Penche du buste : voûté debout, à l'horizontale en rampant.
    let pitch = lerp(a.hunch, FRAC_PI_2, c);
    let hip_y = lerp(r.leg * TALL, CRAWL_HIP / r.px, c) - step_drop(a);
    let base = Mat4::from_translation(pos)
        * Mat4::from_rotation_y(-yaw)
        * Mat4::from_scale(Vec3::new(r.px, r.px * (1.0 - 0.94 * a.sink), r.px));
    let hip = base * Mat4::from_translation(Vec3::new(0.0, hip_y, c * CRAWL_BACK / r.px));
    let mut out = [(Mat4::IDENTITY, [0.0; 3], [0.0; 3]); PARTS];
    let mut lower = [None; PARTS];
    let mut bend = [0.0f32; PARTS];
    let narrow = Mat4::from_scale(Vec3::new(LOWER_NARROW, 1.0, 1.0));
    let stride = a.walk.sin() * a.swing;
    // Jambes. Debout : balancées d'avant en arrière, le genou de celle qui avance plié.
    // (v277) Baissé, les pieds restent au sol. (v290) Il plie les genoux (accroupi sur ses
    // pieds, les cuisses en avant) au lieu de laisser partir ses jambes raides en arrière ;
    // plus bas encore, elles s'allongent derrière lui jusqu'à traîner à plat quand il rampe.
    // (La jambe : longue de `len`, épaisse de deux fois `half` ; le genou à mi-longueur,
    // charnière au bord avant, le bas part en arrière.)
    let (len, half) = (r.leg * TALL, (r.boxes[LEG_L].1[2] - r.boxes[LEG_L].0[2]) / 2.0);
    let knee = |fold: f32| {
        Mat4::from_translation(Vec3::new(0.0, -len / 2.0, -half))
            * Mat4::from_rotation_x(-fold)
            * Mat4::from_translation(Vec3::new(0.0, len / 2.0, half))
    };
    // Les deux coins du dessous du pied (avant, arrière) dans le plan de la jambe (hauteur,
    // recul ; pixels depuis la hanche, cuisse à la verticale), genou plié de `fold` : le bas
    // de la jambe tourne autour du bord avant du genou.
    let sole = |fold: f32| {
        let (sf, cf) = fold.sin_cos();
        let (y, z) = (-len / 2.0 - len / 2.0 * cf, -half + len / 2.0 * sf);
        [(y, z), (y + 2.0 * half * sf, z + 2.0 * half * cf)]
    };
    // Le point le plus bas du pied, cuisse tournée de `thigh` (vers l'avant).
    let foot = |thigh: f32, fold: f32| {
        let (st, ct) = thigh.sin_cos();
        let [a, b] = sole(fold);
        (a.0 * ct - a.1 * st).min(b.0 * ct - b.1 * st)
    };
    // L'angle de la cuisse qui pose juste le pied au sol, pour un genou plié de `fold` (le
    // pied derrière le genou) : le pied descend quand la cuisse avance, jusqu'à passer sous
    // la hanche ; cherché par moitiés. Jambe trop pliée pour toucher le sol : au plus bas.
    let grounded = |fold: f32| {
        let (mut back, mut front) = (fold / 2.0 - 1.65, fold / 2.0 + 0.06);
        if foot(front, fold) + hip_y > 0.0 {
            return front;
        }
        for _ in 0..18 {
            let mid = (back + front) / 2.0;
            if foot(mid, fold) + hip_y > 0.0 {
                back = mid;
            } else {
                front = mid;
            }
        }
        (back + front) / 2.0
    };
    // Genou qui garde le pied sous la hanche, à cette hauteur de hanche : le pli pour lequel
    // le coin du pied le plus loin de la hanche en est à cette distance (cherché par moitiés :
    // plus le genou plie, plus la jambe est courte) ; part des jambes allongées derrière
    // (0 : accroupi sur ses pieds … 1 : à plat ventre).
    let reach = |fold: f32| {
        let [a, b] = sole(fold);
        (a.0 * a.0 + a.1 * a.1).max(b.0 * b.0 + b.1 * b.1).sqrt()
    };
    let squat = {
        let (mut less, mut more) = (0.0f32, 3.0f32);
        for _ in 0..18 {
            let mid = (less + more) / 2.0;
            if reach(mid) > hip_y {
                less = mid;
            } else {
                more = mid;
            }
        }
        less
    };
    let trail = smooth(0.45, 1.0, c);
    let low = (c * 10.0).min(1.0);
    for (i, side, sign) in [(LEG_L, -r.leg_x, 1.0f32), (LEG_R, r.leg_x, -1.0)] {
        // La jambe qui avance (sa cuisse part vers l'avant) : son pied se lève.
        let lifting = (a.walk.cos() * sign).max(0.0) * a.swing;
        // Debout.
        let (thigh0, fold0) = (stride * 0.75 * sign, KNEE_STEP * lifting);
        // Baissé : le pas raccourcit et rallonge le pli (le pied glisse d'avant en arrière),
        // la cuisse se lève un peu pour le pas ; à plat, la jambe ne se lève que vers
        // l'arrière, l'une après l'autre.
        let fold1 = (squat * (1.0 - trail) * (1.0 - 0.22 * stride * sign).min(1.0)).max(0.0);
        let thigh1 = grounded(fold1) + 0.22 * lifting * (1.0 - trail)
            - trail * (stride * 0.1 * sign).max(0.0);
        let (thigh, fold) = (lerp(thigh0, thigh1, low), lerp(fold0, fold1, low));
        // Jamais le pied sous le sol (entre ses deux façons de marcher, au tout début de la
        // descente, le mélange des deux l'y mettait d'une dizaine de centimètres) : la cuisse
        // s'écarte alors, du côté où elle est déjà, juste assez pour le poser.
        let thigh = if foot(thigh, fold) + hip_y >= 0.0 {
            thigh
        } else {
            let away = if thigh >= fold / 2.0 { 1.65 } else { -1.65 };
            let (mut under, mut over) = (thigh, thigh + away);
            for _ in 0..16 {
                let mid = (under + over) / 2.0;
                if foot(mid, fold) + hip_y >= 0.0 {
                    over = mid;
                } else {
                    under = mid;
                }
            }
            over
        };
        let (lo, hi) = tall(i);
        let xf = hip
            * Mat4::from_translation(Vec3::new(side, 0.0, 0.0))
            * Mat4::from_rotation_x(thigh);
        out[i] = (xf, lo, hi);
        lower[i] = Some(xf * knee(fold) * narrow);
        bend[i] = fold;
    }
    let torso = hip * Mat4::from_rotation_x(-pitch);
    for i in [CHEST, BELLY] {
        let (lo, hi) = tall(i);
        out[i] = (torso, lo, hi);
    }
    // Bras : pendants, tendus vers la proie, refermés sur elle ; en rampant, ils tirent le
    // corps l'un après l'autre, tendus devant au ras du sol.
    for (i, side, sign) in [(ARM_L, -r.arm_x, -1.0f32), (ARM_R, r.arm_x, 1.0)] {
        // (Tendus vers la proie, ils ne se balancent presque plus.)
        let loose = (1.0 - a.grab) * (1.0 - 0.75 * (a.arms / 1.25).clamp(0.0, 1.0));
        let stand = a.arms - stride * 0.5 * sign * loose;
        let lie = FRAC_PI_2 - 0.3 + stride * 0.17 * sign;
        let (lo, hi) = tall(i);
        let xf = torso
            * Mat4::from_translation(Vec3::new(side, r.shoulder_y * TALL, 0.0))
            * Mat4::from_rotation_y(sign * 0.32 * a.grab)
            * Mat4::from_rotation_x(pitch + lerp(stand, lie, c))
            * Mat4::from_rotation_z(sign * 0.08);
        out[i] = (xf, lo, hi);
        // (v290) Le coude : à mi-longueur, charnière au bord arrière, l'avant-bras part en
        // avant. Un peu plié au repos (« comme un humain »), plus quand il tend les bras
        // vers nous et quand le bras part en avant en marchant ; en rampant, plié quand le
        // bras tire le corps, tendu quand il repart devant. Quand il mord, ses avant-bras se
        // referment en plus vers l'intérieur, autour de nous (charnière au bord intérieur).
        let reach = (a.arms / 1.25).clamp(0.0, 1.0);
        let ahead = 0.5 - 0.5 * a.walk.sin() * sign;
        let flex_stand = ELBOW_REST + ELBOW_REACH * reach + ELBOW_WALK * a.swing * ahead * loose;
        let flex_lie = 0.55 - 0.45 * a.walk.sin() * sign * a.swing;
        let flex = lerp(flex_stand, flex_lie, c);
        let inward = GRAB_BEND * a.grab * (1.0 - c);
        let inner = if sign < 0.0 { hi[0] } else { lo[0] };
        let corner = Vec3::new(inner, (lo[1] + hi[1]) / 2.0, hi[2]);
        lower[i] = Some(
            xf * Mat4::from_translation(corner)
                * Mat4::from_rotation_x(flex)
                * Mat4::from_rotation_z(-sign * inward)
                * Mat4::from_translation(-corner)
                * narrow,
        );
        bend[i] = flex.max(inward);
    }
    // Tête : elle se redresse d'autant que le buste penche, puis regarde où elle veut.
    let (lo, hi) = tall(HEAD);
    // (v290) Le lacet après avoir défait la penche du buste (avant : dans le repère du buste,
    // où, plié sur nous, « tourner la tête » la faisait rouler sur le côté ; elle ne tournait
    // alors jamais en mordant) : la tête se tourne vers le membre qu'il mord.
    let xf = torso
        * Mat4::from_translation(Vec3::new(0.0, r.body_h * TALL, 0.0))
        * Mat4::from_rotation_x(pitch)
        * Mat4::from_rotation_y(-a.head_yaw)
        * Mat4::from_rotation_x(a.head_pitch)
        * Mat4::from_rotation_z(a.head_roll);
    out[HEAD] = (xf, lo, hi);
    Limbs {
        parts: out,
        lower,
        bend,
    }
}

/// Le cube `c` de la tête (grille 8 × 16 × 8) est dans le creux de la bouche, ouverte de
/// `open` (−0,5 refermée : une fente … 1 grande ouverte). Un ovale creusé dans le bas du
/// visage, profond de plus de la moitié de la tête.
pub fn mouth(c: [usize; 3], d: [usize; 3], open: f32) -> bool {
    mouth_of(MOUTH_STYLE, c, d, open)
}

/// (v299) « La bouche du monstre : fais-moi plusieurs propositions visuelles » : la forme de
/// sa bouche. 0 : l'ovale (celle qu'il a) ; 1 : une fente verticale, du menton au front, qui
/// s'écarte ; 2 : un sourire d'une joue à l'autre, qui s'ouvre vers le bas ; 3 : une gueule
/// ronde bordée de dents ; 4 : la mâchoire fendue en deux depuis le menton ; 5 : l'ovale avec
/// des dents en haut et en bas. « Un mix de ces deux-là [le sourire et l'ovale à dents],
/// quelques propositions, des joues trouées aussi » (`mix`) : 6 : le sourire à dents ; 7 :
/// l'ovale à dents dont les coins remontent ; 8 : le sourire à dents, les joues trouées (on y
/// voit ses dents du fond) ; 9 : l'ovale à dents, les joues trouées ; 10 : le sourire fendu
/// jusqu'aux oreilles, des dents tout le long ; 11 : l'ovale à dents, un coin déchiré sur la
/// joue. `MOUTH_STYLE` : celle des Évidés du jeu (« lui » : la 8, choisie sur les images).
pub const MOUTH_STYLES: u8 = 13;
pub const MOUTH_STYLE: u8 = 8;
/// (v299) « Il peut faire ce visage aussi [la gueule ronde bordée de dents], mais pas quand il
/// mord » : la forme 12, la gueule ronde de la forme 3 avec les joues trouées de la sienne ;
/// il la fait quand il crie (`Evide::maw`).
pub const MAW_STYLE: u8 = 12;

/// (v299) Ce qu'est un cube de sa tête pour sa bouche.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Jaw {
    /// De la tête, comme le reste.
    Flesh,
    /// Dans le creux : rien, et du noir autour.
    Hollow,
    /// Une dent : pleine, couleur d'ivoire.
    Tooth,
}

pub fn mouth_of(style: u8, c: [usize; 3], d: [usize; 3], open: f32) -> bool {
    jaw_of(style, c, d, open) == Jaw::Hollow
}

pub fn jaw_of(style: u8, c: [usize; 3], d: [usize; 3], open: f32) -> Jaw {
    if style >= 6 {
        // Sa place dans une tête de 8 × 16 × 8.
        let cell = |k: usize, n: usize| ((c[k] * 2 + 1) * n / (d[k].max(1) * 2)) as i32;
        let (col, row, lay) = (cell(0, 8), cell(1, 16), cell(2, 8));
        let o = open.clamp(-0.5, 1.0);
        if style == MAW_STYLE {
            // Sa gueule ronde (la forme 3) et les trous de ses joues (ceux de la forme 8).
            if let Some(jaw) = cheeks(col, row, lay, o.max(0.0), None) {
                return jaw;
            }
            return if plain_mouth(3, c, d, open) { Jaw::Hollow } else { Jaw::Flesh };
        }
        return mix(style, col, row, lay, o);
    }
    if plain_mouth(style, c, d, open) {
        Jaw::Hollow
    } else {
        Jaw::Flesh
    }
}

/// (v299) Les mélanges du sourire et de l'ovale à dents (formes 6 à 11), dans une tête de
/// 8 colonnes × 16 rangées × 8 couches (la couche 0 : le visage). Les dents de devant : en
/// haut une colonne sur deux, en bas les autres (elles s'emboîtent quand il referme). Les
/// joues trouées : un trou déchiqueté de chaque côté, et derrière, ses dents du fond.
fn mix(style: u8, col: i32, row: i32, lay: i32, o: f32) -> Jaw {
    let wide = o.max(0.0);
    // 0 au bord de la tête … 3 au milieu.
    let near = col.min(7 - col);
    let smile = matches!(style, 6 | 8 | 10);
    // Le côté déchiré de la forme 11.
    let torn = style == 11 && col >= 4;
    // Les lèvres : pour une colonne, ses rangées ouvertes, et si c'est un coin (peu profond).
    let lips = |col: i32| -> Option<(i32, i32, bool)> {
        let px = col as f32 - 3.5;
        let side = px.abs();
        if smile {
            // Une bande courbe, plus haute vers les joues ; elle s'ouvre surtout au milieu.
            let rows = |px: f32| {
                let line = 3.0 + 0.24 * px * px;
                let bulge = 1.0 - (px / 4.2).powi(2);
                let bottom = (line - 0.55 - 2.8 * wide * bulge).max(1.0);
                let top = line + 0.5 + 1.2 * wide * bulge;
                ((bottom - 0.5).ceil() as i32, (top - 0.5).floor() as i32)
            };
            let (lo, mut hi) = rows(px);
            // (v310) « Plus de blanc là, à la place ça le fait sourire ; agrandis la bouche là et
            // là, sur ces deux pixels » : grande ouverte, de chaque côté des deux dents du
            // milieu, le haut de la bouche monte d'un cube, à la hauteur de la colonne d'à
            // côté (vers la joue) : son bord du haut fait le sourire.
            if side == 1.5 && wide > 0.6 {
                hi = hi.max(rows(2.5).1);
            }
            return Some((lo, hi, side > 3.0));
        }
        // L'ovale, et ses coins qui remontent d'un cube par colonne.
        let half = 2.0 + wide;
        let lo = (3.0 - 2.0 * o).floor() as i32;
        let hi = ((5.0 + 3.5 * o).floor() as i32).max(lo);
        let round = hi - lo >= 3;
        if side < half {
            let edge = side + 1.0 >= half && round;
            return Some(if edge { (lo + 1, hi - 1, false) } else { (lo, hi, false) });
        }
        let k = (side - half + 0.5).ceil() as i32;
        let r = hi - round as i32 + k - 1;
        // (Forme 11 : un coin court d'un côté, large et déchiré de l'autre.)
        let wide_corner = style == 11 && col >= 4;
        if style == 11 && !wide_corner && k > 1 {
            return None;
        }
        Some((r - wide_corner as i32, r, true))
    };
    let deep = lay as f32 + 0.5 < 5.0 + wide;
    if matches!(style, 8 | 9) {
        let rows = match lips(col) {
            Some((a, b, false)) => Some((a, b)),
            _ => None,
        };
        if let Some(jaw) = cheeks(col, row, lay, wide, rows) {
            return jaw;
        }
    }
    if style == 10 && lay <= 5 {
        // La fente continue sur le côté de la tête, en montant vers l'oreille.
        let line = 5 + (0.34 * lay as f32 + 0.3) as i32;
        let more = (1.6 * wide * (1.0 - lay as f32 / 6.0)) as i32;
        if (line - more..=line).contains(&row) {
            if near == 0 {
                return Jaw::Hollow;
            }
            if near == 1 && lay >= 1 {
                return if lay % 2 == 1 { Jaw::Tooth } else { Jaw::Hollow };
            }
        }
    }
    if torn && lay <= 4 {
        // Le coin déchiré : il se prolonge sur la joue, en montant, large de deux ou trois cubes.
        if let Some((_, r, true)) = lips(7) {
            let line = r + (0.45 * lay as f32) as i32;
            let jit = hash(7, lay as usize, 3, 5) % 3 == 0;
            if (line - 1 - jit as i32..=line).contains(&row) {
                if near == 0 {
                    return Jaw::Hollow;
                }
                if near == 1 && lay >= 1 {
                    return if (lay + row) % 2 == 1 { Jaw::Tooth } else { Jaw::Hollow };
                }
            }
        }
    }
    let Some((lo, hi, corner)) = lips(col) else {
        return Jaw::Flesh;
    };
    if !(lo..=hi).contains(&row) || !(if corner { lay < 2 } else { deep }) {
        return Jaw::Flesh;
    }
    // (v306) « Bouche juste cette face et celle-là quand il a la bouche ouverte, sinon on dirait
    // que ce sont ses yeux » ; (v310) « en rouge, ça doit être de la même couleur qu'en bleu » :
    // le sourire ouvert (plus d'une rangée au coin), chaque coin est refermé sur son visage,
    // du blanc de sa peau : ni le cube sombre resté seul en haut, ni la dent du coin.
    if smile && corner && lay == 0 && hi > lo {
        return Jaw::Flesh;
    }
    if lay == 0 && (smile || !corner) {
        let tooth = if hi > lo {
            (row == hi && near % 2 == 1) || (row == lo && near % 2 == 0)
        } else {
            // Sur une seule rangée : une dent sur deux ; refermée (le sourire) : toutes, serrées.
            near % 2 == 1 || (smile && o < -0.25)
        };
        if tooth {
            return Jaw::Tooth;
        }
    }
    Jaw::Hollow
}

/// (v299) Les joues trouées : sur chaque côté de la tête un trou déchiqueté (pas le même des
/// deux côtés), et derrière, l'intérieur de la joue : creux, bordé de ses dents du fond
/// (`lips` : les rangées ouvertes des lèvres à cette colonne, s'il y en a). Rien pour un cube
/// qui n'est ni l'un ni l'autre.
fn cheeks(col: i32, row: i32, lay: i32, wide: f32, lips: Option<(i32, i32)>) -> Option<Jaw> {
    let near = col.min(7 - col);
    let (cz, cy) = (3.4, 4.4 - 0.4 * wide);
    let (rz, ry) = (2.2, 1.6 + 0.7 * wide);
    if near == 0 {
        let jit = (hash(col as usize, row as usize, lay as usize, 11) % 1000) as f32 / 1000.0;
        let q = ((lay as f32 + 0.5 - cz) / rz).powi(2) + ((row as f32 + 0.5 - cy) / ry).powi(2);
        if q <= 0.75 + 0.5 * jit {
            return Some(Jaw::Hollow);
        }
    }
    if near == 1 && (1..=5).contains(&lay) {
        let (mut lo, mut hi) = (cy.floor() as i32 - 1, cy.floor() as i32 + 1);
        if let Some((a, b)) = lips {
            (lo, hi) = (lo.min(a), hi.max(b));
        }
        if (lo..=hi).contains(&row) {
            let tooth = (row == hi && lay % 2 == 1) || (row == lo && lay % 2 == 0);
            return Some(if tooth { Jaw::Tooth } else { Jaw::Hollow });
        }
    }
    None
}

/// Les formes simples (0 à 5).
fn plain_mouth(style: u8, c: [usize; 3], d: [usize; 3], open: f32) -> bool {
    let o = open.clamp(-0.5, 1.0);
    let (x, y, z) = (c[0] as f32, c[1] as f32, c[2] as f32);
    let (sx, sy, sz) = (d[0] as f32 / 8.0, d[1] as f32 / 16.0, d[2] as f32 / 8.0);
    if !matches!(style, 0 | 5) {
        // Le milieu du cube sur une tête de 8 × 16 × 8 (les cubes y sont cubiques).
        let (px, py, pz) = ((x + 0.5) / sx - 4.0, (y + 0.5) / sy, (z + 0.5) / sz);
        let wide = o.max(0.0);
        if pz >= 5.0 + wide {
            return false;
        }
        return match style {
            // Fente verticale : une ellipse haute et étroite.
            1 => {
                let (half, top) = (0.55 + 1.5 * wide + 0.3 * (o + 0.5).min(0.5), 9.0 + 2.5 * wide);
                let (mid, tall) = ((1.0 + top) / 2.0, (top - 1.0) / 2.0);
                (px / half).powi(2) + ((py - mid) / tall).powi(2) <= 1.0
            }
            // Sourire : une bande courbe, plus haute vers les joues.
            2 => {
                let line = 3.2 + 0.26 * px * px;
                px.abs() <= 3.5 && py >= line - (0.55 + 2.6 * wide) && py <= line + 0.5
            }
            // Gueule ronde : un disque ; au bord, à l'entrée, une dent sur deux reste.
            3 => {
                let r = 1.35 + 1.75 * wide + 0.8 * o.min(0.0);
                let (dx, dy) = (px, py - 4.4);
                let far = (dx * dx + dy * dy).sqrt();
                let tooth = pz < 1.2
                    && far > r - 1.0
                    && r > 1.6
                    && ((dy.atan2(dx) / TAU * 12.0).floor() as i32).rem_euclid(2) == 0;
                far <= r && !tooth
            }
            // Mâchoire fendue : un coin dont la pointe monte, ouvert jusqu'au menton.
            _ => {
                let top = 2.0 + 2.0 * (o.min(0.0) + 0.5) + 4.8 * wide;
                px.abs() <= 3.3 && py < top - (1.25 - 0.45 * wide) * px.abs()
            }
        };
    }
    // Demi-largeur autour du milieu, rangées du bas et du haut, profondeur.
    let half = (2.0 + o.max(0.0)) * sx;
    let lo = ((3.0 - 2.0 * o) * sy).floor();
    let hi = ((5.0 + 3.5 * o) * sy).floor().max(lo);
    let deep = (5.0 + o.max(0.0)) * sz;
    let mid = (d[0] as f32 - 1.0) / 2.0;
    if (x - mid).abs() >= half || y < lo || y > hi || z >= deep {
        return false;
    }
    // Coins arrondis dès qu'elle est assez ouverte.
    let corner = (x - mid).abs() + 1.0 >= half && (y == lo || y == hi);
    if corner && hi - lo >= 3.0 {
        return false;
    }
    // (v299) Avec des dents : à l'entrée, en haut et en bas, un cube sur deux reste.
    let tooth = style == 5
        && z < 1.5 * sz
        && hi - lo >= 2.0
        && (y == lo || y == hi)
        && (c[0] / (sx.round().max(1.0) as usize)) % 2 == 0;
    !tooth
}

fn hash(a: usize, b: usize, c: usize, d: usize) -> u32 {
    let mut h = (a as u32).wrapping_mul(0x9e37_79b1)
        ^ (b as u32).wrapping_mul(0x85eb_ca77)
        ^ (c as u32).wrapping_mul(0xc2b2_ae3d)
        ^ (d as u32).wrapping_mul(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^ (h >> 13)
}

/// Sa peau : la couleur du cube `c` du pavé `part` vu par sa face `k` (0 devant, 1 dos, 2 et
/// 3 côtés, 4 dessus, 5 dessous). Blanc ; des plaques gris clair ou jaunâtres ; des côtes et
/// un ventre creux qui se devinent ; à la place des yeux, leur seul contour.
pub fn skin(part: usize, c: [usize; 3], d: [usize; 3], k: usize) -> u32 {
    let [x, y, _] = c;
    if part == HEAD && k == 0 {
        // Deux orbites de 3 × 4 cubes au-dessus de la bouche : un contour, rien dedans (couleur de sa gelée).
        for x0 in [0usize, d[0] - 3] {
            if (x0..x0 + 3).contains(&x) && (10..=13).contains(&(y * 16 / d[1])) {
                let row = y * 16 / d[1];
                let edge = x == x0 || x == x0 + 2 || row == 10 || row == 13;
                return if edge { OUTLINE } else { SOCKET };
            }
        }
    }
    // (v310) « Les 2 pixels en rouge doivent être de la même couleur qu'en bleu » : sur son
    // visage, la plaque jaunâtre du milieu, au-dessus de sa bouche, déborde de deux cubes
    // d'un côté, sur ses deux rangées du bas (ils prennent la couleur du cube d'à côté).
    if part == HEAD && k == 0 && x * 8 / d[0] == 4 && matches!(y * 16 / d[1], 6 | 7) {
        return skin(part, [3 * d[0] / 8, y, c[2]], d, k);
    }
    let middle = x == (d[0] - 1) / 2 || x == d[0] / 2;
    if part == CHEST && k <= 1 && y % 3 == 1 && !middle {
        return GREY;
    }
    if part == BELLY && k == 0 && x > 0 && x + 1 < d[0] && y >= 2 {
        return HOLLOW;
    }
    match hash(part, x / 2, y / 3, c[2] / 2) % 100 {
        0..=13 => GREY,
        14..=24 => YELLOW,
        _ => WHITE,
    }
}

fn srgb(c: u32) -> [f32; 3] {
    [16, 8, 0].map(|s| (((c >> s) & 255) as f32 / 255.0).powf(2.2))
}

/// (v272) « Fais-lui des veines dans les cubes de son corps, qui se propagent partout ; on ne
/// doit pas pouvoir les manger » : le cube `c` du pavé `part` est-il une veine ? Un arbre par
/// pavé (`grow_veins`), poussé une fois pour toutes ; `flesh::Hard::Veins` les rend
/// immangeables : elles restent, nues, dans les creux qu'on lui fait.
pub fn vein(part: usize, c: [usize; 3]) -> bool {
    static VEINS: std::sync::OnceLock<[Vec<bool>; PARTS]> = std::sync::OnceLock::new();
    let d = grids()[part];
    VEINS.get_or_init(grow_veins)[part]
        .get((c[2] * d[1] + c[1]) * d[0] + c[0])
        .copied()
        .unwrap_or(false)
}

/// Couleur d'un cube de l'intérieur de son corps : une veine, ou de la gelée.
pub fn inside(part: usize, c: [usize; 3], d: [usize; 3]) -> u32 {
    if vein(part, c) {
        VEIN[(c[0] + c[1] + c[2]) % 2]
    } else {
        crate::flesh::jelly(c, d)
    }
}

/// Fait pousser ses veines. Dans chaque pavé : un tronc d'un bout à l'autre (les veines
/// passent d'un pavé au suivant par les articulations), puis des rameaux tirés au hasard,
/// chacun parti d'un cube déjà veiné vers un voisin qui n'en touche aucun autre (des fils
/// d'un cube d'épaisseur, sans boucle, qui gagnent tout le pavé). Jamais dans la peau du
/// tour, ni dans le creux de la bouche, ni au sommet du crâne, ni au bout des mains et des
/// pieds. Toujours les mêmes (graine fixe).
fn grow_veins() -> [Vec<bool>; PARTS] {
    let grids = grids();
    std::array::from_fn(|part| {
        let d = grids[part];
        let at = |c: [usize; 3]| (c[2] * d[1] + c[1]) * d[0] + c[0];
        let limb = matches!(part, LEG_L | LEG_R | ARM_L | ARM_R);
        let ok = |c: [usize; 3]| {
            c[0] >= 1
                && c[0] + 1 < d[0]
                && c[2] >= 1
                && c[2] + 1 < d[2]
                && !(part == HEAD && (c[1] + 1 >= d[1] || mouth(c, d, 1.0)))
                && !(limb && c[1] == 0)
        };
        let mut rng = 0x9e37_79b9u32.wrapping_mul(part as u32 + 11) | 1;
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            (rng >> 8) as usize % n.max(1)
        };
        let mut cells = vec![false; d[0] * d[1] * d[2]];
        let mut list: Vec<[usize; 3]> = Vec::new();
        // Le tronc : au milieu (au fond du crâne dans la tête, derrière la bouche), il
        // serpente d'un cube de côté toutes les trois rangées.
        let (mut x, mut z) = (d[0] / 2, if part == HEAD { d[2] - 2 } else { d[2] / 2 });
        for y in 0..d[1] {
            if y % 3 == 2 {
                let (nx, nz) = match next(4) {
                    0 => (x + 1, z),
                    1 => (x.wrapping_sub(1), z),
                    2 => (x, z + 1),
                    _ => (x, z.wrapping_sub(1)),
                };
                if nx < d[0] && nz < d[2] && ok([nx, y, nz]) && ok([x, y, z]) {
                    if !cells[at([x, y, z])] {
                        cells[at([x, y, z])] = true;
                        list.push([x, y, z]);
                    }
                    (x, z) = (nx, nz);
                }
            }
            if ok([x, y, z]) && !cells[at([x, y, z])] {
                cells[at([x, y, z])] = true;
                list.push([x, y, z]);
            }
        }
        // Dans la poitrine, le ventre et la tête : une branche en travers toutes les trois
        // rangées, du tronc jusqu'aux deux flancs (vers les épaules, vers les hanches).
        if !limb {
            for y in (1..d[1]).step_by(3) {
                let Some(root) = list.iter().copied().find(|c| c[1] == y) else {
                    continue;
                };
                for dir in [-1i32, 1] {
                    let mut c = root;
                    loop {
                        let v = c[0] as i32 + dir;
                        if v < 0 {
                            break;
                        }
                        c[0] = v as usize;
                        if c[0] >= d[0] || !ok(c) || cells[at(c)] {
                            break;
                        }
                        // Sans toucher le tronc ailleurs qu'à sa racine.
                        let touches = (0..3).any(|a| {
                            a != 0
                                && [-1i32, 1].into_iter().any(|s| {
                                    let w = c[a] as i32 + s;
                                    let mut m = c;
                                    m[a] = w.max(0) as usize;
                                    w >= 0 && m[a] < d[a] && cells[at(m)]
                                })
                        });
                        if touches {
                            break;
                        }
                        cells[at(c)] = true;
                        list.push(c);
                    }
                }
            }
        }
        // Les rameaux.
        let room = (0..d[0] * d[1] * d[2])
            .filter(|i| ok([i % d[0], i / d[0] % d[1], i / (d[0] * d[1])]))
            .count();
        let goal = (room as f32 * if limb { 0.42 } else { 0.4 }) as usize;
        for _ in 0..60_000 {
            if list.len() >= goal || list.is_empty() {
                break;
            }
            // Une fois sur deux depuis un cube récent : les rameaux s'allongent.
            let from = if next(2) > 0 {
                list[list.len() - 1 - next(list.len().min(6))]
            } else {
                list[next(list.len())]
            };
            let (a, up) = (next(3), next(2) == 1);
            let mut n = from;
            if up {
                n[a] += 1;
            } else if n[a] > 0 {
                n[a] -= 1;
            } else {
                continue;
            }
            if n[a] >= d[a] || !ok(n) || cells[at(n)] {
                continue;
            }
            let mut near = 0;
            for b in 0..3 {
                for s in [-1i32, 1] {
                    let v = n[b] as i32 + s;
                    if v >= 0 && (v as usize) < d[b] {
                        let mut m = n;
                        m[b] = v as usize;
                        near += cells[at(m)] as usize;
                    }
                }
            }
            if near == 1 {
                cells[at(n)] = true;
                list.push(n);
            }
        }
        cells
    })
}

/// Un pavé de L'Évidé, cube par cube : sa peau au-dehors, le noir de sa bouche dans le creux
/// de la tête, de la gelée (`flesh::jelly`) là où on l'a mangé.
#[allow(clippy::too_many_arguments)]
fn emit_part(
    out: &mut Vec<EntityVertex>,
    xf: Mat4,
    min: [f32; 3],
    max: [f32; 3],
    part: usize,
    dims: [usize; 3],
    light: [f32; 3],
    flesh: &Flesh,
    open: f32,
    rows: std::ops::Range<usize>,
    style: u8,
) {
    // (v290) `rows` : les rangées de cubes (de bas en haut) dessinées dans ce repère ; un
    // membre plié est dessiné en deux fois, le bas dans son repère à lui.
    // Les six côtés d'un cube : (axe, sens), dans l'ordre des faces de `skin`.
    const SIDES: [(usize, i32); 6] = [(2, -1), (2, 1), (0, 1), (0, -1), (1, 1), (1, -1)];
    let (lo, hi) = (Vec3::from(min), Vec3::from(max));
    let step = (hi - lo) / Vec3::new(dims[0] as f32, dims[1] as f32, dims[2] as f32);
    let lit = EntityVertex::light(light, true);
    let jaw = |c: [usize; 3]| {
        if part == HEAD {
            jaw_of(style, c, dims, open)
        } else {
            Jaw::Flesh
        }
    };
    let hollow = |c: [usize; 3]| jaw(c) == Jaw::Hollow;
    for z in 0..dims[2] {
        for y in rows.clone() {
            for x in 0..dims[0] {
                let c = [x, y, z];
                let own = jaw(c);
                if flesh.gone(part, dims, c) || own == Jaw::Hollow {
                    continue;
                }
                for (k, (axis, sign)) in SIDES.into_iter().enumerate() {
                    let n = c[axis] as i32 + sign;
                    let outside = n < 0 || n >= dims[axis] as i32;
                    let mut nc = c;
                    if !outside {
                        nc[axis] = n as usize;
                    }
                    let rgb = if outside {
                        if own == Jaw::Tooth {
                            TOOTH[0]
                        } else {
                            skin(part, c, dims, k)
                        }
                    } else if hollow(nc) {
                        if own == Jaw::Tooth {
                            // (v299) Une dent : claire devant, dans l'ombre sur ses côtés.
                            TOOTH[(axis != 2 || z > 0) as usize]
                        } else {
                            // Plus noir au fond de la gorge.
                            MOUTH[(z * 8 / dims[2].max(1)).min(4) / 2]
                        }
                    } else if flesh.gone(part, dims, nc) {
                        // De la gelée, ou une veine mise à nu.
                        inside(part, c, dims)
                    } else {
                        continue;
                    };
                    let mut normal = Vec3::ZERO;
                    normal[axis] = sign as f32;
                    let shade = crate::player_model::face_shade(
                        xf.transform_vector3(normal).normalize_or_zero(),
                    );
                    let g = 0.97 + (hash(part * 7 + k, x, y, z) >> 24) as f32 / 255.0 * 0.06;
                    let color = srgb(rgb).map(|v| v * shade * g);
                    let mut o = lo + Vec3::new(x as f32, y as f32, z as f32) * step;
                    if sign > 0 {
                        o[axis] += step[axis];
                    }
                    let (a1, a2) = ((axis + 1) % 3, (axis + 2) % 3);
                    let (mut e1, mut e2) = (Vec3::ZERO, Vec3::ZERO);
                    e1[a1] = step[a1];
                    e2[a2] = step[a2];
                    let p = |s: f32, t: f32| xf.transform_point3(o + e1 * s + e2 * t);
                    let q = [p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
                    for v in [q[0], q[1], q[2], q[2], q[3], q[0]] {
                        out.push(EntityVertex {
                            pos: v.to_array(),
                            color,
                            uv: [0.0; 2],
                            layer: -1,
                            light: lit,
                        });
                    }
                }
            }
        }
    }
}

/// Rayon contre un pavé `[lo, hi]` : la distance d'entrée, s'il le touche devant soi.
fn ray_box(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for a in 0..3 {
        if d[a].abs() < 1e-9 {
            if o[a] < lo[a] || o[a] > hi[a] {
                return None;
            }
            continue;
        }
        let (x, y) = ((lo[a] - o[a]) / d[a], (hi[a] - o[a]) / d[a]);
        t0 = t0.max(x.min(y));
        t1 = t1.min(x.max(y));
    }
    (t0 <= t1).then_some(t0)
}

/// Une boîte debout sur `pos` (pieds), haute de `h`, ne touche aucun bloc solide.
fn fits(world: &World, pos: Vec3, h: f32) -> bool {
    let (min, max) = (
        pos - Vec3::new(HALF, 0.0, HALF),
        pos + Vec3::new(HALF, h, HALF),
    );
    let (lo, hi) = (min.floor().as_ivec3(), (max - Vec3::splat(1e-4)).floor().as_ivec3());
    for y in lo.y..=hi.y {
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                for (a, b) in crate::player::collision_boxes(world, x, y, z) {
                    if (0..3).all(|k| min[k] < b[k] - 1e-4 && max[k] > a[k] + 1e-4) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// (v277) La plus grande hauteur, de `CRAWL_H` à `HEIGHT`, à laquelle sa boîte tient là ;
/// `None` : même à plat ventre il n'y tient pas (un mur, une marche).
fn room(world: &World, pos: Vec3) -> Option<f32> {
    if !fits(world, pos, CRAWL_H) {
        return None;
    }
    if fits(world, pos, HEIGHT) {
        return Some(HEIGHT);
    }
    let (mut ok, mut bad) = (CRAWL_H, HEIGHT);
    for _ in 0..6 {
        let mid = (ok + bad) / 2.0;
        if fits(world, pos, mid) {
            ok = mid;
        } else {
            bad = mid;
        }
    }
    Some(ok)
}

/// (v277) La hauteur de son corps (le sommet de son modèle, en blocs) quand il est baissé
/// de `crawl` (0 debout … 1 à plat ventre).
pub fn top_of(crawl: f32) -> f32 {
    let a = Anim {
        crawl,
        hunch: 0.18,
        ..Anim::default()
    };
    let mut top = 0.0f32;
    for (_, xf, lo, hi) in segments(Vec3::ZERO, 0.0, &a) {
        for k in 0..8 {
            let c = Vec3::new(
                if k & 1 == 0 { lo[0] } else { hi[0] },
                if k & 2 == 0 { lo[1] } else { hi[1] },
                if k & 4 == 0 { lo[2] } else { hi[2] },
            );
            top = top.max(xf.transform_point3(c).y);
        }
    }
    top
}

/// (v277) « Il ne se penche pas plus que la taille dont il a besoin pour passer sous les
/// blocs ; jusqu'à 1 de hauteur sa taille s'adapte en proportion » : de combien il se baisse
/// (0 debout … 1 à plat ventre) pour que son corps tienne tout juste sous une hauteur `h`.
pub fn crawl_for(h: f32) -> f32 {
    const N: usize = 32;
    static TOPS: std::sync::OnceLock<[f32; N + 1]> = std::sync::OnceLock::new();
    let tops = TOPS.get_or_init(|| std::array::from_fn(|i| top_of(i as f32 / N as f32)));
    // Sous un bloc : à plat ventre.
    if h <= 1.0 {
        return 1.0;
    }
    match (0..=N).find(|&i| tops[i] <= h) {
        Some(0) => 0.0,
        Some(i) => {
            let (a, b) = (tops[i - 1], tops[i]);
            (i as f32 - 1.0 + (a - h) / (a - b).max(1e-4)) / N as f32
        }
        None => 1.0,
    }
}

/// (v272) La case (bloc) où sont ces pieds.
fn cell_of(p: Vec3) -> [i32; 3] {
    [p.x.floor() as i32, (p.y + 0.01).floor() as i32, p.z.floor() as i32]
}

/// (v272) De l'eau dans le bloc où est ce point.
fn water_at(world: &World, p: Vec3) -> bool {
    let id = world.get(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32);
    matches!(
        minelib_world::registry::fluid(id),
        Some((minelib_world::registry::Fluid::Water, _))
    )
}

/// (v272) Cases visitées au plus par une recherche de chemin, et écart entre deux recherches
/// (s) quand il poursuit.
const PATH_BUDGET: usize = 900;
const REPLAN_S: f32 = 0.7;

/// (v272) « Pas de recherche de chemin : règle ça. » Le chemin d'une case à une autre pour
/// quelqu'un qui se déplace comme lui : de case en case sur le sol (quatre directions), une
/// marche d'un bloc montée d'un saut, une chute de trois blocs au plus, à plat ventre là où
/// le plafond est à moins de 3,5 blocs (plus lent : ce chemin compte plus cher), à la nage
/// dans l'eau (en tous sens). A* sur les blocs du monde, `PATH_BUDGET` cases au plus : si le
/// but n'est pas atteint dans ce budget (trop loin, inaccessible), le chemin mène à la case
/// trouvée la plus proche de lui. Rend les cases à suivre, sans celle du départ.
pub fn find_path(world: &World, from: [i32; 3], to: [i32; 3]) -> Vec<[i32; 3]> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashMap};
    let mut solid: HashMap<[i32; 3], bool> = HashMap::new();
    let mut blocked = |c: [i32; 3]| {
        *solid
            .entry(c)
            .or_insert_with(|| !crate::player::collision_boxes(world, c[0], c[1], c[2]).is_empty())
    };
    let wet = |c: [i32; 3]| {
        matches!(
            minelib_world::registry::fluid(world.get(c[0], c[1], c[2])),
            Some((minelib_world::registry::Fluid::Water, _))
        )
    };
    // Peut-il être là, et à quel prix par case : debout 1, à la nage 2,2, à plat ventre 3,4
    // (le rapport des allures).
    let spot = |c: [i32; 3], blocked: &mut dyn FnMut([i32; 3]) -> bool| -> Option<f32> {
        if blocked(c) {
            return None;
        }
        if wet(c) {
            return Some(2.2);
        }
        if !blocked([c[0], c[1] - 1, c[2]]) && !wet([c[0], c[1] - 1, c[2]]) {
            return None;
        }
        // (v277) Selon la hauteur libre au-dessus : 1, 2, 3 blocs, ou toute sa taille.
        let free = (1..=3).take_while(|k| !blocked([c[0], c[1] + k, c[2]])).count();
        Some([3.4, 2.3, 1.5, 1.0][free])
    };
    let far = |c: [i32; 3]| {
        ((c[0] - to[0]).abs() + (c[2] - to[2]).abs()) as f32 + 1.2 * (c[1] - to[1]).abs() as f32
    };
    // (case, coût depuis le départ, d'où l'on vient)
    let mut nodes: Vec<([i32; 3], f32, u32)> = vec![(from, 0.0, u32::MAX)];
    let mut index: HashMap<[i32; 3], u32> = HashMap::from([(from, 0)]);
    let mut open = BinaryHeap::from([(Reverse((far(from) * 100.0) as u32), 0u32)]);
    let (mut best, mut best_far) = (0u32, far(from));
    let mut seen = 0;
    let mut next: Vec<([i32; 3], f32)> = Vec::with_capacity(8);
    while let Some((_, i)) = open.pop() {
        let (c, g, _) = nodes[i as usize];
        let d = far(c);
        if d < best_far {
            (best, best_far) = (i, d);
        }
        // Arrivé : sur sa case, ou tout contre (la proie peut être en l'air).
        if (c[0] - to[0]).abs() + (c[2] - to[2]).abs() <= 1 && (c[1] - to[1]).abs() <= 2 {
            best = i;
            break;
        }
        seen += 1;
        if seen > PATH_BUDGET {
            break;
        }
        next.clear();
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = [c[0] + dx, c[1], c[2] + dz];
            if blocked(n) {
                // Une marche : il lui faut la place de sauter.
                let up = [n[0], n[1] + 1, n[2]];
                if !blocked([c[0], c[1] + 1, c[2]]) {
                    if let Some(k) = spot(up, &mut blocked) {
                        next.push((up, k + 0.6));
                    }
                }
            } else if let Some(k) = spot(n, &mut blocked) {
                next.push((n, k));
            } else {
                // Un vide : il se laisse tomber, de trois blocs au plus.
                for fall in 1..=3 {
                    let down = [n[0], n[1] - fall, n[2]];
                    if blocked(down) {
                        break;
                    }
                    if let Some(k) = spot(down, &mut blocked) {
                        next.push((down, k + 0.3 * fall as f32));
                        break;
                    }
                }
            }
        }
        if wet(c) {
            for dy in [1, -1] {
                let n = [c[0], c[1] + dy, c[2]];
                if !blocked(n) && wet(n) {
                    next.push((n, 2.2));
                }
            }
        }
        for &(n, k) in &next {
            let cost = g + k;
            let j = match index.get(&n) {
                Some(&j) if nodes[j as usize].1 <= cost => continue,
                Some(&j) => {
                    nodes[j as usize] = (n, cost, i);
                    j
                }
                None => {
                    nodes.push((n, cost, i));
                    index.insert(n, nodes.len() as u32 - 1);
                    nodes.len() as u32 - 1
                }
            };
            open.push((Reverse(((cost + far(n)) * 100.0) as u32), j));
        }
    }
    let mut path = Vec::new();
    let mut i = best;
    while i != u32::MAX && i != 0 {
        path.push(nodes[i as usize].0);
        i = nodes[i as usize].2;
    }
    path.reverse();
    path
}

/// Sa proie : le joueur (pieds, yeux).
#[derive(Clone, Copy, Debug)]
pub struct Prey {
    pub feet: Vec3,
    pub eye: Vec3,
    /// (v272) L'endroit de son corps que la prochaine morsure enlèvera, là où il est en ce
    /// moment (un bras qui se balance bouge) : « il se penche jusqu'à la hauteur où il mord »
    /// et « suit le mouvement du bras, sa hauteur aussi, en le mangeant ».
    pub spot: Option<Vec3>,
    /// (v290) Le milieu du membre (ou du buste) où est cet endroit : « il tourne la tête vers
    /// le centre du membre attaqué ».
    pub limb: Option<Vec3>,
    /// (v312) Qui c'est : 0 nous ; sinon un autre joueur du monde en ligne (son rang, plus
    /// un, dans la liste que la partie a donnée à `Horde::update`).
    pub tag: u16,
}

/// Ce qui vient de se passer, pour la partie (sons, blessures).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// Il grogne ou ricane, là.
    Voice(Vec3),
    /// Sa bouche vient de se refermer sur nous (`low` : en rampant, sur nos jambes).
    /// (v312) `who` : son numéro ; `victim` : sur qui (`Prey::tag`) ; `aim` : le tirage de
    /// l'endroit qu'il visait (celui de son `Aims`).
    Bites {
        low: bool,
        at: Vec3,
        seed: u32,
        who: u32,
        victim: u16,
        aim: u32,
    },
    /// (v272) Il choisit où mordre ensuite (`low` : en rampant, nos jambes seulement) : à la
    /// partie de lui dire où c'est (`Prey::spot`). (v312) `victim` : sur qui.
    Aims { low: bool, seed: u32, victim: u16 },
    /// (v299) « Quand on mange le monstre, de la gelée coule comme le sang » ; mort, il fond
    /// en gelée, puis ses veines en gouttes de leur couleur : `count` gouttes de ce liquide
    /// partent de là (`spurt` : un jet, la bouchée vient d'arracher le morceau).
    Drips { at: Vec3, liquid: Liquid, count: u8, spurt: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    /// Sa ronde : il marche, s'arrête, tourne.
    Wander,
    /// Il nous a vus : il vient.
    Chase,
}

/// Un Évidé.
#[derive(Clone, Debug)]
pub struct Evide {
    /// (v272) Son numéro dans la horde (pour le retrouver d'une image à l'autre).
    pub id: u32,
    /// Ses pieds (en rampant : le milieu de sa boîte, à l'avant du corps).
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub anim: Anim,
    /// Ce qu'on lui a mangé (ne revient pas).
    pub flesh: Flesh,
    on_ground: bool,
    crawling: bool,
    /// (v277) La hauteur de sa boîte en ce moment (de `CRAWL_H` à `HEIGHT`) : celle du
    /// passage où il est, pas moins.
    tall: f32,
    mode: Mode,
    /// Où il nous a vus pour la dernière fois, et depuis quand il ne nous voit plus (s).
    known: Option<Vec3>,
    unseen: f32,
    /// Ronde : cap, temps avant d'en changer, à l'arrêt.
    heading: f32,
    turn_in: f32,
    pausing: bool,
    /// Bloqué contre un mur depuis (s) ; détour en cours (s).
    blocked: f32,
    detour: f32,
    /// (v272) Le chemin qu'il suit (cases), vers quelle case, temps avant de le refaire (s).
    path: Vec<[i32; 3]>,
    path_goal: [i32; 3],
    replan: f32,
    /// (v272) Il nage.
    swimming: bool,
    /// Morsure en cours : temps dans le geste (s).
    feed: f32,
    feeding: bool,
    /// (v272) Il a choisi où mordre pour la morsure en cours.
    aimed: bool,
    /// Il sursaute quand on le mord (s).
    flinch: f32,
    /// (v299) La forme de sa bouche (`mouth_of`).
    mouth_style: u8,
    /// (v299) Son cri : ce qu'il en reste (s) ; `maw` : il fait sa gueule ronde (`MAW_STYLE`).
    scream: f32,
    maw: bool,
    /// (v299) Mort, il tombe (`corpse::Ragdoll`) puis fond : secondes de fonte écoulées.
    corpse: Option<crate::corpse::Ragdoll>,
    melt: f32,
    /// (v299) La gelée coule de ce qu'on lui a mangé : le morceau qu'on vient d'arracher (un
    /// jet au pas suivant), puis ses plaies (pavé, cube, âge, gouttes en attente).
    spurt: Option<(usize, [usize; 3])>,
    ooze: Vec<(usize, [usize; 3], f32, f32)>,
    /// Sa fin : depuis quand il s'affaisse (s).
    dying: Option<f32>,
    /// Regard : lacet par rapport au corps (positif vers sa droite), tangage.
    look: (f32, f32),
    /// Tics de la tête : roulis visé, temps avant le prochain.
    tic: (f32, f32),
    /// Temps avant sa prochaine voix (s).
    voice: f32,
    age: f32,
    rng: u32,
    /// (v312) Le tirage de l'endroit visé pour la morsure en cours (son dernier `Aims`).
    aim_seed: u32,
    /// (v312) En ligne, le pantin d'un Évidé que mène un autre joueur : (pseudo du meneur,
    /// son numéro chez lui). Figé (`frozen`) : il ne pense pas, il suit ce que le relais en
    /// publie (`goal`, depuis `silent` secondes ; `fed` : morceaux mangés déjà retirés ;
    /// `cry` : il vient de se mettre à crier).
    pub lead: Option<(String, u16)>,
    goal: Option<minelib_net::evide::Mob>,
    silent: f32,
    fed: usize,
    cry: bool,
    /// Essais : figé dans cette posture (aucun mouvement, aucune pensée).
    frozen: bool,
    /// (v290) Essais : il va et vient autour de ce point (voir `update`).
    shuttle: Option<Vec3>,
}

impl Evide {
    pub fn new(pos: Vec3, yaw: f32, seed: u32) -> Self {
        let mut e = Self {
            id: seed,
            pos,
            vel: Vec3::ZERO,
            yaw,
            anim: Anim {
                hunch: 0.18,
                ..Anim::default()
            },
            flesh: Flesh::of(crate::flesh::Hard::Veins),
            on_ground: false,
            crawling: false,
            tall: HEIGHT,
            mode: Mode::Wander,
            known: None,
            unseen: 0.0,
            heading: yaw,
            turn_in: 0.0,
            pausing: false,
            blocked: 0.0,
            detour: 0.0,
            path: Vec::new(),
            path_goal: [i32::MAX; 3],
            replan: 0.0,
            swimming: false,
            feed: 0.0,
            feeding: false,
            aimed: false,
            flinch: 0.0,
            mouth_style: MOUTH_STYLE,
            scream: 0.0,
            maw: false,
            corpse: None,
            melt: 0.0,
            spurt: None,
            ooze: Vec::new(),
            dying: None,
            look: (0.0, -0.12),
            tic: (0.0, 1.0),
            voice: 0.0,
            age: 0.0,
            rng: seed.wrapping_mul(0x9e37_79b9) | 1,
            aim_seed: 0,
            lead: None,
            goal: None,
            silent: 0.0,
            fed: 0,
            cry: false,
            frozen: false,
            shuttle: None,
        };
        e.voice = 2.0 + 4.0 * e.unit();
        e
    }

    /// Essais : un Évidé figé dans une posture (`debout`, `court`, `rampe`, `mord`, `mange`).
    pub fn posed(pos: Vec3, yaw: f32, pose: &str) -> Self {
        let mut e = Self::new(pos, yaw, 7);
        e.frozen = true;
        let a = &mut e.anim;
        match pose {
            "court" => {
                (a.walk, a.swing, a.hunch, a.arms, a.mouth) = (1.2, 0.9, 0.38, 1.25, 0.35);
            }
            // (v290) « bas25 », « bas50 »… : baissé d'autant (pour cent), en marchant.
            low if low.starts_with("bas") => {
                let c = low[3..].parse::<f32>().unwrap_or(50.0).clamp(0.0, 100.0) / 100.0;
                (a.walk, a.swing, a.crawl, a.mouth) = (1.2, 0.8, c, 0.35);
                e.tall = lerp(HEIGHT, CRAWL_H, c);
            }
            "navette" => {
                (a.swing, a.hunch, a.arms, a.mouth) = (0.9, 0.38, 1.25, 0.35);
                e.shuttle = Some(pos);
            }
            "rampe" => {
                (a.walk, a.swing, a.crawl, a.mouth) = (1.2, 1.0, 1.0, 0.35);
                e.crawling = true;
                e.tall = CRAWL_H;
            }
            "mord" => {
                (a.hunch, a.arms, a.grab, a.mouth, a.head_pitch) = (1.5, 0.35, 1.0, 1.0, -0.2);
            }
            // (v299) « bouche3 » : debout, la bouche de la forme 3 grande ouverte ; « bouche3r » :
            // au repos ; « bouche3f » : refermée.
            b if b.starts_with("bouche") => {
                let rest = &b[6..];
                let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                e.mouth_style = digits.parse::<u8>().unwrap_or(0) % MOUTH_STYLES;
                a.mouth = match &rest[digits.len()..] {
                    "r" => 0.0,
                    "f" => -0.5,
                    _ => 1.0,
                };
            }
            "mange" => {
                let grids = grids();
                for k in 0..9u32 {
                    e.flesh
                        .bite_somewhere(k * 53 + 11, OUR_BITE, &[0, 1, 2, 3, 4, 5, 6], &grids);
                }
            }
            _ => {}
        }
        e
    }

    fn unit(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    /// (v299) Il n'en reste plus rien (tombé, fondu en gelée, ses veines aussi) : à retirer.
    pub fn is_dead(&self) -> bool {
        self.dying.is_some() && self.melt >= MELT_S + VEIN_S + 0.3
    }

    /// (v299) Son corps là où il est : sa posture vivant ; mort, là où son pantin est tombé.
    fn body(&self) -> Limbs {
        let mut l = limbs(self.pos, self.yaw, &self.anim);
        if let Some(c) = &self.corpse {
            let f = c.frames();
            for i in 0..PARTS {
                l.parts[i].0 = f[i][0];
                if l.lower[i].is_some() {
                    l.lower[i] = Some(f[i][1]);
                }
            }
        }
        l
    }

    pub fn is_dying(&self) -> bool {
        self.dying.is_some()
    }

    /// (v290) Le milieu de son pavé `part` (monde), dans sa posture du moment : ce vers quoi
    /// on tourne la tête en le mordant (« le centre du membre attaqué »).
    pub fn part_point(&self, part: usize) -> Vec3 {
        let (xf, lo, hi) = parts(self.pos, self.yaw, &self.anim)[part.min(PARTS - 1)];
        xf.transform_point3((Vec3::from(lo) + Vec3::from(hi)) / 2.0)
    }

    /// Son cou (à peu près) : d'où il regarde, vers où il se penche.
    fn neck(&self) -> Vec3 {
        let r = crate::player_model::rig();
        let c = self.anim.crawl;
        let pitch = lerp(self.anim.hunch, FRAC_PI_2, c);
        let hip = lerp(r.leg * TALL * r.px, CRAWL_HIP, c) - step_drop(&self.anim) * r.px;
        let len = r.body_h * TALL * r.px;
        self.pos
            + Vec3::Y * (hip + len * pitch.cos())
            + forward(self.yaw) * (len * pitch.sin() - c * CRAWL_BACK)
    }

    /// (v272) Où est, dans le monde, le milieu du cube `cell` de son pavé `part` : le morceau
    /// de lui qu'on vise pour le manger, là où il est maintenant.
    pub fn cell_point(&self, part: usize, cell: [usize; 3]) -> Vec3 {
        let part = part.min(PARTS - 1);
        let l = self.body();
        let (xf, lo, hi) = l.parts[part];
        let d = grids()[part];
        // (v290) Sous le genou ou le coude : dans le repère du bas du membre.
        let xf = l.lower[part].filter(|_| cell[1] < d[1] / 2).unwrap_or(xf);
        let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
        let t = Vec3::new(
            (cell[0] as f32 + 0.5) / d[0] as f32,
            (cell[1] as f32 + 0.5) / d[1] as f32,
            (cell[2] as f32 + 0.5) / d[2] as f32,
        );
        xf.transform_point3(lo + (hi - lo) * t)
    }

    /// (v299) La forme de sa bouche en ce moment : la sienne, ou sa gueule ronde quand il crie.
    pub fn face(&self) -> u8 {
        if self.maw {
            MAW_STYLE
        } else {
            self.mouth_style
        }
    }

    /// (v272) Le milieu de sa bouche, dans le monde, dans sa posture du moment.
    pub fn mouth_point(&self) -> Vec3 {
        let (xf, lo, hi) = parts(self.pos, self.yaw, &self.anim)[HEAD];
        xf.transform_point3(Vec3::new(0.0, lo[1] + 0.28 * (hi[1] - lo[1]), lo[2]))
    }

    /// Ses yeux (il n'en a pas : là où ils seraient).
    pub fn eye(&self) -> Vec3 {
        self.neck() + Vec3::Y * 0.28
    }

    /// Voit-il ce point ? Sur son « écran » (le champ du jeu, tourné comme sa tête), à moins
    /// de `SEE` blocs, sans bloc plein entre les deux.
    pub fn sees(&self, world: &World, target: Vec3) -> bool {
        let eye = self.eye();
        let to = target - eye;
        let dist = to.length();
        if dist > SEE {
            return false;
        }
        // (À moins de `FEEL` blocs il nous sent tout autour de lui : penché sur nous pour
        // mordre, sa tête nous dépasse et on n'est plus sur son « écran ».)
        if dist > FEEL {
            let f = forward(self.yaw + self.look.0);
            let right = Vec3::new(-f.z, 0.0, f.x);
            let (depth, side) = (to.dot(f), to.dot(right));
            if depth <= 0.05 || side.abs() > depth * VIEW_TAN_X {
                return false;
            }
            let up = to.y.atan2(depth) - self.look.1;
            if up.tan().abs() > VIEW_TAN_Y || up.abs() > FRAC_PI_2 {
                return false;
            }
        }
        minelib_world::raycast::raycast_with(world, eye.into(), (to / dist.max(1e-3)).into(), dist, |id| {
            id.def().full
        })
        .is_none()
    }

    /// Déplace sa boîte de `delta`, axe par axe, jusqu'au contact : (un mur l'a arrêté, le sol
    /// l'a arrêté, le plafond l'a arrêté).
    fn slide(&mut self, world: &World, delta: Vec3, h: f32) -> (bool, bool, bool) {
        let mut hit = [false; 3];
        for axis in [0usize, 2, 1] {
            let d = delta[axis];
            if d == 0.0 {
                continue;
            }
            let mut p = self.pos;
            p[axis] += d;
            if fits(world, p, h) {
                self.pos = p;
                continue;
            }
            hit[axis] = true;
            let (mut ok, mut bad) = (0.0f32, d);
            for _ in 0..6 {
                let mid = (ok + bad) / 2.0;
                let mut p = self.pos;
                p[axis] += mid;
                if fits(world, p, h) {
                    ok = mid;
                } else {
                    bad = mid;
                }
            }
            self.pos[axis] += ok;
        }
        (
            hit[0] || hit[2],
            hit[1] && delta.y < 0.0,
            hit[1] && delta.y > 0.0,
        )
    }

    /// Un pas de sa vie : il regarde, décide, se déplace, s'anime.
    pub fn update(&mut self, world: &World, dt: f32, prey: Option<Prey>) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(t) = &mut self.dying {
            // (v299) « Quand il meurt il tombe au sol et se transforme progressivement en
            // gelée partout où il était allongé ; ça finit comme le sang mais en gelée, et on
            // voit aussi ses veines sous forme de gouttes, de la couleur de ses veines » : il
            // s'effondre selon les blocs autour de lui (`corpse::Ragdoll`) ; figé (ou au bout
            // de `MELT_WAIT`), sa chair fond cube par cube pendant `MELT_S`, chaque cube en
            // gouttes de gelée qui tachent le sol dessous ; puis ses veines, en `VEIN_S`.
            // (Avant : il s'affaissait sur place en 0,7 s et disparaissait.)
            *t += dt;
            let age = *t;
            self.anim.mouth = approach(self.anim.mouth, 1.0, dt * 6.0);
            if let Some(c) = &mut self.corpse {
                c.update(world, dt);
            }
            if age >= MELT_WAIT || self.corpse.as_ref().is_none_or(|c| c.still()) {
                self.melt += dt;
                let dims = grids();
                let mut gone = self.flesh.melt(false, (self.melt / MELT_S).min(1.0), &dims);
                let mut liquid = Liquid::Jelly;
                if self.melt > MELT_S {
                    liquid = Liquid::Vein;
                    gone = self.flesh.melt(true, ((self.melt - MELT_S) / VEIN_S).min(1.0), &dims);
                }
                // Quelques gouttes parmi ce qui vient de fondre (pas une par cube : il y en
                // a des milliers).
                let every = gone.len().div_ceil(MELT_DROPS).max(1);
                for (part, cell) in gone.into_iter().step_by(every) {
                    events.push(Event::Drips {
                        at: self.cell_point(part, cell),
                        liquid,
                        count: 1,
                        spurt: false,
                    });
                }
            }
            return events;
        }
        // (v299) La gelée coule de ce qu'on lui a mangé : un jet quand le morceau part, puis
        // des gouttes de plus en plus rares pendant `OOZE_S` secondes, de là où est la plaie.
        if let Some((part, cell)) = self.spurt.take() {
            events.push(Event::Drips {
                at: self.cell_point(part, cell),
                liquid: Liquid::Jelly,
                count: OOZE_SPURT,
                spurt: true,
            });
            self.ooze.push((part, cell, 0.0, 0.0));
        }
        for k in 0..self.ooze.len() {
            let o = &mut self.ooze[k];
            o.2 += dt;
            o.3 += dt * OOZE_RATE * (-o.2 / OOZE_FADE_S).exp();
            let n = o.3 as u8;
            o.3 -= f32::from(n);
            let (part, cell) = (o.0, o.1);
            if n > 0 {
                events.push(Event::Drips {
                    at: self.cell_point(part, cell),
                    liquid: Liquid::Jelly,
                    count: n,
                    spurt: false,
                });
            }
        }
        self.ooze.retain(|o| o.2 < OOZE_S);
        let here = ChunkPos::containing(self.pos.x.floor() as i32, self.pos.z.floor() as i32);
        // (v290) Essais (posture « navette ») : figé, il va et vient de côté en marchant, à la
        // vitesse d'une course (captures de la rémanence, caméra immobile).
        if let (true, Some(from)) = (self.frozen && dt > 0.0, self.shuttle) {
            self.age += dt;
            let side = Vec3::new(self.yaw.cos(), 0.0, self.yaw.sin());
            self.pos = from + side * (3.0 * (self.age * 1.6).sin());
            self.anim.walk += dt * 9.0;
        }
        // (v312) Le pantin d'un Évidé mené ailleurs : il rejoint ce que le relais en publie,
        // et crie quand l'autre crie.
        if self.lead.is_some() && dt > 0.0 {
            self.silent += dt;
            self.follow((dt * 14.0).min(1.0), false);
            if std::mem::take(&mut self.cry) {
                events.push(Event::Voice(self.eye()));
            }
        }
        if self.frozen || dt <= 0.0 || !world.contains(here) {
            return events;
        }
        let [walk, run, sneak, gravity, jump] = crate::player::GAIT;
        self.age += dt;
        self.flinch = (self.flinch - dt).max(0.0);
        let grids = grids();
        let legless = self.flesh.eaten_of(LEG_L, grids[LEG_L]) > 0.6
            && self.flesh.eaten_of(LEG_R, grids[LEG_R]) > 0.6;

        // Ce qu'il voit.
        let seen = prey
            .is_some_and(|p| self.sees(world, p.eye) || self.sees(world, p.feet + Vec3::Y * 0.5));
        if let (Some(p), true) = (prey, seen) {
            if self.mode == Mode::Wander {
                // Il vient de nous voir : il le dit.
                self.voice = self.voice.min(0.15);
            }
            self.mode = Mode::Chase;
            self.known = Some(p.feet);
            self.unseen = 0.0;
        } else {
            self.unseen += dt;
            if self.mode == Mode::Chase && self.unseen > GIVE_UP_S {
                self.mode = Mode::Wander;
                self.known = None;
                self.turn_in = 0.0;
            }
        }
        self.voice -= dt;
        if self.voice <= 0.0 {
            self.voice = 6.0 + 8.0 * self.unit();
            events.push(Event::Voice(self.eye()));
            self.scream = SCREAM_S;
        }

        // Ce qu'il veut : un cap, une allure.
        let mut wish = self.yaw;
        let mut speed = 0.0;
        let mut feeding = false;
        // (v272) En mordant : l'allure qui amène sa bouche sur l'endroit visé (il avance,
        // recule, suit le membre qui bouge).
        let mut shuffle: Option<f32> = None;
        match (self.mode, self.known) {
            (Mode::Chase, Some(target)) => {
                let to = target - self.pos;
                let flat = Vec3::new(to.x, 0.0, to.z);
                let d = flat.length();
                if d > 0.05 {
                    wish = flat.x.atan2(-flat.z);
                }
                let hold = if self.crawling { CRAWL_NEAR } else { HOLD };
                let slack = if self.feeding { 0.6 } else { 0.0 };
                if seen && d <= hold + slack && (-2.0..=2.6).contains(&to.y) {
                    feeding = true;
                    // Tourné vers l'endroit visé, sa bouche dessus.
                    let spot = prey
                        .and_then(|p| p.spot)
                        .unwrap_or(target + Vec3::Y * 1.1);
                    let to = spot - self.pos;
                    let flat = Vec3::new(to.x, 0.0, to.z);
                    if flat.length() > 0.2 {
                        wish = flat.x.atan2(-flat.z);
                    }
                    shuffle = Some(if self.crawling {
                        ((flat.length() - CRAWL_HOLD) * 3.0).clamp(-0.6, sneak)
                    } else if self.feed % BITE_S >= BITE_BACK {
                        // (v290) Redressé entre deux morsures : il reste où il est (sa bouche
                        // n'est plus sur nous, il ne court pas après).
                        0.0
                    } else {
                        let miss = spot - self.mouth_point();
                        (miss.dot(forward(self.yaw)) * 5.0).clamp(-2.0, 3.0)
                    });
                } else if self.detour > 0.0 {
                    self.detour -= dt;
                    wish = self.heading;
                    speed = run;
                } else if seen || d > 0.7 {
                    speed = run;
                    // (v272) Par le chemin (`find_path`), refait toutes les `REPLAN_S`
                    // secondes ou dès que la proie change de case ; tout près et en vue, droit
                    // sur elle.
                    self.replan -= dt;
                    let goal = cell_of(target);
                    if self.replan <= 0.0 || (goal != self.path_goal && self.replan < REPLAN_S - 0.25) {
                        self.replan = REPLAN_S;
                        self.path_goal = goal;
                        self.path = find_path(world, cell_of(self.pos), goal);
                    }
                    while let Some(n) = self.path.first() {
                        let mid = Vec3::new(n[0] as f32 + 0.5, n[1] as f32, n[2] as f32 + 0.5);
                        let off = mid - self.pos;
                        if Vec3::new(off.x, 0.0, off.z).length() < 0.35 && off.y.abs() < 1.3 {
                            self.path.remove(0);
                        } else {
                            if !(seen && d < 2.5) && Vec3::new(off.x, 0.0, off.z).length() > 0.05 {
                                wish = off.x.atan2(-off.z);
                            }
                            break;
                        }
                    }
                } else {
                    // Arrivé là où il nous a vus : il continue un peu, il cherche.
                    wish = self.yaw + 0.6 * (self.age * 0.7).sin();
                    speed = walk;
                }
            }
            _ => {
                self.turn_in -= dt;
                if self.turn_in <= 0.0 {
                    // « Il tourne assez régulièrement. »
                    self.pausing = !self.pausing && self.unit() < 0.4;
                    self.turn_in = if self.pausing {
                        1.0 + 1.5 * self.unit()
                    } else {
                        2.0 + 3.5 * self.unit()
                    };
                    self.heading = self.yaw + (self.unit() * 2.0 - 1.0) * 2.1;
                }
                wish = self.heading;
                speed = if self.pausing { 0.0 } else { walk };
            }
        }
        if !feeding {
            self.feed = 0.0;
            self.aimed = false;
        }
        self.feeding = feeding;

        // Pris dans un bloc (sorti d'un œuf tout contre un mur, bloc posé sur lui) : il s'en
        // dégage, par le côté le plus proche ou par le dessus.
        if !fits(world, self.pos, CRAWL_H) {
            let free = [0.35f32, 0.7, 1.05].into_iter().find_map(|r| {
                [
                    Vec3::X,
                    Vec3::NEG_X,
                    Vec3::Z,
                    Vec3::NEG_Z,
                    Vec3::new(1.0, 0.0, 1.0),
                    Vec3::new(1.0, 0.0, -1.0),
                    Vec3::new(-1.0, 0.0, 1.0),
                    Vec3::new(-1.0, 0.0, -1.0),
                    Vec3::Y,
                ]
                .into_iter()
                .map(|d| self.pos + d * r)
                .find(|p| fits(world, *p, CRAWL_H))
            });
            if let Some(p) = free {
                self.pos = p;
            }
        }
        // Sa posture : il rampe sous un plafond trop bas (ou sans jambes), se relève dès
        // qu'il en a la place.
        let dir = forward(self.yaw);
        // (v272) « Il ne nage pas : règle ça. » Dans l'eau assez profonde il nage, à plat,
        // aux allures de nage du joueur ; il flotte à la surface, plonge si sa proie est
        // dessous, se hisse sur la berge.
        let [swim, swim_fast, swim_up] = crate::player::SWIM;
        self.swimming = water_at(world, self.pos + Vec3::Y * 0.7)
            || (self.swimming && water_at(world, self.pos + Vec3::Y * 0.3));
        // (v277) « Il ne se penche pas plus que la taille dont il a besoin pour passer sous
        // les blocs ; jusqu'à 1 de hauteur sa taille s'adapte en proportion » : sa boîte prend
        // la hauteur libre là où il est et juste devant lui (plus loin quand il court : il se
        // baisse avant d'y être), son corps se baisse d'autant (`crawl_for`), son allure
        // aussi, de la sienne debout à celle du sneak à plat ventre. Il se baisse vite, se
        // redresse dès qu'il en a la place. Sans jambes ou à la nage : à plat.
        let here = room(world, self.pos).unwrap_or(CRAWL_H);
        let mut want = here;
        if speed > 0.0 {
            for reach in [0.6, 0.6 + speed * 0.12] {
                if let Some(ahead) = room(world, self.pos + dir * reach) {
                    want = want.min(ahead);
                }
            }
        }
        let flat = legless || self.swimming;
        if flat {
            want = CRAWL_H;
        }
        self.tall = if flat {
            CRAWL_H
        } else if want < self.tall {
            (self.tall - 8.0 * dt).max(want).min(here)
        } else {
            (self.tall + 2.5 * dt).min(want)
        };
        self.crawling = self.tall < LOW;
        let stoop = crawl_for(self.tall);
        if speed > 0.0 {
            speed = if !self.swimming {
                lerp(speed, sneak, stoop)
            } else if self.mode == Mode::Chase {
                swim_fast
            } else {
                swim
            };
        }
        let h = self.tall;

        // Il se tourne, puis avance où il regarde.
        let gap = turn(wish, self.yaw);
        let rate = if self.mode == Mode::Chase { 6.0 } else { 2.5 };
        self.yaw += gap.clamp(-rate * dt, rate * dt);
        let dir = forward(self.yaw);
        let go = shuffle.unwrap_or(speed * gap.cos().max(0.0));
        self.vel.x = dir.x * go;
        self.vel.z = dir.z * go;
        self.vel.y = (self.vel.y - gravity * dt).max(-60.0);
        if self.swimming {
            let dive = seen && self.known.is_some_and(|k| k.y < self.pos.y - 1.0);
            self.vel.y = if dive {
                -swim_up * 0.6
            } else if water_at(world, self.pos + Vec3::Y * 0.95) {
                swim_up
            } else {
                0.0
            };
        }
        let before = self.pos;
        let steps = (dt / 0.02).ceil().max(1.0);
        let (mut wall, mut floor, mut ceiling) = (false, false, false);
        for _ in 0..steps as usize {
            let (w, f, c) = self.slide(world, self.vel * (dt / steps), h);
            wall |= w;
            floor |= f;
            ceiling |= c;
        }
        if floor {
            self.on_ground = true;
            self.vel.y = 0.0;
        } else if (self.pos.y - before.y).abs() > 1e-4 {
            self.on_ground = false;
        }
        if ceiling {
            self.vel.y = 0.0;
        }
        if wall && go > 0.1 {
            self.blocked += dt;
            if self.swimming {
                // La berge : il se hisse dessus.
                let up = self.pos + Vec3::Y * 1.05;
                if fits(world, up, h) && fits(world, up + dir * 0.5, h) {
                    self.vel.y = jump;
                    self.pos.y += 0.05;
                    self.swimming = false;
                }
            } else if self.on_ground {
                let up = self.pos + Vec3::Y * 1.05;
                let ahead = dir * 0.5;
                if fits(world, up, h) && fits(world, up + ahead, h) {
                    // Une marche d'un bloc : il saute, comme un joueur.
                    self.vel.y = jump;
                    self.on_ground = false;
                } else if let Some(low) =
                    room(world, up + ahead).filter(|r| *r < h && fits(world, up, *r))
                {
                    // Un passage bas en haut d'une marche : il se baisse d'abord, puis saute.
                    self.tall = low;
                } else if self.blocked > 0.7 {
                    // Rien à faire ici : il longe l'obstacle un moment.
                    let side = if self.unit() < 0.5 { -1.0 } else { 1.0 };
                    self.heading = self.yaw + side * 1.4;
                    self.detour = 0.9;
                    self.turn_in = self.turn_in.max(0.9);
                    self.blocked = 0.0;
                }
            }
        } else {
            self.blocked = (self.blocked - dt).max(0.0);
        }
        let moved = Vec3::new(self.pos.x - before.x, 0.0, self.pos.z - before.z).length() / dt;

        // Sa morsure : il attrape, se penche, ouvre grand, referme.
        let mut mouth = match self.mode {
            Mode::Chase => 0.35,
            Mode::Wander => 0.0,
        };
        let mut lean = match self.mode {
            Mode::Chase => 0.38,
            Mode::Wander => 0.18 + 0.03 * (self.age * 0.8).sin(),
        };
        let mut arms = match self.mode {
            Mode::Chase => 1.25,
            Mode::Wander => 0.06 + 0.05 * (self.age * 1.1).sin(),
        };
        let mut spot = None;
        let mut limb = None;
        let mut holding = false;
        if let (true, Some(p)) = (feeding, prey) {
            if !self.aimed {
                // Où mordre cette fois : la partie nous dira où c'est, image après image.
                self.aimed = true;
                let seed = self.rng;
                self.unit();
                self.aim_seed = seed;
                events.push(Event::Aims {
                    low: self.crawling,
                    seed,
                    victim: p.tag,
                });
            }
            let was = self.feed % BITE_S;
            self.feed += dt;
            let t = self.feed % BITE_S;
            if was < BITE_AT && t >= BITE_AT && self.feed > BITE_S * 0.5 {
                // (v272) « Si sa bouche se décolle avant, ça ne nous mange pas » : elle se
                // referme dans le vide si l'endroit visé lui a échappé.
                let mouth = self.mouth_point();
                if p.spot.is_none_or(|s| s.distance(mouth) <= TOUCH) {
                    let seed = self.rng;
                    self.unit();
                    events.push(Event::Bites {
                        low: self.crawling,
                        at: mouth,
                        seed,
                        who: self.id,
                        victim: p.tag,
                        aim: self.aim_seed,
                    });
                }
                self.aimed = false;
            }
            mouth = if t < 0.45 {
                0.3
            } else if t < BITE_AT - 0.04 {
                0.3 + 0.7 * ((t - 0.45) / 0.45).min(1.0)
            } else if t < 1.3 {
                -0.5
            } else {
                -0.5 + 0.8 * (t - 1.3) / (BITE_S - 1.3)
            };
            let at = p.spot.unwrap_or(p.feet + Vec3::Y * 1.1);
            // (v290) « … et revient à sa position initiale à la fin de la morsure, pareil pour
            // le monstre » : la bouche refermée, il se redresse, lâche prise et nous regarde
            // (`BITE_BACK`), avant de se pencher pour la morsure suivante.
            let back = t >= BITE_BACK;
            if !back {
                spot = Some(at);
                limb = p.limb;
            }
            if !self.crawling && !back {
                holding = true;
                // (v272) « Il se penche jusqu'à la hauteur où il mord » : son buste se plie
                // aux hanches de ce qu'il manque à sa bouche pour être à la hauteur visée
                // (un cran d'un pas de Newton : sa bouche descend d'environ 1,4 bloc par
                // radian), image après image : elle suit un bras qui monte et descend.
                let miss = at.y - self.mouth_point().y;
                lean = (self.anim.hunch - miss * 0.7).clamp(0.6, LEAN_MAX);
                // Ses bras autour de nous ; plié bas, ils partent devant pour ne pas
                // entrer dans le sol.
                let shoulder = self.neck().y - self.pos.y - 0.25;
                arms = (shoulder / 1.1).clamp(0.0, 1.0).acos().max(0.3);
            }
        }
        if self.flinch > 0.0 {
            mouth = 1.0;
        }
        // (v299) « Il peut faire ce visage aussi, mais pas quand il mord » : quand il crie, il
        // serre les dents, sa bouche s'arrondit en gueule (`MAW_STYLE`) et s'ouvre en grand,
        // puis elle se referme et son sourire revient. Jamais en mordant, ni quand on le mord.
        if feeding || self.flinch > 0.0 {
            (self.scream, self.maw) = (0.0, false);
        } else if self.scream > 0.0 {
            self.scream -= dt;
            if self.maw {
                mouth = if self.scream > MAW_SHUT_S { 1.0 } else { -0.5 };
            } else {
                mouth = -0.5;
                self.maw = self.anim.mouth <= -0.3;
            }
        } else if self.maw {
            mouth = -0.5;
            self.maw = self.anim.mouth > -0.3;
        }

        // Son regard : sur nous s'il nous voit, sinon il balaie.
        let (mut look_yaw, mut look_pitch) = if self.pausing {
            (0.9 * (self.age * 0.9).sin(), -0.1)
        } else {
            (0.25 * (self.age * 0.6).sin(), -0.12)
        };
        if let (Some(p), true) = (prey, seen) {
            // (En mordant : sur l'endroit qu'il mord.)
            let to = spot.unwrap_or(p.eye) - self.eye();
            let flat = Vec3::new(to.x, 0.0, to.z).length();
            look_yaw = turn(to.x.atan2(-to.z), self.yaw).clamp(-1.3, 1.3);
            look_pitch = to.y.atan2(flat.max(0.05)).clamp(-1.45, 0.9);
            if let Some(s) = spot {
                // (v290) « Lui comme moi, la tête doit être tournée vers le centre du corps
                // quand on mord », « vers le centre du membre attaqué » : son visage, depuis
                // sa bouche, vers le milieu du membre qu'il mord ; si la partie ne le dit pas,
                // vers le milieu de notre corps. (Avant : debout, le visage vers le sol, la
                // bouche sur nous d'en haut.)
                // (À la hauteur de l'endroit mordu, ramenée d'un tiers vers ce milieu : « il se
                // penche à la bonne hauteur », le visage vers le membre et non le long de lui.)
                let mid = limb.unwrap_or(Vec3::new(p.feet.x, (p.feet.y + p.eye.y) / 2.0, p.feet.z));
                let aim = Vec3::new(mid.x, lerp(s.y, mid.y, BITE_INWARD), mid.z);
                let to = aim - self.mouth_point();
                let flat = Vec3::new(to.x, 0.0, to.z).length();
                if flat > 0.02 {
                    look_yaw = turn(to.x.atan2(-to.z), self.yaw).clamp(-1.3, 1.3);
                }
                look_pitch = to.y.atan2(flat.max(0.05)).clamp(BITE_PITCH, 0.9);
            }
        }
        self.look.0 = approach(self.look.0, look_yaw, dt * 8.0);
        self.look.1 = approach(self.look.1, look_pitch, dt * 8.0);
        // Ses tics : la tête qui se couche d'un coup sur l'épaule, puis revient.
        self.tic.1 -= dt;
        if self.tic.1 <= 0.0 {
            let calm = if self.mode == Mode::Chase { 0.5 } else { 1.0 };
            self.tic = if self.tic.0 == 0.0 {
                ((self.unit() - 0.5) * 1.1, 0.3 + 0.9 * self.unit())
            } else {
                (0.0, (1.2 + 2.8 * self.unit()) * calm)
            };
        }
        let shiver = if seen { 0.03 * (self.age * 37.0).sin() } else { 0.0 };

        let a = &mut self.anim;
        a.crawl = approach(a.crawl, stoop, dt / 0.3);
        a.hunch = approach(a.hunch, lean, dt * if feeding { 5.0 } else { 3.0 });
        a.arms = approach(a.arms, arms, dt * 5.0);
        let grab = if holding { 1.0 } else { 0.0 };
        a.grab = approach(a.grab, grab, dt * 4.0);
        a.mouth = approach(a.mouth, mouth, dt * if mouth < a.mouth { 22.0 } else { 5.0 });
        a.head_yaw = self.look.0;
        a.head_pitch = self.look.1;
        a.head_roll = approach(a.head_roll, self.tic.0, dt * 14.0) + shiver * dt * 30.0;
        a.head_roll = a.head_roll.clamp(-0.7, 0.7);
        a.walk += moved * dt * lerp(1.6, 4.0, a.crawl);
        let swing = lerp(
            (moved / walk).min(1.0) * 0.9,
            (moved / (sneak * 0.5)).min(1.0),
            a.crawl,
        );
        a.swing = approach(a.swing, swing, dt * 4.0);
        events
    }

    /// (v290) « Le monstre a de la rémanence, ça ne doit pas arriver » : son maillage ajouté
    /// à `out` et, pour chaque sommet, sa place à l'image d'avant ajoutée à `prev`
    /// (l'anticrénelage reprend alors son image d'avant là où il était, et pas là où il n'est
    /// plus). `before` : les repères de ses pavés à l'image d'avant (ce que cette fonction a
    /// rendu alors) ; `None` s'il n'était pas dessiné : immobile. Rend ses repères
    /// d'aujourd'hui.
    pub fn mesh_moved(
        &self,
        light: [f32; 3],
        before: Option<&[Mat4]>,
        out: &mut Vec<EntityVertex>,
        prev: &mut Vec<[f32; 3]>,
    ) -> Vec<Mat4> {
        let dims = grids();
        let l = self.body();
        let mut frames = Vec::with_capacity(2 * PARTS);
        for (i, (xf, lo, hi)) in l.parts.into_iter().enumerate() {
            // Deux morceaux rigides par pavé (deux repères rendus, même pour un pavé d'un
            // seul morceau) : le haut d'un membre, puis son bas, sous le genou ou le coude.
            let rows = dims[i][1];
            let pieces = match l.lower[i] {
                Some(low) => [(xf, rows / 2..rows), (low, 0..rows / 2)],
                None => [(xf, 0..rows), (xf, 0..0)],
            };
            for (k, (m, rows)) in pieces.into_iter().enumerate() {
                let from = out.len();
                let (flesh, open) = (&self.flesh, self.anim.mouth);
                emit_part(out, m, lo, hi, i, dims[i], light, flesh, open, rows, self.face());
                // Chaque morceau est rigide : d'où vient un de ses points = repère d'avant ×
                // (repère d'aujourd'hui)⁻¹. En double précision : en simple, loin du centre
                // du monde, un morceau immobile aurait un mouvement d'arrondi. Le même
                // repère : la même place, au bit près.
                let step = before
                    .and_then(|b| b.get(2 * i + k))
                    .filter(|b| **b != m)
                    .map(|b| b.as_dmat4() * m.as_dmat4().inverse());
                prev.extend(out[from..].iter().map(|v| match step {
                    Some(m) => m
                        .transform_point3(Vec3::from(v.pos).as_dvec3())
                        .as_vec3()
                        .to_array(),
                    None => v.pos,
                }));
                frames.push(m);
            }
        }
        frames
    }

    /// Les pavés qui font son ombre.
    pub fn shadow_boxes(&self) -> Vec<ShadowBox> {
        let dims = grids();
        let l = limbs(self.pos, self.yaw, &self.anim);
        let mut out = Vec::with_capacity(PARTS + 4);
        for (i, (xf, lo, hi)) in l.parts.into_iter().enumerate() {
            if self.flesh.eaten_of(i, dims[i]) >= 0.9 {
                continue;
            }
            // (v290) Un membre plié fait deux pavés d'ombre ; presque droit, un seul.
            match l.lower[i].filter(|_| l.bend[i] >= SHADOW_BEND) {
                Some(low) => {
                    let mid = (lo[1] + hi[1]) / 2.0;
                    let top = Vec3::new(lo[0], mid, lo[2]);
                    out.push(ShadowBox::oriented(xf, top, Vec3::from(hi)));
                    let bottom = Vec3::new(hi[0], mid, hi[2]);
                    out.push(ShadowBox::oriented(low, Vec3::from(lo), bottom));
                }
                None => out.push(ShadowBox::oriented(xf, Vec3::from(lo), Vec3::from(hi))),
            }
        }
        out
    }

    /// Le pavé et le cube visés par ce rayon, à au plus `reach` blocs : (distance, pavé, cube).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(f32, usize, [usize; 3])> {
        if self.dying.is_some() {
            return None;
        }
        let dims = grids();
        let mut best: Option<(f32, usize, [usize; 3])> = None;
        let l = limbs(self.pos, self.yaw, &self.anim);
        // (v290) Morceau par morceau : le bas d'un membre plié là où il est.
        for (i, xf, slo, shi) in segments(self.pos, self.yaw, &self.anim) {
            if self.flesh.eaten_of(i, dims[i]) >= 1.0 {
                continue;
            }
            let inv = xf.inverse();
            let (o, d) = (inv.transform_point3(eye), inv.transform_vector3(dir));
            let Some(t) = ray_box(o, d, Vec3::from(slo), Vec3::from(shi)) else {
                continue;
            };
            if t > reach || best.is_some_and(|b| b.0 <= t) {
                continue;
            }
            // (Le cube : dans la grille du pavé entier.)
            let (lo, hi) = (Vec3::from(l.parts[i].1), Vec3::from(l.parts[i].2));
            let p = (o + d * t - lo) / (hi - lo);
            let mut cell = [0, 1, 2].map(|a| {
                ((p[a] * dims[i][a] as f32).floor().max(0.0) as usize).min(dims[i][a] - 1)
            });
            // (Une rangée du morceau touché : celles du bas sous le pli, celles du haut
            // au-dessus.)
            let half = dims[i][1] / 2;
            if shi[1] < l.parts[i].2[1] {
                cell[1] = cell[1].min(half.saturating_sub(1));
            } else if slo[1] > l.parts[i].1[1] {
                cell[1] = cell[1].max(half);
            }
            best = Some((t, i, cell));
        }
        best
    }

    /// On lui mange un morceau, là (pavé, cube) : « on mange toujours par les bords », le
    /// morceau ne revient pas. Rend ce qu'on a mangé, et s'il en meurt.
    /// (v299) `from` : d'où on le mange (nos yeux) : mort, il tombe à l'opposé.
    pub fn eaten_at(&mut self, part: usize, cell: [usize; 3], from: Vec3) -> Option<bool> {
        if self.dying.is_some() {
            return None;
        }
        let dims = grids();
        self.flesh.bite_at(part, cell, OUR_BITE, &dims)?;
        self.flinch = 0.25;
        self.spurt = Some((part, cell));
        let dead = self.health() == 0;
        if dead {
            self.die(from);
        }
        Some(dead)
    }

    /// (v299) Il meurt : il s'effondre de là où il est, dans sa posture du moment, poussé loin
    /// de `from`.
    fn die(&mut self, from: Vec3) {
        if self.dying.is_some() {
            return;
        }
        self.dying = Some(0.0);
        let l = limbs(self.pos, self.yaw, &self.anim);
        let stance = crate::corpse::Stance {
            parts: l.parts,
            lower: l.lower,
        };
        let away = self.pos - from;
        let push = Vec3::new(away.x, 0.0, away.z).normalize_or(forward(self.yaw + PI)) * 1.6;
        self.corpse = Some(crate::corpse::Ragdoll::new(&stance, self.vel, push, self.rng));
        (self.scream, self.maw) = (0.0, false);
    }

    /// (v312) Tel que le relais et les autres joueurs le voient (`minelib_net::evide::Mob`) :
    /// sa place, sa posture sur dix octets, ce qu'on lui a mangé.
    pub fn wire(&self) -> minelib_net::evide::Mob {
        use minelib_net::evide as net;
        let a = &self.anim;
        let unit = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let sym = |v: f32| unit(v / (2.0 * WIRE_HEAD) + 0.5);
        net::Mob {
            id: self.id as u16,
            // (v318) Comptée depuis l'origine de la partie : `BuildNet::send_evide` la met en
            // place absolue.
            pos: self.pos.to_array().map(f64::from),
            yaw: self.yaw,
            tall: self.tall,
            pose: [
                unit(a.walk.rem_euclid(TAU) / TAU),
                unit(a.swing),
                unit(a.crawl),
                unit(a.hunch / WIRE_HUNCH),
                unit(a.arms / PI),
                unit(a.grab),
                unit((a.mouth + 0.5) / 1.5),
                sym(a.head_yaw),
                sym(a.head_pitch),
                sym(a.head_roll),
            ],
            flags: u8::from(self.crawling) * net::CRAWLING
                | u8::from(self.maw) * net::MAW
                | u8::from(self.dying.is_some()) * net::DYING,
            health: self.health(),
            bites: self
                .flesh
                .bites()
                .iter()
                .map(|b| [b.part, b.start[0], b.start[1], b.start[2]])
                .collect(),
        }
    }

    /// (v312) Le pantin de l'Évidé `m` que mène `owner` : figé, il suit ce que le relais en
    /// publie (`hear`).
    pub fn puppet(owner: &str, m: minelib_net::evide::Mob) -> Self {
        let mut e = Evide::new(Vec3::from(m.pos.map(|v| v as f32)), m.yaw, u32::from(m.id) + 1);
        e.frozen = true;
        e.lead = Some((owner.to_string(), m.id));
        e.goal = Some(m);
        e.follow(1.0, true);
        e
    }

    /// (v312) Ce que le relais vient de publier de lui.
    pub fn hear(&mut self, m: minelib_net::evide::Mob) {
        self.goal = Some(m);
        self.silent = 0.0;
    }

    /// (v312) Le pantin rejoint ce que le relais a publié (`k` : la part du chemin faite à ce
    /// pas) : sa place, sa posture ; les morceaux qu'on lui a mangés depuis (`quiet` : sans
    /// jet de gelée, il arrive déjà entamé) ; sa mort.
    fn follow(&mut self, k: f32, quiet: bool) {
        use minelib_net::evide as net;
        let Some(m) = self.goal.take() else {
            return;
        };
        let unit = |b: u8| f32::from(b) / 255.0;
        let sym = |b: u8| (unit(b) - 0.5) * 2.0 * WIRE_HEAD;
        let turn = crate::player_model::turn_toward;
        if self.dying.is_none() {
            let to = Vec3::from(m.pos.map(|v| v as f32));
            self.pos = if self.pos.distance(to) > 6.0 { to } else { self.pos.lerp(to, k) };
            self.yaw = turn(self.yaw, m.yaw, k);
            let a = &mut self.anim;
            a.walk = turn(a.walk, unit(m.pose[0]) * TAU, k);
            a.swing = lerp(a.swing, unit(m.pose[1]), k);
            a.crawl = lerp(a.crawl, unit(m.pose[2]), k);
            a.hunch = lerp(a.hunch, unit(m.pose[3]) * WIRE_HUNCH, k);
            a.arms = lerp(a.arms, unit(m.pose[4]) * PI, k);
            a.grab = lerp(a.grab, unit(m.pose[5]), k);
            a.mouth = lerp(a.mouth, unit(m.pose[6]) * 1.5 - 0.5, k);
            a.head_yaw = lerp(a.head_yaw, sym(m.pose[7]), k);
            a.head_pitch = lerp(a.head_pitch, sym(m.pose[8]), k);
            a.head_roll = lerp(a.head_roll, sym(m.pose[9]), k);
            self.tall = lerp(self.tall, m.tall.clamp(CRAWL_H, HEIGHT), k);
            self.crawling = m.flags & net::CRAWLING != 0;
            let maw = m.flags & net::MAW != 0;
            self.cry |= maw && !self.maw && !quiet;
            self.maw = maw;
        }
        let dims = grids();
        while self.fed < m.bites.len() {
            let [part, x, y, z] = m.bites[self.fed];
            self.fed += 1;
            let (part, cell) = (usize::from(part), [x, y, z].map(usize::from));
            let eaten = part < PARTS && self.flesh.bite_at(part, cell, OUR_BITE, &dims).is_some();
            if eaten && !quiet && self.dying.is_none() {
                self.flinch = 0.25;
                self.spurt = Some((part, cell));
            }
        }
        if m.flags & net::DYING != 0 {
            // (Poussé en arrière : celui qui l'a mangé était devant lui.)
            self.die(self.pos + forward(self.yaw));
        }
        self.goal = Some(m);
    }

    /// (v290) Ce qu'il lui reste de vie (sur `LIFE`, la vie d'un joueur) : chacune de nos
    /// bouchées lui en a retiré `OUR_BITE_HURTS` (elles sont enregistrées avec lui, rien ne
    /// repousse).
    pub fn health(&self) -> u8 {
        let bites = self.flesh.bites().len().min(255) as u8;
        LIFE.saturating_sub(OUR_BITE_HURTS.saturating_mul(bites))
    }
}

/// Les Évidés du monde.
#[derive(Default)]
pub struct Horde {
    pub list: Vec<Evide>,
    /// Combien sont venus tout seuls (les autres sortent d'un œuf).
    natural: usize,
    /// (v272) Combien il y en a eu en tout (leurs numéros).
    count: u32,
    rng: u32,
    /// Temps avant le prochain tirage d'une apparition (s).
    wait: f32,
}

/// Secondes entre deux tirages d'une apparition naturelle.
const SPAWN_TICK: f32 = 5.0;

impl Horde {
    fn unit(&mut self) -> f32 {
        if self.rng == 0 {
            self.rng = 0x2545_f491;
        }
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / 16_777_216.0
    }

    /// Un Évidé de plus, là (sorti d'un œuf). Faux : il y en a déjà trop.
    pub fn spawn(&mut self, pos: Vec3, yaw: f32) -> bool {
        if self.list.iter().filter(|e| e.lead.is_none()).count() >= MAX {
            return false;
        }
        let seed = (self.unit() * 16_777_216.0) as u32;
        let mut e = Evide::new(pos, yaw, seed);
        self.count += 1;
        e.id = self.count;
        self.list.push(e);
        true
    }

    /// Un endroit où en faire apparaître un autour de `center` : un sol, de quoi tenir au
    /// moins à plat ventre, dans un chunk chargé.
    fn natural_spot(&mut self, world: &World, center: Vec3) -> Option<Vec3> {
        for _ in 0..12 {
            let (angle, far) = (self.unit() * TAU, lerp(SPAWN_NEAR, SPAWN_FAR, self.unit()));
            let (x, z) = (
                (center.x + angle.cos() * far).floor() as i32,
                (center.z + angle.sin() * far).floor() as i32,
            );
            if !world.contains(ChunkPos::containing(x, z)) {
                continue;
            }
            let y0 = center.y.floor() as i32;
            for y in ((y0 - 40)..=(y0 + 40)).rev() {
                let at = Vec3::new(x as f32 + 0.5, y as f32 + 1.0, z as f32 + 0.5);
                if world.get(x, y, z).def().full && fits(world, at, CRAWL_H) {
                    return Some(at);
                }
            }
        }
        None
    }

    /// Un pas pour tous. `night` : il fait nuit (ils apparaissent tout seuls, rarement).
    /// (v312) `preys` : nous (`Prey::tag` 0) et, en ligne, les autres joueurs du monde ;
    /// chacun de nos Évidés s'en prend au plus proche de lui.
    pub fn update(&mut self, world: &World, dt: f32, preys: &[Prey], night: bool) -> Vec<Event> {
        let mut events = Vec::new();
        for e in &mut self.list {
            let near = |p: &Prey| p.feet.distance_squared(e.pos);
            let prey = preys.iter().copied().min_by(|a, b| near(a).total_cmp(&near(b)));
            events.extend(e.update(world, dt, prey));
        }
        let prey = preys.iter().copied().find(|p| p.tag == 0);
        let before = self.list.len();
        self.list.retain(|e| {
            !e.is_dead()
                && e.pos.y > -80.0
                && (e.lead.is_none() || e.dying.is_some() || e.silent < PUPPET_GONE_S)
                && prey.is_none_or(|p| e.frozen || p.feet.distance(e.pos) < DESPAWN)
        });
        self.natural = self.natural.saturating_sub(before - self.list.len());
        if let (true, Some(p)) = (night, prey) {
            self.wait -= dt;
            if self.wait <= 0.0 {
                self.wait = SPAWN_TICK;
                if self.natural < NATURAL_MAX
                    && self.list.len() < MAX
                    && self.unit() < SPAWN_TICK / SPAWN_EVERY_S
                {
                    if let Some(at) = self.natural_spot(world, p.feet) {
                        let yaw = self.unit() * TAU;
                        if self.spawn(at, yaw) {
                            self.natural += 1;
                        }
                    }
                }
            }
        }
        events
    }

    /// (v272) « Il n'est pas enregistré avec le monde : règle ça. » Les Évidés vivants dans
    /// `evides.txt`, dans le dossier du monde : une ligne `e x y z lacet rampe` par monstre,
    /// suivie d'une ligne `b pavé lo(3) hi(3) départ(3)` par morceau qu'on lui a mangé.
    /// (v309) `origin` : origine des coordonnées locales de la partie (blocs, voir
    /// `World::origin_blocks`) ; le fichier garde les places absolues.
    pub fn save(&self, dir: &std::path::Path, origin: [i32; 3]) {
        let mut text = String::new();
        for e in self.list.iter().filter(|e| !e.frozen && e.dying.is_none()) {
            let p = e.pos;
            // (v309) Sur la carte bouclée.
            let wrap = minelib_world::lap::wrap_f64;
            let (x, z) = (wrap(p.x as f64 + origin[0] as f64), wrap(p.z as f64 + origin[2] as f64));
            text += &format!("e {} {} {} {} {}\n", x, p.y, z, e.yaw, e.crawling as u8);
            for b in e.flesh.bites() {
                let n: Vec<String> = [b.lo, b.hi, b.start].iter().flatten().map(u8::to_string).collect();
                text += &format!("b {} {}\n", b.part, n.join(" "));
            }
        }
        let file = dir.join("evides.txt");
        if text.is_empty() {
            let _ = std::fs::remove_file(file);
        } else if let Err(e) = std::fs::write(&file, text) {
            log::warn!("Évidés non enregistrés ({}) : {e}", file.display());
        }
    }

    /// (v272) Les Évidés enregistrés avec ce monde (aucun si le fichier manque). (v309)
    /// Comptés depuis l'origine `origin` de la partie (voir `save`).
    pub fn load(dir: &std::path::Path, origin: [i32; 3]) -> Self {
        let mut horde = Self::default();
        let Ok(text) = std::fs::read_to_string(dir.join("evides.txt")) else {
            return horde;
        };
        let grids = grids();
        let mut bites: Vec<crate::flesh::Bite> = Vec::new();
        let close = |h: &mut Self, bites: &mut Vec<crate::flesh::Bite>| {
            if let Some(e) = h.list.last_mut() {
                e.flesh = Flesh::restored(crate::flesh::Hard::Veins, std::mem::take(bites), &grids);
            }
        };
        for line in text.lines() {
            let mut f = line.split_whitespace();
            match f.next() {
                Some("e") => {
                    close(&mut horde, &mut bites);
                    let n: Vec<f64> = f.filter_map(|v| v.parse().ok()).collect();
                    if n.len() >= 5 && n.iter().all(|v| v.is_finite()) {
                        // (v309) La place la plus proche de l'origine, sur la carte bouclée.
                        let wrap = minelib_world::lap::wrap_f64;
                        let at = Vec3::new(
                            wrap(n[0] - origin[0] as f64) as f32,
                            n[1] as f32,
                            wrap(n[2] - origin[2] as f64) as f32,
                        );
                        let n: Vec<f32> = n.iter().map(|&v| v as f32).collect();
                        if horde.spawn(at, n[3]) {
                            if let Some(e) = horde.list.last_mut() {
                                e.crawling = n[4] != 0.0;
                                e.tall = if e.crawling { CRAWL_H } else { HEIGHT };
                                e.anim.crawl = n[4].clamp(0.0, 1.0);
                            }
                        }
                    }
                }
                Some("b") if !horde.list.is_empty() => {
                    let n: Vec<u8> = f.filter_map(|v| v.parse().ok()).collect();
                    if n.len() == 10 {
                        bites.push(crate::flesh::Bite {
                            part: n[0],
                            lo: [n[1], n[2], n[3]],
                            hi: [n[4], n[5], n[6]],
                            start: [n[7], n[8], n[9]],
                            age: 0.0,
                        });
                    }
                }
                _ => {}
            }
        }
        close(&mut horde, &mut bites);
        horde
    }

    /// L'Évidé visé : (son numéro, distance, pavé, cube).
    pub fn aimed(&self, eye: Vec3, dir: Vec3, reach: f32) -> Option<(u32, f32, usize, [usize; 3])> {
        self.list
            .iter()
            .filter_map(|e| e.aimed(eye, dir, reach).map(|(t, p, c)| (e.id, t, p, c)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// (v312) En ligne : nos Évidés tels que le relais doit les voir, les plus proches de
    /// `center` d'abord (`minelib_net::evide::MAX_PER_OWNER` au plus).
    pub fn wire(&self, center: Vec3) -> Vec<minelib_net::evide::Mob> {
        let mut own: Vec<&Evide> =
            self.list.iter().filter(|e| e.lead.is_none() && !e.frozen).collect();
        own.sort_by(|a, b| a.pos.distance_squared(center).total_cmp(&b.pos.distance_squared(center)));
        own.truncate(minelib_net::evide::MAX_PER_OWNER);
        own.into_iter().map(Evide::wire).collect()
    }

    /// (v312) En ligne : les Évidés que mène `owner`, tels que le relais vient de les
    /// publier : leurs pantins les suivent (`Evide::hear`), les nouveaux arrivent.
    pub fn remote(&mut self, owner: &str, mobs: Vec<minelib_net::evide::Mob>) {
        for m in mobs {
            let known = self.list.iter().position(|e| {
                e.lead
                    .as_ref()
                    .is_some_and(|(o, id)| *id == m.id && o.eq_ignore_ascii_case(owner))
            });
            match known {
                Some(i) => self.list[i].hear(m),
                None if self.list.iter().filter(|e| e.lead.is_some()).count() < PUPPETS => {
                    let mut e = Evide::puppet(owner, m);
                    self.count += 1;
                    e.id = PUPPET_IDS + self.count;
                    self.list.push(e);
                }
                None => {}
            }
        }
    }

    /// (v312) Notre Évidé qui porte ce numéro sur le fil (`Evide::wire`).
    pub fn own(&mut self, id: u16) -> Option<&mut Evide> {
        self.list
            .iter_mut()
            .find(|e| e.lead.is_none() && e.id as u16 == id)
    }

    /// (v272) Celui qui porte ce numéro, s'il est encore là.
    pub fn by_id(&mut self, id: u32) -> Option<&mut Evide> {
        self.list.iter_mut().find(|e| e.id == id)
    }
}

/// L'œuf d'apparition : image 16 × 16 (`W` blanc, `g` plaque grise, `s` ombre du bord, `D`
/// le creux sombre de la bouche).
const EGG: [&str; 16] = [
    "................",
    "......WWWW......",
    ".....WWWWWs.....",
    "....WWgWWWWs....",
    "....WWWWWgWs....",
    "...WWWWWWWWWs...",
    "...WgWWWWWWWs...",
    "...WWWDDDDWWs...",
    "..WWWDDDDDDWgs..",
    "..WWWDDDDDDWWs..",
    "..WWWWDDDDWWWs..",
    "..WgWWWWWWWWWs..",
    "...WWWWWWgWWs...",
    "...WWWWWWWWss...",
    "....sWWWWWss....",
    "......ssss......",
];

/// Couleur de l'œuf au sol, en main vu de loin (sRGB).
pub const EGG_RGB: [u8; 3] = [236, 234, 227];

/// Pixels pleins de l'œuf : (colonne, ligne, couleur linéaire).
pub fn egg_pixels() -> impl Iterator<Item = (usize, usize, [f32; 3])> {
    EGG.iter().enumerate().flat_map(|(y, row)| {
        row.chars().enumerate().filter_map(move |(x, c)| {
            let rgb = match c {
                'W' => WHITE,
                'g' => 0xcfcdc5,
                's' => 0xc4c2ba,
                'D' => MOUTH[1],
                _ => return None,
            };
            Some((x, y, srgb(rgb)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::BlockId;

    /// Un sol plat de pierre en y = 10 (on marche en y = 11), de 96 × 96 blocs.
    fn flat() -> World {
        let mut w = World::new(0);
        for cz in -3..3 {
            for cx in -3..3 {
                w.insert_empty(ChunkPos { x: cx, z: cz });
            }
        }
        for z in -48..48 {
            for x in -48..48 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        w
    }

    fn prey(at: Vec3) -> Prey {
        Prey {
            feet: at,
            eye: at + Vec3::Y * 1.62,
            spot: None,
            limb: None,
            tag: 0,
        }
    }

    fn run(e: &mut Evide, w: &World, secs: f32, p: Option<Prey>) -> Vec<Event> {
        let mut all = Vec::new();
        for _ in 0..(secs * 60.0) as usize {
            all.extend(e.update(w, 1.0 / 60.0, p));
        }
        all
    }

    /// « La même largeur, 3,5 blocs de haut » : debout il fait 3,5 blocs, large comme le
    /// joueur ; à plat ventre, « 3,5 de long, 1 de large et de haut ».
    #[test]
    fn it_is_three_and_a_half_blocks_tall_and_crawls_one_block_high() {
        let size = |a: &Anim| {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for (_, xf, a, b) in segments(Vec3::ZERO, 0.0, a) {
                for k in 0..8 {
                    let c = Vec3::new(
                        if k & 1 == 0 { a[0] } else { b[0] },
                        if k & 2 == 0 { a[1] } else { b[1] },
                        if k & 4 == 0 { a[2] } else { b[2] },
                    );
                    let p = xf.transform_point3(c);
                    (lo, hi) = (lo.min(p), hi.max(p));
                }
            }
            (lo, hi)
        };
        let (lo, hi) = size(&Anim::default());
        assert!((hi.y - HEIGHT).abs() < 0.02 && lo.y.abs() < 0.02, "{lo} {hi}");
        // Large comme le joueur (bras compris : moins d'un bloc), pas plus épais.
        let (plo, phi) = crate::player_model::rig().boxes[CHEST];
        let px = crate::player_model::rig().px;
        assert!((hi.x - lo.x) < 1.0 && (hi.x - lo.x) > (phi[0] - plo[0]) * px, "{lo} {hi}");
        let lying = Anim {
            crawl: 1.0,
            ..Anim::default()
        };
        let (lo, hi) = size(&lying);
        // (3,5 de long des pieds à la tête, à peu près ; ses bras tendus devant en plus.)
        assert!((3.2..=4.1).contains(&(hi.z - lo.z)), "long de {}", hi.z - lo.z);
        assert!(hi.y <= 1.0 && lo.y > -0.05, "haut de {} à {}", lo.y, hi.y);
        assert!(hi.x - lo.x < 1.0, "large de {}", hi.x - lo.x);
    }

    /// (v299) Les formes de bouche proposées : à toute ouverture un creux, qui ne mange ni
    /// ses orbites ni le haut ni le fond de son crâne ; les mélanges (6 et plus) ont des
    /// dents ; les joues trouées percent les côtés de sa tête.
    #[test]
    fn every_mouth_shape_is_a_hollow_below_the_eyes() {
        let d = grids()[HEAD];
        for style in 0..MOUTH_STYLES {
            for open in [-0.5, 0.0, 0.35, 1.0] {
                let (mut hollow, mut teeth, mut cheeks) = (0, 0, 0);
                for z in 0..d[2] {
                    for y in 0..d[1] {
                        for x in 0..d[0] {
                            let side = x == 0 || x + 1 == d[0];
                            match jaw_of(style, [x, y, z], d, open) {
                                Jaw::Flesh => continue,
                                Jaw::Hollow => {
                                    hollow += 1;
                                    cheeks += (side && z >= 2) as usize;
                                }
                                Jaw::Tooth => teeth += 1,
                            }
                            let row = y * 16 / d[1];
                            let socket = (10..=13).contains(&row) && (x < 3 || x + 3 >= d[0]);
                            assert!(!socket, "forme {style} ouverte de {open} : dans une orbite");
                            assert!(row < 14 && z + 1 < d[2], "forme {style} : trop loin");
                            assert!(style < 6 || row < 10, "forme {style} : jusqu'aux yeux");
                        }
                    }
                }
                // (v310) Grande ouverte : de chaque côté des dents du milieu, la bouche monte
                // jusqu'à la rangée des dents des coins (ces deux cubes étaient blancs) ; au
                // milieu, au-dessus des deux dents, son visage reste.
                if style == MOUTH_STYLE && open == 1.0 {
                    let at = |col: usize, row: usize| {
                        jaw_of(style, [col * d[0] / 8, row * d[1] / 16, 0], d, open)
                    };
                    assert_eq!((at(2, 5), at(5, 5)), (Jaw::Hollow, Jaw::Hollow));
                    assert_eq!((at(3, 5), at(4, 5)), (Jaw::Flesh, Jaw::Flesh));
                    assert_eq!((at(3, 4), at(4, 4)), (Jaw::Tooth, Jaw::Tooth));
                    assert_eq!((at(1, 5), at(6, 5)), (Jaw::Tooth, Jaw::Tooth));
                }
                // (v306) Le sourire ouvert : sur son visage, rien de sombre aux deux colonnes
                // du bord (on aurait dit ses yeux), seulement la dent du coin.
                // (v310) Ni dent : ses coins ont le blanc de sa peau.
                if matches!(style, 6 | 8) && open >= 0.35 {
                    for y in 0..d[1] {
                        for x in [0, d[0] - 1] {
                            assert_eq!(jaw_of(style, [x, y, 0], d, open), Jaw::Flesh, "{style} {open}");
                        }
                    }
                }
                assert!(hollow > 0, "forme {style} ouverte de {open} : pas de creux");
                assert_eq!(teeth > 0, style >= 6, "forme {style} ouverte de {open} : {teeth} dents");
                if style >= 8 {
                    assert!(cheeks > 0, "forme {style} ouverte de {open} : les joues entières");
                }
            }
        }
    }

    /// Sa bouche : un creux toujours là, qui s'agrandit beaucoup et se referme en fente ; sa
    /// peau : presque toute blanche, deux contours d'yeux sans rien dedans.
    #[test]
    fn its_mouth_is_a_hollow_and_its_eyes_are_only_outlines() {
        let d = grids()[HEAD];
        let count = |open: f32, deep: usize| {
            let mut n = 0;
            for z in 0..deep {
                for y in 0..d[1] {
                    for x in 0..d[0] {
                        n += mouth([x, y, z], d, open) as usize;
                    }
                }
            }
            n
        };
        let (shut, rest, wide) = (count(-0.5, d[2]), count(0.0, d[2]), count(1.0, d[2]));
        assert!(shut > 0 && shut < rest && rest < wide, "{shut} {rest} {wide}");
        // (v299) Sur son visage (les trous de ses joues, eux, ne changent guère) : refermée,
        // ses dents serrées ; ouverte, bien plus grande.
        let (shut, rest, wide) = (count(-0.5, 1), count(0.0, 1), count(1.0, 1));
        assert!(shut < rest && rest * 3 < wide, "{shut} {rest} {wide}");
        // Jamais jusqu'au sommet du crâne ni jusqu'aux yeux.
        for x in 0..d[0] {
            for y in 10..d[1] {
                assert!(!mouth([x, y, 0], d, 1.0));
            }
        }
        let mut seen = std::collections::HashMap::new();
        for y in 0..d[1] {
            for x in 0..d[0] {
                *seen.entry(skin(HEAD, [x, y, 0], d, 0)).or_insert(0usize) += 1;
            }
        }
        assert_eq!(seen.get(&OUTLINE), Some(&20), "{seen:?}");
        assert_eq!(seen.get(&SOCKET), Some(&4), "{seen:?}");
        // Blanc avant tout, sur tout le corps.
        let grids = grids();
        let (mut white, mut all) = (0usize, 0usize);
        for (p, d) in grids.iter().enumerate() {
            for y in 0..d[1] {
                for x in 0..d[0] {
                    all += 1;
                    white += (skin(p, [x, y, 0], *d, 0) == WHITE) as usize;
                }
            }
        }
        assert!(white * 10 > all * 5 && white < all, "{white} sur {all}");
    }

    /// Il nous voit devant lui jusqu'à 50 blocs, pas derrière lui, pas à travers un mur
    /// plein ; il vient alors à l'allure d'un joueur qui court, et marche sinon.
    #[test]
    fn it_sees_what_is_on_its_screen_and_runs_at_us() {
        let mut w = flat();
        let at = Vec3::new(0.5, 11.0, 0.5);
        let e = Evide::new(at, 0.0, 3);
        // Il regarde vers −z.
        assert!(e.sees(&w, Vec3::new(0.5, 12.6, -30.0)));
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, -60.0)), "plus de 50 blocs");
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, 30.0)), "derrière lui");
        for y in 11..16 {
            for x in -3..4 {
                w.set(x, y, -8, BlockId::STONE);
            }
        }
        assert!(!e.sees(&w, Vec3::new(0.5, 12.6, -30.0)), "à travers un mur");
        // Sans proie : il marche, s'arrête, tourne.
        let w = flat();
        let mut e = Evide::new(at, 0.0, 3);
        let yaw = e.yaw;
        run(&mut e, &w, 12.0, None);
        let [walk, run_speed, ..] = crate::player::GAIT;
        assert!(turn(e.yaw, yaw).abs() > 0.05 || e.pos.distance(at) > 1.0);
        assert!(e.pos.distance(at) <= walk * 12.0 + 0.1);
        // Une proie devant lui, à 30 blocs : il court vers elle.
        let mut e = Evide::new(at, 0.0, 3);
        let p = prey(Vec3::new(0.5, 11.0, -30.0));
        run(&mut e, &w, 0.2, Some(p));
        let from = e.pos;
        run(&mut e, &w, 2.0, Some(p));
        let speed = (e.pos - from).length() / 2.0;
        assert!((speed - run_speed).abs() < 0.4, "il court à {speed}");
        assert!((e.pos.y - 11.0).abs() < 0.01, "il reste au sol : {}", e.pos.y);
    }

    /// (v277) « Il ne se penche pas plus que la taille dont il a besoin pour passer sous les
    /// blocs ; jusqu'à 1 de hauteur sa taille s'adapte en proportion » : sous un plafond à
    /// 3 blocs, à 2 blocs, à 1 bloc, son corps arrive juste dessous, pas plus bas, et il va
    /// d'autant moins vite ; ses pieds restent au sol.
    #[test]
    fn it_stoops_only_as_much_as_the_ceiling_needs() {
        // Plus il se baisse, plus il est bas, sans à-coup, de 3,5 blocs à moins d'un.
        let mut last = top_of(0.0);
        assert!((3.4..=3.5).contains(&last), "{last}");
        for i in 1..=32 {
            let top = top_of(i as f32 / 32.0);
            assert!(top <= last + 1e-4 && last - top < 0.25, "{i} : {last} puis {top}");
            last = top;
        }
        assert!(last <= CRAWL_H);
        for h in [3.5f32, 3.0, 2.5, 2.0, 1.5, 1.0] {
            let top = top_of(crawl_for(h));
            assert!(top <= h + 1e-3 && top > h - 0.12, "sous {h} : haut de {top}");
        }
        let [_, run, sneak, ..] = crate::player::GAIT;
        let mut speeds = Vec::new();
        for clear in [3, 2, 1] {
            let mut w = flat();
            for z in -22..=-4 {
                for x in -48..48 {
                    w.set(x, 11 + clear, z, BlockId::STONE);
                }
            }
            let p = prey(Vec3::new(0.5, 11.0, -30.0));
            let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 5);
            e.mode = Mode::Chase;
            e.known = Some(p.feet);
            let (mut lowest, mut highest, mut gone, mut time) = (f32::MAX, 0.0f32, 0.0f32, 0.0f32);
            for _ in 0..(40 * 60) {
                let before = e.pos;
                e.update(&w, 1.0 / 60.0, Some(p));
                let mut top = 0.0f32;
                let mut feet = f32::MAX;
                for (_, xf, lo, hi) in segments(e.pos, e.yaw, &e.anim) {
                    for k in 0..8 {
                        let c = Vec3::new(
                            if k & 1 == 0 { lo[0] } else { hi[0] },
                            if k & 2 == 0 { lo[1] } else { hi[1] },
                            if k & 4 == 0 { lo[2] } else { hi[2] },
                        );
                        let y = xf.transform_point3(c).y - 11.0;
                        (top, feet) = (top.max(y), feet.min(y));
                    }
                }
                assert!(feet > -0.15, "plafond à {clear} : un pied à {feet}");
                if e.pos.z < -9.0 && e.pos.z > -18.0 {
                    (lowest, highest) = (lowest.min(top), highest.max(top));
                    gone += (e.pos - before).length();
                    time += 1.0 / 60.0;
                }
                if e.pos.z < -26.0 {
                    break;
                }
            }
            assert!(time > 0.5, "plafond à {clear} : il n'est pas passé ({})", e.pos);
            let clear = clear as f32;
            assert!(highest <= clear + 0.02, "plafond à {clear} : il dépasse ({highest})");
            assert!(lowest > clear - 0.45, "plafond à {clear} : baissé jusqu'à {lowest}");
            speeds.push(gone / time);
            // Ressorti : debout de toute sa taille.
            assert!(e.tall > 3.4 && !e.crawling, "plafond à {clear} : {} après", e.tall);
        }
        assert!(speeds[0] < run && speeds[0] > speeds[1] + 0.5 && speeds[1] > speeds[2] + 0.5);
        assert!((speeds[2] - sneak).abs() < 0.1, "{speeds:?}");
    }

    /// Sous un plafond trop bas il rampe, à l'allure du sneak, et se relève dehors ; une
    /// marche d'un bloc, il la saute.
    #[test]
    fn it_crawls_under_low_ceilings_and_jumps_one_block_steps() {
        let mut w = flat();
        // Un tunnel d'un bloc de haut, de z = −6 à −14, dans un mur d'un bord à l'autre du
        // monde (plus étroit, il en ferait le tour : c'est plus court que de ramper).
        for z in -14..=-6 {
            for x in -48..48 {
                for y in 11..16 {
                    if !(x == 0 && y == 11) {
                        w.set(x, y, z, BlockId::STONE);
                    }
                }
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, -0.5), 0.0, 5);
        let p = prey(Vec3::new(0.5, 11.0, -22.0));
        // (Il nous a vus avant qu'on passe derrière le mur.)
        e.mode = Mode::Chase;
        e.known = Some(p.feet);
        let [_, _, sneak, ..] = crate::player::GAIT;
        let mut crawled = false;
        let mut slow = true;
        for _ in 0..(40.0 * 60.0) as usize {
            let before = e.pos;
            e.update(&w, 1.0 / 60.0, Some(p));
            if e.pos.z < -7.0 && e.pos.z > -13.0 {
                crawled |= e.crawling && e.anim.crawl > 0.99;
                slow &= (e.pos - before).length() * 60.0 <= sneak + 0.05;
            }
            if e.feeding {
                break;
            }
        }
        assert!(crawled && slow, "rampé {crawled}, lentement {slow}");
        assert!(e.pos.z < -19.0 && !e.crawling, "ressorti debout : {} {}", e.pos, e.crawling);
        // Apparu à moitié dans un mur, il s'en dégage et vient quand même.
        let mut w = flat();
        for y in 11..16 {
            for x in -1..=1 {
                w.set(x, y, 1, BlockId::STONE);
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.9), 0.0, 5);
        run(&mut e, &w, 3.0, Some(prey(Vec3::new(0.5, 11.0, -12.0))));
        assert!(e.pos.z < -6.0, "{}", e.pos);
        // Une marche d'un bloc sur son chemin.
        let mut w = flat();
        for z in -40..=-5 {
            for x in -3..=3 {
                w.set(x, 11, z, BlockId::STONE);
            }
        }
        let mut e = Evide::new(Vec3::new(0.5, 11.0, -0.5), 0.0, 5);
        run(&mut e, &w, 4.0, Some(prey(Vec3::new(0.5, 12.0, -30.0))));
        assert!(e.pos.y > 11.9 && e.pos.z < -8.0, "{}", e.pos);
    }

    /// (v299) « Il peut faire ce visage aussi, mais pas quand il mord » : quand il crie, il
    /// serre les dents puis sa bouche s'arrondit en gueule, grande ouverte, et son sourire
    /// revient ; jamais pendant qu'il nous mord.
    #[test]
    fn it_makes_its_round_maw_when_it_cries_but_never_while_biting() {
        let w = flat();
        // Seul : il crie de temps en temps.
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
        let (mut cries, mut maws, mut shown, mut widest) = (0, 0, 0.0f32, 0.0f32);
        for _ in 0..(45 * 60) {
            let was = e.maw;
            for ev in e.update(&w, 1.0 / 60.0, None) {
                cries += matches!(ev, Event::Voice(_)) as usize;
            }
            assert_eq!(e.face() == MAW_STYLE, e.maw);
            if e.maw {
                maws += (!was) as usize;
                shown += 1.0 / 60.0;
                widest = widest.max(e.anim.mouth);
            } else if was {
                // Elle s'est refermée avant que son sourire revienne.
                assert!(e.anim.mouth < -0.25, "{}", e.anim.mouth);
            }
        }
        assert!(cries >= 2 && (cries - 1..=cries).contains(&maws), "{cries} cris, {maws} gueules");
        assert!(widest > 0.95, "ouverte de {widest}");
        let each = shown / maws as f32;
        assert!((1.0..1.8).contains(&each), "sa gueule {each} s par cri");
        // Il nous mord : il crie aussi, mais garde sa bouche à lui.
        let p = prey(Vec3::new(0.5, 11.0, -1.6));
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
        let (mut bites, mut cries) = (0, 0);
        for _ in 0..(20 * 60) {
            for ev in e.update(&w, 1.0 / 60.0, Some(p)) {
                bites += matches!(ev, Event::Bites { .. }) as usize;
                cries += matches!(ev, Event::Voice(_)) as usize;
            }
            assert!(!(e.feeding && e.maw), "sa gueule ronde en mordant");
        }
        assert!(bites >= 5 && cries >= 1, "{bites} morsures, {cries} cris");
    }

    /// (v312) En ligne : un Évidé dit sur le fil (`Horde::wire`) a, dans le jeu d'un autre
    /// joueur, un pantin qui le suit (`Horde::remote`) : sa place, sa posture, les morceaux
    /// qu'on lui mange (les mêmes cubes), sa mort (il tombe aussi là-bas) ; le pantin ne pense
    /// pas, n'est pas compté parmi nos Évidés, et s'en va quand on n'en dit plus rien.
    #[test]
    fn a_puppet_follows_what_the_relay_publishes() {
        let w = flat();
        let p = prey(Vec3::new(0.5, 11.0, -9.0));
        let mut ours = Horde::default();
        assert!(ours.spawn(Vec3::new(0.5, 11.0, 6.5), 0.0));
        let mut theirs = Horde::default();
        let dims = grids();
        let pass = |ours: &Horde, theirs: &mut Horde| {
            let data = minelib_net::evide::pack(&ours.wire(p.feet));
            theirs.remote("Meneur", minelib_net::evide::unpack(&data).unwrap());
        };
        for step in 0..(3 * 60) {
            ours.update(&w, 1.0 / 60.0, &[p], false);
            if step % 3 == 0 {
                pass(&ours, &mut theirs);
            }
            // Sans proie : s'il pensait, il resterait en ronde ; il ne fait que suivre.
            theirs.update(&w, 1.0 / 60.0, &[], false);
        }
        assert_eq!(theirs.list.len(), 1);
        let (a, b) = (&ours.list[0], &theirs.list[0]);
        assert_eq!(b.lead, Some(("Meneur".to_string(), a.id as u16)));
        assert!(b.id >= PUPPET_IDS && b.frozen);
        // Il a couru vers nous, et le pantin avec lui (à son retard près).
        assert!(a.pos.z < 3.0, "{}", a.pos);
        assert!(a.pos.distance(b.pos) < 1.2, "{} {}", a.pos, b.pos);
        let near = |x: f32, y: f32| (x - y).abs() < 0.15;
        assert!(near(a.anim.mouth, b.anim.mouth) && near(a.anim.hunch, b.anim.hunch), "{:?} {:?}", a.anim, b.anim);
        assert!(near(a.anim.arms, b.anim.arms) && near(a.tall, b.tall));
        assert_eq!(theirs.wire(p.feet), Vec::new(), "un pantin n'est pas à nous");
        // On lui mange un morceau chez son meneur : le pantin perd les mêmes cubes, et la
        // gelée en coule là-bas aussi.
        let from = ours.list[0].pos + Vec3::new(0.0, 1.5, -1.0);
        assert_eq!(ours.list[0].eaten_at(CHEST, [1, 2, 0], from), Some(false));
        pass(&ours, &mut theirs);
        let drips: Vec<Event> = (0..3).flat_map(|_| theirs.update(&w, 1.0 / 60.0, &[], false)).collect();
        assert!(drips.iter().any(|e| matches!(e, Event::Drips { spurt: true, .. })), "{drips:?}");
        for part in 0..PARTS {
            assert_eq!(
                ours.list[0].flesh.eaten_of(part, dims[part]),
                theirs.list[0].flesh.eaten_of(part, dims[part]),
                "pavé {part}"
            );
        }
        assert_eq!(theirs.list[0].health(), LIFE - OUR_BITE_HURTS);
        // Mangé jusqu'à sa mort : le pantin meurt et tombe aussi.
        for part in [LEG_L, LEG_R, ARM_L, ARM_R] {
            assert!(ours.list[0].eaten_at(part, [0, 0, 0], from).is_some());
        }
        assert!(ours.list[0].is_dying());
        pass(&ours, &mut theirs);
        theirs.update(&w, 1.0 / 60.0, &[], false);
        assert!(theirs.list[0].is_dying() && theirs.list[0].corpse.is_some());
        assert_eq!(theirs.list[0].health(), 0);
        // Un pantin vivant dont on ne dit plus rien s'en va ; rien n'en est enregistré.
        let mut quiet = Horde::default();
        let mut one = Horde::default();
        assert!(one.spawn(Vec3::new(0.5, 11.0, 6.5), 0.0));
        quiet.remote("Meneur", one.wire(p.feet));
        assert_eq!(quiet.list.len(), 1);
        for _ in 0..((PUPPET_GONE_S + 0.5) * 60.0) as usize {
            quiet.update(&w, 1.0 / 60.0, &[p], false);
        }
        assert!(quiet.list.is_empty());
        // Et il n'est pas compté parmi les nôtres : seize à nous, même avec des pantins.
        let mut crowd = Horde::default();
        crowd.remote("Meneur", one.wire(p.feet));
        for _ in 0..MAX {
            assert!(crowd.spawn(Vec3::new(0.5, 11.0, 0.5), 0.0));
        }
        assert!(!crowd.spawn(Vec3::new(0.5, 11.0, 0.5), 0.0));
    }

    /// Près de nous il s'arrête, se penche, et sa bouche se referme sur nous à intervalles ;
    /// on le mange par morceaux qui ne reviennent pas, et il en meurt.
    #[test]
    fn it_bites_us_and_dies_when_we_eat_it() {
        let w = flat();
        let p = prey(Vec3::new(0.5, 11.0, -1.6));
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
        let mut events = Vec::new();
        let mut bites = 0;
        for _ in 0..(8 * 60) {
            for ev in e.update(&w, 1.0 / 60.0, Some(p)) {
                if let Event::Bites { low: false, at, .. } = ev {
                    bites += 1;
                    // Au moment où sa bouche se referme : plié sur nous, ses bras autour de
                    // nous ; sans endroit précis, il vise notre poitrine : sa bouche y est.
                    assert!(e.anim.hunch > 0.8 && e.anim.grab > 0.9, "{:?}", e.anim);
                    let miss = at.distance(p.feet + Vec3::Y * 1.1);
                    assert!(miss < 0.2, "sa bouche à {miss} de notre poitrine");
                }
                events.push(ev);
            }
        }
        assert!((3..=5).contains(&bites), "{bites} morsures en 8 s");
        assert!(events.iter().any(|e| matches!(e, Event::Voice(_))));
        assert_eq!(
            events.iter().filter(|e| matches!(e, Event::Aims { low: false, .. })).count(),
            bites + 1,
            "il choisit où mordre avant chaque morsure"
        );
        // On le vise et on le mange : chaque bouchée retire quelque chose de plus.
        let eye = p.eye;
        let dims = grids();
        let mut eaten = 0.0;
        let mut dead = false;
        let mut bites = 0;
        assert_eq!(e.health(), crate::save::MAX_HEALTH, "autant de vie qu'un joueur");
        for n in 0..400 {
            // On vise le milieu de chacun de ses pavés, tour à tour.
            let (xf, lo, hi) = parts(e.pos, e.yaw, &e.anim)[n % PARTS];
            let mid = xf.transform_point3((Vec3::from(lo) + Vec3::from(hi)) / 2.0);
            let dir = (mid - eye).normalize();
            let Some((t, part, cell)) = e.aimed(eye, dir, 6.0) else {
                continue;
            };
            assert!((0.0..6.0).contains(&t));
            if let Some(d) = e.eaten_at(part, cell, eye) {
                let now = e.flesh.eaten(&dims);
                assert!(now > eaten, "bouchée {n} : rien de plus");
                eaten = now;
                dead = d;
                bites += 1;
                assert_eq!(e.health(), LIFE.saturating_sub(OUR_BITE_HURTS * bites));
                if dead {
                    break;
                }
            }
        }
        // (v290) « On le tue en 5 morsures. »
        assert!(dead && bites == 5, "{bites} bouchées, mangé à {eaten}");
        assert!(e.aimed(eye, Vec3::NEG_Z, 6.0).is_none());
        // (v299) Mort, il tombe et se fige : couché au sol (son corps ne dépasse plus un
        // bloc et demi), pas dessous ; puis il fond en gelée, ses veines ensuite, en gouttes
        // de leur couleur ; à la fin il n'en reste rien.
        let before = run(&mut e, &w, 3.2, Some(p));
        assert!(!e.is_dead());
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let (mut mesh, mut prev) = (Vec::new(), Vec::new());
        e.mesh_moved([1.0; 3], None, &mut mesh, &mut prev);
        for v in &mesh {
            (lo, hi) = (lo.min(v.pos[1]), hi.max(v.pos[1]));
        }
        assert!(lo > 10.95 && hi < 12.6, "tombé de {lo} à {hi}");
        let after = run(&mut e, &w, 9.0, Some(p));
        let drips = |list: &[Event], which: Liquid| {
            list.iter()
                .filter(|e| matches!(e, Event::Drips { liquid, .. } if *liquid == which))
                .count()
        };
        // (La gelée a coulé de chaque bouchée, puis de tout le corps ; les veines à la fin.)
        assert!(drips(&before, Liquid::Jelly) + drips(&after, Liquid::Jelly) > 60);
        assert!(drips(&after, Liquid::Vein) > 20, "{}", drips(&after, Liquid::Vein));
        assert!(e.is_dead());
        let mut mesh = Vec::new();
        e.mesh_moved([1.0; 3], None, &mut mesh, &mut prev);
        assert!(mesh.is_empty(), "il reste {} sommets", mesh.len());
    }

    /// (v272) Ses veines : dans chaque pavé, d'un seul tenant, jamais dans la peau du tour ;
    /// on a beau le manger, elles restent toutes.
    #[test]
    fn its_veins_spread_everywhere_and_cannot_be_eaten() {
        let grids = grids();
        let mut all = 0;
        for (part, d) in grids.iter().enumerate() {
            let cells: Vec<[usize; 3]> = (0..d[0] * d[1] * d[2])
                .map(|i| [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])])
                .filter(|c| vein(part, *c))
                .collect();
            let share = cells.len() as f32 / (d[0] * d[1] * d[2]) as f32;
            assert!((0.04..=0.25).contains(&share), "pavé {part} : {share}");
            all += cells.len();
            for c in &cells {
                assert!(c[0] >= 1 && c[0] + 1 < d[0] && c[2] >= 1 && c[2] + 1 < d[2], "{c:?}");
                assert!(!(part == HEAD && mouth(*c, *d, 1.0)));
            }
            // Du haut en bas du pavé (sauf le bout des membres et le sommet du crâne).
            let rows: std::collections::HashSet<usize> = cells.iter().map(|c| c[1]).collect();
            assert!(rows.len() + 1 >= d[1], "pavé {part} : {} rangées sur {}", rows.len(), d[1]);
            // D'un seul tenant : tout se rejoint de proche en proche.
            let mut seen = std::collections::HashSet::from([cells[0]]);
            let mut todo = vec![cells[0]];
            while let Some(c) = todo.pop() {
                for n in &cells {
                    let far: usize = (0..3).map(|a| c[a].abs_diff(n[a])).sum();
                    if far == 1 && seen.insert(*n) {
                        todo.push(*n);
                    }
                }
            }
            assert_eq!(seen.len(), cells.len(), "pavé {part} : veines en plusieurs morceaux");
        }
        assert!(all > 200, "{all} cubes de veine");
        // On le mange autant qu'on peut : aucune veine ne part.
        let mut e = Evide::new(Vec3::ZERO, 0.0, 4);
        for k in 0..400u32 {
            e.flesh.bite_somewhere(k * 97 + 3, OUR_BITE, &[0, 1, 2, 3, 4, 5, 6], &grids);
        }
        assert!(e.flesh.eaten(&grids) > 0.6, "{}", e.flesh.eaten(&grids));
        for (part, d) in grids.iter().enumerate() {
            for i in 0..d[0] * d[1] * d[2] {
                let c = [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])];
                assert!(!(vein(part, c) && e.flesh.gone(part, *d, c)), "veine mangée : {part} {c:?}");
            }
        }
    }

    /// (v272) « Montre-moi la radio des veines » : L'Évidé debout, de face et de profil, son
    /// corps en gris (plus clair où il est plus épais), ses veines en clair. Écrit dans le
    /// fichier nommé par MINELIB_IRM (essai à lancer à la main).
    #[test]
    #[ignore]
    fn radio_of_the_veins() {
        let Ok(path) = std::env::var("MINELIB_IRM") else {
            return;
        };
        const SCALE: f32 = 230.0;
        let (w, h) = (640usize, 860usize);
        let mut body = vec![0.0f32; w * h];
        let mut veins = vec![0.0f32; w * h];
        let grids = grids();
        for (part, (xf, lo, hi)) in parts(Vec3::ZERO, 0.0, &Anim::default()).into_iter().enumerate() {
            let d = grids[part];
            let (lo, hi) = (Vec3::from(lo), Vec3::from(hi));
            let step = (hi - lo) / Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32);
            for i in 0..d[0] * d[1] * d[2] {
                let c = [i % d[0], i / d[0] % d[1], i / (d[0] * d[1])];
                if part == HEAD && mouth(c, d, 0.35) {
                    continue;
                }
                let mid = lo + (Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) + 0.5) * step;
                let p = xf.transform_point3(mid);
                let half = xf.transform_vector3(step).abs() * 0.5 * SCALE;
                for (view, side) in [(0usize, p.x), (1, p.z)] {
                    let (cx, cy) = (160.0 + 320.0 * view as f32 + side * SCALE, h as f32 - 30.0 - p.y * SCALE);
                    let hx = if view == 0 { half.x } else { half.z };
                    for y in (cy - half.y).round() as usize..(cy + half.y).round() as usize {
                        for x in (cx - hx).round() as usize..(cx + hx).round() as usize {
                            if x < w && y < h {
                                body[y * w + x] += 1.0;
                                veins[y * w + x] += vein(part, c) as u8 as f32;
                            }
                        }
                    }
                }
            }
        }
        let mut img = image::RgbImage::from_pixel(w as u32, h as u32, image::Rgb([6, 9, 14]));
        for (i, px) in img.pixels_mut().enumerate() {
            if body[i] > 0.0 {
                let g = (26.0 + 9.0 * body[i]).min(120.0);
                let v = (veins[i] * 95.0).min(170.0);
                *px = image::Rgb([
                    (g * 0.8 + v * 0.75).min(255.0) as u8,
                    (g * 0.92 + v * 0.9).min(255.0) as u8,
                    (g + v).min(255.0) as u8,
                ]);
            }
        }
        img.save(path).unwrap();
    }

    /// (v290) Genoux et coudes : debout, baissé à toute hauteur et à plat ventre, ses pieds
    /// touchent le sol (ni dessous, ni en l'air quand il ne marche pas) ; ses genoux plient
    /// quand il se baisse, ses coudes au repos ; à plat ventre, ses jambes sont allongées.
    #[test]
    fn its_knees_and_elbows_bend_and_its_feet_stay_on_the_ground() {
        let feet = |a: &Anim| {
            segments(Vec3::ZERO, 0.0, a)
                .into_iter()
                .filter(|s| s.0 == LEG_L || s.0 == LEG_R)
                .map(|(i, xf, lo, hi)| {
                    let mut low = f32::MAX;
                    for k in 0..8 {
                        let c = Vec3::new(
                            if k & 1 == 0 { lo[0] } else { hi[0] },
                            if k & 2 == 0 { lo[1] } else { hi[1] },
                            if k & 4 == 0 { lo[2] } else { hi[2] },
                        );
                        low = low.min(xf.transform_point3(c).y);
                    }
                    (i, low)
                })
                .fold([f32::MAX; 2], |mut m, (i, y)| {
                    m[i] = m[i].min(y);
                    m
                })
        };
        for i in 0..=20 {
            let crawl = i as f32 / 20.0;
            let still = Anim {
                crawl,
                hunch: 0.18,
                ..Anim::default()
            };
            let low = feet(&still);
            // (Au tout début de la descente, entre ses deux façons de marcher, un pied peut
            // quitter le sol de quelques centimètres : jamais dessous.)
            for y in low {
                assert!(y > -0.02 && (i == 1 || y < 0.03), "baissé de {crawl} : un pied à {y}");
            }
            let l = limbs(Vec3::ZERO, 0.0, &still);
            let knee = l.bend[LEG_L];
            match i {
                0 => assert!(knee == 0.0, "debout : genou plié de {knee}"),
                4..=12 => assert!(knee > 0.6, "baissé de {crawl} : genou plié de {knee}"),
                20 => assert!(knee < 0.05, "à plat ventre : genou plié de {knee}"),
                _ => {}
            }
            assert!(l.bend[ARM_L] > 0.2, "baissé de {crawl} : coude plié de {}", l.bend[ARM_L]);
            // En marchant : jamais sous le sol, et jamais les deux pieds en l'air.
            for k in 0..16 {
                let walking = Anim {
                    walk: k as f32 * TAU / 16.0,
                    swing: 0.9,
                    ..still
                };
                let low = feet(&walking);
                assert!(low[0].min(low[1]) > -0.02, "baissé de {crawl}, pas {k} : {low:?}");
                assert!(low[0].min(low[1]) < 0.12, "baissé de {crawl}, pas {k} : {low:?}");
            }
        }
        // Debout, la jambe qui avance plie le genou ; celle qui recule reste tendue.
        let step = Anim {
            walk: 0.0,
            swing: 1.0,
            ..Anim::default()
        };
        let l = limbs(Vec3::ZERO, 0.0, &step);
        assert!(l.bend[LEG_L] > 0.5 && l.bend[LEG_R] == 0.0, "{:?}", l.bend);
    }

    /// (v290) Planche de ses postures vues de côté (genoux et coudes) : debout, en marchant,
    /// baissé d'un quart, de moitié, des trois quarts, à plat ventre, penché sur nous pour
    /// mordre. Écrite dans le fichier nommé par MINELIB_IRM (essai à lancer à la main).
    #[test]
    #[ignore]
    fn postures_seen_from_the_side() {
        let Ok(path) = std::env::var("MINELIB_IRM") else {
            return;
        };
        let pose = |walk: f32, swing: f32, crawl: f32| Anim {
            walk,
            swing,
            crawl,
            hunch: 0.18,
            ..Anim::default()
        };
        let poses = [
            pose(0.0, 0.0, 0.0),
            pose(0.0, 0.9, 0.0),
            pose(1.57, 0.9, 0.0),
            pose(1.2, 0.8, 0.25),
            pose(1.2, 0.8, 0.5),
            pose(4.3, 0.8, 0.5),
            pose(1.2, 0.8, 0.75),
            pose(1.2, 0.8, 1.0),
            Anim {
                hunch: 1.6,
                arms: 0.5,
                grab: 1.0,
                mouth: 1.0,
                ..Anim::default()
            },
            Anim {
                hunch: 0.38,
                arms: 1.25,
                walk: 1.57,
                swing: 0.9,
                ..Anim::default()
            },
        ];
        let size = 384u32;
        let mut img = image::RgbImage::from_pixel(size * poses.len() as u32, size * 2, image::Rgb([40, 44, 52]));
        for (n, a) in poses.iter().enumerate() {
            let mut e = Evide::new(Vec3::new(3.0, 70.0, -5.0), 0.0, 7);
            e.anim = a.clone();
            let (mut mesh, mut prev) = (Vec::new(), Vec::new());
            e.mesh_moved([1.0; 3], None, &mut mesh, &mut prev);
            for (v, angle) in [(0u32, -1.57f32), (1, -0.6)] {
                let target = e.pos + Vec3::new(0.0, 1.6, 0.6);
                let eye = target + Vec3::new(angle.sin(), 0.05, -angle.cos()) * 6.2;
                let proj = Mat4::perspective_infinite_reverse_rh(48f32.to_radians(), 1.0, 0.05);
                let vp = proj * Mat4::look_at_rh(eye, target, Vec3::Y);
                let Some([shot, _]) = minelib_render::draw_offscreen(vp, size, &mesh, &[], &[], &[]) else {
                    eprintln!("pas de carte graphique : essai sauté");
                    return;
                };
                for (i, px) in shot.iter().enumerate() {
                    if px[3] < 0.5 {
                        continue;
                    }
                    let (x, y) = (i as u32 % size, i as u32 / size);
                    img.put_pixel(n as u32 * size + x, v * size + y, image::Rgb([225, 222, 210]));
                }
            }
        }
        img.save(path).unwrap();
    }

    /// (v272) « Il se penche jusqu'à la hauteur où il mord » et « suit le mouvement du bras,
    /// sa hauteur aussi » : sa bouche vient sur l'endroit visé, d'une jambe à une épaule, et
    /// y reste quand cet endroit se balance.
    #[test]
    fn it_bends_down_to_what_it_bites_and_follows_a_moving_arm() {
        let w = flat();
        let feet = Vec3::new(0.5, 11.0, -1.8);
        for (side, height) in [(0.1f32, 0.55f32), (0.36, 0.95), (-0.36, 1.3), (0.0, 1.55)] {
            let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
            let spot = feet + Vec3::new(side, height, 0.0);
            // (v290) Le milieu du membre : 0,12 bloc derrière l'endroit mordu (il est sur le
            // devant du membre), un peu plus haut.
            let limb = spot + Vec3::new(0.0, 0.12, -0.12);
            let p = Prey {
                spot: Some(spot),
                limb: Some(limb),
                ..prey(feet)
            };
            // (v290) Il se redresse entre deux morsures : on regarde au moment où sa bouche
            // se referme, et entre deux.
            let (mut bites, mut low, mut straight) = (0, f32::MAX, f32::MAX);
            for _ in 0..(7 * 60) {
                let events = e.update(&w, 1.0 / 60.0, Some(p));
                for ev in events {
                    let Event::Bites { at, .. } = ev else {
                        continue;
                    };
                    bites += 1;
                    let miss = at.distance(spot);
                    assert!(miss < 0.2, "hauteur {height} : sa bouche à {miss}");
                    // Plus c'est bas, plus il se plie (au-delà de l'horizontale pour une
                    // jambe).
                    let hunch = e.anim.hunch;
                    assert!(hunch > 2.9 - 1.1 * height, "hauteur {height} : {hunch}");
                    // « La tête tournée vers le centre du membre attaqué » : son visage (le
                    // devant de sa tête, −z) regarde vers le membre (devant lui, à peu près à
                    // l'horizontale), et non plus vers le sol.
                    let (xf, _, _) = parts(e.pos, e.yaw, &e.anim)[HEAD];
                    let face = xf.transform_vector3(Vec3::NEG_Z).normalize();
                    let to = (limb - at).normalize();
                    assert!(face.dot(to) > 0.6, "hauteur {height} : visage à {}", face.dot(to));
                    assert!(face.y > -0.6, "hauteur {height} : le visage vers le sol ({})", face.y);
                    // « Il revient à sa position initiale à la fin de la morsure » : depuis
                    // la morsure d'avant, il s'est redressé.
                    assert!(
                        bites == 1 || straight < 0.5,
                        "hauteur {height} : resté plié de {straight} entre deux morsures"
                    );
                    straight = f32::MAX;
                }
                if bites > 0 {
                    straight = straight.min(e.anim.hunch);
                }
                // Ses mains ne passent pas sous le sol.
                low = segments(e.pos, e.yaw, &e.anim)
                    .iter()
                    .filter(|s| s.0 == ARM_L || s.0 == ARM_R)
                    .map(|(_, xf, lo, _)| xf.transform_point3(Vec3::new(0.0, lo[1], 0.0)).y)
                    .fold(low, f32::min);
            }
            assert!(bites >= 3, "hauteur {height} : {bites} morsures en 7 s");
            assert!(low > 10.95, "hauteur {height} : une main à {low}");
        }
        // Un bras qui se balance : 0,25 bloc d'avant en arrière et 0,15 de haut en bas, une
        // fois et demie par seconde. Sa bouche le suit : elle se referme dessus (à moins de
        // `TOUCH`, sinon elle mord dans le vide) presque à chaque fois.
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 9);
        let mut bites = 0;
        for i in 0..(10 * 60) {
            let t = i as f32 / 60.0;
            let swing = (t * 1.5 * TAU).sin();
            let spot = feet + Vec3::new(0.36, 1.0 + 0.15 * swing.abs(), 0.25 * swing);
            let p = Prey {
                spot: Some(spot),
                limb: Some(feet + Vec3::new(0.3, 1.1, 0.0)),
                ..prey(feet)
            };
            for ev in e.update(&w, 1.0 / 60.0, Some(p)) {
                if let Event::Bites { at, .. } = ev {
                    assert!(at.distance(spot) <= TOUCH);
                    bites += 1;
                }
            }
        }
        assert!(bites >= 4, "{bites} morsures en 10 s sur un bras qui bouge");
    }

    /// (v272) Un mur entre lui et nous : il en fait le tour par le chemin le plus court au
    /// lieu de buter dessus ; un bassin en travers : il le passe à la nage, à plat, à la
    /// surface, et se hisse sur l'autre berge.
    #[test]
    fn it_finds_its_way_around_walls_and_swims_across_water() {
        let mut w = flat();
        for x in -6..=6 {
            for y in 11..16 {
                w.set(x, y, -5, BlockId::STONE);
            }
        }
        let p = prey(Vec3::new(0.5, 11.0, -10.5));
        let path = find_path(&w, [0, 11, 0], cell_of(p.feet));
        // Par un bout du mur : 7 cases de côté, 10 de long, 6 ou 7 de retour.
        assert!((20..=28).contains(&path.len()), "{} cases", path.len());
        assert!(path.iter().all(|c| c[1] == 11 && !(c[2] == -5 && c[0].abs() <= 6)));
        assert!(path.iter().any(|c| c[0].abs() == 7 && c[2] == -5), "{path:?}");
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 5);
        e.mode = Mode::Chase;
        e.known = Some(p.feet);
        let mut reached = None;
        for i in 0..(9 * 60) {
            e.update(&w, 1.0 / 60.0, Some(p));
            if e.feeding {
                reached = Some(i as f32 / 60.0);
                break;
            }
        }
        // 24 blocs à 6,6 blocs/s : moins de cinq secondes (en butant sur le mur, jamais).
        assert!(reached.is_some_and(|t| t < 5.5), "{reached:?} : {}", e.pos);
        // Hors de portée de la recherche : le chemin mène au plus près.
        let far = find_path(&flat(), [0, 11, 0], [0, 11, -44]);
        assert!(far.len() >= 40 && far.last().is_some_and(|c| c[2] <= -40), "{}", far.len());

        // Un bassin de 9 blocs de large, d'un bord à l'autre du monde, profond de 2.
        let mut w = flat();
        for z in -13..=-5 {
            for x in -48..48 {
                w.set(x, 10, z, BlockId::WATER);
                w.set(x, 9, z, BlockId::WATER);
                w.set(x, 8, z, BlockId::STONE);
            }
        }
        let p = prey(Vec3::new(0.5, 11.0, -18.5));
        let mut e = Evide::new(Vec3::new(0.5, 11.0, 0.5), 0.0, 5);
        let (mut swam, mut floated, mut reached) = (0.0f32, true, None);
        for i in 0..(14 * 60) {
            e.update(&w, 1.0 / 60.0, Some(p));
            if e.swimming && e.pos.z < -6.0 && e.pos.z > -12.0 {
                swam += 1.0 / 60.0;
                floated &= e.crawling && (9.9..=10.4).contains(&e.pos.y);
            }
            if e.feeding {
                reached = Some(i as f32 / 60.0);
                break;
            }
        }
        let [_, swim_fast, _] = crate::player::SWIM;
        assert!(swam > 5.0 / swim_fast && floated, "nagé {swam} s, à la surface : {floated}");
        assert!(reached.is_some() && e.pos.z < -15.0 && !e.swimming, "{reached:?} {}", e.pos);
    }

    /// (v272) Enregistrés avec le monde : on les retrouve où ils étaient, entamés comme on
    /// les avait laissés ; plus de fichier quand il n'y en a plus.
    #[test]
    fn they_are_saved_with_the_world() {
        let dir = std::env::temp_dir().join(format!("minelib_evides_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let grids = grids();
        let mut h = Horde::default();
        assert!(h.spawn(Vec3::new(3.5, 70.0, -8.25), 1.25));
        assert!(h.spawn(Vec3::new(-40.5, 12.0, 100.5), -0.5));
        for k in 0..7u32 {
            h.list[1].flesh.bite_somewhere(k * 31 + 5, OUR_BITE, &[0, 2, 3, 5], &grids);
        }
        h.list[1].crawling = true;
        h.save(&dir, [0; 3]);
        let back = Horde::load(&dir, [0; 3]);
        assert_eq!(back.list.len(), 2);
        for (a, b) in h.list.iter().zip(&back.list) {
            assert_eq!((a.pos, a.yaw, a.crawling), (b.pos, b.yaw, b.crawling));
            assert_eq!(a.flesh.bites(), b.flesh.bites());
            assert_eq!(a.flesh.eaten(&grids), b.flesh.eaten(&grids));
        }
        assert!(back.list[1].flesh.eaten(&grids) > 0.05 && back.list[0].flesh.is_whole());
        h.list.clear();
        h.save(&dir, [0; 3]);
        assert!(!dir.join("evides.txt").exists() && Horde::load(&dir, [0; 3]).list.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Il apparaît tout seul la nuit, rarement (une fois par vingt minutes en moyenne), dans
    /// les 256 blocs autour de nous, jamais le jour ; un œuf en pose un tout de suite.
    #[test]
    fn it_comes_at_night_rarely_and_from_an_egg_at_once() {
        let w = flat();
        let p = prey(Vec3::new(0.5, 11.0, 0.5));
        let mut h = Horde::default();
        for _ in 0..(3600.0 / SPAWN_TICK) as usize {
            h.update(&w, SPAWN_TICK, &[p], false);
        }
        assert!(h.list.is_empty(), "le jour");
        // Dix heures de nuit, les monstres retirés à mesure : une trentaine d'apparitions.
        let mut came = 0;
        for _ in 0..(36_000.0 / SPAWN_TICK) as usize {
            h.update(&w, SPAWN_TICK, &[p], true);
            for e in h.list.drain(..) {
                let d = Vec3::new(e.pos.x - 0.5, 0.0, e.pos.z - 0.5).length();
                assert!((SPAWN_NEAR - 1.0..=SPAWN_FAR + 1.0).contains(&d), "à {d}");
                assert!((e.pos.y - 11.0).abs() < 0.01);
                came += 1;
            }
            h.natural = 0;
        }
        assert!((12..=55).contains(&came), "{came} en dix heures de nuit");
        assert!(h.spawn(Vec3::new(2.5, 11.0, 2.5), 0.0));
        assert_eq!(h.list.len(), 1);
        assert_eq!(egg_pixels().count(), EGG.iter().flat_map(|r| r.chars()).filter(|c| *c != '.').count());
        assert!(EGG.iter().all(|r| r.chars().count() == 16));
    }

    /// « Tu me montreras l'IRM des couches du monstre » : ses couches de devant vers le dos
    /// (sa peau, sa gelée, le creux de sa bouche), écrites dans le fichier nommé par
    /// MINELIB_IRM (essai à lancer à la main).
    #[test]
    #[ignore]
    fn irm_of_the_evide() {
        let Ok(path) = std::env::var("MINELIB_IRM") else {
            return;
        };
        let img = crate::player_model::irm(
            grids(),
            |part, c, d| {
                if part == HEAD && mouth(c, d, 0.35) {
                    return None;
                }
                let [x, y, z] = c;
                let face = if z == 0 {
                    Some(0)
                } else if z + 1 == d[2] {
                    Some(1)
                } else if x + 1 == d[0] {
                    Some(2)
                } else if x == 0 {
                    Some(3)
                } else if y + 1 == d[1] {
                    Some(4)
                } else if y == 0 {
                    Some(5)
                } else {
                    None
                };
                Some(match face {
                    Some(k) => skin(part, c, d, k),
                    None => inside(part, c, d),
                })
            },
            TALL,
        );
        img.save(path).unwrap();
    }
}
