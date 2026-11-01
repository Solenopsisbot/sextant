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
    /// Optional resource-pack root. This can be a pack directory or a Horizon prepared-resource
    /// directory containing `assets/<namespace>/textures/block/*.png`.
    #[serde(default)]
    pub textures: Option<PathBuf>,
    /// Optional bearer token for API, texture, and WebSocket access. The root HTML remains public
    /// so a browser can load the login shell before supplying the token.
    #[serde(default)]
    pub auth_token: Option<String>,
    /// Optional scheduler state file surfaced in fleet snapshots.
    #[serde(default)]
    pub scheduler: Option<PathBuf>,
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
    let request_target = first.split_whitespace().nth(1).unwrap_or("/");
    let protected = request_target != "/";
    if protected && !authorized(config, request_target, &headers) {
        return write_http(
            stream,
            "401 Unauthorized",
            "text/plain",
            b"Sextant token required",
        );
    }
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
        if method == "GET" && path.starts_with("/textures/") {
            return texture(stream, config, path);
        }
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

fn authorized(config: &FleetConfig, target: &str, headers: &BTreeMap<String, String>) -> bool {
    let Some(expected) = config
        .auth_token
        .as_deref()
        .filter(|token| !token.is_empty())
    else {
        return true;
    };
    headers
        .get("x-sextant-token")
        .is_some_and(|token| token == expected)
        || target
            .split("token=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .is_some_and(|token| token == expected)
}

fn texture(stream: &mut TcpStream, config: &FleetConfig, request_path: &str) -> Result<(), String> {
    let Some(root) = config.textures.as_ref() else {
        return write_http(
            stream,
            "404 Not Found",
            "text/plain",
            b"textures are not configured",
        );
    };
    let clean_path = request_path.split('?').next().unwrap_or(request_path);
    let relative = clean_path.strip_prefix("/textures/").unwrap_or("");
    if relative.is_empty()
        || relative.contains("..")
        || relative.contains('\\')
        || !relative.ends_with(".png")
    {
        return write_http(
            stream,
            "400 Bad Request",
            "text/plain",
            b"invalid texture path",
        );
    }
    let relative = relative.strip_suffix(".png").unwrap_or(relative);
    let mut pieces = relative.splitn(2, '/');
    let namespace = pieces.next().unwrap_or("minecraft");
    let name = pieces.next().unwrap_or("");
    if name.is_empty() || name.contains('/') {
        return write_http(
            stream,
            "400 Bad Request",
            "text/plain",
            b"invalid texture name",
        );
    }
    let assets = root.join("assets");
    let base = if assets.is_dir() {
        assets
    } else {
        root.clone()
    };
    let file = base
        .join(namespace)
        .join("textures")
        .join("block")
        .join(format!("{name}.png"));
    match std::fs::read(file) {
        Ok(bytes) => write_http_with_cache(stream, "200 OK", "image/png", &bytes),
        Err(_) => write_http(stream, "404 Not Found", "image/png", &[]),
    }
}

fn write_http(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    write_http_with_cache(stream, status, content_type, body)
}

fn write_http_with_cache(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: public, max-age=3600\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())
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
    json!({"bots": snapshots(config), "jobs": jobs(config), "capabilities": ["state", "command", "world", "websocket", "jobs"]})
        .to_string()
}
fn jobs(config: &FleetConfig) -> Value {
    let Some(path) = config.scheduler.as_ref() else {
        return Value::Array(Vec::new());
    };
    match crate::swarm::Scheduler::open(path, 120) {
        Ok(scheduler) => serde_json::to_value(scheduler.jobs().collect::<Vec<_>>())
            .unwrap_or(Value::Array(Vec::new())),
        Err(error) => json!({"error": error}),
    }
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
            let mut blocks = b.cmd(&format!(
                "blocks {} {} {} {} {} {}",
                x - 16,
                y - 8,
                z - 16,
                x + 16,
                y + 8,
                z + 16
            ))?;
            if let Ok(entities) = b.cmd("entities 48")
                && let Some(object) = blocks.as_object_mut()
            {
                object.insert("entities".into(), entities["entities"].clone());
            }
            Ok(blocks)
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

const DASHBOARD: &str = r#"<!doctype html>
<meta charset="utf-8"><title>Sextant Spectator</title>
<style>
:root{color-scheme:dark}body{font:14px system-ui;background:#10151c;color:#e8eef5;margin:0}
header{padding:14px 18px;background:#182431;position:sticky;top:0;z-index:2;display:flex;gap:20px;align-items:center}
main{display:grid;grid-template-columns:300px 1fr;gap:14px;padding:14px}.panel{background:#182431;border:1px solid #2d4356;border-radius:6px;padding:12px}
button,input,select{padding:7px;background:#223344;color:#fff;border:1px solid #456;border-radius:4px}button{cursor:pointer}
.bot{padding:9px;background:#203040;margin:6px 0;border-radius:5px;cursor:pointer}.bot.selected{outline:2px solid #7fda9b}.ok{color:#7fda9b}.muted{color:#9db0c0}
canvas{width:100%;height:calc(100vh - 120px);min-height:460px;background:#0b0f13;border:1px solid #345;border-radius:5px;image-rendering:pixelated}
.toolbar{display:flex;gap:7px;align-items:center;flex-wrap:wrap;margin-bottom:8px}pre{white-space:pre-wrap;max-height:180px;overflow:auto}
</style>
<header><b>Sextant Spectator</b><span id="conn" class="muted">connecting…</span><span id="selected" class="muted">no bot selected</span></header>
<main><section class="panel"><div id="bots"></div><div id="jobs"></div><div class="toolbar"><label>Layer <input id="layer" type="number" value="64" style="width:65px"></label><button onclick="refreshWorld()">Refresh world</button></div><div class="toolbar"><input id="cmd" placeholder="status or command"><button onclick="send()">Send</button></div><pre id="log"></pre><p class="muted">Click a bot to follow it. The renderer uses loaded block state data from Horizon and never writes saves.</p></section>
<section class="panel"><canvas id="map" width="1200" height="760"></canvas><div class="muted">Isometric spectator view · scroll/zoom is intentionally kept deterministic for headless monitoring.</div></section></main>
<script>
let ws,selected=null,lastState=null,lastWorld=null;const textures=new Map();const $=id=>document.getElementById(id);const token=localStorage.getItem('sextantToken')||prompt('Sextant token (leave blank if disabled)')||'';if(token)localStorage.setItem('sextantToken',token);
const colors={grass:'#6ba34b',dirt:'#8a5a3b',stone:'#777',cobble:'#666',wood:'#9a6b3f',log:'#76502f',planks:'#b18855',sand:'#d9c27b',glass:'#9bd7e5',water:'#3c79c9',lava:'#d85d25',leaves:'#3e7d45',ore:'#9ca6af',brick:'#98584c',obsidian:'#252039',snow:'#e8f2f7',ice:'#9fd7e8'};
function color(state){let s=(state||'').toLowerCase();for(const [k,v] of Object.entries(colors))if(s.includes(k))return v;let h=0;for(const c of s)h=(h*31+c.charCodeAt(0))>>>0;return `hsl(${h%360} 32% 45%)`}
function authQuery(){return token?'?token='+encodeURIComponent(token):''}function api(path,options={}){return fetch(path,{...options,headers:{...(options.headers||{}),...(token?{'X-Sextant-Token':token}:{})}})}
function textureUrl(state){let n=(state||'').split('[')[0];return '/textures/'+n+'.png'+authQuery()}
function texture(state){let url=textureUrl(state),img=textures.get(url);if(img===undefined){img=new Image();textures.set(url,img);img.onload=draw;img.onerror=()=>{textures.set(url,null);draw()};img.src=url}return img}
function pos(v){return v?.player?.position||[0,0,0]}
function draw(){let c=$('map'),g=c.getContext('2d');g.clearRect(0,0,c.width,c.height);g.save();g.translate(c.width/2,c.height/2+80);let scale=12;let layer=Number($('layer').value)||64;for(const b of (lastWorld?.blocks||[])){if(b[1]!==layer)continue;let dx=b[0]-(lastWorld.from?.[0]||0),dz=b[2]-(lastWorld.from?.[2]||0);let sx=(dx-dz)*scale*.5,sy=(dx+dz)*scale*.25;let img=texture(b[3]);g.beginPath();g.moveTo(sx,sy);g.lineTo(sx+scale*.5,sy+scale*.25);g.lineTo(sx,sy+scale*.5);g.lineTo(sx-scale*.5,sy+scale*.25);g.closePath();if(img&&img.complete&&img.naturalWidth){g.save();g.clip();g.drawImage(img,sx-scale*.5,sy,scale,scale*.5);g.restore()}else{g.fillStyle=color(b[3]);g.fill();}}
for(const [name,v] of Object.entries(lastState?.bots||{})){let p=pos(v),dx=p[0]-(lastWorld?.from?.[0]||0),dz=p[2]-(lastWorld?.from?.[2]||0);let sx=(dx-dz)*scale*.5,sy=(dx+dz)*scale*.25-(p[1]-layer)*scale*.45;g.fillStyle='#7fda9b';g.beginPath();g.arc(sx,sy,7,0,7);g.fill();g.fillStyle='#fff';g.fillText(name,sx+9,sy)}
for(const e of (lastWorld?.entities||[])){let p=e.pos||[0,0,0];if(Math.abs(p[1]-layer)>2)continue;let dx=p[0]-(lastWorld?.from?.[0]||0),dz=p[2]-(lastWorld?.from?.[2]||0),sx=(dx-dz)*scale*.5,sy=(dx+dz)*scale*.25-(p[1]-layer)*scale*.45;g.fillStyle='#f06b9a';g.beginPath();g.moveTo(sx,sy-6);g.lineTo(sx+5,sy+5);g.lineTo(sx-5,sy+5);g.closePath();g.fill();}g.restore();}
function drawBots(d){lastState=d;let a=Object.entries(d.bots||{});$('bots').innerHTML=a.map(([n,v])=>`<div class="bot ${selected===n?'selected':''}" onclick="selectBot('${n}')"><b>${n}</b> <span class="ok">${v.player?'online':'offline'}</span><br><span class="muted">${JSON.stringify(pos(v))}</span></div>`).join('');$('jobs').innerHTML=(d.jobs||[]).map(j=>`<div class="muted">${j.id}: ${j.status}${j.worker?' · '+j.worker:''}</div>`).join('');if(selected&&d.bots[selected]?.player){$('layer').value=Math.floor(pos(d.bots[selected])[1])}draw()}
function selectBot(n){selected=n;$('selected').textContent=n;drawBots(lastState||{bots:{}});refreshWorld()}
function poll(){api('/api/state').then(r=>r.json()).then(d=>{drawBots(d);$('conn').textContent='live'}).catch(()=>$('conn').textContent='offline')}
function refreshWorld(){if(!selected)return;api('/api/world?bot='+encodeURIComponent(selected)).then(r=>r.json()).then(w=>{lastWorld=w;draw()}).catch(e=>$('log').textContent=e)}
function send(){if(!selected)return $('log').textContent='select a bot first';let c=$('cmd').value;api('/api/command',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({bot:selected,command:c})}).then(r=>r.text()).then(t=>{$('log').textContent=t;poll()})}
$('layer').addEventListener('input',draw);setInterval(()=>{poll();refreshWorld()},1500);poll();try{ws=new WebSocket(`ws://${location.host}/ws${authQuery()}`);ws.onmessage=e=>{try{let d=JSON.parse(e.data);if(d.bots)drawBots(d)}catch{}}}catch{}
</script>"#;
