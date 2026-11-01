//! How a bot moves along its path: from obviously a bot to like a player.
//!
//! A style is a set of behaviours, each one a knob: how fast it turns and whether it eases
//! into a turn, how long it takes to react, whether it overshoots a turn and corrects, how
//! precisely it walks the centre of each block, whether it pauses or glances around. Four
//! presets cover the range; a TOML table overrides any knob (`Style::with_overrides`).
//!
//! This shapes how the movement looks. It isn't tuned against any anticheat's checks.

use serde::Deserialize;

/// The knobs. Angles in degrees, times in game ticks (1/20 s).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct Style {
    pub name: String,
    /// The fastest turn per tick; 0 snaps straight to the target (a bot's tell).
    pub turn_speed: f32,
    /// Ease turns: fast at first, slowing into the target, as a hand on a mouse does.
    pub ease: bool,
    /// Random error added to each aim, in degrees.
    pub aim_jitter: f32,
    /// Chance per turn of overshooting by `overshoot` degrees, then correcting back.
    pub overshoot_chance: f32,
    pub overshoot: f32,
    /// Ticks between deciding to move and moving, chosen per step between these.
    pub reaction: (u32, u32),
    /// How far from each block's centre the walk may wander, in blocks (0 walks the centre).
    pub wobble: f32,
    /// Sprint where the path allows it.
    pub sprint: bool,
    /// Chance per step of a short stop, and how long (ticks).
    pub pause_chance: f32,
    pub pause: (u32, u32),
    /// Chance per step of glancing away from the path and back.
    pub glance_chance: f32,
    /// Pitch while walking: looking at the ground ahead, in degrees (positive is down).
    pub walk_pitch: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self::casual()
    }
}

impl Style {
    /// Snaps, never hesitates, walks every block's exact centre: unmistakably a bot.
    pub fn robot() -> Self {
        Self {
            name: "robot".into(),
            turn_speed: 0.0,
            ease: false,
            aim_jitter: 0.0,
            overshoot_chance: 0.0,
            overshoot: 0.0,
            reaction: (0, 0),
            wobble: 0.0,
            sprint: true,
            pause_chance: 0.0,
            pause: (0, 0),
            glance_chance: 0.0,
            walk_pitch: 0.0,
        }
    }
    /// Smooth and quick, but precise: a bot that is easy to watch.
    pub fn tidy() -> Self {
        Self {
            name: "tidy".into(),
            turn_speed: 45.0,
            ease: true,
            aim_jitter: 0.5,
            reaction: (1, 2),
            wobble: 0.05,
            walk_pitch: 10.0,
            ..Self::robot()
        }
    }
    /// A relaxed player: some delay, some wander, the odd pause or look around.
    pub fn casual() -> Self {
        Self {
            name: "casual".into(),
            turn_speed: 25.0,
            ease: true,
            aim_jitter: 2.0,
            overshoot_chance: 0.1,
            overshoot: 6.0,
            reaction: (2, 6),
            wobble: 0.2,
            sprint: true,
            pause_chance: 0.02,
            pause: (5, 20),
            glance_chance: 0.04,
            walk_pitch: 15.0,
        }
    }
    /// Closest to a person: reaction times that vary, overshoot and correct, wander, pauses.
    pub fn player_like() -> Self {
        Self {
            name: "player-like".into(),
            turn_speed: 18.0,
            ease: true,
            aim_jitter: 3.5,
            overshoot_chance: 0.25,
            overshoot: 9.0,
            reaction: (3, 10),
            wobble: 0.3,
            sprint: true,
            pause_chance: 0.05,
            pause: (8, 40),
            glance_chance: 0.1,
            walk_pitch: 20.0,
        }
    }

