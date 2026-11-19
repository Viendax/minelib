//! Objets au sol et coffres, côté joueur : la vue du monde (`view`), les demandes en
//! attente et ce qu'il faut verser à l'inventaire. La règle (voir `minelib_net::items`) :
//! un objet quitte l'inventaire avant de partir, n'y revient que sur un refus, et n'y
//! entre que sur la réponse de l'arbitre — le relais en ligne, ce jeu en solo.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use glam::{Mat4, Vec3};
use minelib_net::game::BuildMsg;
use minelib_net::items::{Credit, ItemStore, Ledger, Pile};
use minelib_render::{EntityVertex, RayBoxKind};
use minelib_world::World;

use crate::inventory::Item;

/// Fichier des objets d'un monde (au sol et dans les coffres).
pub const FILE: &str = "objets.bin";
/// On ramasse un objet à cette distance (blocs).
pub const PICK_RANGE: f32 = 1.5;
/// Un objet se lâche à cette distance devant soi, comme dans Minecraft.
pub const DROP_REACH: f32 = 2.2;
/// Délai avant de pouvoir ramasser un objet qui vient d'apparaître (le sien : plus long,
/// le temps de s'en éloigner).
const PICK_DELAY: f32 = 0.6;
const OWN_PICK_DELAY: f32 = 2.0;
/// Durée du vol d'un objet lâché (s).
const FLIGHT: f32 = 0.45;

pub struct Items {
    /// Ce jeu arbitre lui-même (solo) : `view` fait alors foi.
    local: bool,
    pub view: ItemStore,
    ledger: Ledger,
    /// Demandes à envoyer à l'arbitre (chunk concerné, demande).
    pub outbox: Vec<([i32; 2], BuildMsg)>,
    /// Piles gagnées, à verser à l'inventaire.
    pub credits: Vec<Pile>,
    /// Objets vus apparaître : quand, d'où ils ont été lancés, et si c'est le mien.
    born: HashMap<u64, (Instant, Option<Vec3>, bool)>,
    /// Objets qu'on a demandé à ramasser (pas deux demandes pour le même).
    asked: HashMap<u64, Instant>,
    /// Le monde a changé depuis le dernier enregistrement.
    pub dirty: bool,
    start: Instant,
}

fn chunk_at(x: f32, z: f32) -> [i32; 2] {
    [
        (x.floor() as i32).div_euclid(16),
        (z.floor() as i32).div_euclid(16),
    ]
}

impl Items {
    pub fn new(local: bool) -> Self {
        Self {
            local,
            view: ItemStore::default(),
            ledger: Ledger::starting_at(rand::random()),
            outbox: Vec::new(),
            credits: Vec::new(),
            born: HashMap::new(),
            asked: HashMap::new(),
            dirty: false,
            start: Instant::now(),
        }
    }

    /// Ce jeu arbitre-t-il lui-même (solo) ?
    pub fn is_local(&self) -> bool {
        self.local
    }

    /// États enregistrés avec le monde `dir`.
    pub fn load(dir: &Path) -> Vec<BuildMsg> {
        std::fs::read(dir.join(FILE))
            .ok()
            .and_then(|b| minelib_net::game::decode::<Vec<BuildMsg>>(&b))
            .unwrap_or_default()
    }

    /// Enregistre les objets du monde (solo, ou propriétaire d'un monde en ligne).
    pub fn save(&mut self, dir: &Path) {
        if !self.dirty {
            return;
        }
        let snap = self.view.snapshot();
        let path = dir.join(FILE);
        let ok = if snap.is_empty() {
            std::fs::remove_file(&path).is_ok() || !path.exists()
        } else {
            std::fs::write(&path, minelib_net::game::encode(&snap)).is_ok()
        };
        self.dirty = !ok;
    }

    /// Solo : les objets de la sauvegarde reviennent tels quels.
    pub fn restore(&mut self, saved: &[BuildMsg]) {
        for m in saved {
            self.view.seed(m);
        }
    }

    /// Lâche `pile` (déjà retirée de l'inventaire) de `from` vers `to`.
    pub fn drop_item(&mut self, from: Vec3, to: Vec3, pile: Pile) {
        let id: u64 = rand::random();
        self.born.insert(id, (Instant::now(), Some(from), true));
        let req = self.ledger.drop_item(id, to.to_array(), pile);
        self.outbox.push((chunk_at(to.x, to.z), req));
    }

