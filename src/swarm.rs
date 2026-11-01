//! Durable work scheduling for a Sextant fleet.
//!
//! The scheduler is deliberately independent of sockets and UI. A dashboard or a worker process
//! can open the same state file, claim a job with a lease, renew it while doing work, and complete
//! or fail it. Expired leases are returned to the queue deterministically, allowing another bot to
//! take over after a crash.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    /// The task-plan path or opaque task payload consumed by the worker.
    pub task: String,
    #[serde(default)]
    pub priority: u32,
    #[serde(default = "queued")]
    pub status: Status,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default = "max_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub lease_until: u64,
    #[serde(default)]
    pub error: Option<String>,
}

fn queued() -> Status {
    Status::Queued
}
fn max_attempts() -> u32 {
    3
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    #[serde(default = "schema")]
    schema: u32,
    #[serde(default)]
    jobs: BTreeMap<String, Job>,
}
fn schema() -> u32 {
    1
}

pub struct Scheduler {
    path: PathBuf,
    lease_seconds: u64,
    state: State,
}

impl Scheduler {
    pub fn open(path: impl AsRef<Path>, lease_seconds: u64) -> Result<Self, String> {
        let path = path.as_ref().to_owned();
        let state = if path.exists() {
            read_state(&path)?
        } else {
            State {
                schema: 1,
                jobs: BTreeMap::new(),
            }
        };
        if state.schema != 1 {
            return Err(format!("unsupported scheduler schema {}", state.schema));
        }
        Ok(Self {
            path,
            lease_seconds: lease_seconds.max(1),
            state,
        })
    }

    pub fn jobs(&self) -> impl Iterator<Item = &Job> {
        self.state.jobs.values()
    }

    pub fn job(&self, id: &str) -> Option<&Job> {
        self.state.jobs.get(id)
    }

    pub fn enqueue(
        &mut self,
        id: impl Into<String>,
        task: impl Into<String>,
        priority: u32,
        max_attempts: u32,
    ) -> Result<(), String> {
        let id = id.into();
        if id.is_empty() || self.state.jobs.contains_key(&id) {
            return Err(format!("job {id:?} is empty or already exists"));
        }
        self.state.jobs.insert(
            id.clone(),
            Job {
                id,
                task: task.into(),
                priority,
                status: Status::Queued,
                worker: None,
                attempts: 0,
                max_attempts: max_attempts.max(1),
                lease_until: 0,
                error: None,
            },
        );
        self.persist()
    }

    /// Move expired work back to the queue, or permanently fail it after its retry budget.
    pub fn reassign_expired(&mut self, now: u64) -> Result<usize, String> {
        let mut changed = 0;
        for job in self.state.jobs.values_mut() {
            if job.status != Status::Running || job.lease_until > now {
                continue;
            }
            job.worker = None;
            job.lease_until = 0;
            if job.attempts < job.max_attempts {
                job.status = Status::Queued;
                job.error = Some("worker lease expired; reassigned".into());
            } else {
                job.status = Status::Failed;
                job.error = Some("worker lease expired; retry budget exhausted".into());
            }
            changed += 1;
        }
        if changed > 0 {
            self.persist()?;
        }
        Ok(changed)
    }

    /// Claim the highest-priority queued job. Ties are resolved by job id for stable swarm work.
    pub fn claim(&mut self, worker: &str, now: u64) -> Result<Option<Job>, String> {
        self.reassign_expired(now)?;
        let id = self
            .state
            .jobs
            .values()
            .filter(|j| j.status == Status::Queued)
            .max_by(|a, b| a.priority.cmp(&b.priority).then_with(|| b.id.cmp(&a.id)))
            .map(|j| j.id.clone());
        let Some(id) = id else { return Ok(None) };
        let job = self.state.jobs.get_mut(&id).expect("selected job exists");
        job.status = Status::Running;
        job.worker = Some(worker.to_owned());
        job.attempts += 1;
        job.lease_until = now.saturating_add(self.lease_seconds);
        job.error = None;
        let result = job.clone();
        self.persist()?;
        Ok(Some(result))
    }

    pub fn renew(&mut self, id: &str, worker: &str, now: u64) -> Result<Job, String> {
        let job = self
            .state
            .jobs
            .get_mut(id)
            .ok_or_else(|| format!("unknown job {id}"))?;
        if job.status != Status::Running || job.worker.as_deref() != Some(worker) {
            return Err(format!("job {id} is not leased to {worker}"));
        }
        job.lease_until = now.saturating_add(self.lease_seconds);
        let result = job.clone();
        self.persist()?;
        Ok(result)
    }

