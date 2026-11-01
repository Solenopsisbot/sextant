//! Declarative, resumable multi-step work for one bot.
use crate::bot::{self, Bot, Trip};
use crate::planner::Goal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub name: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Step {
    Go {
        x: i32,
        y: i32,
        z: i32,
    },
    Chat {
        text: String,
    },
    Wait {
        ticks: u32,
    },
    Mine {
        blocks: Vec<String>,
        #[serde(default = "one")]
        count: u32,
        #[serde(default = "radius")]
        search_radius: i32,
    },
    Craft {
        recipe: String,
        #[serde(default = "one")]
        times: u32,
    },
    Farm {
        crops: Vec<String>,
        #[serde(default = "radius")]
        search_radius: i32,
        #[serde(default = "one")]
        rounds: u32,
    },
    Status,
}

fn one() -> u32 {
    1
}
fn radius() -> i32 {
    64
}

pub fn load(path: &std::path::Path) -> Result<Plan, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let plan: Plan = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if plan.steps.is_empty() {
        return Err("task plan has no steps".into());
    }
    Ok(plan)
}

/// Execute a plan in order. Every step emits a compact progress line; callers can mirror it to
/// chat, a terminal, or a WebSocket without coupling the runner to a UI.
pub fn run<F>(bot: &mut Bot, plan: &Plan, trip: &Trip<'_>, mut progress: F) -> Result<(), String>
where
    F: FnMut(usize, usize, &str),
{
    let total = plan.steps.len();
    for (index, step) in plan.steps.iter().enumerate() {
        let label = format!("step {}/{}: {}", index + 1, total, label(step));
        // The same progress is visible in an attached game, which makes long headless jobs
        // understandable when a player is watching the world rather than the terminal.
        let _ = bot.cmd(&format!("chat [Sextant] {label}"));
        progress(index + 1, total, &label);
        match step {
            Step::Go { x, y, z } => bot::go(
                bot,
                Goal {
                    pos: [*x, *y, *z],
                    radius: 0.0,
                },
                trip,
                |_| {},
            )?,
            Step::Chat { text } => {
                bot.cmd(&format!("chat {text}"))?;
            }
            Step::Wait { ticks } => {
                bot.cmd(&format!("wait {ticks}"))?;
            }
            Step::Mine {
                blocks,
                count,
                search_radius,
            } => {
                bot::mine(bot, blocks, *count, *search_radius, trip, |_| {})?;
            }
            Step::Craft { recipe, times } => {
                bot.craft(recipe, *times)?;
            }
            Step::Farm {
                crops,
                search_radius,
                rounds,
            } => {
                bot::farm(bot, crops, *search_radius, *rounds, trip, |_| {})?;
            }
            Step::Status => {
                bot.cmd("state")?;
            }
        }
    }
    let _ = bot.cmd("chat [Sextant] task complete");
    progress(total, total, "complete");
    Ok(())
}

fn label(step: &Step) -> &'static str {
    match step {
        Step::Go { .. } => "go",
        Step::Chat { .. } => "chat",
        Step::Wait { .. } => "wait",
        Step::Mine { .. } => "mine",
        Step::Craft { .. } => "craft",
        Step::Farm { .. } => "farm",
        Step::Status => "status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ordered_steps() {
        let p: Plan = toml::from_str("name='build'\n[[steps]]\naction='chat'\ntext='hello'\n[[steps]]\naction='go'\nx=1\ny=64\nz=-2").unwrap();
        assert_eq!(p.steps.len(), 2);
        assert!(matches!(p.steps[1], Step::Go { x: 1, y: 64, z: -2 }));
    }
}
