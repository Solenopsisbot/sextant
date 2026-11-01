//! A bot over its control socket: reading the world, and walking a planned path.
//!
//! The socket speaks text lines in and one JSON line out per command (Horizon's
//! `horizon-client/src/control.rs`). Commands run on the game thread between frames, so each
//! round trip is about a frame; lines written together are answered in one frame, so a control
//! update (read the state, turn, press keys) is sent as one batch.

use crate::planner::{self, Goal, Kind, Path, Settings, Step};
use crate::style::{Rng, Style};
use crate::world::{Pos, Region, cube_of};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// A game tick: the pace of the control loop.
const TICK: Duration = Duration::from_millis(50);
/// The world's height range (26.3's overworld), for clipping block reads.
const MIN_Y: i32 = -64;
const MAX_Y: i32 = 319;

pub struct Bot {
    out: UnixStream,
    lines: BufReader<UnixStream>,
}

/// Where the bot is and how it looks, from `state`.
#[derive(Clone, Copy, Debug)]
pub struct Me {
    pub pos: [f64; 3],
    pub feet: Pos,
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl Bot {
    pub fn connect(socket: &std::path::Path) -> Result<Self, String> {
        let out = UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
        let lines = BufReader::new(out.try_clone().map_err(|e| e.to_string())?);
        Ok(Self { out, lines })
    }

    /// Sends `cmds` together and returns their replies in order; an error reply is an error.
    pub fn batch(&mut self, cmds: &[String]) -> Result<Vec<Value>, String> {
        // An empty line would get a reply nobody reads, and every reply after it would be
        // matched to the wrong command.
        if cmds.is_empty() {
            return Ok(Vec::new());
        }
        let mut text = cmds.join("\n");
        text.push('\n');
        self.out
            .write_all(text.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut replies = Vec::with_capacity(cmds.len());
        for cmd in cmds {
            let mut line = String::new();
            self.lines.read_line(&mut line).map_err(|e| e.to_string())?;
            let v: Value =
                serde_json::from_str(&line).map_err(|e| format!("{cmd}: {e}: {line}"))?;
            if let Some(e) = v.get("error").and_then(Value::as_str) {
                return Err(format!("{cmd}: {e}"));
            }
            replies.push(v);
        }
        Ok(replies)
    }

    pub fn cmd(&mut self, cmd: &str) -> Result<Value, String> {
        Ok(self.batch(&[cmd.to_owned()])?.remove(0))
    }

    pub fn me(&mut self) -> Result<Me, String> {
        parse_me(&self.cmd("state")?)
    }

    /// Reads every 32-block cube within `radius` blocks (horizontally) of `centre` that the
    /// region doesn't have yet, 32 blocks above and below its height. The reads go out as one
    /// batch, answered within a frame or two rather than a frame each.
    pub fn load_around(
        &mut self,
        region: &mut Region,
        centre: Pos,
        radius: i32,
    ) -> Result<usize, String> {
        let (lo, hi) = (
            cube_of([
                centre[0] - radius,
                (centre[1] - 32).max(MIN_Y),
                centre[2] - radius,
            ]),
            cube_of([
                centre[0] + radius,
                (centre[1] + 32).min(MAX_Y),
                centre[2] + radius,
            ]),
        );
        let mut cmds = Vec::new();
        for x in (lo.0[0]..=hi.0[0]).step_by(32) {
            for y in (lo.0[1]..=hi.0[1]).step_by(32) {
                for z in (lo.0[2]..=hi.0[2]).step_by(32) {
                    if !region.has_cube([x, y, z]) {
                        let (a, b) = cube_of([x, y, z]);
                        cmds.push(format!(
                            "blocks {} {} {} {} {} {}",
                            a[0], a[1], a[2], b[0], b[1], b[2]
                        ));
                    }
                }
            }
        }
        for reply in self.batch(&cmds)? {
            region.add_reply(&reply)?;
        }
        Ok(cmds.len())
    }
}

pub fn parse_me(state: &Value) -> Result<Me, String> {
    let p = &state["player"];
    let num = |v: &Value| {
        v.as_f64()
            .ok_or("state without a player (is it in a world?)")
    };
    let pos = [
        num(&p["position"][0])?,
        num(&p["position"][1])?,
        num(&p["position"][2])?,
    ];
    Ok(Me {
        pos,
        feet: [
            pos[0].floor() as i32,
            (pos[1] + 0.001).floor() as i32,
            pos[2].floor() as i32,
        ],
        yaw: num(&p["yaw"])? as f32,
        pitch: num(&p["pitch"])? as f32,
        on_ground: p["on_ground"].as_bool().unwrap_or(true),
    })
}

/// Minecraft's yaw toward (dx, dz): 0 faces +Z, 90 faces -X.
pub fn yaw_toward(dx: f64, dz: f64) -> f32 {
    (-dx).atan2(dz).to_degrees() as f32
}

/// `a - b` as the shortest turn, in (-180, 180].
pub fn wrap(d: f32) -> f32 {
    let d = (d + 180.0).rem_euclid(360.0) - 180.0;
    if d == -180.0 { 180.0 } else { d }
}

/// Walks the bot along a path in `style`; leaves its movement keys held for the next path.
pub struct Walker<'a> {
    pub bot: &'a mut Bot,
    pub style: &'a Style,
    pub rng: Rng,
    /// Extra degrees still to be corrected after an overshoot.
    overshoot: f32,
}

impl<'a> Walker<'a> {
    pub fn new(bot: &'a mut Bot, style: &'a Style, seed: u64) -> Self {
        Self {
            bot,
            style,
            rng: Rng::new(seed),
            overshoot: 0.0,
        }
    }

