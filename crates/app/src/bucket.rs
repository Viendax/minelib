//! (v281) Le seau : vide, d'eau ou de lave. Demande de Viendax (24 novembre, 16 h 37) :
//! « ajoute le seau en survie : prendre la source d'eau, de lave, la reposer ; et en créatif
//! aussi : le seau vide enlève les sources sans se remplir, et le seau plein ne se vide
//! jamais ».
//!
//! - Seau vide, clic droit : la première source d'eau ou de lave visée (l'eau qui coule ne se
//!   prend pas : le regard passe au travers, comme l'air) est enlevée ; en survie, un seau
//!   plein de ce fluide remplace un seau vide.
//! - Seau plein, clic droit : le regard traverse l'eau et la lave jusqu'au bloc visé ; la
//!   source est posée sur sa face (ou à sa place, si c'est une plante, une torche, une fleur
//!   que le fluide emporte) ; en survie, un seau vide remplace un seau plein.
//! - L'écoulement fait le reste (`fluids`) : la lave qui touche l'eau durcit, etc.
//!
//! Image 16 × 16 faite pour le jeu : un seau de métal (bord clair, deux cerclages, une anse en
//! fil), son contenu vu par-dessus (vide : le fond sombre ; l'eau ; la lave).

use minelib_world::raycast::{raycast, raycast_with, Hit};
use minelib_world::registry::{fluid, washed_by_fluid, Fluid, FLUID_SOURCE};
use minelib_world::{BlockId, World};

/// Image du seau, ligne par ligne : `h` anse, `R` bord, `C` contenu, `L` reflet, `M` métal,
/// `D` ombre, `B` cerclage. `.` transparent.
const PAIL: [&str; 16] = [
    "................",
    "....hhhhhhhh....",
    "...h........h...",
    "..h..........h..",
    "..h..........h..",
    ".RRRRRRRRRRRRRR.",
    ".RCCCCCCCCCCCCR.",
    "..LMMMMMMMMMMD..",
    "..LMMMMMMMMMMD..",
    "..BBBBBBBBBBBB..",
    "...LMMMMMMMMD...",
    "...LMMMMMMMMD...",
    "...BBBBBBBBBB...",
    "....LMMMMMMD....",
    "....DDDDDDDD....",
    "................",
];

/// Couleur du métal du seau (sRGB) : le seau tenu par le modèle (F5), au sol.
pub const RGB: [u8; 3] = [160, 164, 170];

/// Couleur de ce qu'il contient, vu par-dessus (sRGB).
pub fn content_rgb(content: Option<Fluid>) -> [u8; 3] {
    match content {
        None => [52, 54, 58],
        Some(Fluid::Water) => [58, 108, 222],
        Some(Fluid::Lava) => [236, 112, 28],
    }
}

fn lin(c: [u8; 3]) -> [f32; 3] {
    c.map(|v| (v as f32 / 255.0).powf(2.2))
}

/// Pixels pleins de l'image du seau : (colonne, ligne, couleur linéaire).
pub fn pixels(content: Option<Fluid>) -> impl Iterator<Item = (usize, usize, [f32; 3])> {
    PAIL.iter().enumerate().flat_map(move |(y, row)| {
        row.chars().enumerate().filter_map(move |(x, c)| {
            let rgb = match c {
                'h' => [70, 72, 78],
                'R' => [205, 208, 214],
                'C' => content_rgb(content),
                'L' => [215, 218, 224],
                'M' => RGB,
                'D' => [105, 108, 115],
                'B' => [125, 128, 135],
                _ => return None,
            };
            Some((x, y, lin(rgb)))
        })
    })
}

/// Ce que fait le seau vide visé depuis `eye` dans la direction `dir`, à `reach` blocs au
/// plus : la source qu'il prend (sa case, son fluide). Le regard s'arrête au premier bloc
/// visable ou à la première source ; l'eau et la lave qui coulent ne l'arrêtent pas.
pub fn source_aimed(world: &World, eye: [f32; 3], dir: [f32; 3], reach: f32) -> Option<([i32; 3], Fluid)> {
    let is_source = |id: BlockId| fluid(id).is_some_and(|(_, level)| level == FLUID_SOURCE);
    let hit = raycast_with(world, eye, dir, reach, |id| id.is_targetable() || is_source(id))?;
    let (f, _) = fluid(hit.id).filter(|_| is_source(hit.id))?;
    Some((hit.block, f))
}

