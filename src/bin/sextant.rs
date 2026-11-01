//! `sextant`: walk a Horizon bot to a place, in a style from robot to player-like.
//!
//! ```sh
//! sextant --socket .flotilla/ctl-Miner.sock go 120 64 -40 --style player-like
//! sextant --socket ... plan 120 64 -40        # print the route without moving
//! sextant styles                               # the presets and their knobs
//! ```

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sextant::bot::{self, Bot};
use sextant::build::Blueprint;
use sextant::planner::{Goal, Settings};
use sextant::style::Style;
use std::path::PathBuf;
use std::{collections::BTreeMap, fs};

#[derive(Parser)]
#[command(version, about = "Pathing for Horizon bots, from robot to player-like")]
struct Cli {
    /// The bot's control socket (horizon-ctl's, or Flotilla's .flotilla/ctl-NAME.sock)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// A style preset (robot, tidy, casual, player-like) or a TOML file of knobs over one
    #[arg(long, global = true, default_value = "casual")]
    style: String,
    /// Mine through what's in the way
    #[arg(long, global = true, default_value_t = true, action = clap::ArgAction::Set)]
    mine: bool,
    /// Place blocks to pillar up and bridge gaps (the bot needs blocks in its hotbar)
    #[arg(long, global = true)]
    build: bool,
    /// Arrive within this many blocks of the target
    #[arg(long, global = true, default_value_t = 0.0)]
    radius: f32,
    /// The hotbar item placed for pillars and bridges
    #[arg(long, global = true, default_value = "cobblestone")]
    block: String,
    /// The hotbar item to mine with (default: whatever it holds)
    #[arg(long, global = true)]
    tool: Option<String>,
    /// Persistent terrain cache for plan and eta commands
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    /// Waypoint file for save/list/go commands
    #[arg(long, global = true, default_value = ".sextant-waypoints.toml")]
    waypoints: PathBuf,
    /// Task file used by pause/resume
    #[arg(long, global = true, default_value = ".sextant-task.toml")]
    task: PathBuf,
    /// Optional cross-process reservation file for fleet coordination
    #[arg(long, global = true)]
    reservation: Option<PathBuf>,
    /// Print every step, and where the bot was when one fails
    #[arg(long, short, global = true)]
    verbose: bool,
    /// The seed of the style's randomness
    #[arg(long, global = true, default_value_t = 1)]
    seed: u64,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Walk to X Y Z (feet), replanning as it goes
    Go(Target),
    /// Plan the route to X Y Z from where the bot stands, without moving
    Plan(Target),
    /// Print the presets
    Styles,
    /// Stop movement immediately and release every held key
    Stop,
    /// Pause movement by releasing keys (the current command can be resumed by starting it again)
    Pause,
    /// Resume is a convenience alias for reporting that a new command is required
    Resume,
    /// Estimate the route without printing every step
    Eta(Target),
    /// Print the bot's current control state
    Status,
    /// Print movement and travel capabilities reported by Horizon
    Capabilities,
    /// Craft a supported player-inventory recipe
    Craft(CraftArgs),
    /// Mine matching blocks in nearest-first order
    Mine(MineArgs),
    /// Save, list, or navigate to named coordinates
    Waypoint(WaypointArgs),
    /// Build a JSON blueprint, placing supports before upper blocks
    Build(BuildArgs),
    /// Follow the nearest matching entity for a bounded number of updates
    Follow(FollowArgs),
    /// Explore outward in a square spiral for a bounded number of legs
    Explore(ExploreArgs),
    /// Dig a one-block-wide tunnel to X Y Z
    Tunnel(Target),
    /// Walk to the highest nearby grass block
    Surface(SurfaceArgs),
    /// Walk along an axis for a number of blocks
    Axis(AxisArgs),
    /// Find loaded blocks of a kind
    Find(FindArgs),
    /// Walk away from a target by a distance
    Invert(InvertArgs),
    /// Harvest and replant mature crops
    Farm(FarmArgs),
}

#[derive(clap::Args)]
struct WaypointArgs {
    #[command(subcommand)]
    action: WaypointAction,
}

#[derive(Subcommand)]
enum WaypointAction {
    /// Save the bot's current feet position under NAME
    Save { name: String },
    /// List saved waypoints
    List,
    /// Walk to NAME
    Go { name: String },
}

#[derive(Default, Serialize, Deserialize)]
struct Waypoints(BTreeMap<String, [i32; 3]>);