    /// Pose `pile` (déjà retirée de l'inventaire) dans un coffre.
    pub fn chest_put(&mut self, pos: [i32; 3], slot: u8, pile: Pile) {
        let req = self.ledger.chest_put(pos, slot, pile);
        self.outbox
            .push(([pos[0].div_euclid(16), pos[2].div_euclid(16)], req));
    }

    /// Demande `count` objets d'un emplacement de coffre (versés à la réponse).
    pub fn chest_take(&mut self, pos: [i32; 3], slot: u8, count: u8) {
        let req = self.ledger.chest_take(pos, slot, count);
        self.outbox
            .push(([pos[0].div_euclid(16), pos[2].div_euclid(16)], req));
    }

    /// Ramasse ce qui traîne autour de `feet` (si `room` dit que l'inventaire a de la
    /// place pour la pile).
    pub fn pick_up(&mut self, feet: Vec3, room: impl Fn(Pile) -> bool) {
        let center = feet + Vec3::Y * 0.9;
        let now = Instant::now();
        self.asked
            .retain(|_, t| now.duration_since(*t).as_secs_f32() < 3.0);
        let near: Vec<(u64, [f32; 3])> = self
            .view
            .live_drops()
            .filter(|(id, d)| {
                let age = self.born.get(id).map_or(f32::MAX, |b| {
                    let wait = if b.2 { OWN_PICK_DELAY } else { PICK_DELAY };
                    now.duration_since(b.0).as_secs_f32() - wait
                });
                age > 0.0
                    && !self.asked.contains_key(id)
                    && Vec3::from(d.pos).distance(center) < PICK_RANGE
                    && room(d.pile)
            })
            .map(|(id, d)| (id, d.pos))
            .collect();
        for (id, pos) in near {
            self.asked.insert(id, now);
            let req = self.ledger.take_item(id);
            self.outbox.push((chunk_at(pos[0], pos[2]), req));
        }
    }

    /// Solo : ce jeu arbitre ses propres demandes. En ligne, elles restent dans `outbox`
    /// (la session les envoie au relais).
    pub fn arbitrate(&mut self, me: &str) {
        if !self.local {
            return;
        }
        for (_, req) in std::mem::take(&mut self.outbox) {
            let out = match self.view.apply(&req, me) {
                Ok(v) => v,
                Err(seq) => vec![BuildMsg::Denied { seq }],
            };
            for m in out {
                self.hear(&m, me, None);
            }
        }
    }

    /// Solo : le bloc en `pos` a changé ; si c'était un coffre garni, son contenu tombe.
    pub fn block_changed(&mut self, pos: [i32; 3], me: &str) {
        if !self.local {
            return;
        }
        for m in self.view.spill(pos, rand::random::<u64>) {
            self.hear(&m, me, None);
        }
    }

    /// État publié par l'arbitre (ou refus). `thrower` : position du joueur nommé par le
    /// message, si on le voit (l'objet part de lui).
    pub fn hear(&mut self, m: &BuildMsg, me: &str, thrower: Option<Vec3>) {
        match m {
            BuildMsg::Item { id, drop, .. } => {
                if drop.is_some() {
                    self.born
                        .entry(*id)
                        .or_insert((Instant::now(), thrower, false));
                } else {
                    self.born.remove(id);
                    self.asked.remove(id);
                }
                self.dirty = true;
            }
            BuildMsg::Slot { .. } => self.dirty = true,
            BuildMsg::Denied { .. } => {}
            _ => return,
        }
        // En solo l'arbitre a déjà changé `view` ; le refaire ne change rien.
        self.view.absorb(m);
        if let Some(Credit::Give(p)) = self.ledger.on_message(m, me) {
            self.credits.push(p);
        }
    }

