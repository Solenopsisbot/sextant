//! What the planner knows about the world: each block's meaning to a player on foot.
//!
//! Blocks come from a game as state strings (`minecraft:oak_stairs[facing=north,...]`, the
//! `blocks` control command). `classify` turns a name into a [`Block`]; anything it doesn't
//! know is a plain solid, full block, which is the safe guess for walking. A position nobody
//! has read is [`Block::Unknown`], which the planner never walks into.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// A block position (x, y, z).
pub type Pos = [i32; 3];

/// One block, as the planner sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Block {
    /// Never read: unloaded or outside what was fetched.
    Unknown,
    /// Nothing to collide with (air, grass, flowers, torches, signs...).
    Passable,
    /// Passable but slow: cobwebs, sweet berry bushes, powder snow.
    Sticky,
    /// A full collision box you can stand on, broken in `hardness` (vanilla's units).
    Solid {
        hardness: f32,
        falls: bool,
    },
    /// Taller than a block (fences, walls): can't be stepped or jumped onto.
    Tall {
        hardness: f32,
    },
    /// Never broken by a player (bedrock, barriers, portals' frames).
    Unbreakable,
    /// Hurts on touch or under foot (fire, magma, cactus, campfires).
    Danger,
    Water,
    Lava,
    /// Climbable without collision (ladders, vines, scaffolding).
    Climbable,
}

impl Block {
    /// You can be in it (feet or head).
    pub fn passable(self) -> bool {
        matches!(
            self,
            Block::Passable | Block::Sticky | Block::Water | Block::Climbable
        )
    }
    /// You can stand on top of it.
    pub fn standable(self) -> bool {
        matches!(self, Block::Solid { .. } | Block::Unbreakable)
    }
    /// It can be mined out of the way, and how long that takes (hardness).
    pub fn breakable(self) -> Option<f32> {
        match self {
            Block::Solid { hardness, .. } | Block::Tall { hardness } => Some(hardness),
            _ => None,
        }
    }
}

/// The meaning of block `name` (with or without `minecraft:` and its `[properties]`).
pub fn classify(state: &str) -> Block {
    let name = state.split('[').next().unwrap_or(state);
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let props = state.find('[').map_or("", |i| &state[i..]);
    let has = |s: &str| name.contains(s);
    let ends = |s: &str| name.ends_with(s);
    match name {
        "air" | "cave_air" | "void_air" => return Block::Passable,
        "water" | "bubble_column" | "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" => {
            return Block::Water;
        }
        "lava" => return Block::Lava,
        "bedrock"
        | "barrier"
        | "end_portal_frame"
        | "end_portal"
        | "end_gateway"
        | "nether_portal"
        | "command_block"
        | "chain_command_block"
        | "repeating_command_block"
        | "structure_block"
        | "jigsaw"
        | "light"
        | "reinforced_deepslate"
        | "test_block"
        | "test_instance_block" => {
            return Block::Unbreakable;
        }
        "fire" | "soul_fire" | "magma_block" | "cactus" | "wither_rose" | "pointed_dripstone" => {
            return Block::Danger;
        }
        "cobweb" | "sweet_berry_bush" | "powder_snow" => return Block::Sticky,
        "ladder"
        | "vine"
        | "scaffolding"
        | "twisting_vines"
        | "twisting_vines_plant"
        | "weeping_vines"
        | "weeping_vines_plant"
        | "cave_vines"
        | "cave_vines_plant" => {
            return Block::Climbable;
        }
        "snow" if props.contains("layers=1") => return Block::Passable,
        _ => {}
    }
    if (has("campfire") && !props.contains("lit=false")) || name == "lava_cauldron" {
        return Block::Danger;
    }
    // No collision box.
    let passable = [
        "short_grass",
        "tall_grass",
        "fern",
        "large_fern",
        "dead_bush",
        "dandelion",
        "poppy",
        "blue_orchid",
        "allium",
        "azure_bluet",
        "oxeye_daisy",
        "cornflower",
        "lily_of_the_valley",
        "sunflower",
        "lilac",
        "rose_bush",
        "peony",
        "torchflower",
        "pitcher_plant",
        "sugar_cane",
        "wheat",
        "carrots",
        "potatoes",
        "beetroots",
        "nether_wart",
        "redstone_wire",
        "tripwire",
        "brown_mushroom",
        "red_mushroom",
        "crimson_fungus",
        "warped_fungus",
        "crimson_roots",
        "warped_roots",
        "nether_sprouts",
        "hanging_roots",
        "glow_lichen",
        "sculk_vein",
        "spore_blossom",
        "small_dripleaf",
        "pink_petals",
        "leaf_litter",
        "wildflowers",
        "bush",
        "firefly_bush",
        "short_dry_grass",
        "tall_dry_grass",
        "structure_void",
    ];
    if passable.contains(&name)
        || ends("_sapling")
        || ends("_tulip")
        || ends("torch")
        || ends("_button")
        || ends("pressure_plate")
        || ends("_sign")
        || ends("_banner")
        || ends("rail")
        || ends("lever")
        || ends("_coral_fan")
        || ends("_coral")
        // A carpet is 1/16 of a block: walked over, not climbed.
        || ends("_carpet")
    {
        return Block::Passable;
    }
    if ends("_fence") || ends("_wall") || (ends("_fence_gate") && !props.contains("open=true")) {
        return Block::Tall { hardness: 2.0 };
    }
    if ends("_fence_gate") || (ends("_door") && props.contains("open=true")) {
        return Block::Passable;
    }
    let falls = matches!(
        name,
        "sand" | "red_sand" | "gravel" | "suspicious_sand" | "suspicious_gravel"
    ) || ends("_concrete_powder");
    Block::Solid {
        hardness: hardness(name),
        falls,
    }
}