#[derive(clap::Args)]
struct MineArgs {
    /// Block names, with or without the minecraft: namespace
    #[arg(required = true)]
    blocks: Vec<String>,
    /// Number of blocks to mine
    #[arg(long, short, default_value_t = 1)]
    count: u32,
    /// Search radius for loaded matching blocks
    #[arg(long, default_value_t = 64)]
    search_radius: i32,
}

#[derive(clap::Args)]
struct CraftArgs {
    recipe: String,
    #[arg(long, short, default_value_t = 1)]
    times: u32,
}

#[derive(clap::Args)]
struct FarmArgs {
    /// Crop block names (wheat, carrots, potatoes, beetroots)
    #[arg(required = true)]
    crops: Vec<String>,
    #[arg(long, default_value_t = 32)]
    search_radius: i32,
    #[arg(long, default_value_t = 1)]
    rounds: u32,
}

#[derive(clap::Args)]
struct BuildArgs {
    /// Blueprint JSON file
    file: PathBuf,
    /// Optional world-space origin X Y Z; defaults to the bot's feet
    #[arg(long, num_args = 3, value_names = ["X", "Y", "Z"])]
    origin: Option<Vec<i32>>,
}

#[derive(clap::Args)]
struct FollowArgs {
    /// Entity kind, for example player, minecraft:pig, or item
    kind: String,
    #[arg(long, default_value_t = 32.0)]
    search_radius: f64,
    #[arg(long, default_value_t = 2.0)]
    stop_distance: f32,
    #[arg(long, default_value_t = 20)]
    rounds: u32,
}

#[derive(clap::Args)]
struct ExploreArgs {
    #[arg(long, default_value_t = 8)]
    legs: u32,
    #[arg(long, default_value_t = 32)]
    leg_length: i32,
}

#[derive(clap::Args)]
struct SurfaceArgs {
    #[arg(long, default_value_t = 128)]
    search_radius: i32,
}

#[derive(clap::Args)]
struct AxisArgs {
    /// north, south, east, or west
    direction: String,
    #[arg(default_value_t = 64)]
    distance: i32,
}

#[derive(clap::Args)]
struct FindArgs {
    block: String,
    #[arg(long, default_value_t = 64)]
    search_radius: i32,
}

#[derive(clap::Args)]
struct InvertArgs {
    x: i32,
    y: i32,
    z: i32,
    #[arg(default_value_t = 64)]
    distance: i32,
}

/// A block position; coordinates may be negative (`go -- -120 64 -40` isn't needed).
#[derive(clap::Args)]
struct Target {
    #[arg(allow_negative_numbers = true)]
    x: i32,
    #[arg(allow_negative_numbers = true)]
    y: i32,
    #[arg(allow_negative_numbers = true)]
    z: i32,
}

#[derive(Serialize, Deserialize)]
struct TaskRecord {
    target: [i32; 3],
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("sextant: {e}");
        std::process::exit(1);
    }
}

fn style(arg: &str) -> Result<Style, String> {
    if let Some(s) = Style::preset(arg) {
        return Ok(s);
    }
    let text =
        std::fs::read_to_string(arg).map_err(|e| format!("style {arg}: not a preset, and {e}"))?;
    let table: toml::Table = toml::from_str(&text).map_err(|e| format!("{arg}: {e}"))?;
    Style::with_overrides(&table)
}

