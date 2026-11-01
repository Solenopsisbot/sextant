//! `sextant`: walk a Horizon bot to a place, in a style from robot to player-like.
//!
//! ```sh
//! sextant --socket .flotilla/ctl-Miner.sock go 120 64 -40 --style player-like
//! sextant --socket ... plan 120 64 -40        # print the route without moving
//! sextant styles                               # the presets and their knobs
//! ```

use clap::{Parser, Subcommand};
use sextant::bot::{self, Bot};
use sextant::planner::{Goal, Settings};
use sextant::style::Style;
use std::path::PathBuf;

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
            let (path, read) = bot.plan_loaded(me.feet, goal, &settings)?;
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
            Ok(())
        }
    }
}