    /// Triangles des objets au sol proches de `cam` : (v185) le vrai modèle de l'objet,
    /// comme tenu en main (bloc avec sa forme et ses textures, plante en croix, torche avec
    /// sa flamme ; deux pour une pile), qui flotte et tourne sur lui-même, éclairé comme les
    /// joueurs par `light` (lumière du ciel à sa place : abrité ou non, jour ou nuit) ; juste
    /// lâché, il vole de la main au sol.
    pub fn build(&self, cam: Vec3, light: &dyn Fn(Vec3) -> [f32; 3], out: &mut Vec<EntityVertex>) {
        let t = self.start.elapsed().as_secs_f32();
        for (id, d) in self.view.live_drops() {
            let rest = Vec3::from(d.pos);
            if rest.distance_squared(cam) > 64.0 * 64.0 {
                continue;
            }
            let Some(item) = Item::from_code(d.pile.item) else {
                continue;
            };
            // Chaque objet a son propre décalage (ils ne bougent pas tous ensemble).
            let phase = (id % 628) as f32 / 100.0;
            let mut at = rest + Vec3::Y * (0.12 + 0.06 * (t * 2.0 + phase).sin());
            if let Some((born, Some(from), _)) = self.born.get(&id) {
                let k = born.elapsed().as_secs_f32() / FLIGHT;
                if k < 1.0 {
                    at = from.lerp(at, k) + Vec3::Y * 1.2 * k * (1.0 - k);
                }
            }
            let l = light(at + Vec3::Y * 0.15);
            let spin = Mat4::from_translation(at) * Mat4::from_rotation_y(t * 1.2 + phase);
            emit_ground_item(out, spin, item, l);
            if d.pile.count > 1 {
                let side = spin * Mat4::from_translation(Vec3::new(0.09, 0.07, 0.07));
                emit_ground_item(out, side, item, l);
            }
        }
    }

    /// (v185) Solo : un objet dans l'eau remonte à sa surface et suit le courant (arrêté par
    /// les murs), descend les chutes d'eau ; sorti de l'eau (elle a séché, il est arrivé au
    /// bout), il retombe au sol. En ligne, l'arbitre ne fait pas couler l'eau : les objets
    /// restent où ils sont tombés.
    pub fn drift(&mut self, world: &World, dt: f32) {
        use minelib_world::registry::{fluid, Fluid, FLUID_FALLING};
        if !self.local || dt <= 0.0 {
            return;
        }
        let solid = |x: i32, y: i32, z: i32| world.get(x, y, z).is_solid();
        let mut moved = false;
        for d in self.view.drops.values_mut().flatten() {
            let p = Vec3::from(d.pos);
            let (x, z) = (p.x.floor() as i32, p.z.floor() as i32);
            if !world.contains(minelib_world::ChunkPos::containing(x, z)) {
                continue;
            }
            let wet = |y: i32| crate::fluids::surface(world, [x, y, z]);
            let falling =
                |y: i32| fluid(world.get(x, y, z)).is_some_and(|(_, l)| l == FLUID_FALLING);
            // Case où il baigne (posé au fond, son bas touche la case du dessous).
            let mut y = (p.y + 0.05).floor() as i32;
            if wet(y).is_none() && wet(y - 1).is_some_and(|(_, h)| (y - 1) as f32 + h > p.y) {
                y -= 1;
            }
            let mut next = p;
            if wet(y).is_some() && (falling(y - 1) || (falling(y) && wet(y - 1).is_some())) {
                // Dans une chute d'eau : il descend avec elle.
                next.y = p.y - FALL * dt;
            } else if wet(y).is_some() {
                // Jusqu'en haut de l'eau (pas en remontant une chute), à fleur de surface.
                while wet(y + 1).is_some() && !falling(y + 1) && y < 255 {
                    y += 1;
                }
                let (f, h) = wet(y).unwrap_or((Fluid::Water, 1.0));
                let float = y as f32 + h - FLOAT;
                next.y = if float > p.y {
                    (p.y + RISE * dt).min(float)
                } else {
                    float
                };
                let speed = if f == Fluid::Water { ITEM_CURRENT } else { 0.0 };
                let push = crate::fluids::flow(world, [x, y, z]) * speed * dt;
                for axis in [0, 2] {
                    if push[axis] == 0.0 {
                        continue;
                    }
                    let mut q = next;
                    q[axis] += push[axis];
                    // Le bord de l'objet ne rentre pas dans un mur.
                    let mut edge = q;
                    edge[axis] += push[axis].signum() * 0.15;
                    let c = |v: f32| v.floor() as i32;
                    if !solid(c(edge.x), c(q.y + 0.05), c(edge.z)) {
                        next = q;
                    }
                }
            } else {
                // Hors de l'eau : il retombe jusqu'au premier sol.
                let mut ground = y;
                while ground > 0 && !solid(x, ground - 1, z) {
                    ground -= 1;
                }
                if p.y > ground as f32 + 1e-3 {
                    next.y = (p.y - FALL * dt).max(ground as f32);
                }
            }
            if next != p {
                d.pos = next.to_array();
                moved = true;
            }
        }
        if moved {
            self.dirty = true;
        }
    }
}