fn run(cli: Cli) -> Result<(), String> {
    let style = style(&cli.style)?;
    let settings = Settings {
        sprint: style.sprint,
        allow_break: cli.mine,
        allow_place: cli.build,
        ..Settings::default()
    };
    match cli.command {
        Cmd::Styles => {
            for s in [
                Style::robot(),
                Style::tidy(),
                Style::casual(),
                Style::player_like(),
            ] {
                println!("{}", toml::to_string(&s).map_err(|e| e.to_string())?);
            }
            Ok(())
        }
        command @ (Cmd::Stop | Cmd::Pause) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            bot.cmd("release")?;
            match command {
                Cmd::Stop => println!("stopped"),
                Cmd::Pause => println!("paused (keys released; rerun the command to resume)"),
                _ => unreachable!(),
            }
            Ok(())
        }
        Cmd::Resume => {
            let record: TaskRecord = toml::from_str(
                &fs::read_to_string(&cli.task)
                    .map_err(|e| format!("{}: {e}", cli.task.display()))?,
            )
            .map_err(|e| format!("{}: {e}", cli.task.display()))?;
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(
                &mut bot,
                Goal {
                    pos: record.target,
                    radius: cli.radius,
                },
                &trip,
                |line| eprintln!("{line}"),
            )?;
            let _ = fs::remove_file(&cli.task);
            println!("resumed to {:?}", record.target);
            Ok(())
        }
        Cmd::Status => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            println!("{}", bot.cmd("state")?);
            Ok(())
        }
        Cmd::Capabilities => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&bot.travel_capabilities()?)
                    .map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Cmd::Craft(CraftArgs { recipe, times }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            println!("crafted {} items", bot.craft(&recipe, times)?);
            Ok(())
        }
        Cmd::Eta(Target { x, y, z }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let me = bot.me()?;
            let goal = Goal {
                pos: [x, y, z],
                radius: cli.radius,
            };
            let started = std::time::Instant::now();
            let (path, read) =
                bot.plan_loaded_cached(me.feet, goal, &settings, cli.cache.as_deref())?;
            println!(
                "eta {:.1} s, {} steps, {}, {} nodes, {} tiles, planned in {} ms",
                path.cost / 20.0,
                path.steps.len(),
                if path.complete { "complete" } else { "partial" },
                path.explored,
                read,
                started.elapsed().as_millis()
            );
            Ok(())
        }
        Cmd::Mine(MineArgs {
            blocks,
            count,
            search_radius,
        }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            let mined = bot::mine(&mut bot, &blocks, count, search_radius, &trip, |line| {
                eprintln!("{line}")
            })?;
            println!("mined {mined} block(s)");
            Ok(())
        }
        Cmd::Build(BuildArgs { file, origin }) => {
            let blueprint = Blueprint::load(&file)?;
            let origin = origin.map(|v| [v[0], v[1], v[2]]);
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            let placed = bot::build(&mut bot, &blueprint, origin, &trip, |line| {
                eprintln!("{line}")
            })?;
            println!("placed {placed} block(s)");
            Ok(())
        }
        Cmd::Follow(FollowArgs {
            kind,
            search_radius,
            stop_distance,
            rounds,
        }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::follow(
                &mut bot,
                &kind,
                search_radius,
                stop_distance,
                rounds,
                &trip,
                |line| eprintln!("{line}"),
            )?;
            Ok(())
        }
        Cmd::Explore(ExploreArgs { legs, leg_length }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::explore(&mut bot, legs, leg_length, &trip, |line| {
                eprintln!("{line}")
            })?;
            Ok(())
        }
        Cmd::Find(FindArgs {
            block,
            search_radius,
        }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            for pos in bot.find_blocks(&block, search_radius)? {
                println!("{} {} {}", pos[0], pos[1], pos[2]);
            }
            Ok(())
        }
        Cmd::Surface(SurfaceArgs { search_radius }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let me = bot.me()?;
            let mut targets = bot.find_blocks("grass_block", search_radius)?;
            targets.sort_by_key(|p| {
                (
                    -(p[1]),
                    (p[0] - me.feet[0]).pow(2) + (p[2] - me.feet[2]).pow(2),
                )
            });
            let target = targets.first().copied().ok_or("no surface found")?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(
                &mut bot,
                Goal {
                    pos: [target[0], target[1] + 1, target[2]],
                    radius: cli.radius,
                },
                &trip,
                |line| eprintln!("{line}"),
            )?;
            Ok(())
        }
        Cmd::Axis(AxisArgs {
            direction,
            distance,
        }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let me = bot.me()?;
            let (dx, dz) = match direction.to_ascii_lowercase().as_str() {
                "north" | "n" => (0, -distance),
                "south" | "s" => (0, distance),
                "east" | "e" => (distance, 0),
                "west" | "w" => (-distance, 0),
                _ => return Err("axis direction must be north, south, east, or west".into()),
            };
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(
                &mut bot,
                Goal {
                    pos: [me.feet[0] + dx, me.feet[1], me.feet[2] + dz],
                    radius: cli.radius,
                },
                &trip,
                |line| eprintln!("{line}"),
            )?;
            Ok(())
        }
        Cmd::Tunnel(Target { x, y, z }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(
                &mut bot,
                Goal {
                    pos: [x, y, z],
                    radius: cli.radius,
                },
                &trip,
                |line| eprintln!("{line}"),
            )?;
            Ok(())
        }
        Cmd::Invert(InvertArgs { x, y, z, distance }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let me = bot.me()?;
            let v = [me.feet[0] - x, me.feet[1] - y, me.feet[2] - z];
            let len = ((v[0] * v[0] + v[2] * v[2]) as f64).sqrt().max(1.0);
            let target = [
                me.feet[0] + (v[0] as f64 / len * distance as f64) as i32,
                me.feet[1],
                me.feet[2] + (v[2] as f64 / len * distance as f64) as i32,
            ];
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(
                &mut bot,
                Goal {
                    pos: target,
                    radius: cli.radius,
                },
                &trip,
                |line| eprintln!("{line}"),
            )?;
            Ok(())
        }
        Cmd::Farm(FarmArgs {
            crops,
            search_radius,
            rounds,
        }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            let harvested = bot::farm(&mut bot, &crops, search_radius, rounds, &trip, |line| {
                eprintln!("{line}")
            })?;
            println!("harvested {harvested} crop(s)");
            Ok(())
        }
        Cmd::Waypoint(WaypointArgs { action }) => {
            let path = cli.waypoints.clone();
            let mut waypoints = fs::read_to_string(&path)
                .ok()
                .and_then(|s| toml::from_str::<Waypoints>(&s).ok())
                .unwrap_or_default();
            match action {
                WaypointAction::List => {
                    for (name, pos) in &waypoints.0 {
                        println!("{name}: {} {} {}", pos[0], pos[1], pos[2]);
                    }
                }
                WaypointAction::Save { name } => {
                    let mut bot = Bot::connect(
                        cli.socket
                            .as_deref()
                            .ok_or("--socket: which bot (its control socket)")?,
                    )?;
                    let pos = bot.me()?.feet;
                    waypoints.0.insert(name.clone(), pos);
                    fs::write(
                        &path,
                        toml::to_string_pretty(&waypoints).map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                    println!("saved {name} at {pos:?} ({})", path.display());
                }
                WaypointAction::Go { name } => {
                    let pos = waypoints
                        .0
                        .get(&name)
                        .copied()
                        .ok_or_else(|| format!("unknown waypoint {name}"))?;
                    let mut bot = Bot::connect(
                        cli.socket
                            .as_deref()
                            .ok_or("--socket: which bot (its control socket)")?,
                    )?;
                    let trip = bot::Trip {
                        settings: settings.clone(),
                        style: &style,
                        seed: cli.seed,
                        verbose: cli.verbose,
                        block: cli.block.clone(),
                        tool: cli.tool.clone(),
                    };
                    bot::go(
                        &mut bot,
                        Goal {
                            pos,
                            radius: cli.radius,
                        },
                        &trip,
                        |line| eprintln!("{line}"),
                    )?;
                    println!("arrived at waypoint {name} ({pos:?})");
                }
            }
            Ok(())
        }
        Cmd::Plan(Target { x, y, z }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let me = bot.me()?;
            let goal = Goal {
                pos: [x, y, z],
                radius: cli.radius,
            };
            let started = std::time::Instant::now();
            let (path, read) =
                bot.plan_loaded_cached(me.feet, goal, &settings, cli.cache.as_deref())?;
            println!(
                "{} steps from {:?}, {}, {:.1} s of walking, {} nodes in {} ms ({read} tiles read)",
                path.steps.len(),
                me.feet,
                if path.complete { "complete" } else { "partial" },
                path.cost / 20.0,
                path.explored,
                started.elapsed().as_millis()
            );
            for s in &path.steps {
                let extra = match (&s.mine.is_empty(), s.place) {
                    (true, None) => String::new(),
                    (_, place) => format!("  mine {:?} place {:?}", s.mine, place),
                };
                println!("  {:?} -> {:?}{extra}", s.kind, s.to);
            }
            Ok(())
        }
        Cmd::Go(Target { x, y, z }) => {
            let _reservation = cli
                .reservation
                .as_ref()
                .map(|path| {
                    sextant::coordination::Reservation::acquire(
                        path,
                        &format!("{}:{x}:{y}:{z}", std::process::id()),
                    )
                })
                .transpose()?;
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            let goal = Goal {
                pos: [x, y, z],
                radius: cli.radius,
            };
            let started = std::time::Instant::now();
            fs::write(
                &cli.task,
                toml::to_string(&TaskRecord { target: [x, y, z] }).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("{}: {e}", cli.task.display()))?;
            let trip = bot::Trip {
                settings: settings.clone(),
                style: &style,
                seed: cli.seed,
                verbose: cli.verbose,
                block: cli.block.clone(),
                tool: cli.tool.clone(),
            };
            bot::go(&mut bot, goal, &trip, |line| eprintln!("{line}"))?;
            let me = bot.me()?;
            println!(
                "arrived at {:?} in {:.1} s ({} style)",
                me.feet,
                started.elapsed().as_secs_f32(),
                style.name
            );
            let _ = fs::remove_file(&cli.task);
            Ok(())
        }
    }
}
