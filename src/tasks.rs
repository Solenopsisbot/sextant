//! Declarative, resumable multi-step work for one bot.
use crate::bot::{self, Bot, Trip};
use crate::planner::Goal;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
        execute_step(bot, step, trip)?;
    }
    let _ = bot.cmd("chat [Sextant] task complete");
    progress(total, total, "complete");
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunState {
    pub plan: Option<String>,
    pub current_step: usize,
    pub total_steps: usize,
    pub status: RunStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub updated_unix: u64,
}

impl RunState {
    fn new(plan: &Plan) -> Self {
        Self {
            plan: plan.name.clone(),
            current_step: 0,
            total_steps: plan.steps.len(),
            status: RunStatus::Running,
            attempts: 0,
            last_error: None,
            updated_unix: now(),
        }
    }
}

pub fn load_state(path: &std::path::Path) -> Result<RunState, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Run from the last durable boundary. A process killed during a step retries that step on the
/// next invocation; completed steps are never repeated. State writes use rename so dashboard
/// readers never observe a half-written record.
pub fn run_resumable<F>(
    bot: &mut Bot,
    plan: &Plan,
    trip: &Trip<'_>,
    path: &std::path::Path,
    restart: bool,
    mut progress: F,
) -> Result<RunState, String>
where
    F: FnMut(usize, usize, &str),
{
    let mut state = if !restart && path.exists() {
        load_state(path)?
    } else {
        RunState::new(plan)
    };
    if state.total_steps != plan.steps.len() {
        return Err("task state belongs to a different plan".into());
    }
    if state.status == RunStatus::Succeeded {
        return Ok(state);
    }
    state.status = RunStatus::Running;
    state.last_error = None;
    save_state(path, &state)?;
    for index in state.current_step..plan.steps.len() {
        state.attempts = state.attempts.saturating_add(1);
        state.updated_unix = now();
        save_state(path, &state)?;
        let label_text = format!(
            "step {}/{}: {}",
            index + 1,
            plan.steps.len(),
            label(&plan.steps[index])
        );
        let _ = bot.cmd(&format!("chat [Sextant] {label_text}"));
        progress(index + 1, plan.steps.len(), &label_text);
        if let Err(error) = execute_step(bot, &plan.steps[index], trip) {
            state.status = RunStatus::Failed;
            state.last_error = Some(error.clone());
            state.updated_unix = now();
            save_state(path, &state)?;
            return Err(error);
        }
        state.current_step = index + 1;
        state.updated_unix = now();
        save_state(path, &state)?;
    }
    state.status = RunStatus::Succeeded;
    state.updated_unix = now();
    save_state(path, &state)?;
    let _ = bot.cmd("chat [Sextant] task complete");
    progress(plan.steps.len(), plan.steps.len(), "complete");
    Ok(state)
}

fn execute_step(bot: &mut Bot, step: &Step, trip: &Trip<'_>) -> Result<(), String> {
    match step {
        Step::Go { x, y, z } => bot::go(
            bot,
            Goal {
                pos: [*x, *y, *z],
                radius: 0.0,
            },
            trip,
            |_| {},
        ),
        Step::Chat { text } => bot.cmd(&format!("chat {text}")).map(|_| ()),
        Step::Wait { ticks } => bot.cmd(&format!("wait {ticks}")).map(|_| ()),
        Step::Mine {
            blocks,
            count,
            search_radius,
        } => bot::mine(bot, blocks, *count, *search_radius, trip, |_| {}).map(|_| ()),
        Step::Craft { recipe, times } => bot.craft(recipe, *times).map(|_| ()),
        Step::Farm {
            crops,
            search_radius,
            rounds,
        } => bot::farm(bot, crops, *search_radius, *rounds, trip, |_| {}).map(|_| ()),
        Step::Status => bot.cmd("state").map(|_| ()),
    }
}

fn save_state(path: &std::path::Path, state: &RunState) -> Result<(), String> {
    let temp = path.with_extension("tmp");
    let text = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
    std::fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|e| format!("{}: {e}", path.display()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
