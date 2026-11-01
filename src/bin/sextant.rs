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
use sextant::{dashboard, swarm, tasks, worker};
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
    #[arg(long = "task", global = true, default_value = ".sextant-task.toml")]
    task_file: PathBuf,
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
    /// Walk into a portal at X Y Z and wait for the dimension change
    Portal(PortalArgs),
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
    /// Mount or dismount a vehicle
    Mount(MountArgs),
    /// Launch, boost, or inspect Elytra flight
    Elytra(ElytraArgs),
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
    /// Run or validate a declarative multi-step plan
    Task(TaskArgs),
    /// Serve the optional fleet dashboard and WebSocket control plane
    Dashboard(DashboardArgs),
    /// Queue and lease durable work across a bot swarm
    Swarm(SwarmArgs),
    /// Run an autonomous worker that claims and executes swarm jobs
    Worker(WorkerArgs),
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
struct MountArgs {
    /// Entity id from `entities`
    id: Option<i32>,
    #[arg(long)]
    dismount: bool,
}

#[derive(clap::Args)]
struct ElytraArgs {
    #[arg(value_parser = ["launch", "boost", "status"])]
    action: String,
}

#[derive(clap::Args)]
struct TaskArgs {
    #[command(subcommand)]
    action: TaskAction,
}

