//! Minimal big-endian NBT reader used for portable Sponge schematics.
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub enum Nbt {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Bytes(Vec<u8>),
    String(String),
    List(Vec<Nbt>),
    Compound(BTreeMap<String, Nbt>),
}

impl Nbt {
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        match self { Self::Compound(c) => c.get(key), _ => None }
    }
    pub fn compound(&self) -> Option<&BTreeMap<String, Nbt>> {
        match self { Self::Compound(c) => Some(c), _ => None }
    }
    pub fn i32(&self) -> Option<i32> {
        match self { Self::Byte(v) => Some(*v as i32), Self::Short(v) => Some(*v as i32), Self::Int(v) => Some(*v), _ => None }
    }
    pub fn bytes(&self) -> Option<&[u8]> {
        match self { Self::Bytes(v) => Some(v), _ => None }
    }
}

pub fn read_root(bytes: &[u8]) -> Result<Nbt, String> {
    let mut r = Reader { b: bytes, at: 0 };
    let tag = r.u8()?;
    if tag != 10 { return Err("schematic NBT root is not a compound".into()); }
    let _name = r.string()?;
    r.payload(tag)
}

struct Reader<'a> { b: &'a [u8], at: usize }
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).ok_or("NBT size overflow")?;
        let out = self.b.get(self.at..end).ok_or("truncated NBT")?;
        self.at = end; Ok(out)
    }
    fn u8(&mut self) -> Result<u8, String> { Ok(self.take(1)?[0]) }
    fn i8(&mut self) -> Result<i8, String> { Ok(self.u8()? as i8) }
    fn u16(&mut self) -> Result<u16, String> { Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap())) }
    fn i16(&mut self) -> Result<i16, String> { Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap())) }
    fn i32(&mut self) -> Result<i32, String> { Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap())) }
    fn i64(&mut self) -> Result<i64, String> { Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap())) }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| "NBT string is not UTF-8".into())
    }
    fn payload(&mut self, tag: u8) -> Result<Nbt, String> {
        Ok(match tag {
            1 => Nbt::Byte(self.i8()?), 2 => Nbt::Short(self.i16()?),
            3 => Nbt::Int(self.i32()?), 4 => Nbt::Long(self.i64()?),
            7 => { let n = self.i32()?; if n < 0 { return Err("negative NBT byte array".into()); } Nbt::Bytes(self.take(n as usize)?.to_vec()) },
            8 => Nbt::String(self.string()?),
            9 => { let item = self.u8()?; let n = self.i32()?; if n < 0 { return Err("negative NBT list".into()); } let mut v=Vec::with_capacity(n as usize); for _ in 0..n { v.push(self.payload(item)?); } Nbt::List(v) },
            10 => { let mut c=BTreeMap::new(); loop { let t=self.u8()?; if t==0 { break; } let name=self.string()?; c.insert(name,self.payload(t)?); } Nbt::Compound(c) },
            11 | 12 => { let n=self.i32()?; if n<0 { return Err("negative NBT array".into()); } let width=if tag==11 {4} else {8}; let _=self.take(n as usize * width)?; Nbt::Bytes(Vec::new()) },
            _ => return Err(format!("unsupported NBT tag {tag}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_compound_int() {
        let bytes = [10, 0, 0, 3, 0, 1, b'X', 0, 0, 0, 7, 0];
        assert_eq!(read_root(&bytes).unwrap().get("X").and_then(Nbt::i32), Some(7));
    }
}
