// In-game Sextant command bridge. The injected module runs inside Horizon's client crate and
// delegates movement to the same agent pathfinder and tick tasks that horizon-ctl uses.
use super::{Game, agent::Handled, event_loop::EventLoopCtl};
use std::sync::mpsc::channel;
use winit::event_loop::ControlFlow;

struct NoopEventLoop;

impl EventLoopCtl for NoopEventLoop {
    fn exit(&self) {}

    fn set_control_flow(&self, _flow: ControlFlow) {}
}

fn mapped_words(words: &[&str]) -> Option<Vec<String>> {
    let ["sextant", rest @ ..] = words else {
        return None;
    };
    if rest.is_empty() || rest[0] == "help" {
        log::info!("Sextant: /sextant go X Y Z, /sextant stop, /sextant jump, /sextant break, /sextant place");
        return Some(Vec::new());
    }
    let mut mapped = rest.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
    match mapped[0].as_str() {
        "go" => mapped[0] = "goto".into(),
        "cancel" => mapped[0] = "stop".into(),
        _ => {}
    }
    Some(mapped)
}

pub(super) fn command(game: &mut Game<'_>, command: &str, ran: &mut bool) {
    let words = command.split_whitespace().collect::<Vec<_>>();
    let Some(mapped) = mapped_words(&words) else {
        return;
    };
    *ran = true;
    if mapped.is_empty() {
        return;
    }
    let refs = mapped.iter().map(String::as_str).collect::<Vec<_>>();
    let line = refs.join(" ");
    let (reply, _replies) = channel();
    match game.agent_command(&refs, &line, &reply, &NoopEventLoop) {
        Ok(Handled::Reply(value)) => log::info!("Sextant: {value}"),
        Ok(Handled::Deferred) => log::info!("Sextant: running {}", refs[0]),
        Ok(Handled::No) => {
            *ran = false;
        }
        Err(error) => log::warn!("Sextant: {error}"),
    }
}