/// Vanilla hardness of common blocks (how long they take to break); 2.0 for the rest.
fn hardness(name: &str) -> f32 {
    let ends = |s: &str| name.ends_with(s);
    match name {
        "dirt" | "coarse_dirt" | "rooted_dirt" | "farmland" | "dirt_path" | "sand" | "red_sand"
        | "soul_sand" | "soul_soil" | "clay" | "mud" | "snow_block" | "ice" | "packed_ice" => 0.5,
        "grass_block" | "mycelium" | "podzol" | "gravel" => 0.6,
        "netherrack" | "nylium" | "crimson_nylium" | "warped_nylium" => 0.4,
        "glass" | "glowstone" | "sea_lantern" => 0.3,
        "stone" | "smooth_stone" | "andesite" | "diorite" | "granite" | "tuff" | "calcite"
        | "stone_bricks" | "mossy_cobblestone" | "dripstone_block" => 1.5,
        "cobblestone" | "cobbled_deepslate" | "bricks" | "nether_bricks" => 2.0,
        "deepslate" | "end_stone" => 3.0,
        "obsidian" | "crying_obsidian" => 50.0,
        "ancient_debris" => 30.0,
        _ if ends("_leaves") => 0.2,
        _ if ends("_wool") => 0.8,
        _ if ends("_ore") => 3.0,
        _ if ends("_log") || ends("_wood") || ends("_planks") || ends("_stem") => 2.0,
        _ if ends("_glass") || ends("_glass_pane") => 0.3,
        _ if ends("terracotta") => 1.25,
        _ if ends("_concrete") => 1.8,
        _ => 2.0,
    }
}

/// Blocks read from a game, by position. What isn't here is [`Block::Unknown`].
#[derive(Default)]
pub struct Region {
    tiles: HashMap<Pos, Box<[Block]>>,
    missing: RefCell<HashSet<Pos>>,
    track: bool,
}

impl Region {
    pub fn new() -> Self {
        Self::default()
    }

    /// The block at `p`.
    pub fn get(&self, p: Pos) -> Block {
        let (key, index) = tile_index(p);
        let block = self.tiles.get(&key).map_or(Block::Unknown, |t| t[index]);
        if self.track && block == Block::Unknown {
            self.missing.borrow_mut().insert(key);
        }
        block
    }

    pub fn set(&mut self, p: Pos, b: Block) {
        let (key, index) = tile_index(p);
        self.tiles
            .entry(key)
            .or_insert_with(|| vec![Block::Unknown; 8192].into_boxed_slice())[index] = b;
    }

    pub fn track_missing(&mut self) {
        self.track = true;
        self.missing.borrow_mut().clear();
    }

    pub fn take_missing(&self) -> Vec<Pos> {
        self.missing.borrow_mut().drain().collect()
    }

