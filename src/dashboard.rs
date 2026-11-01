//! Optional local dashboard and WebSocket control plane for headless fleets.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BotConfig {
    pub name: String,
    pub socket: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FleetConfig {
    pub bots: Vec<BotConfig>,
    #[serde(default = "default_bind")]
    pub bind: String,
}
fn default_bind() -> String {
    "127.0.0.1:8765".into()
}

pub fn load(path: &std::path::Path) -> Result<FleetConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let fleet: FleetConfig =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if fleet.bots.is_empty() {
        return Err("fleet has no bots".into());
    }
    Ok(fleet)
}

pub fn serve(config: FleetConfig) -> Result<(), String> {
    let listener = TcpListener::bind(&config.bind).map_err(|e| format!("{}: {e}", config.bind))?;
    eprintln!("Sextant dashboard: http://{}", config.bind);
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let cfg = config.clone();
                std::thread::spawn(move || {
                    let _ = handle(&mut stream, &cfg);
                });
            }
            Err(e) => eprintln!("dashboard connection: {e}"),
        }
    }
    Ok(())
}

fn handle(stream: &mut TcpStream, config: &FleetConfig) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = [0u8; 64 * 1024];
    let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let head = request.split("\r\n\r\n").next().unwrap_or("");
    let mut lines = head.lines();
    let first = lines.next().unwrap_or("");
    let headers: BTreeMap<_, _> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    if first.starts_with("GET /ws") {
        let key = headers
            .get("sec-websocket-key")
            .ok_or("WebSocket key missing")?;
        let mut h = Sha1::new();
        h.update(key.as_bytes());
        h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept = STANDARD.encode(h.finalize());
        write!(stream, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").map_err(|e| e.to_string())?;
        websocket(stream, config)
    } else {
        let method = first.split_whitespace().next().unwrap_or("GET");
        let path = first.split_whitespace().nth(1).unwrap_or("/");
        let body = if path == "/" {
            DASHBOARD.into()
        } else if path.starts_with("/api/state") {
            state(config)
        } else if path.starts_with("/api/world") {
            world(config, path)
        } else if method == "POST" && path == "/api/command" {
            command(config, request.split("\r\n\r\n").nth(1).unwrap_or(""))
        } else {
            json!({"error":"not found"}).to_string()
        };
        let status = if body.contains("\"error\":\"not found\"") {
            "404 Not Found"
        } else {
            "200 OK"
        };
        write!(stream, "HTTP/1.1 {status}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{body}", if path == "/" { "text/html; charset=utf-8" } else { "application/json" }, body.len()).map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn snapshots(config: &FleetConfig) -> Value {
    let mut map = serde_json::Map::new();
    for bot in &config.bots {
        let value = crate::bot::Bot::connect(&bot.socket)
            .and_then(|mut b| b.cmd("state"))
            .unwrap_or_else(|e| json!({"error":e}));
        map.insert(bot.name.clone(), value);
    }
    Value::Object(map)
}
fn state(config: &FleetConfig) -> String {
    json!({"bots": snapshots(config), "capabilities": ["state", "command", "world", "websocket"]})
        .to_string()
}
fn world(config: &FleetConfig, path: &str) -> String {
    let name = path
        .split("bot=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .unwrap_or("");
    let Some(bot) = config.bots.iter().find(|b| b.name == name) else {
        return json!({"error":"unknown bot"}).to_string();
    };
    let value = crate::bot::Bot::connect(&bot.socket)
        .and_then(|mut b| {
            let s = b.cmd("state")?;
            let p = &s["player"]["position"];
            let x = p[0].as_f64().unwrap_or(0.0) as i32;
            let y = p[1].as_f64().unwrap_or(64.0) as i32;
            let z = p[2].as_f64().unwrap_or(0.0) as i32;
            b.cmd(&format!(
                "blocks {} {} {} {} {} {}",
                x - 16,
                y - 8,
                z - 16,
                x + 16,
                y + 8,
                z + 16
            ))
        })
        .unwrap_or_else(|e| json!({"error":e}));
    value.to_string()
}
fn command(config: &FleetConfig, body: &str) -> String {
    let req: Value = serde_json::from_str(body).unwrap_or_default();
    let name = req["bot"].as_str().unwrap_or("");
    let line = req["command"].as_str().unwrap_or("");
    let Some(bot) = config.bots.iter().find(|b| b.name == name) else {
        return json!({"error":"unknown bot"}).to_string();
    };
    crate::bot::Bot::connect(&bot.socket)
        .and_then(|mut b| b.cmd(line))
        .unwrap_or_else(|e| json!({"error":e}))
        .to_string()
}
fn websocket(stream: &mut TcpStream, config: &FleetConfig) -> Result<(), String> {
    write_frame(stream, &state(config).into_bytes())?;
    let mut frame = [0u8; 4096];
    loop {
        let n = match stream.read(&mut frame) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        if n < 2 {
            continue;
        }
        let len = (frame[1] & 0x7f) as usize;
        let mut at = 2;
        let mask = frame[1] & 0x80 != 0;
        let mut key = [0u8; 4];
        if mask {
            key.copy_from_slice(&frame[at..at + 4]);
            at += 4;
        }
        let end = at + len.min(frame.len() - at);
        let mut text = frame[at..end].to_vec();
        if mask {
            for (i, b) in text.iter_mut().enumerate() {
                *b ^= key[i % 4];
            }
        }
        if let Ok(req) = serde_json::from_slice::<Value>(&text) {
            let response = command(config, &req.to_string());
            write_frame(stream, response.as_bytes())?;
        }
        write_frame(stream, state(config).as_bytes())?;
    }
}
fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    let len = payload.len();
    let mut h = vec![0x81];
    if len < 126 {
        h.push(len as u8);
    } else if len <= u16::MAX as usize {
        h.push(126);
        h.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        h.push(127);
        h.extend_from_slice(&(len as u64).to_be_bytes());
    }
    stream
        .write_all(&h)
        .and_then(|_| stream.write_all(payload))
        .map_err(|e| e.to_string())
}

const DASHBOARD: &str = r#"<!doctype html><meta charset=utf-8><title>Sextant Fleet</title><style>body{font:14px system-ui;background:#10151c;color:#e8eef5;margin:0}header{padding:16px;background:#182431;position:sticky;top:0}main{display:grid;grid-template-columns:320px 1fr;gap:16px;padding:16px}button,input{padding:8px;background:#223344;color:#fff;border:1px solid #456;border-radius:4px}pre{white-space:pre-wrap}.bot{padding:10px;background:#182431;margin:6px 0;border-radius:5px}.ok{color:#7fda9b}canvas{width:100%;height:520px;background:#0b0f13;border:1px solid #345}</style><header><b>Sextant Fleet</b> <span id=conn>connecting…</span></header><main><section><div id=bots></div><input id=cmd placeholder='command, e.g. status'><button onclick=send()>Send</button><pre id=log></pre></section><section><canvas id=map width=900 height=520></canvas><p>Top-down loaded world view. Select a bot to inspect its state; commands are sent through the same authenticated control socket.</p></section></main><script>let ws,selected;const $=id=>document.getElementById(id);function draw(d){let c=$('map'),x=c.getContext('2d');x.clearRect(0,0,c.width,c.height);let a=Object.entries(d.bots||{});a.forEach(([n,v],i)=>{let p=v.player?.position||[0,0,0],px=c.width/2+p[0]*3,py=c.height/2+p[2]*3;x.fillStyle=['#7fda9b','#f4c95d','#df6b8b','#80bfff'][i%4];x.beginPath();x.arc(px,py,7,0,7);x.fill();x.fillText(n,px+10,py)});$('bots').innerHTML=a.map(([n,v])=>`<div class=bot onclick="selected='${n}'"><b>${n}</b> <span class=ok>${v.player?'online':'offline'}</span><br>${JSON.stringify(v.player?.position||v.error||{})}</div>`).join('')}function poll(){fetch('/api/state').then(r=>r.json()).then(d=>{draw(d);$('conn').textContent='live'}).catch(()=>$('conn').textContent='offline')}function send(){if(!selected)return $('log').textContent='select a bot first';let c=$('cmd').value;fetch('/api/command',{method:'POST',body:JSON.stringify({bot:selected,command:c})}).then(r=>r.text()).then(t=>$('log').textContent=t)}setInterval(poll,1000);poll();try{ws=new WebSocket(`ws://${location.host}/ws`);ws.onmessage=e=>{try{draw(JSON.parse(e.data))}catch{}}}catch{}</script>"#;
