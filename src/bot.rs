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
    /// Blocks per tick.
    pub velocity: [f64; 3],
    /// Dead (its death screen is open): nothing it is told to do happens.
    pub dead: bool,
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

/// The bot's state; an error once it has died, so no walk carries on for a dead bot.
pub fn parse_me(state: &Value) -> Result<Me, String> {
    let me = read_me(state)?;
    if me.dead {
        return Err(format!("{DIED} at {:.1?}", me.pos));
    }
    Ok(me)
}

/// What a dead bot's errors start with: `go` stops at once instead of replanning.
pub const DIED: &str = "the bot died";

fn read_me(state: &Value) -> Result<Me, String> {
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
        velocity: [0, 1, 2].map(|i| p["velocity"][i].as_f64().unwrap_or(0.0)),
        dead: state["screen"].as_str() == Some("dead")
            || p["health"].as_f64().is_some_and(|h| h <= 0.0),
    })
}

/// How far along segment a -> b the point p is: 0 at a, 1 at b.
fn progress(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    let (vx, vz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = (vx * vx + vz * vz).max(1e-9);
    ((p[0] - a[0]) * vx + (p[1] - a[1]) * vz) / len2
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The point `d` blocks further along the line from `from`, on segment `seg` (ending at
/// `line[seg]`); the line's end if it is nearer.
fn ahead(line: &[[f64; 2]], seg: usize, from: [f64; 2], d: f64) -> [f64; 2] {
    let (mut p, mut left, mut i) = (from, d, seg);
    while i < line.len() {
        let next = line[i];
        let l = dist(p, next);
        if l >= left {
            let t = left / l.max(1e-9);
            return [p[0] + (next[0] - p[0]) * t, p[1] + (next[1] - p[1]) * t];
        }
        left -= l;
        p = next;
        i += 1;
    }
    p
}

/// `p` moved `by` blocks sideways, square to segment a -> b.
fn offset(p: [f64; 2], a: [f64; 2], b: [f64; 2], by: f64) -> [f64; 2] {
    let (vx, vz) = (b[0] - a[0], b[1] - a[1]);
    let len = (vx * vx + vz * vz).sqrt().max(1e-9);
    [p[0] - vz / len * by, p[1] + vx / len * by]
}

/// A turn sharper than 45 degrees at a block centre within `d` blocks ahead along the line.
fn sharp_turn_within(line: &[[f64; 2]], seg: usize, at: [f64; 2], d: f64) -> bool {
    let mut travelled = dist(at, line[seg]);
    for i in seg..line.len().saturating_sub(1) {
        if travelled > d {
            return false;
        }
        if i >= 1 {
            let (a, b, c) = (line[i - 1], line[i], line[i + 1]);
            let h1 = yaw_toward(b[0] - a[0], b[1] - a[1]);
            let h2 = yaw_toward(c[0] - b[0], c[1] - b[1]);
            if wrap(h2 - h1).abs() > 45.0 {
                return true;
            }
        }
        travelled += dist(line[i], line[i + 1]);
    }
    false
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

/// How a run of walking ends.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RunEnd {
    /// On the last block's centre, sneaking for the last stretch: Minecraft doesn't let a
    /// sneaking player walk off an edge, so however late "stop" arrives, it can't slide into
    /// a gap or a drop next to the block.
    Stop,
    /// Straight on off the last block's edge into a planned drop, landing at this block.
    IntoFall(Pos),
}

/// Walks the bot along a path in `style`; leaves its movement keys held for the next path.
pub struct Walker<'a> {
    pub bot: &'a mut Bot,
    pub style: &'a Style,
    pub rng: Rng,
    /// Extra degrees still to be corrected after an overshoot.
    overshoot: f32,
    /// Print each step as it starts.
    pub verbose: bool,
    /// The hotbar item placed for pillars and bridges.
    pub block: String,
    /// The hotbar item selected before mining, if any (else what it holds).
    pub tool: Option<String>,
}

impl<'a> Walker<'a> {
    pub fn new(bot: &'a mut Bot, style: &'a Style, seed: u64) -> Self {
        Self {
            bot,
            style,
            rng: Rng::new(seed),
            overshoot: 0.0,
            verbose: false,
            block: "cobblestone".into(),
            tool: None,
        }
    }

    fn wait_ticks(&mut self, n: u32) {
        std::thread::sleep(TICK * n);
    }

    /// One path. Runs of plain walking (Walk and Diagonal steps with nothing to mine or
    /// place) are followed as one line (`follow`); every other step on its own. An error
    /// means the world isn't what the plan assumed: replan.
    pub fn walk(&mut self, path: &Path) -> Result<(), String> {
        let plain = |s: &Step| {
            matches!(s.kind, Kind::Walk | Kind::Diagonal) && s.mine.is_empty() && s.place.is_none()
        };
        let mut i = 0;
        self.react()?;
        while i < path.steps.len() {
            if plain(&path.steps[i]) {
                let mut end = i;
                while end + 1 < path.steps.len() && plain(&path.steps[end + 1]) {
                    end += 1;
                }
                // How the run ends: walking straight off into a planned drop, or stopping on
                // the last block (the path ends, or a step up, mining, a pillar or a bridge
                // starts there).
                let ending = match path.steps.get(end + 1) {
                    Some(next) if matches!(next.kind, Kind::Fall(_)) && next.mine.is_empty() => {
                        RunEnd::IntoFall(next.to)
                    }
                    _ => RunEnd::Stop,
                };
                self.follow(&path.steps[i..=end], ending)?;
                i = end + 1;
                continue;
            }
            let step = &path.steps[i];
            let me = self.bot.me()?;
            if self.verbose {
                eprintln!(
                    "  step {i}: {:?} to {:?} from {:?}, mine {:?} place {:?}",
                    step.kind, step.to, me.feet, step.mine, step.place
                );
            }
            self.maybe_pause()?;
            if !step.mine.is_empty() {
                self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
                if let Some(tool) = &self.tool {
                    self.bot.cmd(&format!("select {tool}"))?;
                }
            }
            for m in &step.mine {
                self.bot.cmd(&format!("break {} {} {}", m[0], m[1], m[2]))?;
            }
            match step.kind {
                Kind::Pillar => self.pillar(step)?,
                Kind::MineDown => self.wait_until_at(step, 30)?,
                _ => {
                    if let Some(p) = step.place {
                        self.bot.batch(&["key w up".into(), "key ctrl up".into()])?;
                        self.bot.cmd(&format!(
                            "place {} {} {} --item {}",
                            p[0], p[1], p[2], self.block
                        ))?;
                    }
                    self.move_to(step)?;
                }
            }
            // A path that ends with a special step (a step up, a drop) ends on its block's
            // centre, standing still, like one that ends walking.
            if i + 1 == path.steps.len() && !plain(step) && step.kind != Kind::Pillar {
                self.centre_on(step.to)?;
            }
            i += 1;
        }
        self.bot.batch(&[
            "key w up".into(),
            "key ctrl up".into(),
            "key space up".into(),
            "key shift up".into(),
        ])?;
        Ok(())
    }

    /// Pure pursuit along a run of walking steps, all on one level: the bot steers at a point
    /// `LOOKAHEAD` blocks ahead of it on the line through the blocks' centres, so it corrects
    /// any drift back onto the line and cuts a corner by a fraction of a block, inside the
    /// corner's own block. Off the line by more than `ASTRAY` it heads back to the line first.
    /// No sprinting within `SLOW` blocks of a sharp turn or of the end; how the run ends is
    /// `ending` (sneaking onto the last block, or on off its edge into a planned drop).
    fn follow(&mut self, run: &[Step], ending: RunEnd) -> Result<(), String> {
        /// Sneak for the last stretch of a stopping run (a sneaking player moves 0.3 times as
        /// fast and can't walk off an edge).
        const SNEAK: f64 = 0.9;
        let precise = ending == RunEnd::Stop;
        const LOOKAHEAD: f64 = 0.9;
        const ASTRAY: f64 = 0.4;
        const SLOW: f64 = 2.5;
        let level = run[0].to[1];
        let mut me = self.bot.me()?;
        // The line: from where the bot stands, through each block's centre.
        let mut line = vec![[me.pos[0], me.pos[2]]];
        line.extend(
            run.iter()
                .map(|s| [f64::from(s.to[0]) + 0.5, f64::from(s.to[2]) + 0.5]),
        );
        // Into a drop: the line goes on to the landing, so the bot aims past the edge and
        // walks off it, rather than turning back to the last block's centre.
        if let RunEnd::IntoFall(landing) = ending {
            line.push([f64::from(landing[0]) + 0.5, f64::from(landing[2]) + 0.5]);
        }
        if self.verbose {
            eprintln!(
                "  follow {} blocks from {:?} to {:?}{}",
                run.len(),
                me.feet,
                run[run.len() - 1].to,
                if precise { ", stopping there" } else { "" }
            );
        }
        // The wander: a sideways offset from the line that drifts slowly, kept within the
        // block (a player's box hangs over a neighbour only past 0.3).
        let wander_max = f64::from(self.style.wobble).min(0.25);
        let mut wander = 0.0_f64;
        let mut seg = 1;
        let limit = 40 + 20 * run.len() as u32;
        for _ in 0..limit {
            // Dropped into the planned drop at the run's end: that's the next step done early.
            if let RunEnd::IntoFall(landing) = ending
                && me.on_ground
                && me.feet == landing
            {
                return Ok(());
            }
            if me.on_ground && me.feet[1] != level && !matches!(ending, RunEnd::IntoFall(_)) {
                return Err(format!(
                    "left the route's level at {:.2?} (feet {:?}, route at y {level})",
                    me.pos, me.feet
                ));
            }
            let at = [me.pos[0], me.pos[2]];
            // Advance past segments whose end the bot has passed; now and then a player stops
            // for a moment as it reaches a block.
            let reached = seg;
            while seg + 1 < line.len() && progress(line[seg - 1], line[seg], at) >= 1.0 {
                seg += 1;
            }
            if seg != reached {
                self.maybe_pause()?;
            }
            let end = line[line.len() - 1];
            let to_end = dist(at, end);
            let moving = (me.velocity[0] * me.velocity[0] + me.velocity[2] * me.velocity[2]).sqrt();
            let done = if precise {
                // Near the centre and still: momentum mustn't carry it on past the centre
                // after it counts as stopped (it steers back, sneaking, until it is).
                to_end < 0.25 && moving < 0.06
            } else if let RunEnd::IntoFall(_) = ending {
                // On to the drop: done once off the last block, falling or landed.
                !me.on_ground || me.feet[1] < level
            } else {
                seg + 1 == line.len() && progress(line[seg - 1], line[seg], at) >= 0.8
            };
            if done {
                self.bot
                    .batch(&["key w up".into(), "key shift up".into()])?;
                return Ok(());
            }
            // Where on the line the bot is, how far off it, and the point to steer at.
            let (a, b) = (line[seg - 1], line[seg]);
            let t = progress(a, b, at).clamp(0.0, 1.0);
            let foot = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            let astray = dist(at, foot);
            wander = (wander + f64::from(self.rng.spread(0.05))).clamp(-wander_max, wander_max);
            let aim = if astray > ASTRAY {
                foot
            } else {
                offset(ahead(&line, seg, foot, LOOKAHEAD), a, b, wander)
            };
            let (dx, dz) = (aim[0] - at[0], aim[1] - at[1]);
            let (yaw, pitch, facing) = self.aim(me, yaw_toward(dx, dz));
            // The last stretch of a stopping run is sneaked: slow enough to stop where it
            // should, and it can't walk off an edge whatever the controls' delay.
            // Arriving fast (off a drop, say) it must start sooner: sneaking only holds the
            // bot at an edge while on the ground, and a bounce carries it over one.
            let speed = (me.velocity[0] * me.velocity[0] + me.velocity[2] * me.velocity[2]).sqrt();
            let sneak = precise && to_end < SNEAK.max(0.4 + 3.0 * speed);
            let calm = astray > ASTRAY
                || sharp_turn_within(&line, seg, at, SLOW)
                || (precise && to_end < SLOW);
            // Near the end, momentum alone carries it the rest of the way.
            let forward = facing && !(precise && (to_end < 0.12 || to_end < 2.5 * speed));
            let sprint = self.style.sprint && forward && !calm && !sneak;
            if self.verbose && std::env::var_os("SEXTANT_TRACE").is_some() {
                eprintln!(
                    "    tick: at {:.2?} speed {speed:.3} ground {} to_end {to_end:.2} seg {seg} sneak {sneak} forward {forward} sprint {sprint}",
                    me.pos, me.on_ground
                );
            }
            let replies = self.bot.batch(&[
                format!("look {yaw} {pitch}"),
                format!("key w {}", if forward { "down" } else { "up" }),
                format!("key ctrl {}", if sprint { "down" } else { "up" }),
                format!("key shift {}", if sneak { "down" } else { "up" }),
                "key space up".into(),
                "state".into(),
            ])?;
            me = parse_me(&replies[5])?;
        }
        Err(format!(
            "didn't finish following to {:?}: at {:.2?} (on ground {}, speed {:.3}, yaw {:.0})",
            run[run.len() - 1].to,
            me.pos,
            me.on_ground,
            (me.velocity[0] * me.velocity[0] + me.velocity[2] * me.velocity[2]).sqrt(),
            me.yaw
        ))
    }

    /// This tick's view toward heading `want`, in the style: the turn (eased, capped, with
    /// jitter) and, now and then, a swing past the heading that it corrects. The swing moves
    /// the view only: the feet follow the route, so `facing` (walk forward) holds only while
    /// the view is within 15 degrees of the real heading. Turning further first, in place, is
    /// what players do at a corner.
    fn aim(&mut self, me: Me, want: f32) -> (f32, f32, bool) {
        let turn = wrap(want - me.yaw);
        if turn.abs() > 30.0
            && self.overshoot == 0.0
            && self.rng.chance(self.style.overshoot_chance)
        {
            self.overshoot = self.style.overshoot * turn.signum();
        }
        let remaining = wrap(want + self.overshoot - me.yaw);
        if self.overshoot != 0.0 && remaining.abs() < 2.0 {
            self.overshoot = 0.0;
        }
        let yaw = wrap(
            me.yaw + self.style.turn_step(remaining) + self.rng.spread(self.style.aim_jitter * 0.2),
        );
        let pitch = self.style.walk_pitch + self.rng.spread(self.style.aim_jitter * 0.2);
        (yaw, pitch, wrap(want - yaw).abs() < 15.0)
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

    /// One step on its own: a step up, a drop, or a walk that mines or places first. Done once
    /// the feet are in its block, on the ground; the walking runs around it put the bot on a
    /// block's centre where that matters. A step up taps jump once, as a player does, rather
    /// than holding it and bouncing on past the block.
    fn move_to(&mut self, step: &Step) -> Result<Me, String> {
        let target = [f64::from(step.to[0]) + 0.5, f64::from(step.to[2]) + 0.5];
        let limit = match step.kind {
            Kind::Fall(n) => 40 + 4 * n as u32,
            _ => 40,
        };
        let mut me = self.bot.me()?;
        let start_y = me.pos[1];
        // Ticks since the last jump: one jump per step up, another only if it didn't rise.
        let mut since_jump: Option<u32> = None;
        for _ in 0..limit {
            if me.feet == step.to && me.on_ground {
                return Ok(me);
            }
            // Lower than the step on the ground: it slid off the route (or fell further).
            let fell = me.on_ground
                && me.feet[1] < step.to[1]
                && (matches!(step.kind, Kind::Fall(_)) || me.pos[1] < start_y - 0.5);
            if fell {
                return Err(format!(
                    "left the route's level at {:.2?} (feet {:?}, step to {:?})",
                    me.pos, me.feet, step.to
                ));
            }
            let (dx, dz) = (target[0] - me.pos[0], target[1] - me.pos[2]);
            let flat = (dx * dx + dz * dz).sqrt();
            let (yaw, pitch, facing) = self.aim(me, yaw_toward(dx, dz));
            let forward = facing && flat > 0.15;
            since_jump = since_jump.map(|t| t + 1);
            let risen = me.pos[1] > start_y + 0.3;
            let jump = step.kind == Kind::Ascend
                && me.on_ground
                && !risen
                && flat < 1.3
                && since_jump.is_none_or(|t| t > 10);
            if jump {
                since_jump = Some(0);
            }
            // Up in the air from a step up, it sneaks so it lands sneaking: an edge then holds it,
            // rather than the jump's momentum carrying it off a block narrower than its landing.
            let sneak = step.kind == Kind::Ascend && risen;
            let replies = self.bot.batch(&[
                format!("look {yaw} {pitch}"),
                format!("key w {}", if forward { "down" } else { "up" }),
                "key ctrl up".into(),
                format!("key space {}", if jump { "down" } else { "up" }),
                format!("key shift {}", if sneak { "down" } else { "up" }),
                "state".into(),
            ])?;
            me = parse_me(&replies[5])?;
        }
        Err(format!(
            "didn't reach {:?} ({:?}): at {:.2?} (feet {:?}, on ground {})",
            step.to, step.kind, me.pos, me.feet, me.on_ground
        ))
    }

    /// Jump and place a block under the feet at the top of the jump.
    fn pillar(&mut self, step: &Step) -> Result<(), String> {
        let place = step.place.ok_or("a pillar step without a block to place")?;
        // A jump needs the headroom over this block: off its centre, the bot's box can reach
        // under a block beside it and hit its head there.
        self.centre_on(place)?;
        self.bot.batch(&[
            "key w up".into(),
            "key ctrl up".into(),
            "look 0 90".into(),
            "key space down".into(),
        ])?;
        for _ in 0..12 {
            let me = self.bot.me()?;
            if self.verbose && std::env::var_os("SEXTANT_TRACE").is_some() {
                eprintln!(
                    "    pillar tick: at {:.2?} ground {} (placing at {:?})",
                    me.pos, me.on_ground, place
                );
            }
            if me.pos[1] >= f64::from(place[1]) + 1.05 {
                self.bot.cmd("key space up")?;
                self.bot.cmd(&format!(
                    "place {} {} {} --item {}",
                    place[0], place[1], place[2], self.block
                ))?;
                return self.wait_until_at(step, 20);
            }
        }
        self.bot.cmd("key space up")?;
        Err("the jump didn't get high enough to place under".into())
    }

    /// Stand still on block `at`'s centre: sneak toward it (no falling off the block) until
    /// within 0.18 of it and barely moving. 0.18 is what a pillar needs: the bot's box is
    /// 0.6 wide, so further off it reaches under the next block's column and a jump hits its
    /// head there. Short of that in time, within 0.25 still does.
    fn centre_on(&mut self, at: Pos) -> Result<(), String> {
        let centre = [f64::from(at[0]) + 0.5, f64::from(at[2]) + 0.5];
        let mut me = self.bot.me()?;
        for _ in 0..40 {
            let (dx, dz) = (centre[0] - me.pos[0], centre[1] - me.pos[2]);
            let off = (dx * dx + dz * dz).sqrt();
            let speed = (me.velocity[0] * me.velocity[0] + me.velocity[2] * me.velocity[2]).sqrt();
            if off < 0.18 && speed < 0.03 {
                self.bot
                    .batch(&["key w up".into(), "key shift up".into()])?;
                return Ok(());
            }
            let yaw = yaw_toward(dx, dz);
            let forward = off > 0.1 && speed < 0.05;
            let replies = self.bot.batch(&[
                format!("look {yaw} {}", self.style.walk_pitch),
                format!("key w {}", if forward { "down" } else { "up" }),
                "key ctrl up".into(),
                "key shift down".into(),
                "state".into(),
            ])?;
            me = parse_me(&replies[4])?;
        }
        self.bot
            .batch(&["key w up".into(), "key shift up".into()])?;
        let off = ((centre[0] - me.pos[0]).powi(2) + (centre[1] - me.pos[2]).powi(2)).sqrt();
        if off < 0.25 {
            return Ok(());
        }
        Err(format!(
            "couldn't stand still on {at:?}'s centre: at {:.2?}",
            me.pos
        ))
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
/// How a trip is made: what the planner may do, the style, and the walker's options.
pub struct Trip<'a> {
    pub settings: Settings,
    pub style: &'a Style,
    /// The seed of the style's randomness.
    pub seed: u64,
    /// Print each step.
    pub verbose: bool,
    /// The hotbar item placed for pillars and bridges.
    pub block: String,
    /// The hotbar item selected before mining.
    pub tool: Option<String>,
}

/// Walks the bot to `goal`, replanning as it goes. Whatever happens, it lets go of every key
/// at the end: keys held from a walk that failed would go on moving the bot, through a
/// death and a respawn too.
pub fn go(bot: &mut Bot, goal: Goal, trip: &Trip, log: impl FnMut(&str)) -> Result<(), String> {
    let result = go_inner(bot, goal, trip, log);
    let released = bot.cmd("release");
    result.and(released.map(|_| ()))
}

fn go_inner(
    bot: &mut Bot,
    goal: Goal,
    trip: &Trip,
    mut log: impl FnMut(&str),
) -> Result<(), String> {
    let (settings, style) = (&trip.settings, trip.style);
    let mut region = Region::new();
    let mut walker_seed = trip.seed;
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
            if path.complete {
                return Ok(());
            }
            let d = (0..3)
                .map(|i| f64::from(goal.pos[i] - me.feet[i]).powi(2))
                .sum::<f64>()
                .sqrt();
            return Err(format!(
                "no way forward from {:?}: {d:.1} blocks from the goal is as close as it gets (is the goal somewhere a player can stand?)",
                me.feet
            ));
        }
        walker_seed = walker_seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut walker = Walker::new(bot, style, walker_seed);
        walker.verbose = trip.verbose;
        walker.block = trip.block.clone();
        walker.tool = trip.tool.clone();
        match walker.walk(&path) {
            Ok(()) if path.complete => {
                bot.cmd("release")?;
                return Ok(());
            }
            Ok(()) => {}
            Err(e) if e.starts_with(DIED) => {
                bot.cmd("release")?;
                return Err(e);
            }
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
    fn the_line_geometry_follows_the_route() {
        let (a, b) = ([0.5, 0.5], [3.5, 0.5]);
        assert!((progress(a, b, [2.0, 9.0]) - 0.5).abs() < 1e-9);
        // A point ahead turns the corner of an L-shaped line.
        let line = [[0.5, 0.5], [2.5, 0.5], [2.5, 3.5]];
        let p = ahead(&line, 1, [2.0, 0.5], 1.0);
        assert!(
            (p[0] - 2.5).abs() < 1e-9 && (p[1] - 1.0).abs() < 1e-9,
            "{p:?}"
        );
        assert_eq!(
            ahead(&line, 2, [2.5, 3.0], 5.0),
            [2.5, 3.5],
            "clamped at the end"
        );
        // Sideways from an eastward segment is along z.
        let o = offset([1.0, 0.5], a, b, 0.2);
        assert!(
            (o[0] - 1.0).abs() < 1e-9 && (o[1] - 0.7).abs() < 1e-9,
            "{o:?}"
        );
        assert!(sharp_turn_within(&line, 1, [1.0, 0.5], 2.0));
        assert!(!sharp_turn_within(&line, 1, [0.5, 0.5], 1.0));
    }

    #[test]
    fn state_reads_feet_from_the_position() {
        let state = serde_json::json!({"player": {"position": [-3.5, 64.0, 10.2], "yaw": 12.0, "pitch": 5.0, "on_ground": true}});
        let me = parse_me(&state).unwrap();
        assert_eq!(me.feet, [-4, 64, 10]);
        assert!(parse_me(&serde_json::json!({})).is_err());
        let dead = serde_json::json!({"screen": "dead", "player": {"position": [0.0, 64.0, 0.0], "yaw": 0.0, "pitch": 0.0, "health": 0.0}});
        assert!(parse_me(&dead).unwrap_err().starts_with(DIED));
    }
}