    /// A `blocks` reply: `{"from": [x,y,z], "to": [x,y,z], "blocks": [[x,y,z,"state"], ...]}`,
    /// every non-air block of the box. The box is now known: what it didn't list is air.
    pub fn add_reply(&mut self, reply: &serde_json::Value) -> Result<(), String> {
        let pos = |v: &serde_json::Value| -> Option<Pos> {
            let a = v.as_array()?;
            Some([
                a.first()?.as_i64()? as i32,
                a.get(1)?.as_i64()? as i32,
                a.get(2)?.as_i64()? as i32,
            ])
        };
        let lo = pos(&reply["from"]).ok_or("blocks reply without from")?;
        let hi = pos(&reply["to"]).ok_or("blocks reply without to")?;
        // Replace the whole read, including blocks that have since become air.
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                for x in lo[0]..=hi[0] {
                    self.set([x, y, z], Block::Passable);
                }
            }
        }
        let mut palette = HashMap::new();
        for b in reply["blocks"]
            .as_array()
            .ok_or("blocks reply without blocks")?
        {
            let p = pos(b).ok_or("bad block entry")?;
            let state = b
                .get(3)
                .and_then(|s| s.as_str())
                .ok_or("block entry without a state")?;
            self.set(p, *palette.entry(state).or_insert_with(|| classify(state)));
        }
        Ok(())
    }

    /// Whether the 32-block cube that holds `p` has been read (see [`cube_of`]).
    pub fn has_cube(&self, p: Pos) -> bool {
        let (lo, hi) = cube_of(p);
        (lo[0]..=hi[0]).step_by(16).all(|x| {
            (lo[2]..=hi[2]).step_by(16).all(|z| {
                self.tiles
                    .get(&[x, lo[1], z])
                    .is_some_and(|t| !t.contains(&Block::Unknown))
            })
        })
    }
}

/// Socket read tiles: one chunk wide, 32 blocks high.
pub fn tile_index(p: Pos) -> (Pos, usize) {
    let key = [
        p[0].div_euclid(16) * 16,
        p[1].div_euclid(32) * 32,
        p[2].div_euclid(16) * 16,
    ];
    (
        key,
        ((p[1] - key[1]) * 256 + (p[2] - key[2]) * 16 + p[0] - key[0]) as usize,
    )
}

/// The 32x32x32 cube (aligned to multiples of 32) that holds `p`: the unit regions are read
/// in (`blocks` takes at most 32768 blocks per request).
pub fn cube_of(p: Pos) -> (Pos, Pos) {
    let lo = p.map(|v| v.div_euclid(32) * 32);
    (lo, lo.map(|v| v + 31))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_mean_what_a_walker_needs() {
        assert_eq!(classify("minecraft:air"), Block::Passable);
        assert_eq!(classify("minecraft:short_grass"), Block::Passable);
        assert_eq!(classify("minecraft:oak_sapling[stage=0]"), Block::Passable);
        assert_eq!(
            classify("minecraft:wall_torch[facing=north]"),
            Block::Passable
        );
        assert!(
            matches!(classify("minecraft:stone"), Block::Solid { hardness, .. } if hardness == 1.5)
        );
        assert!(matches!(
            classify("minecraft:gravel"),
            Block::Solid { falls: true, .. }
        ));
        assert!(matches!(
            classify("minecraft:oak_fence[east=true]"),
            Block::Tall { .. }
        ));
        assert_eq!(
            classify("minecraft:oak_fence_gate[open=true]"),
            Block::Passable
        );
        assert_eq!(classify("minecraft:lava[level=0]"), Block::Lava);
        assert_eq!(classify("minecraft:bedrock"), Block::Unbreakable);
        assert_eq!(classify("minecraft:campfire[lit=true]"), Block::Danger);
        assert!(matches!(
            classify("minecraft:campfire[lit=false]"),
            Block::Solid { .. }
        ));
        assert_eq!(classify("minecraft:snow[layers=1]"), Block::Passable);
        // Unknown blocks are solid: the safe guess for walking.
        assert!(matches!(
            classify("minecraft:some_new_block"),
            Block::Solid { .. }
        ));
    }

    #[test]
    fn a_read_box_is_air_where_it_lists_nothing() {
        let mut r = Region::new();
        let reply = serde_json::json!({
            "from": [0, 64, 0], "to": [31, 95, 31],
            "blocks": [[5, 64, 5, "minecraft:grass_block[snowy=false]"]]
        });
        r.add_reply(&reply).unwrap();
        assert!(r.get([5, 64, 5]).standable());
        assert_eq!(r.get([5, 65, 5]), Block::Passable);
        assert_eq!(r.get([40, 64, 5]), Block::Unknown);
        assert!(r.has_cube([10, 70, 10]));
        assert_eq!(cube_of([-1, 64, 33]), ([-32, 64, 32], [-1, 95, 63]));
    }
}