/// (v185) Objet au sol, comme en main : un bloc à sa forme (côté de 0,28 bloc), une
/// torche avec sa flamme ; le reste (baguette, écran, tête) en petit pavé de sa couleur.
fn emit_ground_item(out: &mut Vec<EntityVertex>, xf: Mat4, item: Item, light: [f32; 3]) {
    let center = |size: f32| {
        xf * Mat4::from_scale(Vec3::splat(size))
            * Mat4::from_translation(Vec3::new(-0.5, 0.0, -0.5))
    };
    match item {
        Item::Block(b) => crate::player_model::emit_block_item(out, center(BLOCK_SIZE), b, light),
        Item::Torch(chem) => {
            let rgb = chem
                .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
                .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
            crate::player_model::emit_torch_item(out, center(TORCH_SIZE), rgb, light);
        }
        // (v201) Lampe torche : couchée sur le flanc, éteinte.
        Item::Flashlight => {
            let lying = xf
                * Mat4::from_scale(Vec3::splat(FLASHLIGHT_SIZE))
                * Mat4::from_translation(Vec3::new(0.0, 5.0 / 16.0, 0.0))
                * Mat4::from_rotation_z(-std::f32::consts::FRAC_PI_2)
                * Mat4::from_translation(Vec3::new(-0.5, -6.7 / 16.0, -0.5));
            crate::player_model::emit_flashlight_item(out, lying, false, light);
        }
        _ => {
            let h = BLOCK_SIZE / 2.0;
            crate::player_model::emit_item(
                out,
                xf,
                [-h, 0.0, -h],
                [h, 2.0 * h, h],
                icon_kind(item),
                light,
            );
        }
    }
}

/// Côté d'un bloc posé au sol comme objet (blocs) ; taille de la torche (sa vraie taille
/// fois ce facteur).
const BLOCK_SIZE: f32 = 0.28;
const TORCH_SIZE: f32 = 0.6;
/// (v201) Lampe torche au sol : sa vraie taille fois ce facteur.
const FLASHLIGHT_SIZE: f32 = 0.6;
/// Dans l'eau : vitesse de l'objet porté par le courant, de sa remontée, de sa chute une
/// fois sorti (blocs/s), et de combien il s'enfonce sous la surface.
const ITEM_CURRENT: f32 = 2.0;
const RISE: f32 = 1.2;
const FALL: f32 = 8.0;
const FLOAT: f32 = 0.12;

/// Aspect d'un objet au sol : la texture de son bloc, ou une couleur.
fn icon_kind(item: Item) -> RayBoxKind {
    match item {
        Item::Block(b) => RayBoxKind::Block {
            layer: b.def().tex[1] as u32,
            uv: [0.0, 0.0, 1.0, 1.0],
            glow: b.def().light.is_some(),
        },
        Item::Torch(chem) => {
            let rgb = chem
                .and_then(|i| minelib_world::flame::CHEMICALS.get(i))
                .map_or(minelib_world::flame::WOOD_FLAME, |c| c.rgb);
            RayBoxKind::Flat(rgb)
        }
        Item::Wand => RayBoxKind::Flat([120, 80, 200]),
        Item::Flashlight => RayBoxKind::Flat(crate::flashlight::BODY),
        Item::Screen(_) => RayBoxKind::Flat([30, 30, 36]),
        // Tête à skin (au sol, elle devient une tête simple : voir `Item::code`).
        Item::Head(_) => RayBoxKind::Flat([214, 166, 132]),
    }
}

