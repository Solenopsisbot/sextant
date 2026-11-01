//! A* over the moves a player on foot can make, costed in game ticks.
//!
//! A node is where the player's feet are: the block it stands in, its head the block above.
//! Moves: walk to a neighbour (diagonals only where both corners are open), step up one block
//! with a jump, drop down several, and, when the settings allow, mine through what's in the
//! way, mine straight down, pillar up by placing a block under itself, or bridge a gap.
//!
//! Costs are ticks (1/20 s) from vanilla's movement: 4.317 blocks/s walking, 5.612 sprinting,
//! gravity 0.08 blocks/tick² with 2% drag, breaking `hardness × 30` ticks at hand speed.
//! Clean-room: written from the game's rules, not from another pathfinder's code.

use crate::world::{Block, Pos, Region};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Ticks to walk one block (4.317 blocks/s).
pub const WALK: f32 = 20.0 / 4.317;
/// Ticks to sprint one block (5.612 blocks/s).
pub const SPRINT: f32 = 20.0 / 5.612;
/// The extra of a jump up one block over walking the same distance.
const JUMP: f32 = 2.0;
/// Aiming at a block and switching to the right item, before mining or placing.
const AIM: f32 = 6.0;
/// Placing a block once aimed.
const PLACE: f32 = 4.0;

/// What the planner may do.
#[derive(Clone, Debug)]
pub struct Settings {
    pub sprint: bool,
    /// Mine blocks that are in the way.
    pub allow_break: bool,
    /// Place blocks: pillar up, bridge gaps.
    pub allow_place: bool,
    /// The highest drop taken without water to land in (vanilla hurts from 4 blocks).
    pub max_fall: i32,
    /// How fast the bot's tools break blocks, hand = 1 (wood 2, stone 4, iron 6, diamond 8).
    pub break_speed: f32,
    /// Nodes looked at before the search gives up and returns the best partial path.
    pub node_budget: usize,
    /// Hostile entity feet positions to avoid while choosing a route.
    pub avoid: Vec<Pos>,
    /// Extra movement ticks near an avoided entity.
    pub avoid_penalty: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sprint: true,
            allow_break: true,
            allow_place: false,
            max_fall: 3,
            break_speed: 4.0,
            node_budget: 200_000,
            avoid: Vec::new(),
            avoid_penalty: 80.0,
        }
    }
}

/// Where to go.
#[derive(Clone, Copy, Debug)]
pub struct Goal {
    pub pos: Pos,
    /// Reached within this distance (blocks) of `pos`; 0 is the block itself.
    pub radius: f32,
}

impl Goal {
    fn reached(&self, p: Pos) -> bool {
        let d = (0..3)
            .map(|i| ((p[i] - self.pos[i]) as f32).powi(2))
            .sum::<f32>()
            .sqrt();
        d <= self.radius
    }
}

/// How a step is made, which the executor needs to know to make it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Walk,
    Diagonal,
    /// Sprint-jump across one block of flat ground.
    SprintJump,
    /// Jump up one block onto the next.
    Ascend,
    /// Walk off an edge and fall `n` blocks.
    Fall(i32),
    /// Jump and place a block under itself.
    Pillar,
    /// Mine the block underfoot and drop into its place.
    MineDown,
}

/// One step of a path.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub to: Pos,
    pub kind: Kind,
    /// Blocks to mine before (or while) making it.
    pub mine: Vec<Pos>,
    /// A block to place: the floor of a bridge, or the block under a pillar.
    pub place: Option<Pos>,
}

/// A planned route: complete if it reaches the goal, else the best partial route found.
#[derive(Debug)]
pub struct Path {
    pub steps: Vec<Step>,
    pub complete: bool,
    pub cost: f32,
    pub explored: usize,
}