    fn wait_ticks(&mut self, n: u32) {
        std::thread::sleep(TICK * n);
    }

    /// One path, step by step. An error means the world changed under it: replan.
    pub fn walk(&mut self, path: &Path) -> Result<(), String> {
        let mut heading: Option<f32> = None;
        let mut me = self.bot.me()?;
        for (i, step) in path.steps.iter().enumerate() {
            let last = i + 1 == path.steps.len();
            // A person reacts where the route turns, not at every block of a straight line.
            let dir = yaw_toward(
                f64::from(step.to[0] - me.feet[0]),
                f64::from(step.to[2] - me.feet[2]),
            );
            if heading.is_none_or(|h| wrap(dir - h).abs() > 30.0) {
                self.react()?;
            }
            heading = Some(dir);
            self.maybe_pause()?;
            for m in &step.mine {
                self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
                self.bot.cmd(&format!("break {} {} {}", m[0], m[1], m[2]))?;
            }
            match step.kind {
                Kind::Pillar => self.pillar(step)?,
                Kind::MineDown => self.wait_until_at(step, 30)?,
                _ => {
                    if let Some(p) = step.place {
                        self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
                        self.bot.cmd(&format!("place {} {} {}", p[0], p[1], p[2]))?;
                    }
                    me = self.move_to(step, last)?;
                    continue;
                }
            }
            me = self.bot.me()?;
        }
        self.bot.batch(&[
            "key w up".into(),
            "key ctrl up".into(),
            "key space up".into(),
        ])?;
        Ok(())
    }

    fn react(&mut self) -> Result<(), String> {
        let react = self.rng.between(self.style.reaction);
        if react > 0 {
            self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
            self.wait_ticks(react);
        }
        Ok(())
    }

    /// Now and then, a short stop with a look around.
    fn maybe_pause(&mut self) -> Result<(), String> {
        if !self.rng.chance(self.style.pause_chance) {
            return Ok(());
        }
        self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
        let me = self.bot.me()?;
        if self.rng.chance(self.style.glance_chance * 4.0) {
            let away = me.yaw + self.rng.spread(70.0);
            self.bot
                .cmd(&format!("look {away} {}", self.rng.spread(20.0)))?;
        }
        let pause = self.rng.between(self.style.pause);
        self.wait_ticks(pause);
        self.bot.cmd(&format!("look {} {}", me.yaw, me.pitch))?;
        Ok(())
    }

