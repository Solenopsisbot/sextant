//! Data types shared by browser spectator views and live fleet streams.
//!
//! The dashboard intentionally keeps rendering in the browser, while this module provides a
//! stable, renderer-independent snapshot format. Horizon remains the source of truth; Sextant
//! only normalizes control-socket replies and interpolates between observations for smooth views.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Pos = [i32; 3];
pub type Vec3 = [f64; 3];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockSnapshot {
    pub pos: Pos,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub id: Option<i32>,
    pub kind: String,
    pub pos: Vec3,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
    #[serde(default)]
    pub velocity: Vec3,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

impl EntitySnapshot {
    /// Interpolate two observations. `t` is clamped so a delayed packet cannot overshoot.
    pub fn interpolate(&self, next: &Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: f64, b: f64| a + (b - a) * t;
        Self {
            id: next.id.or(self.id),
            kind: if next.kind.is_empty() {
                self.kind.clone()
            } else {
                next.kind.clone()
            },
            pos: [
                mix(self.pos[0], next.pos[0]),
                mix(self.pos[1], next.pos[1]),
                mix(self.pos[2], next.pos[2]),
            ],
            yaw: mix(self.yaw as f64, next.yaw as f64) as f32,
            pitch: mix(self.pitch as f64, next.pitch as f64) as f32,
            velocity: [
                mix(self.velocity[0], next.velocity[0]),
                mix(self.velocity[1], next.velocity[1]),
                mix(self.velocity[2], next.velocity[2]),
            ],
            name: next.name.clone().or_else(|| self.name.clone()),
            metadata: if next.metadata.is_null() {
                self.metadata.clone()
            } else {
                next.metadata.clone()
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    pub from: Pos,
    pub to: Pos,
    #[serde(default)]
    pub blocks: Vec<BlockSnapshot>,
    #[serde(default)]
    pub entities: Vec<EntitySnapshot>,
    #[serde(default)]
    pub tick: u64,
}

impl WorldSnapshot {
    /// Normalize Horizon's `blocks` reply (`[x,y,z,state]`) into the stream format.
    pub fn from_control(reply: &Value) -> Result<Self, String> {
        let from = pos(&reply["from"]).ok_or("world reply without from")?;
        let to = pos(&reply["to"]).ok_or("world reply without to")?;
        let mut blocks = Vec::new();
        for block in reply["blocks"]
            .as_array()
            .ok_or("world reply without blocks")?
        {
            let p = block.as_array().ok_or("world block is not an array")?;
            let pos = [
                p.first()
                    .and_then(Value::as_i64)
                    .ok_or("world block without x")? as i32,
                p.get(1)
                    .and_then(Value::as_i64)
                    .ok_or("world block without y")? as i32,
                p.get(2)
                    .and_then(Value::as_i64)
                    .ok_or("world block without z")? as i32,
            ];
            let state = p
                .get(3)
                .and_then(Value::as_str)
                .ok_or("world block without state")?
                .to_owned();
            blocks.push(BlockSnapshot { pos, state });
        }
        Ok(Self {
            from,
            to,
            blocks,
            entities: Vec::new(),
            tick: reply["tick"].as_u64().unwrap_or_default(),
        })
    }

    pub fn with_entities(mut self, entities: Vec<EntitySnapshot>) -> Self {
        self.entities = entities;
        self
    }

    /// Return only blocks on a Y layer, preserving the source ordering for painter's algorithms.
    pub fn layer(&self, y: i32) -> impl Iterator<Item = &BlockSnapshot> {
        self.blocks.iter().filter(move |block| block.pos[1] == y)
    }

    /// Restrict entities to a horizontal radius around a point.
    pub fn entities_near(
        &self,
        centre: Vec3,
        radius: f64,
    ) -> impl Iterator<Item = &EntitySnapshot> {
        let radius_sq = radius.max(0.0).powi(2);
        self.entities.iter().filter(move |entity| {
            let dx = entity.pos[0] - centre[0];
            let dz = entity.pos[2] - centre[2];
            dx * dx + dz * dz <= radius_sq
        })
    }

    /// Merge entity observations into a newer world snapshot by ID, retaining unidentifiable
    /// entities from the newer frame and interpolating matching IDs when requested.
    pub fn interpolate_entities(&self, next: &Self, t: f64) -> Self {
        let mut entities = Vec::with_capacity(next.entities.len());
        for current in &next.entities {
            let Some(id) = current.id else {
                entities.push(current.clone());
                continue;
            };
            if let Some(previous) = self.entities.iter().find(|entity| entity.id == Some(id)) {
                entities.push(previous.interpolate(current, t));
            } else {
                entities.push(current.clone());
            }
        }
        Self {
            from: next.from,
            to: next.to,
            blocks: next.blocks.clone(),
            entities,
            tick: next.tick,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub target: Vec3,
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            target: [0.0, 64.0, 0.0],
            yaw: 45.0,
            pitch: 35.0,
            distance: 32.0,
        }
    }
}

impl Camera {
    pub fn orbit(&mut self, yaw: f64, pitch: f64) {
        self.yaw = (self.yaw + yaw).rem_euclid(360.0);
        self.pitch = (self.pitch + pitch).clamp(-89.0, 89.0);
    }

    pub fn zoom(&mut self, amount: f64) {
        self.distance = (self.distance * (1.0 - amount * 0.1)).clamp(2.0, 512.0);
    }

    /// Project a world point into camera-relative screen coordinates. The browser can scale and
    /// translate these coordinates to its canvas without needing to duplicate camera math.
    pub fn project(&self, point: Vec3) -> [f64; 2] {
        let dx = point[0] - self.target[0];
        let dy = point[1] - self.target[1];
        let dz = point[2] - self.target[2];
        let yaw = self.yaw.to_radians();
        let pitch = self.pitch.to_radians();
        let x = dx * yaw.cos() - dz * yaw.sin();
        let depth = dx * yaw.sin() + dz * yaw.cos();
        [x, dy * pitch.cos() - depth * pitch.sin()]
    }
}

fn pos(value: &Value) -> Option<Pos> {
    let values = value.as_array()?;
    Some([
        values.first()?.as_i64()? as i32,
        values.get(1)?.as_i64()? as i32,
        values.get(2)?.as_i64()? as i32,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_blocks_and_filters_layers() {
        let world = WorldSnapshot::from_control(&serde_json::json!({
            "from": [0, 60, 0], "to": [2, 62, 2],
            "blocks": [[0, 60, 0, "minecraft:stone"], [1, 61, 1, "minecraft:glass"]]
        }))
        .unwrap();
        assert_eq!(world.layer(61).count(), 1);
        assert_eq!(world.blocks[0].state, "minecraft:stone");
    }

    #[test]
    fn interpolation_is_clamped_and_keeps_identity() {
        let a = EntitySnapshot {
            id: Some(7),
            kind: "pig".into(),
            pos: [0.0, 0.0, 0.0],
            yaw: 0.0,
            pitch: 0.0,
            velocity: [0.0; 3],
            name: None,
            metadata: Value::Null,
        };
        let b = EntitySnapshot {
            id: Some(7),
            kind: "pig".into(),
            pos: [10.0, 2.0, -4.0],
            yaw: 90.0,
            pitch: 10.0,
            velocity: [2.0, 0.0, 1.0],
            name: Some("Dinnerbone".into()),
            metadata: serde_json::json!({"baby":true}),
        };
        let mid = a.interpolate(&b, 0.5);
        assert_eq!(mid.id, Some(7));
        assert_eq!(mid.pos, [5.0, 1.0, -2.0]);
        assert_eq!(a.interpolate(&b, 2.0).pos, b.pos);
    }

    #[test]
    fn camera_projection_is_deterministic() {
        let camera = Camera::default();
        assert_eq!(camera.project(camera.target), [0.0, 0.0]);
        let projected = camera.project([1.0, 64.0, 0.0]);
        assert!((projected[0] - 2.0_f64.sqrt() / 2.0).abs() < 1e-12);
        assert!((projected[1] + 2.0_f64.sqrt() / 2.0 * 35.0_f64.to_radians().sin()).abs() < 1e-12);
    }
}