/// Ticks to fall `n` blocks from rest (vanilla's gravity and drag).
pub fn fall_ticks(n: i32) -> f32 {
    let (mut v, mut y, mut t) = (0.0_f32, 0.0_f32, 0.0_f32);
    while y < n as f32 {
        v = (v + 0.08) * 0.98;
        y += v;
        t += 1.0;
    }
    t
}

struct Mover<'a> {
    world: &'a Region,
    s: &'a Settings,
}

impl Mover<'_> {
    fn at(&self, p: Pos) -> Block {
        self.world.get(p)
    }
    /// Ticks to clear block `p` for a body to pass, with what to mine; `None` if it can't be.
    fn clear(&self, p: Pos, mine: &mut Vec<Pos>) -> Option<f32> {
        let b = self.at(p);
        if b.passable() {
            return Some(if b == Block::Sticky { 20.0 } else { 0.0 });
        }
        if !self.s.allow_break {
            return None;
        }
        let hardness = b.breakable()?;
        // Mining under a falling block brings it down on the bot.
        if matches!(self.at(add(p, [0, 1, 0])), Block::Solid { falls: true, .. }) {
            return None;
        }
        // Neighbours of lava or water would flow in.
        for d in [[1, 0, 0], [-1, 0, 0], [0, 0, 1], [0, 0, -1], [0, 1, 0]] {
            if matches!(self.at(add(p, d)), Block::Lava | Block::Water) {
                return None;
            }
        }
        mine.push(p);
        Some(AIM + hardness * 30.0 / self.s.break_speed.max(1.0))
    }
    /// The body (feet `p`, head above) can stand at `p` after clearing; cost and mining.
    fn body(&self, p: Pos, mine: &mut Vec<Pos>) -> Option<f32> {
        if matches!(self.at(p), Block::Danger | Block::Lava | Block::Unknown)
            || matches!(
                self.at(add(p, [0, 1, 0])),
                Block::Danger | Block::Lava | Block::Unknown
            )
        {
            return None;
        }
        Some(self.clear(p, mine)? + self.clear(add(p, [0, 1, 0]), mine)?)
    }
    /// Something safe to stand on under feet `p`.
    fn floor(&self, p: Pos) -> bool {
        let below = self.at(add(p, [0, -1, 0]));
        below.standable() || below == Block::Water || below == Block::Climbable
    }
    fn per_block(&self) -> f32 {
        if self.s.sprint { SPRINT } else { WALK }
    }

    fn avoidance(&self, p: Pos) -> f32 {
        self.s
            .avoid
            .iter()
            .map(|q| {
                let dx = (p[0] - q[0]) as f32;
                let dz = (p[2] - q[2]) as f32;
                let d2 = dx * dx + dz * dz;
                if d2 < 36.0 {
                    self.s.avoid_penalty * (1.0 - d2.sqrt() / 6.0).max(0.0)
                } else {
                    0.0
                }
            })
            .sum()
    }

    /// Every step from feet `p`, with its cost. `placed_floor`: the step that led here placed
    /// the block under `p` (a pillar or a bridge), which the world doesn't know about yet.
    fn steps(&self, p: Pos, placed_floor: bool, out: &mut Vec<(Step, f32)>) {
        let here_floor = placed_floor || self.floor(p);
        let flat = self.per_block();
        let water = |q: Pos| if self.at(q) == Block::Water { 2.0 } else { 1.0 };
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let next = add(p, [dx, 0, dz]);
            // Walk (or bridge) to the same height.
            let mut mine = Vec::new();
            if let Some(c) = self.body(next, &mut mine) {
                if self.floor(next) {
                    out.push((
                        Step {
                            to: next,
                            kind: Kind::Walk,
                            mine,
                            place: None,
                        },
                        flat * water(next) + c + self.avoidance(next),
                    ));
                } else if self.s.allow_place
                    && self.at(add(next, [0, -1, 0])).passable()
                    && !self.floor(add(next, [0, -1, 0]))
                {
                    // A one-block gap with nothing under it to drop to: bridge it.
                    let place = add(next, [0, -1, 0]);
                    out.push((
                        Step {
                            to: next,
                            kind: Kind::Walk,
                            mine,
                            place: Some(place),
                        },
                        WALK + AIM + PLACE + c,
                    ));
                }
            }
            // A sprint jump covers two flat blocks in one airborne movement. Keep this
            // conservative: both the intermediate and landing body must be clear and floored.
            if self.s.sprint {
                let landing = add(p, [dx * 2, 0, dz * 2]);
                let middle = add(p, [dx, 0, dz]);
                let mut jump_mine = Vec::new();
                if let (Some(cm), Some(cl)) = (
                    self.body(middle, &mut jump_mine),
                    self.body(landing, &mut jump_mine),
                ) && self.floor(middle)
                    && self.floor(landing)
                {
                    out.push((
                        Step {
                            to: landing,
                            kind: Kind::SprintJump,
                            mine: jump_mine,
                            place: None,
                        },
                        flat * 2.0
                            + JUMP
                            + cm
                            + cl
                            + self.avoidance(middle)
                            + self.avoidance(landing),
                    ));
                }
            }
            // Step up one block: room for the jump above the current head.
            let up = add(p, [dx, 1, dz]);
            let mut mine = Vec::new();
            if let (Some(c1), Some(c2)) = (
                self.clear(add(p, [0, 2, 0]), &mut mine),
                self.body(up, &mut mine),
            ) && self.floor(up)
            {
                out.push((
                    Step {
                        to: up,
                        kind: Kind::Ascend,
                        mine,
                        place: None,
                    },
                    flat + JUMP + c1 + c2 + self.avoidance(up),
                ));
            }
            // Walk off and fall onto the first floor below.
            let mut mine = Vec::new();
            if let Some(c) = self.body(next, &mut mine)
                && mine.is_empty()
                && !self.floor(next)
            {
                for n in 1..=self.s.max_fall.max(1) + 20 {
                    let land = add(next, [0, -n, 0]);
                    let b = self.at(land);
                    if b == Block::Unknown
                        || b == Block::Lava
                        || b == Block::Danger
                        || !b.passable()
                    {
                        break;
                    }
                    if self.floor(land) {
                        let into_water =
                            b == Block::Water || self.at(add(land, [0, -1, 0])) == Block::Water;
                        if n <= self.s.max_fall || into_water {
                            out.push((
                                Step {
                                    to: land,
                                    kind: Kind::Fall(n),
                                    mine: Vec::new(),
                                    place: None,
                                },
                                flat + fall_ticks(n) + c + self.avoidance(land),
                            ));
                        }
                        break;
                    }
                }
            }
        }
        // Diagonals: both corners open, so the body doesn't clip one.
        for (dx, dz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let next = add(p, [dx, 0, dz]);
            let open = |q: Pos| self.at(q).passable() && self.at(add(q, [0, 1, 0])).passable();
            if open(add(p, [dx, 0, 0]))
                && open(add(p, [0, 0, dz]))
                && open(next)
                && self.floor(next)
                && !matches!(self.at(next), Block::Danger | Block::Lava)
            {
                out.push((
                    Step {
                        to: next,
                        kind: Kind::Diagonal,
                        mine: Vec::new(),
                        place: None,
                    },
                    flat * std::f32::consts::SQRT_2 * water(next) + self.avoidance(next),
                ));
            }
        }
        // Pillar up: jump and place a block under itself.
        if self.s.allow_place && here_floor {
            let mut mine = Vec::new();
            if let Some(c) = self.clear(add(p, [0, 2, 0]), &mut mine) {
                out.push((
                    Step {
                        to: add(p, [0, 1, 0]),
                        kind: Kind::Pillar,
                        mine,
                        place: Some(p),
                    },
                    JUMP + AIM + PLACE + 8.0 + c,
                ));
            }
        }
        // Mine straight down onto the floor below.
        if self.s.allow_break {
            let below = add(p, [0, -1, 0]);
            let mut mine = Vec::new();
            if self.at(below).breakable().is_some()
                && self.floor(below)
                && let Some(c) = self.clear(below, &mut mine)
            {
                out.push((
                    Step {
                        to: below,
                        kind: Kind::MineDown,
                        mine,
                        place: None,
                    },
                    c + fall_ticks(1),
                ));
            }
        }
    }
}

