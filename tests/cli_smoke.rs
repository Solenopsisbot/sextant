use sextant::bot::Bot;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
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
        "worker",
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

#[test]
fn vehicle_and_elytra_use_primitive_horizon_commands() {
    let path = std::env::temp_dir().join(format!(
        "sextant-vehicle-controls-{}.sock",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind control socket");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept control client");
        let mut lines = BufReader::new(stream.try_clone().expect("clone control client"));
        let replies = [
            ("key shift tap", r#"{"ok":true}"#),
            ("jump", r#"{"ok":true}"#),
            ("select firework_rocket", r#"{"ok":true}"#),
            ("use-item", r#"{"ok":true}"#),
            ("state", r#"{"player":{"fall_flying":true}}"#),
        ];
        for (expected, reply) in replies {
            let mut line = String::new();
            lines.read_line(&mut line).expect("read control command");
            assert_eq!(line.trim(), expected);
            writeln!(stream, "{reply}").expect("write control reply");
        }
    });
    let mut bot = Bot::connect(&path).expect("connect control socket");
    bot.dismount().expect("dismount");
    bot.elytra("launch").expect("launch");
    bot.elytra("boost").expect("boost");
    assert_eq!(bot.elytra("status").expect("status")["fall_flying"], true);
    server.join().expect("control server thread");
    let _ = fs::remove_file(path);
}
