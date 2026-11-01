//! Clean, portable building plans for Sextant.
//!
//! A blueprint is deliberately independent of a live bot: it describes blocks relative to an
//! origin and can be parsed, transformed, sorted, and inspected in tests before a future bot
//! executor places it in the world.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::nbt::{self, Nbt};
use crate::world::Pos;

fn read_varint(data: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let mut out = 0;
    for shift in (0..35).step_by(7) {
        let b = *data.get(*cursor).ok_or("truncated schematic BlockData")?;
        *cursor += 1;
        out |= u32::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err("schematic BlockData varint is too long".into())
}

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

    /// Load the native JSON format or a Sponge `.schem` binary schematic. The schematic
    /// reader intentionally accepts the portable palette/block-data subset shared by WorldEdit
    /// and Litematica; unsupported legacy numeric palettes return a useful error.
    pub fn load_auto(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|e| format!("blueprint {}: {e}", path.display()))?;
        if bytes.first().is_some_and(|b| *b == b'{' || *b == b'[') {
            return Self::parse(
                std::str::from_utf8(&bytes).map_err(|e| format!("blueprint JSON: {e}"))?,
            );
        }
        Self::parse_schematic(&bytes)
    }

    fn parse_schematic(bytes: &[u8]) -> Result<Self, String> {
        let root = nbt::read_root(bytes)?;
        let view = if root.get("Width").is_some() {
            &root
        } else {
            root.get("Schematic")
                .ok_or("schematic root is not a compound")?
        };
        let dimension = |name: &str| {
            view.get(name)
                .and_then(Nbt::i32)
                .ok_or_else(|| format!("schematic missing {name}"))
        };
        let width = dimension("Width")?;
        let height = dimension("Height")?;
        let length = dimension("Length")?;
        if width <= 0 || height <= 0 || length <= 0 {
            return Err("schematic has invalid dimensions".into());
        }
        let palette = view
            .get("Palette")
            .and_then(Nbt::compound)
            .ok_or("schematic has no block palette")?;
        let mut names = vec![String::new(); palette.len()];
        for (name, value) in palette {
            let index = value.i32().ok_or("schematic palette index is not an int")? as usize;
            if index >= names.len() {
                return Err("schematic palette index out of range".into());
            }
            names[index] = if name.contains(':') {
                name.clone()
            } else {
                format!("minecraft:{name}")
            };
        }
        if names.iter().any(String::is_empty) {
            return Err("schematic palette has gaps".into());
        }
        let data = view
            .get("BlockData")
            .and_then(Nbt::bytes)
            .ok_or("schematic has no BlockData palette stream")?;
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(length as usize);
        let mut cursor = 0;
        let mut blocks = Vec::with_capacity(expected);
        for i in 0..expected {
            let index = read_varint(data, &mut cursor)? as usize;
            if index >= names.len() {
                return Err(format!("schematic block {i} palette index out of range"));
            }
            if names[index] != "minecraft:air" {
                let x = (i as i32) % width;
                let z = (i as i32 / width) % length;
                let y = i as i32 / (width * length);
                blocks.push(BlueprintBlock {
                    offset: [x, y, z],
                    block: names[index].clone(),
                });
            }
        }
        let blueprint = Self {
            name: String::new(),
            blocks,
        };
        blueprint.validate()?;
        Ok(blueprint)
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

fn read_varint(data: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let mut out = 0;
    for shift in (0..35).step_by(7) {
        let b = *data.get(*cursor).ok_or("truncated schematic BlockData")?;
        *cursor += 1;
        out |= u32::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err("schematic BlockData varint is too long".into())
}

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

    /// Load the native JSON format or a Sponge `.schem` binary schematic. The schematic
    /// reader intentionally accepts the portable palette/block-data subset shared by WorldEdit
    /// and Litematica; unsupported legacy numeric palettes return a useful error.
    pub fn load_auto(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let bytes =
            std::fs::read(path).map_err(|e| format!("blueprint {}: {e}", path.display()))?;
        if bytes.first().is_some_and(|b| *b == b'{' || *b == b'[') {
            return Self::parse(
                std::str::from_utf8(&bytes).map_err(|e| format!("blueprint JSON: {e}"))?,
            );
        }
        Self::parse_schematic(&bytes)
    }

    fn parse_schematic(bytes: &[u8]) -> Result<Self, String> {
        let root = nbt::read_root(bytes)?;
        let view = if root.get("Width").is_some() {
            &root
        } else {
            root.get("Schematic")
                .ok_or("schematic root is not a compound")?
        };
        let dimension = |name: &str| {
            view.get(name)
                .and_then(Nbt::i32)
                .ok_or_else(|| format!("schematic missing {name}"))
        };
        let width = dimension("Width")?;
        let height = dimension("Height")?;
        let length = dimension("Length")?;
        if width <= 0 || height <= 0 || length <= 0 {
            return Err("schematic has invalid dimensions".into());
        }
        let palette = view
            .get("Palette")
            .and_then(Nbt::compound)
            .ok_or("schematic has no block palette")?;
        let mut names = vec![String::new(); palette.len()];
        for (name, value) in palette {
            let index = value.i32().ok_or("schematic palette index is not an int")? as usize;
            if index >= names.len() {
                return Err("schematic palette index out of range".into());
            }
            names[index] = if name.contains(':') {
                name.clone()
            } else {
                format!("minecraft:{name}")
            };
        }
        if names.iter().any(String::is_empty) {
            return Err("schematic palette has gaps".into());
        }
        let data = view
            .get("BlockData")
            .and_then(Nbt::bytes)
            .ok_or("schematic has no BlockData palette stream")?;
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(length as usize);
        let mut cursor = 0;
        let mut blocks = Vec::with_capacity(expected);
        for i in 0..expected {
            let index = read_varint(data, &mut cursor)? as usize;
            if index >= names.len() {
                return Err(format!("schematic block {i} palette index out of range"));
            }
            if names[index] != "minecraft:air" {
                let x = (i as i32) % width;
                let z = (i as i32 / width) % length;
                let y = i as i32 / (width * length);
                blocks.push(BlueprintBlock {
                    offset: [x, y, z],
                    block: names[index].clone(),
                });
            }
        }
        let blueprint = Self {
            name: String::new(),
            blocks,
        };
        blueprint.validate()?;
        Ok(blueprint)
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

/// Small clean-room reader for the compound/short/int/byte-array subset used by Sponge schematics.
mod nbt {
    use std::collections::BTreeMap;

    #[allow(dead_code)]
    #[derive(Clone, Debug)]
    pub enum Nbt {
        Byte(i8),
        Short(i16),
        Int(i32),
        String(String),
        Bytes(Vec<u8>),
        List(Vec<Nbt>),
        Compound(BTreeMap<String, Nbt>),
        Other,
    }
    impl Nbt {
        pub fn compound(&self) -> Option<&BTreeMap<String, Nbt>> {
            if let Self::Compound(v) = self {
                Some(v)
            } else {
                None
            }
        }
        pub fn get(&self, key: &str) -> Option<&Nbt> {
            self.compound()?.get(key)
        }
        pub fn i32(&self) -> Option<i32> {
            match self {
                Self::Int(v) => Some(*v),
                Self::Short(v) => Some(*v as i32),
                Self::Byte(v) => Some(*v as i32),
                _ => None,
            }
        }
        pub fn bytes(&self) -> Option<&[u8]> {
            if let Self::Bytes(v) = self {
                Some(v)
            } else {
                None
            }
        }
    }
    struct R<'a> {
        b: &'a [u8],
        p: usize,
    }
    impl<'a> R<'a> {
        fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
            let e = self.p.checked_add(n).ok_or("NBT overflow")?;
            let v = self.b.get(self.p..e).ok_or("truncated NBT")?;
            self.p = e;
            Ok(v)
        }
        fn u8(&mut self) -> Result<u8, String> {
            Ok(self.take(1)?[0])
        }
        fn i8(&mut self) -> Result<i8, String> {
            Ok(self.u8()? as i8)
        }
        fn u16(&mut self) -> Result<u16, String> {
            Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
        }
        fn i16(&mut self) -> Result<i16, String> {
            Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
        }
        fn i32(&mut self) -> Result<i32, String> {
            Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
        }
        fn string(&mut self) -> Result<String, String> {
            let n = self.u16()? as usize;
            String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "invalid NBT string".into())
        }
        fn value(&mut self, tag: u8) -> Result<Nbt, String> {
            match tag {
                1 => Ok(Nbt::Byte(self.i8()?)),
                2 => Ok(Nbt::Short(self.i16()?)),
                3 => Ok(Nbt::Int(self.i32()?)),
                8 => Ok(Nbt::String(self.string()?)),
                7 => {
                    let n = self.i32()?;
                    if n < 0 {
                        return Err("negative NBT array".into());
                    }
                    Ok(Nbt::Bytes(self.take(n as usize)?.to_vec()))
                }
                9 => {
                    let t = self.u8()?;
                    let n = self.i32()?;
                    if n < 0 {
                        return Err("negative NBT list".into());
                    }
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(self.value(t)?)
                    }
                    Ok(Nbt::List(v))
                }
                10 => {
                    let mut m = BTreeMap::new();
                    loop {
                        let t = self.u8()?;
                        if t == 0 {
                            break;
                        }
                        let k = self.string()?;
                        m.insert(k, self.value(t)?);
                    }
                    Ok(Nbt::Compound(m))
                }
                11 => {
                    let n = self.i32()?;
                    if n < 0 {
                        return Err("negative NBT array".into());
                    }
                    self.take(n as usize * 4)?;
                    Ok(Nbt::Other)
                }
                12 => {
                    let n = self.i32()?;
                    if n < 0 {
                        return Err("negative NBT array".into());
                    }
                    self.take(n as usize * 8)?;
                    Ok(Nbt::Other)
                }
                _ => Err(format!("unsupported NBT tag {tag}")),
            }
        }
    }
    pub fn read_root(bytes: &[u8]) -> Result<Nbt, String> {
        let mut r = R { b: bytes, p: 0 };
        let t = r.u8()?;
        if t != 10 {
            return Err("schematic root is not a compound".into());
        }
        let _ = r.string()?;
        r.value(10)
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