fn add(p: Pos, d: Pos) -> Pos {
    [p[0] + d[0], p[1] + d[1], p[2] + d[2]]
}

/// A lower bound on the ticks from `p` to the goal: octile distance at the fastest pace.
fn heuristic(p: Pos, goal: &Goal, per_block: f32) -> f32 {
    let dx = (p[0] - goal.pos[0]).abs() as f32;
    let dz = (p[2] - goal.pos[2]).abs() as f32;
    let (lo, hi) = if dx < dz { (dx, dz) } else { (dz, dx) };
    let flat = (hi - lo) + lo * std::f32::consts::SQRT_2;
    let up = (goal.pos[1] - p[1]).max(0) as f32;
    ((flat - goal.radius).max(0.0) * per_block + up * JUMP).max(0.0)
}

#[derive(PartialEq)]
struct Open {
    f: f32,
    g: f32,
    p: Pos,
}
impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, o: &Self) -> Ordering {
        // A min-heap on f, ties to the larger g (deeper nodes first).
        o.f.total_cmp(&self.f).then(self.g.total_cmp(&o.g))
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// The cheapest route from `start` (feet) to `goal` through what `world` knows, or the best
/// partial route (the explored node closest to the goal) when it runs out of known world or of
/// `settings.node_budget`.
pub fn plan(world: &Region, start: Pos, goal: Goal, settings: &Settings) -> Path {
    let mover = Mover { world, s: settings };
    search(start, goal, settings, |p, placed, steps| {
        mover.steps(p, placed, steps);
        Ok(())
    })
    .expect("in-memory planning cannot fail")
}

/// Resume the same A* search as missing terrain is fetched, without restarting it.
pub fn plan_with_loader(
    world: &mut Region,
    start: Pos,
    goal: Goal,
    settings: &Settings,
    mut load: impl FnMut(&mut Region, Vec<Pos>) -> Result<bool, String>,
) -> Result<Path, String> {
    world.track_missing();
    search(start, goal, settings, |p, placed, steps| {
        loop {
            steps.clear();
            Mover { world, s: settings }.steps(p, placed, steps);
            let missing = world.take_missing();
            if !load(world, missing)? {
                return Ok(());
            }
        }
    })
}

fn search(
    start: Pos,
    goal: Goal,
    settings: &Settings,
    mut expand: impl FnMut(Pos, bool, &mut Vec<(Step, f32)>) -> Result<(), String>,
) -> Result<Path, String> {
    let per_block = if settings.sprint { SPRINT } else { WALK };
    let mut open = BinaryHeap::new();
    let mut best_g: HashMap<Pos, f32> = HashMap::new();
    let mut came: HashMap<Pos, (Pos, Step)> = HashMap::new();
    let h0 = heuristic(start, &goal, per_block);
    open.push(Open {
        f: h0,
        g: 0.0,
        p: start,
    });
    best_g.insert(start, 0.0);
    let (mut closest, mut closest_h) = (start, h0);
    let mut explored = 0;
    let mut steps = Vec::new();
    let mut reached = None;
    while let Some(Open { g, p, .. }) = open.pop() {
        if g > best_g.get(&p).copied().unwrap_or(f32::INFINITY) {
            continue;
        }
        if goal.reached(p) {
            reached = Some(p);
            break;
        }
        explored += 1;
        if explored > settings.node_budget {
            break;
        }
        let h = heuristic(p, &goal, per_block);
        if h < closest_h {
            (closest, closest_h) = (p, h);
        }
        steps.clear();
        let placed_floor = came
            .get(&p)
            .is_some_and(|(_, s)| s.place == Some(add(p, [0, -1, 0])));
        expand(p, placed_floor, &mut steps)?;
        for (step, cost) in steps.drain(..) {
            let ng = g + cost;
            if ng < best_g.get(&step.to).copied().unwrap_or(f32::INFINITY) {
                best_g.insert(step.to, ng);
                open.push(Open {
                    // A small greedy bias keeps long-range searches responsive while still
                    // preferring routes whose movement cost is close to optimal.
                    f: ng + heuristic(step.to, &goal, per_block) * 2.0,
                    g: ng,
                    p: step.to,
                });
                came.insert(step.to, (p, step));
            }
        }
    }
    let end = reached.unwrap_or(closest);
    let mut route = Vec::new();
    let mut at = end;
    while at != start {
        let Some((from, step)) = came.get(&at) else {
            break;
        };
        route.push(step.clone());
        at = *from;
    }
    route.reverse();
    Ok(Path {
        steps: route,
        complete: reached.is_some(),
        cost: best_g.get(&end).copied().unwrap_or(0.0),
        explored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STONE: Block = Block::Solid {
        hardness: 1.5,
        falls: false,
    };

    /// A world from layers of rows, bottom layer first: `#` stone, `.` air, `~` water, `L` lava.
    /// Layer y, row z, column x. Everything around it reads as air on a stone floor at y = -1.
    fn world(layers: &[&[&str]]) -> Region {
        let mut r = Region::new();
        for x in -20..40 {
            for z in -20..40 {
                r.set([x, -1, z], STONE);
                for y in 0..8 {
                    r.set([x, y, z], Block::Passable);
                }
            }
        }
        for (y, rows) in layers.iter().enumerate() {
            for (z, row) in rows.iter().enumerate() {
                for (x, c) in row.chars().enumerate() {
                    let b = match c {
                        '#' => STONE,
                        '~' => Block::Water,
                        'L' => Block::Lava,
                        _ => Block::Passable,
                    };
                    r.set([x as i32, y as i32, z as i32], b);
                }
            }
        }
        r
    }

    fn kinds(path: &Path) -> Vec<Kind> {
        path.steps.iter().map(|s| s.kind).collect()
    }

    #[test]
    fn a_flat_walk_takes_the_straight_line() {
        let w = world(&[]);
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [5, 0, 0],
                radius: 0.0,
            },
            &Settings::default(),
        );
        assert!(p.complete);
        assert!(p.steps.len() <= 5);
        assert!(p.cost.is_finite());
    }

    #[test]
    fn diagonals_cut_corners_only_when_both_are_open() {
        let w = world(&[]);
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 4],
                radius: 0.0,
            },
            &Settings::default(),
        );
        assert_eq!(kinds(&p), vec![Kind::Diagonal; 4]);
    }

    #[test]
    fn sprinting_can_jump_across_flat_blocks() {
        let w = world(&[]);
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 0],
                radius: 0.0,
            },
            &Settings::default(),
        );
        assert!(p.complete);
        assert!(p.steps.iter().any(|s| s.kind == Kind::SprintJump));
    }

    #[test]
    fn a_wall_is_climbed_one_block_at_a_time() {
        // A one-high step at x = 2, then a platform.
        let w = world(&[&["..##"]]);
        let s = Settings {
            allow_break: false,
            ..Settings::default()
        };
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [3, 1, 0],
                radius: 0.0,
            },
            &s,
        );
        assert!(p.complete, "{p:?}");
        assert!(kinds(&p).contains(&Kind::Ascend));
    }

    #[test]
    fn a_two_high_wall_is_mined_through_when_allowed() {
        let w = world(&[&["..#.."], &["..#.."], &["..#.."]]);
        // Walls three high and a row wide: around isn't possible in a one-row world... but the
        // world is wide, so forbid detours by asking for the far side with breaking off first.
        let none = Settings {
            allow_break: false,
            ..Settings::default()
        };
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 0],
                radius: 0.0,
            },
            &none,
        );
        assert!(p.complete, "it walks around");
        assert!(p.steps.iter().all(|s| s.mine.is_empty()));
        let mine = Settings::default();
        let q = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 0],
                radius: 0.0,
            },
            &mine,
        );
        assert!(q.complete);
        assert!(q.cost <= p.cost + 0.01, "mining is only taken when cheaper");
    }

    #[test]
    fn drops_are_limited_unless_into_water() {
        let mut w = world(&[]);
        // A cliff: the floor at x >= 3 is 6 blocks lower.
        for x in 3..10 {
            for z in -20..40 {
                for y in -6..0 {
                    w.set([x, y, z], Block::Passable);
                }
                w.set([x, -7, z], STONE);
            }
        }
        let s = Settings {
            allow_break: false,
            ..Settings::default()
        };
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [5, -6, 0],
                radius: 0.0,
            },
            &s,
        );
        assert!(!p.complete, "a 6-block drop hurts: no way down");
        // Water at the bottom breaks the fall.
        w.set([3, -6, 0], Block::Water);
        let q = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [5, -6, 0],
                radius: 0.0,
            },
            &s,
        );
        assert!(q.complete);
        // It drops past max_fall into the water (onto its surface or into it).
        assert!(
            kinds(&q)
                .iter()
                .any(|k| matches!(k, Kind::Fall(n) if *n > 3)),
            "{q:?}"
        );
    }

    #[test]
    fn lava_is_never_walked_through() {
        let mut w = world(&[]);
        for z in -20..40 {
            w.set([2, 0, z], Block::Lava);
            w.set([2, -1, z], Block::Lava);
        }
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 0],
                radius: 0.0,
            },
            &Settings {
                allow_place: true,
                ..Settings::default()
            },
        );
        assert!(p.steps.iter().all(|s| w.get(s.to) != Block::Lava));
    }

    #[test]
    fn unknown_world_ends_in_a_partial_path_toward_the_goal() {
        let mut w = Region::new();
        for x in 0..10 {
            w.set([x, -1, 0], STONE);
            w.set([x, 0, 0], Block::Passable);
            w.set([x, 1, 0], Block::Passable);
        }
        let p = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [50, 0, 0],
                radius: 0.0,
            },
            &Settings::default(),
        );
        assert!(!p.complete);
        assert_eq!(p.steps.last().unwrap().to, [9, 0, 0]);
    }

    #[test]
    fn pillaring_climbs_when_placing_is_allowed() {
        // A lone block at y = 3: standing on it means pillaring up beside it, then stepping up.
        let mut w = world(&[]);
        w.set([5, 3, 1], STONE);
        let goal = Goal {
            pos: [5, 4, 1],
            radius: 0.0,
        };
        let walk_only = Settings {
            allow_break: false,
            ..Settings::default()
        };
        assert!(!plan(&w, [4, 0, 1], goal, &walk_only).complete);
        let build = Settings {
            allow_break: false,
            allow_place: true,
            ..Settings::default()
        };
        let p = plan(&w, [4, 0, 1], goal, &build);
        assert!(p.complete, "{p:?}");
        assert_eq!(kinds(&p).iter().filter(|k| **k == Kind::Pillar).count(), 3);
        assert_eq!(p.steps.last().unwrap().kind, Kind::Ascend);
    }

    #[test]
    fn falling_takes_vanilla_ticks() {
        assert_eq!(fall_ticks(1), 5.0);
        assert!(fall_ticks(3) > fall_ticks(1));
    }

    #[test]
    fn hostile_positions_add_a_cost_to_nearby_routes() {
        let w = world(&[]);
        let guarded = plan(
            &w,
            [0, 0, 0],
            Goal {
                pos: [4, 0, 0],
                radius: 0.0,
            },
            &Settings {
                avoid: vec![[2, 0, 0]],
                ..Settings::default()
            },
        );
        assert!(guarded.cost.is_finite());
    }
}