/// Où retombe un objet lâché de `eye` dans la direction `dir` : devant le joueur, posé sur
/// le premier sol en dessous ; contre un mur, il tombe à ses pieds.
pub fn landing(world: &World, eye: Vec3, dir: Vec3) -> Vec3 {
    let flat = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero();
    let solid = |p: Vec3| {
        world
            .get(p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
            .is_solid()
    };
    // Le plus loin possible devant soi sans traverser un mur.
    let mut reach = 0.0;
    let mut d = 0.2;
    while d <= DROP_REACH {
        if solid(eye + flat * d) {
            break;
        }
        reach = d;
        d += 0.2;
    }
    let mut p = eye + flat * reach;
    // Chute jusqu'au sol.
    for _ in 0..256 {
        let below = p - Vec3::Y * 0.25;
        if below.y < 0.0 || solid(below) {
            break;
        }
        p = below;
    }
    Vec3::new(p.x, p.y.floor().max(0.0), p.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "moi";

    fn stone(n: u8) -> Pile {
        Pile { item: 1, count: n }
    }

    #[test]
    fn solo_drops_come_back_only_once_and_chests_keep_their_piles() {
        let mut it = Items::new(true);
        // Lâché : au sol, plus dans l'inventaire (rien à verser).
        it.drop_item(Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), stone(5));
        it.arbitrate(ME);
        assert!(it.credits.is_empty());
        assert_eq!(it.view.live_drops().count(), 1);
        // Trop tôt pour le ramasser (on vient de le lâcher).
        it.pick_up(Vec3::new(2.0, 0.0, 0.0), |_| true);
        assert!(it.outbox.is_empty());
        // Plus tard : ramassé, versé une seule fois, même en repassant dessus.
        for b in it.born.values_mut() {
            b.0 -= std::time::Duration::from_secs(5);
        }
        for _ in 0..3 {
            it.pick_up(Vec3::new(2.0, 0.0, 0.0), |_| true);
            it.arbitrate(ME);
        }
        assert_eq!(it.credits, vec![stone(5)]);
        assert_eq!(it.view.live_drops().count(), 0);
        // Inventaire plein : l'objet reste au sol.
        it.credits.clear();
        it.drop_item(Vec3::ZERO, Vec3::ZERO, stone(1));
        it.arbitrate(ME);
        for b in it.born.values_mut() {
            b.0 -= std::time::Duration::from_secs(5);
        }
        it.pick_up(Vec3::ZERO, |_| false);
        assert!(it.outbox.is_empty());
        // Coffre : posé, repris en deux fois ; trop demandé : refusé, rien de versé.
        let chest = [0, 64, 0];
        it.chest_put(chest, 3, stone(10));
        it.arbitrate(ME);
        it.chest_take(chest, 3, 4);
        it.chest_take(chest, 3, 7);
        it.chest_take(chest, 3, 6);
        it.arbitrate(ME);
        assert_eq!(it.credits, vec![stone(4), stone(6)]);
        assert!(it.view.chests.is_empty());
        // Pile refusée par le coffre (emplacement pris par autre chose) : rendue.
        it.credits.clear();
        it.chest_put(chest, 0, stone(64));
        it.chest_put(chest, 0, Pile { item: 2, count: 3 });
        it.arbitrate(ME);
        assert_eq!(it.credits, vec![Pile { item: 2, count: 3 }]);
        // Coffre cassé : son contenu tombe, une fois.
        it.block_changed(chest, ME);
        it.block_changed(chest, ME);
        assert_eq!(it.view.totals()[&1], 65);
        assert_eq!(it.view.live_drops().count(), 2);
    }

    #[test]
    fn a_saved_world_gives_back_exactly_its_items() {
        let dir = std::env::temp_dir().join(format!("minelib_objets_{}", rand::random::<u32>()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut it = Items::new(true);
        it.drop_item(Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0), stone(9));
        it.chest_put([5, 6, 7], 29, stone(64));
        it.arbitrate(ME);
        it.save(&dir);
        let saved = Items::load(&dir);
        let mut back = Items::new(true);
        back.restore(&saved);
        // Chargée deux fois : rien en double.
        back.restore(&saved);
        assert_eq!(back.view.totals(), it.view.totals());
        assert_eq!(back.view.totals()[&1], 73);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (v185) Au sol, un objet a le modèle qu'il a en main : un bloc avec ses textures de
    /// dessus, de côté et de dessous ; une dalle à sa hauteur ; une plante en croix ; une
    /// torche avec sa flamme.
    #[test]
    fn ground_items_have_their_real_shape_and_textures() {
        use minelib_world::BlockId;
        let reg = minelib_world::registry::registry();
        let grass = reg.by_key("grass").unwrap();
        let mut out = Vec::new();
        crate::player_model::emit_block_item(&mut out, Mat4::IDENTITY, grass, [1.0; 3]);
        assert_eq!(out.len(), 36);
        let t = grass.def().tex;
        let layers = |pred: &dyn Fn(&EntityVertex) -> bool| -> Vec<i32> {
            let mut l: Vec<i32> = out.iter().filter(|v| pred(v)).map(|v| v.layer).collect();
            l.dedup();
            l
        };
        // Dessus (tous les sommets à y = 1), dessous (y = 0).
        let top: Vec<i32> = out
            .chunks_exact(6)
            .filter(|f| f.iter().all(|v| v.pos[1] == 1.0))
            .map(|f| f[0].layer)
            .collect();
        let bottom: Vec<i32> = out
            .chunks_exact(6)
            .filter(|f| f.iter().all(|v| v.pos[1] == 0.0))
            .map(|f| f[0].layer)
            .collect();
        assert_eq!(top, vec![t[0] as i32]);
        assert_eq!(bottom, vec![t[2] as i32]);
        assert!(layers(&|v| v.pos[1] > 0.0 && v.pos[1] < 1.0).is_empty());
        // Dalle du bas : moitié de haut, la moitié basse de l'image des côtés.
        let slab = reg
            .defs
            .iter()
            .position(|d| d.key.starts_with("slab:") && d.key.ends_with(":bottom"))
            .map(|i| BlockId(i as u16))
            .unwrap();
        let mut out = Vec::new();
        crate::player_model::emit_block_item(&mut out, Mat4::IDENTITY, slab, [1.0; 3]);
        assert!(out.iter().all(|v| v.pos[1] <= 0.5 + 1e-6));
        assert!(out.iter().any(|v| v.uv[1] == 0.5));
        // Plante : deux plans en croix.
        let plant = reg
            .defs
            .iter()
            .position(|d| matches!(d.shape, minelib_world::registry::Shape::Cross))
            .map(|i| BlockId(i as u16))
            .unwrap();
        let mut out = Vec::new();
        crate::player_model::emit_block_item(&mut out, Mat4::IDENTITY, plant, [1.0; 3]);
        assert_eq!(out.len(), 12);
        // Torche : bâton texturé et flamme.
        let mut out = Vec::new();
        emit_ground_item(&mut out, Mat4::IDENTITY, Item::Torch(None), [1.0; 3]);
        assert!(out.iter().any(|v| v.layer == minelib_render::ENTITY_FLAME));
        assert!(out
            .iter()
            .any(|v| v.layer == BlockId::TORCH.def().tex[1] as i32));
    }

    /// (v185) En solo, un objet tombé dans un ruisseau remonte à la surface, descend le
    /// courant et s'arrête contre le mur du bout ; l'eau partie, il retombe au sol.
    #[test]
    fn solo_items_float_down_a_stream_and_fall_when_it_dries() {
        use minelib_world::{BlockId, ChunkPos};
        let reg = minelib_world::registry::registry();
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 9, z, BlockId::STONE);
            }
            w.set(x, 10, 7, BlockId::STONE);
            w.set(x, 10, 9, BlockId::STONE);
        }
        // Rigole le long de +X : source en x = 2, puis 7, 6… ; mur en x = 8.
        w.set(2, 10, 8, BlockId::WATER);
        for (i, x) in (3..8).enumerate() {
            w.set(
                x,
                10,
                8,
                reg.by_key(&format!("water_flow:{}", 7 - i)).unwrap(),
            );
        }
        w.set(8, 10, 8, BlockId::STONE);
        let mut it = Items::new(true);
        it.drop_item(Vec3::ZERO, Vec3::new(3.5, 10.0, 8.5), stone(1));
        it.arbitrate(ME);
        let pos = |it: &Items| Vec3::from(it.view.live_drops().next().unwrap().1.pos);
        for _ in 0..60 {
            it.drift(&w, 1.0 / 60.0);
        }
        let p = pos(&it);
        assert!(p.x > 4.5, "emporté : {p}");
        assert!(p.y > 10.2, "à la surface : {p}");
        for _ in 0..600 {
            it.drift(&w, 1.0 / 60.0);
        }
        let p = pos(&it);
        assert!(p.x < 8.0 - 0.1 && p.x > 7.5, "arrêté par le mur : {p}");
        // L'eau sèche : il retombe sur le fond.
        for x in 2..8 {
            w.set(x, 10, 8, BlockId::AIR);
        }
        for _ in 0..120 {
            it.drift(&w, 1.0 / 60.0);
        }
        assert_eq!(pos(&it).y, 10.0);
        // En ligne : rien ne bouge.
        let mut it = Items::new(false);
        it.view.drops.insert(
            7,
            Some(minelib_net::items::Dropped {
                pos: [3.5, 12.0, 8.5],
                pile: stone(1),
            }),
        );
        it.drift(&w, 0.5);
        assert_eq!(pos(&it), Vec3::new(3.5, 12.0, 8.5));
    }
}
