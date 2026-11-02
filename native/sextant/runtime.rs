// In-game Sextant jobs. The injected module runs inside Horizon's client and reuses its existing
// agent pathfinder, break task, entity query, and input loop.
use super::{Game, agent::Handled, event_loop::EventLoopCtl};
use serde_json::Value;
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use winit::event_loop::ControlFlow;

struct NoopEventLoop;
impl EventLoopCtl for NoopEventLoop {
    fn exit(&self) {}
    fn set_control_flow(&self, _flow: ControlFlow) {}
}

#[derive(Clone, Copy)]
enum Phase { Find, Entities, Goto([i32; 3]), Break([i32; 3]) }

enum Job {
    Mine { block: String, left: u32, radius: i32, phase: Phase, reply: Option<Receiver<String>> },
    Explore { origin: [f64; 3], leg: u32, legs: u32, length: i32, reply: Option<Receiver<String>> },
    Follow { kind: Option<String>, left: u32, phase: Phase, reply: Option<Receiver<String>> },
}

thread_local! { static JOB: RefCell<Option<Job>> = const { RefCell::new(None) }; }

fn issue(game: &mut Game<'_>, words: &[String]) -> Option<Receiver<String>> {
    let refs = words.iter().map(String::as_str).collect::<Vec<_>>();
    let line = refs.join(" ");
    let (sender, receiver) = channel();
    match game.agent_command(&refs, &line, &sender, &NoopEventLoop) {
        Ok(Handled::Reply(value)) => { let _ = sender.send(value.to_string()); Some(receiver) }
        Ok(Handled::Deferred) => Some(receiver),
        Ok(Handled::No) => None,
        Err(error) => { log::warn!("Sextant: {error}"); None }
    }
}

fn find(game: &mut Game<'_>, block: &str, radius: i32) -> Option<Receiver<String>> {
    issue(game, &vec!["find-block".into(), block.into(), radius.to_string()])
}
fn entities(game: &mut Game<'_>, kind: Option<&str>) -> Option<Receiver<String>> {
    let mut words = vec!["entities".into(), "32".into()];
    if let Some(kind) = kind { words.push(kind.into()); }
    issue(game, &words)
}
fn goto(game: &mut Game<'_>, target: [i32; 3]) -> Option<Receiver<String>> {
    issue(game, &vec!["goto".into(), target[0].to_string(), target[1].to_string(), target[2].to_string(), "--near".into(), "2".into(), "--timeout".into(), "120".into()])
}
fn break_block(game: &mut Game<'_>, target: [i32; 3]) -> Option<Receiver<String>> {
    issue(game, &vec!["break".into(), target[0].to_string(), target[1].to_string(), target[2].to_string(), "--timeout".into(), "120".into()])
}

fn position(value: &Value, key: &str) -> Option<[i32; 3]> {
    let p = value[key].as_array()?;
    Some([p.first()?.as_i64()? as i32, p.get(1)?.as_i64()? as i32, p.get(2)?.as_i64()? as i32])
}
fn ok(text: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(text).ok()?;
    if value.get("error").is_some() { log::warn!("Sextant job failed: {}", value["error"]); None } else { Some(value) }
}

fn next_phase(game: &mut Game<'_>) {
    JOB.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(job) = slot.as_mut() else { return };
        match job {
            Job::Mine { block, radius, phase, reply, .. } => {
                *reply = match phase { Phase::Find => find(game, block, *radius), Phase::Goto(p) => goto(game, *p), Phase::Break(p) => break_block(game, *p), _ => None };
            }
            Job::Explore { origin, leg, length, reply, .. } => {
                let dirs = [[1, 0], [0, 1], [-1, 0], [0, -1]];
                let d = dirs[(*leg / 2 % 4) as usize];
                let distance = *length * (*leg / 2 + 1) as i32;
                let target = [(origin[0] + f64::from(d[0] * distance)).round() as i32, origin[1].round() as i32, (origin[2] + f64::from(d[1] * distance)).round() as i32];
                *reply = goto(game, target);
            }
            Job::Follow { kind, phase, reply, .. } => *reply = match phase { Phase::Entities => entities(game, kind.as_deref()), Phase::Goto(p) => goto(game, *p), _ => None },
        }
    });
}

fn finish(game: &mut Game<'_>, completed: bool) {
    JOB.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(job) = slot.as_mut() else { return };
        match job {
            Job::Mine { left, phase, .. } => if !completed || *left <= 1 { *slot = None } else { *left -= 1; *phase = Phase::Find; drop(slot); next_phase(game) },
            Job::Explore { leg, legs, .. } => if !completed || *leg + 1 >= *legs { *slot = None } else { *leg += 1; drop(slot); next_phase(game) },
            Job::Follow { left, phase, .. } => if !completed || *left <= 1 { *slot = None } else { *left -= 1; *phase = Phase::Entities; drop(slot); next_phase(game) },
        }
    });
}