    /// Steer toward the step's block, holding forward (and jump to step up). An intermediate
    /// step is done once the feet are in its block: the bot keeps moving into the next one
    /// rather than stopping on every block's centre. The last step ends near its centre.
    /// Each tick is one round trip: the controls and the `state` that follows them.
    fn move_to(&mut self, step: &Step, last: bool) -> Result<Me, String> {
        let wobble = if last { 0.0 } else { self.style.wobble };
        let target = [
            f64::from(step.to[0]) + 0.5 + f64::from(self.rng.spread(wobble)),
            f64::from(step.to[1]),
            f64::from(step.to[2]) + 0.5 + f64::from(self.rng.spread(wobble)),
        ];
        let limit = match step.kind {
            Kind::Fall(n) => 40 + 4 * n as u32,
            _ => 40,
        };
        let mut me = self.bot.me()?;
        // The step's direction, from where it starts: progress is measured along it.
        let (sx, sz) = (target[0] - me.pos[0], target[2] - me.pos[2]);
        let len = (sx * sx + sz * sz).sqrt().max(1e-6);
        let dir = (sx / len, sz / len);
        for _ in 0..limit {
            let (dx, dz) = (target[0] - me.pos[0], target[2] - me.pos[2]);
            let flat = (dx * dx + dz * dz).sqrt();
            let landed = !matches!(step.kind, Kind::Fall(_) | Kind::Ascend) || me.on_ground;
            // How far short of the target along the step (negative once past it), and how far
            // to its side.
            let short = dx * dir.0 + dz * dir.1;
            let side = (dx * dir.1 - dz * dir.0).abs();
            let arrived = if last {
                flat < 0.35 && me.feet[1] == step.to[1] && landed
            } else {
                // Passed the block's centre on its level, not far off to the side: done, and
                // the bot carries on rather than turning back for a block it's already past.
                me.feet[1] == step.to[1]
                    && landed
                    && (me.feet == step.to || (short < 0.3 && side < 0.9))
            };
            if arrived {
                return Ok(me);
            }
            let mut want = yaw_toward(dx, dz);
            let turn = wrap(want - me.yaw);
            if turn.abs() > 30.0
                && self.overshoot == 0.0
                && self.rng.chance(self.style.overshoot_chance)
            {
                self.overshoot = self.style.overshoot * turn.signum();
            }
            want += self.overshoot;
            let remaining = wrap(want - me.yaw);
            if self.overshoot != 0.0 && remaining.abs() < 2.0 {
                self.overshoot = 0.0;
            }
            // Wrapped: a yaw that only ever adds up turns into thousands of degrees.
            let yaw = wrap(
                me.yaw
                    + self.style.turn_step(remaining)
                    + self.rng.spread(self.style.aim_jitter * 0.2),
            );
            let pitch = self.style.walk_pitch + self.rng.spread(self.style.aim_jitter * 0.2);
            // Forward only once facing the block: a person doesn't walk sideways. Close to it the
            // aim must be tighter, or a turn slower than the walk circles the block forever.
            let allowed = (flat as f32 * 40.0).clamp(10.0, 50.0);
            let facing = wrap(want - yaw).abs() < allowed;
            // Coast into the last block rather than sprint past it.
            let sprint =
                self.style.sprint && facing && !(last && flat < 2.5) && step.kind != Kind::Ascend;
            let forward = facing && !(last && flat < 0.2);
            let jump = step.kind == Kind::Ascend && flat < 1.4;
            let replies = self.bot.batch(&[
                format!("look {yaw} {pitch}"),
                format!("key w {}", if forward { "down" } else { "up" }),
                format!("key ctrl {}", if sprint { "down" } else { "up" }),
                format!("key space {}", if jump { "down" } else { "up" }),
                "state".into(),
            ])?;
            me = parse_me(&replies[4])?;
        }
        Err(format!("didn't reach {:?} ({:?})", step.to, step.kind))
    }

