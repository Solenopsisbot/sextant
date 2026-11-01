//! Clean, portable building plans for Sextant.
//!
//! A blueprint is deliberately independent of a live bot: it describes blocks relative to an
//! origin and can be parsed, transformed, sorted, and inspected in tests before a future bot
//! executor places it in the world.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::world::Pos;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Blueprint {
    /// Human-readable name shown in progress messages.
    #[serde(default)]
    pub name: String,
    /// Relative block placements. Duplicate offsets are rejected by [`Blueprint::validate`].
    pub blocks: Vec<BlueprintBlock>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlueprintBlock {
    pub offset: Pos,
    /// A namespaced block state, optionally including properties.
    pub block: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildAction<'a> {
    Place { pos: Pos, block: &'a str },
}

impl Blueprint {
    pub fn parse(text: &str) -> Result<Self, String> {
        let blueprint: Self =
            serde_json::from_str(text).map_err(|e| format!("blueprint JSON: {e}"))?;
        blueprint.validate()?;
        Ok(blueprint)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("blueprint {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.blocks.len() > 10_000_000 {
            return Err("blueprint has too many blocks".into());
        }
        let mut seen = std::collections::HashSet::with_capacity(self.blocks.len());
        for (i, block) in self.blocks.iter().enumerate() {
            if block.block.trim().is_empty() {
                return Err(format!("blueprint block {i} has an empty block name"));
            }
            if !seen.insert(block.offset) {
                return Err(format!("blueprint has duplicate offset {:?}", block.offset));
            }
        }
        Ok(())
    }

    /// Return world-space placements, sorted from low to high so supports are built first.
    pub fn actions(&self, origin: Pos) -> Vec<BuildAction<'_>> {
        let mut blocks: Vec<_> = self.blocks.iter().collect();
        blocks.sort_by_key(|b| (b.offset[1], b.offset[2], b.offset[0]));
        blocks
            .into_iter()
            .map(|b| BuildAction::Place {
                pos: [
                    origin[0] + b.offset[0],
                    origin[1] + b.offset[1],
                    origin[2] + b.offset[2],
                ],
                block: &b.block,
            })
            .collect()
    }

    /// Rotate around the Y axis clockwise `quarter_turns` times.
    pub fn rotate_y(&mut self, quarter_turns: u8) {
        for block in &mut self.blocks {
            for _ in 0..quarter_turns % 4 {
                let [x, y, z] = block.offset;
                block.offset = [-z, y, x];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_supports_before_walls() {
        let b = Blueprint::parse(
            r#"{"name":"lamp","blocks":[{"offset":[0,1,0],"block":"minecraft:glass"},{"offset":[0,0,0],"block":"minecraft:stone"}]}"#,
        )
        .unwrap();
        let actions = b.actions([10, 64, -2]);
        assert_eq!(
            actions[0],
            BuildAction::Place {
                pos: [10, 64, -2],
                block: "minecraft:stone"
            }
        );
        assert_eq!(
            actions[1],
            BuildAction::Place {
                pos: [10, 65, -2],
                block: "minecraft:glass"
            }
        );
    }

    #[test]
    fn duplicate_offsets_and_empty_names_are_rejected() {
        assert!(Blueprint::parse(r#"{"blocks":[{"offset":[0,0,0],"block":"stone"},{"offset":[0,0,0],"block":"dirt"}]}"#).is_err());
        assert!(Blueprint::parse(r#"{"blocks":[{"offset":[0,0,0],"block":""}]}"#).is_err());
    }

    #[test]
    fn rotation_is_reversible() {
        let mut b = Blueprint {
            name: String::new(),
            blocks: vec![BlueprintBlock {
                offset: [2, 3, 1],
                block: "stone".into(),
            }],
        };
        b.rotate_y(1);
        assert_eq!(b.blocks[0].offset, [-1, 3, 2]);
        b.rotate_y(3);
        assert_eq!(b.blocks[0].offset, [2, 3, 1]);
    }
}
