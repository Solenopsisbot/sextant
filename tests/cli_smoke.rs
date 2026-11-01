use std::fs;
use std::process::Command;

fn run(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sextant"))
        .args(args)
        .output()
        .expect("sextant binary should run");
    assert!(
        out.status.success(),
        "{}: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("help output is UTF-8")
}

#[test]
fn help_lists_every_top_level_operation() {
    let help = run(&["--help"]);
    for command in [
        "go",
        "portal",
        "plan",
        "styles",
        "stop",
        "pause",
        "resume",
        "eta",
        "status",
        "capabilities",
        "mount",
        "elytra",
        "craft",
        "mine",
        "waypoint",
        "build",
        "follow",
        "explore",
        "tunnel",
        "surface",
        "axis",
        "find",
        "invert",
        "farm",
        "task",
        "dashboard",
        "swarm",
    ] {
        assert!(
            help.contains(command),
            "missing {command} in CLI help: {help}"
        );
    }
}

#[test]
fn styles_is_available_without_a_socket() {
    let output = run(&["styles"]);
    assert!(output.contains("name = \"robot\""));
    assert!(output.contains("name = \"player-like\""));
}

#[test]
fn swarm_cli_persists_and_reassigns_work() {
    let path = std::env::temp_dir().join(format!("sextant-cli-swarm-{}.json", std::process::id()));
    let path_arg = path.to_string_lossy().to_string();
    let queued = run(&[
        "swarm",
        "--state",
        &path_arg,
        "enqueue",
        "mine",
        "mine.toml",
        "--priority",
        "10",
    ]);
    assert!(queued.contains("queued"));
    let claimed = run(&["swarm", "--state", &path_arg, "claim", "Miner"]);
    assert!(claimed.contains("Miner"));
    let listed = run(&["swarm", "--state", &path_arg, "list"]);
    assert!(listed.contains("running"));
    let _ = fs::remove_file(path);
}
