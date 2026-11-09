//! Types de blocs. L'identifiant est un `u16` ; les couleurs sont choisies côté shader.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, Hash)]
pub struct BlockId(pub u16);

impl BlockId {
    /// Torche posée au sol ou contre un mur.
    pub fn is_torch(self) -> bool {
        self == BlockId::TORCH || matches!(self.def().shape, crate::registry::Shape::WallTorch(_))
    }

    /// Torche contre le mur situé du côté `d` (0 +X, 1 −X, 4 +Z, 5 −Z).
    pub fn wall_torch(d: u8) -> BlockId {
        crate::registry::registry().by_key(&format!("wall_torch:{d}")).unwrap_or(BlockId::TORCH)
    }

    pub const AIR: BlockId = BlockId(0);
    pub const STONE: BlockId = BlockId(1);
    pub const DIRT: BlockId = BlockId(2);
    pub const GRASS: BlockId = BlockId(3);
    pub const SAND: BlockId = BlockId(4);
    pub const WOOD: BlockId = BlockId(5);
    pub const LEAVES: BlockId = BlockId(6);
    pub const SNOW: BlockId = BlockId(7);
    pub const BEDROCK: BlockId = BlockId(8);
    pub const PLANKS: BlockId = BlockId(9);
    pub const BRICK: BlockId = BlockId(10);
    /// Torche : bâton + flamme, émet de la lumière (couleur stockée dans le monde).
    pub const TORCH: BlockId = BlockId(11);
    /// Identifiant de rendu de la flamme (jamais stocké dans un chunk).
    pub const FLAME: BlockId = BlockId(12);
    pub const COBBLESTONE: BlockId = BlockId(13);
    pub const GRAVEL: BlockId = BlockId(14);
    /// Eau : pas encore de fluides, un bloc qu'on traverse.
    pub const WATER: BlockId = BlockId(15);
    pub const LAVA: BlockId = BlockId(16);
    pub const SANDSTONE: BlockId = BlockId(17);
    pub const CLAY: BlockId = BlockId(18);
    pub const COAL_ORE: BlockId = BlockId(19);
    pub const IRON_ORE: BlockId = BlockId(20);
    pub const GOLD_ORE: BlockId = BlockId(21);
    pub const DIAMOND_ORE: BlockId = BlockId(22);
    /// Verre : laisse voir (et passer la lumière) à travers.
    pub const GLASS: BlockId = BlockId(23);
    pub const ICE: BlockId = BlockId(24);
    pub const OBSIDIAN: BlockId = BlockId(25);
    pub const STONE_BRICKS: BlockId = BlockId(26);
    pub const MOSSY_COBBLESTONE: BlockId = BlockId(27);
    pub const TERRACOTTA: BlockId = BlockId(28);
    pub const WOOL: BlockId = BlockId(29);

    /// Nombre d'identifiants de blocs (taille de la table de textures du shader).
    pub const COUNT: usize = 32;

    /// Blocs « normaux » que le joueur peut poser.
    pub const BUILDING: [BlockId; 20] = [
        Self::STONE,
        Self::DIRT,
        Self::GRASS,
        Self::SAND,
        Self::WOOD,
        Self::LEAVES,
        Self::SNOW,
        Self::PLANKS,
        Self::BRICK,
        Self::COBBLESTONE,
        Self::STONE_BRICKS,
        Self::MOSSY_COBBLESTONE,
        Self::GLASS,
        Self::SANDSTONE,
        Self::GRAVEL,
        Self::CLAY,
        Self::OBSIDIAN,
        Self::TERRACOTTA,
        Self::WOOL,
        Self::ICE,
    ];

    /// Barrière invisible (cartes Minecraft).
    pub const BARRIER: BlockId = BlockId(30);

    /// Définition complète (forme, textures…) dans le registre.
    #[inline]
    pub fn def(self) -> &'static crate::registry::BlockDef {
        crate::registry::registry().get(self)
    }

    /// Cube plein et opaque : cache ses voisins et arrête la lumière.
    #[inline]
    pub fn is_opaque(self) -> bool {
        self.def().full
    }

    /// Le bloc bloque-t-il les déplacements ?
    #[inline]
    pub fn is_solid(self) -> bool {
        self.def().solid
    }

    /// Le bloc peut-il être visé (casser, poser contre) ?
    #[inline]
    pub fn is_targetable(self) -> bool {
        self.def().target
    }

    /// Peut-on casser ce bloc ?
    pub fn is_breakable(self) -> bool {
        self.is_targetable() && self != Self::BEDROCK && self != Self::BARRIER
    }

    pub fn name(self) -> &'static str {
        &self.def().name
    }
}