fn poll(game: &mut Game<'_>) {
    let result = JOB.with(|slot| {
        let mut slot = slot.borrow_mut();
        let job = slot.as_mut()?;
        let receiver = match job { Job::Mine { reply, .. } | Job::Explore { reply, .. } | Job::Follow { reply, .. } => reply.take() }?;
        match receiver.try_recv() {
            Ok(text) => Some((text, true)),
            Err(TryRecvError::Empty) => { match job { Job::Mine { reply, .. } | Job::Explore { reply, .. } | Job::Follow { reply, .. } => *reply = Some(receiver) }; None }
            Err(TryRecvError::Disconnected) => Some((String::new(), false)),
        }
    });
    let Some((text, connected)) = result else { return };
    if !connected || ok(&text).is_none() { JOB.with(|slot| *slot.borrow_mut() = None); return }
    let value = ok(&text).unwrap();
    JOB.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(job) = slot.as_mut() else { return };
        match job {
            Job::Mine { phase, reply, .. } => match phase {
                Phase::Find => if let Some(p) = value["nearest"].as_array().and_then(|v| v.first()).and_then(|v| position(v, "pos")) { *phase = Phase::Goto(p); *reply = goto(game, p) } else { *slot = None },
                Phase::Goto(p) => { let target = *p; *phase = Phase::Break(target); *reply = break_block(game, target) },
                Phase::Break(_) => { drop(slot); finish(game, true) },
                _ => {},
            },
            Job::Explore { .. } => { drop(slot); finish(game, true) },
            Job::Follow { phase, reply, .. } => match phase {
                Phase::Entities => if let Some(p) = value["entities"].as_array().and_then(|v| v.first()).and_then(|v| position(v, "pos")) { *phase = Phase::Goto(p); *reply = goto(game, p) } else { *slot = None },
                Phase::Goto(_) => { drop(slot); finish(game, true) },
                _ => {},
            },
        }
    });
}

fn mapped(words: &[&str]) -> Option<Vec<String>> {
    let ["sextant", rest @ ..] = words else { return None };
    if rest.is_empty() || rest[0] == "help" { log::info!("Sextant: /sextant go X Y Z, mine BLOCK [COUNT] [RADIUS], explore LEGS LENGTH, follow [KIND] [ROUNDS], stop"); return Some(Vec::new()) }
    let mut out = rest.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if out[0] == "go" { out[0] = "goto".into() }
    if out[0] == "cancel" { out[0] = "stop".into() }
    Some(out)
}

pub(super) fn command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    let words = command.split_whitespace().collect::<Vec<_>>();
    let Some(words) = mapped(&words) else { return };
    *ran = true;
    if words.is_empty() { return }
    match words[0].as_str() {
        "mine" if words.len() >= 2 => { let left = words.get(2).and_then(|s| s.parse().ok()).unwrap_or(1); let radius = words.get(3).and_then(|s| s.parse().ok()).unwrap_or(48); JOB.with(|slot| *slot.borrow_mut() = Some(Job::Mine { block: words[1].clone(), left, radius, phase: Phase::Find, reply: None })); next_phase(game) }
        "explore" => { let legs = words.get(1).and_then(|s| s.parse().ok()).unwrap_or(8); let length = words.get(2).and_then(|s| s.parse().ok()).unwrap_or(32); let origin = game.player.as_ref().map_or([0.0; 3], |p| [p.position.x, p.position.y, p.position.z]); JOB.with(|slot| *slot.borrow_mut() = Some(Job::Explore { origin, leg: 0, legs, length, reply: None })); next_phase(game) }
        "follow" => { let kind = words.get(1).cloned(); let left = words.get(2).and_then(|s| s.parse().ok()).unwrap_or(20); JOB.with(|slot| *slot.borrow_mut() = Some(Job::Follow { kind, left, phase: Phase::Entities, reply: None })); next_phase(game) }
        _ => { let refs = words.iter().map(String::as_str).collect::<Vec<_>>(); let line = refs.join(" "); let (sender, _) = channel(); match game.agent_command(&refs, &line, &sender, &NoopEventLoop) { Ok(Handled::No) => *ran = false, Ok(_) => log::info!("Sextant: running {}", refs[0]), Err(error) => log::warn!("Sextant: {error}") } }
    }
}

pub(super) fn tick(game: &mut Game<'_>) { poll(game); }
