//! Autonomous worker loop for a single bot in a shared Sextant swarm.
use crate::bot::{Bot, Trip};
use crate::dashboard::BotConfig;
use crate::planner::Settings;
use crate::style::Style;
use crate::swarm::Scheduler;
use crate::tasks;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Config<'a> {
    pub worker: &'a str,
    pub bot: &'a BotConfig,
    pub scheduler: &'a Path,
    pub lease_seconds: u64,
    pub once: bool,
    pub style: Style,
}

/// Claim and execute work until the queue is empty, or once when used by a supervisor/cron job.
/// A heartbeat renews the lease independently of task execution, so long paths do not get
/// reassigned while the bot is still alive.
pub fn run(config: Config<'_>) -> Result<usize, String> {
    let mut completed = 0;
    loop {
        let current_time = now();
        let mut scheduler = Scheduler::open(config.scheduler, config.lease_seconds)?;
        let Some(job) = scheduler.claim(config.worker, current_time)? else {
            return Ok(completed);
        };
        let alive = Arc::new(AtomicBool::new(true));
        let heartbeat_alive = Arc::clone(&alive);
        let scheduler_path = config.scheduler.to_owned();
        let worker = config.worker.to_owned();
        let job_id = job.id.clone();
        let lease = config.lease_seconds.max(3);
        let heartbeat = thread::spawn(move || {
            while heartbeat_alive.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs((lease / 3).max(1)));
                if !heartbeat_alive.load(Ordering::Relaxed) {
                    break;
                }
                if let Ok(mut scheduler) = Scheduler::open(&scheduler_path, lease) {
                    let _ = scheduler.renew(&job_id, &worker, now());
                }
            }
        });
        let result = execute_job(&job.task, config.bot, &config.style, config.scheduler);
        alive.store(false, Ordering::Relaxed);
        let _ = heartbeat.join();
        let mut finish = Scheduler::open(config.scheduler, config.lease_seconds)?;
        match result {
            Ok(()) => {
                finish.complete(&job.id, config.worker)?;
                completed += 1;
            }
            Err(error) => {
                finish.retry(&job.id, config.worker, error.clone())?;
                eprintln!("worker {}: job {} failed: {error}", config.worker, job.id);
            }
        }
        if config.once {
            return Ok(completed);
        }
    }
}

fn execute_job(
    task_path: &str,
    bot_config: &BotConfig,
    style: &Style,
    scheduler_path: &Path,
) -> Result<(), String> {
    let plan_path = Path::new(task_path);
    let plan = tasks::load(plan_path)?;
    let mut bot = Bot::connect(&bot_config.socket)?;
    let trip = Trip {
        settings: Settings {
            sprint: style.sprint,
            ..Settings::default()
        },
        style,
        seed: 1,
        verbose: false,
        block: "cobblestone".into(),
        tool: None,
    };
    let state = plan_path.with_extension("state.json");
    tasks::run_resumable(
        &mut bot,
        &plan,
        &trip,
        &state,
        false,
        |step, total, line| {
            eprintln!("{} {step}/{total}: {line}", bot_config.name);
        },
    )
    .map(|_| ())
    .map_err(|error| format!("{} (scheduler {})", error, scheduler_path.display()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_uses_a_safe_worker_style() {
        let style = Style::casual();
        assert!(style.sprint);
    }
}