    /// A preset by name: robot, tidy, casual, player-like.
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "robot" => Some(Self::robot()),
            "tidy" => Some(Self::tidy()),
            "casual" => Some(Self::casual()),
            "player-like" | "player_like" | "human" => Some(Self::player_like()),
            _ => None,
        }
    }

    /// The preset `table.preset` (default casual) with the table's other keys over it.
    pub fn with_overrides(table: &toml::Table) -> Result<Self, String> {
        let base_name = table
            .get("preset")
            .and_then(|v| v.as_str())
            .unwrap_or("casual");
        let base = Self::preset(base_name).ok_or_else(|| format!("no style preset {base_name}"))?;
        let mut merged = toml::Table::try_from(&base).map_err(|e| e.to_string())?;
        for (k, v) in table {
            if k != "preset" {
                merged.insert(k.clone(), v.clone());
            }
        }
        merged
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string())
    }

    /// The yaw change to make this tick toward a target `remaining` degrees away.
    pub fn turn_step(&self, remaining: f32) -> f32 {
        if self.turn_speed <= 0.0 {
            return remaining;
        }
        let limit = if self.ease {
            // Fast while far, slowing as it closes in: half the distance a tick, capped.
            (remaining.abs() * 0.5).clamp(1.0, self.turn_speed)
        } else {
            self.turn_speed
        };
        remaining.clamp(-limit, limit)
    }
}

impl serde::Serialize for Style {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("Style", 13)?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("turn_speed", &self.turn_speed)?;
        st.serialize_field("ease", &self.ease)?;
        st.serialize_field("aim_jitter", &self.aim_jitter)?;
        st.serialize_field("overshoot_chance", &self.overshoot_chance)?;
        st.serialize_field("overshoot", &self.overshoot)?;
        st.serialize_field("reaction", &self.reaction)?;
        st.serialize_field("wobble", &self.wobble)?;
        st.serialize_field("sprint", &self.sprint)?;
        st.serialize_field("pause_chance", &self.pause_chance)?;
        st.serialize_field("pause", &self.pause)?;
        st.serialize_field("glance_chance", &self.glance_chance)?;
        st.serialize_field("walk_pitch", &self.walk_pitch)?;
        st.end()
    }
}

/// A small, seeded random source (xorshift64*): styles must be reproducible from a seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
    /// Uniform in [-a, a].
    pub fn spread(&mut self, a: f32) -> f32 {
        (self.unit() * 2.0 - 1.0) * a
    }
    pub fn between(&mut self, (lo, hi): (u32, u32)) -> u32 {
        if hi <= lo {
            lo
        } else {
            lo + (self.next_u64() % u64::from(hi - lo + 1)) as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_run_from_bot_to_player() {
        let (robot, human) = (Style::robot(), Style::player_like());
        assert_eq!(robot.turn_step(90.0), 90.0, "a robot snaps");
        assert!(
            human.turn_step(90.0) < 90.0,
            "a player turns over several ticks"
        );
        assert!(human.reaction.1 > robot.reaction.1);
        assert!(Style::preset("human").is_some() && Style::preset("nope").is_none());
    }

    #[test]
    fn a_table_overrides_any_knob_of_its_preset() {
        let table: toml::Table =
            toml::from_str("preset = \"robot\"\nwobble = 0.4\nreaction = [5, 9]").unwrap();
        let s = Style::with_overrides(&table).unwrap();
        assert_eq!(s.turn_speed, 0.0);
        assert_eq!(s.wobble, 0.4);
        assert_eq!(s.reaction, (5, 9));
        let bad: toml::Table = toml::from_str("turn_sped = 3").unwrap();
        assert!(
            Style::with_overrides(&bad).is_err(),
            "a typo is an error, not ignored"
        );
    }

    #[test]
    fn eased_turns_slow_into_the_target() {
        let s = Style::casual();
        let far = s.turn_step(100.0);
        let near = s.turn_step(6.0);
        assert!(far > near && near <= 6.0);
    }

    #[test]
    fn the_random_source_is_reproducible() {
        let (mut a, mut b) = (Rng::new(42), Rng::new(42));
        assert_eq!((a.next_u64(), a.unit()), (b.next_u64(), b.unit()));
        let mut r = Rng::new(7);
        assert!((0..1000).all(|_| (0.0..1.0).contains(&r.unit())));
        assert!((0..100).all(|_| (3..=10).contains(&r.between((3, 10)))));
    }
}