#[derive(Subcommand)]
enum TaskAction {
    /// Validate a plan without connecting to a bot
    Validate { file: PathBuf },
    /// Run all steps in order, announcing progress in game chat
    Run {
        file: PathBuf,
        /// Durable state file; defaults to FILE.state.json
        #[arg(long)]
        state: Option<PathBuf>,
        /// Discard prior progress and start from step one
        #[arg(long)]
        restart: bool,
    },
    /// Print durable progress without connecting to a bot
    Status {
        file: PathBuf,
        #[arg(long)]
        state: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct DashboardArgs {
    /// TOML fleet file containing [[bots]] name/socket entries
    #[arg(long, default_value = "sextant-fleet.toml")]
    fleet: PathBuf,
    /// Override the fleet file's bind address
    #[arg(long)]
    bind: Option<String>,
}

#[derive(clap::Args)]
struct SwarmArgs {
    /// JSON or TOML scheduler state file
    #[arg(long, default_value = "sextant-swarm.json")]
    state: PathBuf,
    #[arg(long, default_value_t = 120)]
    lease_seconds: u64,
    #[command(subcommand)]
    action: SwarmAction,
}

#[derive(clap::Args)]
struct WorkerArgs {
    /// Fleet file containing the bot socket entries
    #[arg(long, default_value = "sextant-fleet.toml")]
    fleet: PathBuf,
    /// Bot name and scheduler worker identity
    #[arg(long)]
    worker: String,
    /// Shared JSON or TOML scheduler state
    #[arg(long, default_value = "sextant-swarm.json")]
    scheduler: PathBuf,
    #[arg(long, default_value_t = 120)]
    lease_seconds: u64,
    /// Claim at most one job, then exit
    #[arg(long)]
    once: bool,
}

#[derive(Subcommand)]
enum SwarmAction {
    /// Add a task-plan path to the queue
    Enqueue {
        id: String,
        task: String,
        #[arg(long, default_value_t = 0)]
        priority: u32,
        #[arg(long, default_value_t = 3)]
        max_attempts: u32,
    },
    /// Print all jobs and their lease state
    List,
    /// Claim the highest-priority available job for WORKER
    Claim { worker: String },
    /// Renew a worker's lease
    Renew { id: String, worker: String },
    /// Mark a leased job complete
    Complete { id: String, worker: String },
    /// Mark a leased job failed
    Fail {
        id: String,
        worker: String,
        error: String,
    },
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

#[derive(clap::Args)]
struct PortalArgs {
    #[arg(allow_negative_numbers = true)]
    x: i32,
    #[arg(allow_negative_numbers = true)]
    y: i32,
    #[arg(allow_negative_numbers = true)]
    z: i32,
    #[arg(long, default_value_t = 200)]
    timeout_ticks: u32,
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
                &fs::read_to_string(&cli.task_file)
                    .map_err(|e| format!("{}: {e}", cli.task_file.display()))?,
            )
            .map_err(|e| format!("{}: {e}", cli.task_file.display()))?;
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
            let _ = fs::remove_file(&cli.task_file);
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
        Cmd::Mount(MountArgs { id, dismount }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            if dismount {
                println!("{}", bot.dismount()?);
            } else {
                let id = id.ok_or("mount needs ENTITY_ID or --dismount")?;
                println!("{}", bot.mount(id)?);
            }
            Ok(())
        }
        Cmd::Elytra(ElytraArgs { action }) => {
            let mut bot = Bot::connect(
                cli.socket
                    .as_deref()
                    .ok_or("--socket: which bot (its control socket)")?,
            )?;
            println!("{}", bot.elytra(&action)?);
            Ok(())
        }
        Cmd::Task(TaskArgs { action }) => match action {
            TaskAction::Validate { file } => {
                let plan = tasks::load(&file)?;
                println!("valid task: {} step(s)", plan.steps.len());
                Ok(())
            }
            TaskAction::Run {
                file,
                state,
                restart,
            } => {
                let plan = tasks::load(&file)?;
                let state = state.unwrap_or_else(|| file.with_extension("state.json"));
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
                let result = tasks::run_resumable(
                    &mut bot,
                    &plan,
                    &trip,
                    &state,
                    restart,
                    |step, total, line| eprintln!("[{step}/{total}] {line}"),
                )?;
                println!(
                    "task {:?}: {}/{} steps ({} attempts)",
                    result.status, result.current_step, result.total_steps, result.attempts
                );
                Ok(())
            }
            TaskAction::Status { file, state } => {
                let path = state.unwrap_or_else(|| file.with_extension("state.json"));
                let result = tasks::load_state(&path)?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
                );
                Ok(())
            }
        },
        Cmd::Dashboard(DashboardArgs { fleet, bind }) => {
            let mut config = dashboard::load(&fleet)?;
            if let Some(bind) = bind {
                config.bind = bind;
            }
            dashboard::serve(config)
        }
        Cmd::Swarm(SwarmArgs {
            state,
            lease_seconds,
            action,
        }) => {
            let mut scheduler = swarm::Scheduler::open(&state, lease_seconds)?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            match action {
                SwarmAction::Enqueue {
                    id,
                    task,
                    priority,
                    max_attempts,
                } => {
                    scheduler.enqueue(id, task, priority, max_attempts)?;
                    println!("queued");
                }
                SwarmAction::List => println!(
                    "{}",
                    serde_json::to_string_pretty(&scheduler.jobs().collect::<Vec<_>>())
                        .map_err(|e| e.to_string())?
                ),
                SwarmAction::Claim { worker } => println!(
                    "{}",
                    serde_json::to_string_pretty(&scheduler.claim(&worker, now)?)
                        .map_err(|e| e.to_string())?
                ),
                SwarmAction::Renew { id, worker } => println!(
                    "{}",
                    serde_json::to_string_pretty(&scheduler.renew(&id, &worker, now)?)
                        .map_err(|e| e.to_string())?
                ),
                SwarmAction::Complete { id, worker } => println!(
                    "{}",
                    serde_json::to_string_pretty(&scheduler.complete(&id, &worker)?)
                        .map_err(|e| e.to_string())?
                ),
                SwarmAction::Fail { id, worker, error } => println!(
                    "{}",
                    serde_json::to_string_pretty(&scheduler.fail(&id, &worker, error)?)
                        .map_err(|e| e.to_string())?
                ),
            }
            Ok(())
        }
        Cmd::Worker(WorkerArgs {
            fleet,
            worker: worker_name,
            scheduler,
            lease_seconds,
            once,
        }) => {
            let config = dashboard::load(&fleet)?;
            let bot = config
                .bots
                .iter()
                .find(|bot| bot.name == worker_name)
                .ok_or_else(|| {
                    format!("worker {worker_name} is not present in {}", fleet.display())
                })?;
            let completed = worker::run(worker::Config {
                worker: &worker_name,
                bot,
                scheduler: &scheduler,
                lease_seconds,
                once,
                style: Style::casual(),
            })?;
            println!("worker {worker_name}: completed {completed} job(s)");
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
                &cli.task_file,
                toml::to_string(&TaskRecord { target: [x, y, z] }).map_err(|e| e.to_string())?,
            )
            .map_err(|e| format!("{}: {e}", cli.task_file.display()))?;
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
            let _ = fs::remove_file(&cli.task_file);
            Ok(())
        }
        Cmd::Portal(PortalArgs {
            x,
            y,
            z,
            timeout_ticks,
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
            let dimension = bot::portal(
                &mut bot,
                Goal {
                    pos: [x, y, z],
                    radius: cli.radius,
                },
                &trip,
                timeout_ticks,
            )?;
            println!("entered {dimension}");
            Ok(())
        }
    }
}