    /// Jump and place a block under the feet at the top of the jump.
    fn pillar(&mut self, step: &Step) -> Result<(), String> {
        let place = step.place.ok_or("a pillar step without a block to place")?;
        self.bot.batch(&[
            "key w up".into(),
            "key ctrl up".into(),
            "look 0 90".into(),
            "key space down".into(),
        ])?;
        for _ in 0..12 {
            let me = self.bot.me()?;
            if me.pos[1] >= f64::from(place[1]) + 1.05 {
                self.bot.cmd("key space up")?;
                self.bot
                    .cmd(&format!("place {} {} {}", place[0], place[1], place[2]))?;
                return self.wait_until_at(step, 20);
            }
        }
        self.bot.cmd("key space up")?;
        Err("the jump didn't get high enough to place under".into())
    }

    fn wait_until_at(&mut self, step: &Step, ticks: u32) -> Result<(), String> {
        for _ in 0..ticks {
            let me = self.bot.me()?;
            if me.feet == step.to && me.on_ground {
                return Ok(());
            }
        }
        Err(format!("didn't land at {:?}", step.to))
    }
}

/// Reads the world around the bot, plans to `goal`, walks, and repeats until there: long
/// trips go in segments, each to the edge of what has been read.
pub fn go(
    bot: &mut Bot,
    goal: Goal,
    settings: &Settings,
    style: &Style,
    seed: u64,
    mut log: impl FnMut(&str),
) -> Result<(), String> {
    let mut region = Region::new();
    let mut walker_seed = seed;
    let mut stuck_at: Option<Pos> = None;
    for attempt in 0..64 {
        let me = bot.me()?;
        let toward = [
            me.feet[0] + (goal.pos[0] - me.feet[0]).clamp(-48, 48),
            me.feet[1],
            me.feet[2] + (goal.pos[2] - me.feet[2]).clamp(-48, 48),
        ];
        let read = bot.load_around(&mut region, me.feet, 40)?
            + bot.load_around(&mut region, toward, 40)?;
        let path = planner::plan(&region, me.feet, goal, settings);
        log(&format!(
            "segment {attempt}: from {:?}, {} steps, {} ({} nodes, {read} cubes read)",
            me.feet,
            path.steps.len(),
            if path.complete {
                "reaches the goal"
            } else {
                "partial"
            },
            path.explored
        ));
        if path.steps.is_empty() {
            bot.cmd("release")?;
            return if path.complete {
                Ok(())
            } else {
                Err("no way forward from here".into())
            };
        }
        walker_seed = walker_seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut walker = Walker::new(bot, style, walker_seed);
        match walker.walk(&path) {
            Ok(()) if path.complete => {
                bot.cmd("release")?;
                return Ok(());
            }
            Ok(()) => {}
            Err(e) => {
                log(&format!("replanning: {e}"));
                // Stuck twice where it stands: what it walked into may have changed, read it
                // again. Otherwise the world it has is still good.
                let here = bot.me()?.feet;
                if stuck_at == Some(here) {
                    region = Region::new();
                }
                stuck_at = Some(here);
            }
        }
    }
    bot.cmd("release")?;
    Err("gave up after 64 segments".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaw_follows_minecraft() {
        assert_eq!(yaw_toward(0.0, 1.0), 0.0); // +Z: south
        assert!((yaw_toward(-1.0, 0.0) - 90.0).abs() < 1e-4); // -X: west
        assert!((yaw_toward(1.0, 0.0) + 90.0).abs() < 1e-4); // +X: east
        assert_eq!(wrap(350.0), -10.0);
        assert_eq!(wrap(-190.0), 170.0);
    }

    #[test]
    fn state_reads_feet_from_the_position() {
        let state = serde_json::json!({"player": {"position": [-3.5, 64.0, 10.2], "yaw": 12.0, "pitch": 5.0, "on_ground": true}});
        let me = parse_me(&state).unwrap();
        assert_eq!(me.feet, [-4, 64, 10]);
        assert!(parse_me(&serde_json::json!({})).is_err());
    }
}