    pub fn complete(&mut self, id: &str, worker: &str) -> Result<Job, String> {
        let job = self
            .state
            .jobs
            .get_mut(id)
            .ok_or_else(|| format!("unknown job {id}"))?;
        check_owner(job, worker)?;
        job.status = Status::Succeeded;
        job.worker = None;
        job.lease_until = 0;
        job.error = None;
        let result = job.clone();
        self.persist()?;
        Ok(result)
    }

    pub fn fail(
        &mut self,
        id: &str,
        worker: &str,
        error: impl Into<String>,
    ) -> Result<Job, String> {
        let job = self
            .state
            .jobs
            .get_mut(id)
            .ok_or_else(|| format!("unknown job {id}"))?;
        check_owner(job, worker)?;
        job.status = Status::Failed;
        job.worker = None;
        job.lease_until = 0;
        job.error = Some(error.into());
        let result = job.clone();
        self.persist()?;
        Ok(result)
    }

    /// Return a failed attempt to the queue while retry budget remains. Workers use this for
    /// actionable task errors; lease expiry is handled separately by `reassign_expired`.
    pub fn retry(
        &mut self,
        id: &str,
        worker: &str,
        error: impl Into<String>,
    ) -> Result<Job, String> {
        let job = self
            .state
            .jobs
            .get_mut(id)
            .ok_or_else(|| format!("unknown job {id}"))?;
        check_owner(job, worker)?;
        job.worker = None;
        job.lease_until = 0;
        job.error = Some(error.into());
        job.status = if job.attempts < job.max_attempts {
            Status::Queued
        } else {
            Status::Failed
        };
        let result = job.clone();
        self.persist()?;
        Ok(result)
    }

    fn persist(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let text = if self.path.extension().and_then(|x| x.to_str()) == Some("toml") {
            toml::to_string_pretty(&self.state).map_err(|e| e.to_string())?
        } else {
            serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())?
        };
        let temp = self.path.with_extension("tmp");
        fs::write(&temp, text).map_err(|e| format!("{}: {e}", temp.display()))?;
        fs::rename(&temp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn check_owner(job: &Job, worker: &str) -> Result<(), String> {
    if job.status != Status::Running || job.worker.as_deref() != Some(worker) {
        Err(format!("job {} is not leased to {worker}", job.id))
    } else {
        Ok(())
    }
}

fn read_state(path: &Path) -> Result<State, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if path.extension().and_then(|x| x.to_str()) == Some("toml") {
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    } else {
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(ext: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sextant-swarm-{}-{ext}", std::process::id()))
    }

    #[test]
    fn claims_priority_deterministically_and_persists_json() {
        let path = path("json");
        let _ = fs::remove_file(&path);
        let mut scheduler = Scheduler::open(&path, 10).unwrap();
        scheduler.enqueue("b", "second", 1, 2).unwrap();
        scheduler.enqueue("a", "first", 1, 2).unwrap();
        assert_eq!(scheduler.claim("worker", 100).unwrap().unwrap().id, "a");
        drop(scheduler);
        let scheduler = Scheduler::open(&path, 10).unwrap();
        assert_eq!(scheduler.job("a").unwrap().status, Status::Running);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn expired_lease_is_reassigned_then_can_complete_from_toml() {
        let path = path("toml");
        let _ = fs::remove_file(&path);
        let mut scheduler = Scheduler::open(&path, 5).unwrap();
        scheduler.enqueue("job", "task.toml", 0, 2).unwrap();
        scheduler.claim("one", 10).unwrap();
        assert_eq!(scheduler.reassign_expired(15).unwrap(), 1);
        let job = scheduler.claim("two", 15).unwrap().unwrap();
        assert_eq!(job.attempts, 2);
        scheduler.complete("job", "two").unwrap();
        assert_eq!(scheduler.job("job").unwrap().status, Status::Succeeded);
        drop(scheduler);
        let scheduler = Scheduler::open(&path, 5).unwrap();
        assert_eq!(scheduler.job("job").unwrap().status, Status::Succeeded);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn expired_retry_budget_becomes_failed() {
        let path = path("budget.json");
        let _ = fs::remove_file(&path);
        let mut scheduler = Scheduler::open(&path, 1).unwrap();
        scheduler.enqueue("job", "task", 0, 1).unwrap();
        scheduler.claim("one", 1).unwrap();
        scheduler.reassign_expired(2).unwrap();
        assert_eq!(scheduler.job("job").unwrap().status, Status::Failed);
        let _ = fs::remove_file(path);
    }
}
