//! Clean-room resource-pack block-model loading for the spectator renderer.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Model {
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub textures: BTreeMap<String, String>,
    #[serde(default)]
    pub elements: Vec<Element>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Element {
    pub from: [f32; 3],
    pub to: [f32; 3],
    #[serde(default)]
    pub faces: BTreeMap<String, Face>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Face {
    #[serde(default)]
    pub uv: Option<[f32; 4]>,
    pub texture: String,
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub tintindex: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct Library {
    root: PathBuf,
}

impl Library {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_owned(),
        }
    }

    pub fn load(&self, id: &str) -> Result<Model, String> {
        let (namespace, name) = split_id(id);
        self.load_inner(namespace, &format!("{name}.json"), &mut Vec::new())
    }

    fn load_inner(
        &self,
        namespace: &str,
        name: &str,
        stack: &mut Vec<String>,
    ) -> Result<Model, String> {
        let key = format!("{namespace}:{name}");
        if stack.contains(&key) {
            return Err(format!("model parent cycle at {key}"));
        }
        stack.push(key);
        let path = self.base().join(namespace).join("models/block").join(name);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let child: Model =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut model = if let Some(parent) = child.parent.as_deref() {
            let (parent_ns, parent_name) = if let Some((ns, n)) = parent.split_once(':') {
                (ns, n)
            } else {
                (namespace, parent)
            };
            self.load_inner(parent_ns, &format!("{parent_name}.json"), stack)?
        } else {
            Model::default()
        };
        model.parent = child.parent;
        model.textures.extend(child.textures);
        if !child.elements.is_empty() {
            model.elements = child.elements;
        }
        stack.pop();
        Ok(model)
    }

    pub fn texture_path(&self, id: &str) -> PathBuf {
        let (namespace, name) = split_id(id);
        self.base()
            .join(namespace)
            .join("textures/block")
            .join(format!("{name}.png"))
    }

    fn base(&self) -> PathBuf {
        let assets = self.root.join("assets");
        if assets.is_dir() {
            assets
        } else {
            self.root.clone()
        }
    }
}

pub fn split_id(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or(("minecraft", id))
}

/// Resolve a model face texture variable (`#side`) through inherited model texture maps.
pub fn resolve_texture(model: &Model, reference: &str) -> String {
    let mut current = reference.to_owned();
    for _ in 0..16 {
        let Some(key) = current.strip_prefix('#') else {
            break;
        };
        let Some(next) = model.textures.get(key) else {
            break;
        };
        if next == &current {
            break;
        }
        current = next.clone();
    }
    current
        .strip_prefix("minecraft:")
        .unwrap_or(&current)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherits_parent_and_resolves_face_texture() {
        let root = std::env::temp_dir().join(format!("sextant-pack-model-{}", std::process::id()));
        let dir = root.join("assets/minecraft/models/block");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("parent.json"), r##"{"textures":{"side":"minecraft:stone"},"elements":[{"from":[0,0,0],"to":[16,16,16],"faces":{"north":{"texture":"#side"}}}]}"##).unwrap();
        std::fs::write(
            dir.join("child.json"),
            r#"{"parent":"minecraft:parent","textures":{"side":"minecraft:oak_planks"}}"#,
        )
        .unwrap();
        let model = Library::new(&root).load("minecraft:child").unwrap();
        assert_eq!(resolve_texture(&model, "#side"), "oak_planks");
        assert_eq!(model.elements[0].faces["north"].texture, "#side");
        let _ = std::fs::remove_dir_all(root);
    }
}