/// Où le seau plein pose sa source, visé depuis `eye` : le regard traverse l'eau et la lave
/// jusqu'au premier bloc visable ; la source va à sa place s'il est emporté par les fluides
/// (plante, torche, fleur), sinon sur la face visée, si elle est libre (air ou fluide).
pub fn place_aimed(world: &World, eye: [f32; 3], dir: [f32; 3], reach: f32) -> Option<[i32; 3]> {
    let hit: Hit = raycast(world, eye, dir, reach)?;
    let at = if washed_by_fluid(hit.id) {
        hit.block
    } else {
        hit.adjacent()
    };
    takes_fluid(world.get(at[0], at[1], at[2])).then_some(at)
}

/// Une source peut-elle aller dans une case qui tient ce bloc : l'air, un fluide, ou ce que les
/// fluides emportent ?
pub fn takes_fluid(id: BlockId) -> bool {
    id == BlockId::AIR || fluid(id).is_some() || washed_by_fluid(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use minelib_world::ChunkPos;

    /// Un sol de pierre en y = 10, un bassin d'eau (sources) en x = 4..6, et de l'eau qui
    /// coule en x = 2 et 3, une herbe haute en x = 8.
    fn pond() -> World {
        let mut w = World::new(0);
        w.insert_empty(ChunkPos { x: 0, z: 0 });
        for x in 0..16 {
            for z in 0..16 {
                w.set(x, 10, z, BlockId::STONE);
            }
        }
        for x in 4..=6 {
            w.set(x, 11, 8, Fluid::Water.block(FLUID_SOURCE));
        }
        w.set(2, 11, 8, Fluid::Water.block(5));
        w.set(3, 11, 8, Fluid::Water.block(6));
        let reg = minelib_world::registry::registry();
        let grass = (1..reg.len())
            .map(|i| BlockId(i as u16))
            .find(|b| washed_by_fluid(*b) && b.def().key.starts_with("plant:"))
            .expect("une plante");
        w.set(8, 11, 8, grass);
        w
    }

    /// Le seau vide prend la première source visée (pas l'eau qui coule, que le regard
    /// traverse) ; un mur devant l'arrête.
    #[test]
    fn the_empty_bucket_takes_the_source_aimed_through_flowing_water() {
        let w = pond();
        let eye = [0.5, 12.2, 8.5];
        let dir = [1.0, -0.12, 0.0];
        assert_eq!(source_aimed(&w, eye, dir, 6.0), Some(([4, 11, 8], Fluid::Water)));
        // Trop loin : rien.
        assert_eq!(source_aimed(&w, eye, dir, 2.5), None);
        // Un mur entre les deux : rien.
        let mut walled = pond();
        walled.set(1, 12, 8, BlockId::STONE);
        walled.set(1, 11, 8, BlockId::STONE);
        assert_eq!(source_aimed(&walled, eye, dir, 6.0), None);
    }

    /// Le seau plein pose sur la face visée à travers l'eau ; à la place d'une plante ; jamais
    /// dans un bloc plein.
    #[test]
    fn the_full_bucket_places_on_the_face_aimed_through_water() {
        let w = pond();
        // En regardant le fond du bassin : la source va sur le sol, dans l'eau (une source).
        let at = place_aimed(&w, [5.5, 13.0, 8.5], [0.0, -1.0, 0.0], 5.0);
        assert_eq!(at, Some([5, 11, 8]));
        // Sur l'herbe haute : à sa place.
        let at = place_aimed(&w, [8.5, 13.0, 8.5], [0.0, -1.0, 0.0], 5.0);
        assert_eq!(at, Some([8, 11, 8]));
        // Sur le sol nu.
        let at = place_aimed(&w, [10.5, 13.0, 8.5], [0.0, -1.0, 0.0], 5.0);
        assert_eq!(at, Some([10, 11, 8]));
        assert!(!takes_fluid(BlockId::STONE));
    }

    /// L'image : le contenu se voit, et seulement lui change d'un seau à l'autre.
    #[test]
    fn the_pail_shows_what_it_holds() {
        let empty: Vec<_> = pixels(None).collect();
        let water: Vec<_> = pixels(Some(Fluid::Water)).collect();
        assert_eq!(empty.len(), water.len());
        let changed = empty.iter().zip(&water).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 12, "la ligne du contenu");
        assert!(empty.len() > 100);
    }
}
