//! Optional local dashboard and WebSocket control plane for headless fleets.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
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
    /// Optional file containing the current token. Reloaded for every request for rotation.
    #[serde(default)]
    pub auth_token_file: Option<PathBuf>,
    /// Requests per minute per remote peer; defaults to 120.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
    /// Optional scheduler state file surfaced in fleet snapshots.
    #[serde(default)]
    pub scheduler: Option<PathBuf>,
}
fn default_bind() -> String {
    "127.0.0.1:8765".into()
}
fn default_rate_limit() -> u32 {
    120
}
static RATE_LIMITS: OnceLock<Mutex<HashMap<String, (std::time::Instant, u32)>>> = OnceLock::new();

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
                let peer = stream
                    .peer_addr()
                    .map(|address| address.ip().to_string())
                    .unwrap_or_else(|_| "unknown".into());
                std::thread::spawn(move || {
                    let _ = handle_with_peer(&mut stream, &cfg, &peer);
                });
            }
            Err(e) => eprintln!("dashboard connection: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
fn handle(stream: &mut TcpStream, config: &FleetConfig) -> Result<(), String> {
    handle_with_peer(stream, config, "test")
}

fn handle_with_peer(
    stream: &mut TcpStream,
    config: &FleetConfig,
    peer: &str,
) -> Result<(), String> {
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
    if !rate_allowed(config.rate_limit_per_minute, peer) {
        return write_http(
            stream,
            "429 Too Many Requests",
            "text/plain",
            b"rate limit exceeded",
        );
    }
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
        } else if path.starts_with("/api/model") {
            model(config, path)
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
    let expected = config
        .auth_token_file
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|token| token.trim().to_owned())
        .or_else(|| config.auth_token.clone());
    let Some(expected) = expected.filter(|token| !token.is_empty()) else {
        return true;
    };
    headers
        .get("x-sextant-token")
        .is_some_and(|token| token == expected.as_str())
        || target
            .split("token=")
            .nth(1)
            .and_then(|value| value.split('&').next())
            .is_some_and(|token| token == expected)
}

fn rate_allowed(limit: u32, peer: &str) -> bool {
    if limit == 0 {
        return false;
    }
    let now = std::time::Instant::now();
    let limits = RATE_LIMITS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut limits = limits.lock().expect("rate limiter lock");
    let entry = limits.entry(peer.to_owned()).or_insert((now, 0));
    if now.duration_since(entry.0).as_secs() >= 60 {
        *entry = (now, 0);
    }
    if entry.1 >= limit {
        return false;
    }
    entry.1 += 1;
    true
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
    json!({"bots": snapshots(config), "jobs": jobs(config), "capabilities": ["state", "command", "world", "model", "websocket", "jobs"]})
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
fn model(config: &FleetConfig, path: &str) -> String {
    let name = path
        .split("name=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .unwrap_or("");
    let Some(root) = config.textures.as_ref() else {
        return json!({"error":"textures are not configured"}).to_string();
    };
    match crate::pack::Library::new(root).load(name) {
        Ok(model) => serde_json::to_string(&model)
            .unwrap_or_else(|e| json!({"error":e.to_string()}).to_string()),
        Err(error) => json!({"error": error}).to_string(),
    }
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
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    write_frame(stream, &state(config).into_bytes())?;
    let mut subscribed: Option<String> = None;
    let mut interval = Duration::from_millis(500);
    let mut next_push = std::time::Instant::now();
    loop {
        if let Some(bot) = subscribed.as_deref()
            && std::time::Instant::now() >= next_push
        {
            let payload = world(config, &format!("/api/world?bot={bot}"));
            write_frame(stream, payload.as_bytes())?;
            next_push = std::time::Instant::now() + interval;
        }
        let Some((opcode, text)) = read_frame(stream)? else {
            continue;
        };
        match opcode {
            0x8 => return Ok(()),
            0x9 => write_control_frame(stream, 0xA, &text)?,
            0x1 => {
                if let Ok(req) = serde_json::from_slice::<Value>(&text) {
                    if let Some(bot) = req["subscribe"].as_str() {
                        if config.bots.iter().any(|entry| entry.name == bot) {
                            subscribed = Some(bot.to_owned());
                            interval = Duration::from_millis(
                                req["interval_ms"]
                                    .as_u64()
                                    .unwrap_or(500)
                                    .clamp(100, 10_000),
                            );
                            next_push = std::time::Instant::now();
                        }
                    } else if req["command"].is_string() {
                        let response = command(config, &req.to_string());
                        write_frame(stream, response.as_bytes())?;
                    }
                }
            }
            _ => {}
        }
    }
}

fn read_frame(stream: &mut TcpStream) -> Result<Option<(u8, Vec<u8>)>, String> {
    let mut header = [0u8; 2];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(None);
        }
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let marker = (header[1] & 0x7f) as usize;
    let length = match marker {
        n @ 0..=125 => n,
        126 => {
            let mut bytes = [0u8; 2];
            stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            u16::from_be_bytes(bytes) as usize
        }
        127 => {
            let mut bytes = [0u8; 8];
            stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            u64::from_be_bytes(bytes)
                .try_into()
                .map_err(|_| "WebSocket frame too large")?
        }
        _ => unreachable!(),
    };
    if length > 512 * 1024 {
        return Err("WebSocket frame exceeds 512 KiB".into());
    }
    if !masked {
        return Err("client WebSocket frame was not masked".into());
    }
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask).map_err(|e| e.to_string())?;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Ok(Some((opcode, payload)))
}
fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    write_control_frame(stream, 0x1, payload)
}

fn write_control_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<(), String> {
    if payload.len() > 512 * 1024 {
        return Err("WebSocket payload exceeds 512 KiB".into());
    }
    let len = payload.len();
    let mut h = vec![0x80 | (opcode & 0x0f)];
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
let ws,selected=null,lastState=null,lastWorld=null,cam={yaw:45,pitch:35,zoom:12},drag=null;const textures=new Map();const $=id=>document.getElementById(id);const token=localStorage.getItem('sextantToken')||prompt('Sextant token (leave blank if disabled)')||'';if(token)localStorage.setItem('sextantToken',token);
const colors={grass:'#6ba34b',dirt:'#8a5a3b',stone:'#777',cobble:'#666',wood:'#9a6b3f',log:'#76502f',planks:'#b18855',sand:'#d9c27b',glass:'#9bd7e5',water:'#3c79c9',lava:'#d85d25',leaves:'#3e7d45',ore:'#9ca6af',brick:'#98584c',obsidian:'#252039',snow:'#e8f2f7',ice:'#9fd7e8'};
function color(state){let s=(state||'').toLowerCase();for(const [k,v] of Object.entries(colors))if(s.includes(k))return v;let h=0;for(const c of s)h=(h*31+c.charCodeAt(0))>>>0;return `hsl(${h%360} 32% 45%)`}
function authQuery(){return token?'?token='+encodeURIComponent(token):''}function api(path,options={}){return fetch(path,{...options,headers:{...(options.headers||{}),...(token?{'X-Sextant-Token':token}:{})}})}
function textureUrl(state){let n=(state||'').split('[')[0];return '/textures/'+n+'.png'+authQuery()}
function texture(state){let url=textureUrl(state),img=textures.get(url);if(img===undefined){img=new Image();textures.set(url,img);img.onload=draw;img.onerror=()=>{textures.set(url,null);draw()};img.src=url}return img}
function pos(v){return v?.player?.position||[0,0,0]}
function project(p,layer){let dx=p[0]-(lastWorld?.from?.[0]||0),dy=p[1]-layer,dz=p[2]-(lastWorld?.from?.[2]||0),a=cam.yaw*Math.PI/180,q=cam.pitch*Math.PI/180,x=dx*Math.cos(a)-dz*Math.sin(a),depth=dx*Math.sin(a)+dz*Math.cos(a);return [x*cam.zoom,(dy*Math.cos(q)-depth*Math.sin(q))*cam.zoom]}
function draw(){let c=$('map'),g=c.getContext('2d');g.clearRect(0,0,c.width,c.height);g.save();g.translate(c.width/2,c.height/2+80);let scale=cam.zoom;let layer=Number($('layer').value)||64;for(const b of (lastWorld?.blocks||[])){if(b[1]!==layer)continue;let [sx,sy]=project([b[0],b[1],b[2]],layer);let img=texture(b[3]);g.beginPath();g.moveTo(sx,sy);g.lineTo(sx+scale*.5,sy+scale*.25);g.lineTo(sx,sy+scale*.5);g.lineTo(sx-scale*.5,sy+scale*.25);g.closePath();if(img&&img.complete&&img.naturalWidth){g.save();g.clip();g.drawImage(img,sx-scale*.5,sy,scale,scale*.5);g.restore()}else{g.fillStyle=color(b[3]);g.fill();}}
for(const [name,v] of Object.entries(lastState?.bots||{})){let p=pos(v),[sx,sy]=project(p,layer);g.fillStyle='#7fda9b';g.beginPath();g.arc(sx,sy,7,0,7);g.fill();g.fillStyle='#fff';g.fillText(name,sx+9,sy)}
for(const e of (lastWorld?.entities||[])){let p=e.pos||[0,0,0];if(Math.abs(p[1]-layer)>8)continue;let [sx,sy]=project(p,layer);g.fillStyle='#f06b9a';g.beginPath();g.moveTo(sx,sy-6);g.lineTo(sx+5,sy+5);g.lineTo(sx-5,sy+5);g.closePath();g.fill();}g.restore();}
function drawBots(d){lastState=d;let a=Object.entries(d.bots||{});$('bots').innerHTML=a.map(([n,v])=>`<div class="bot ${selected===n?'selected':''}" onclick="selectBot('${n}')"><b>${n}</b> <span class="ok">${v.player?'online':'offline'}</span><br><span class="muted">${JSON.stringify(pos(v))}</span></div>`).join('');$('jobs').innerHTML=(d.jobs||[]).map(j=>`<div class="muted">${j.id}: ${j.status}${j.worker?' · '+j.worker:''}</div>`).join('');if(selected&&d.bots[selected]?.player){$('layer').value=Math.floor(pos(d.bots[selected])[1])}draw()}
function selectBot(n){selected=n;$('selected').textContent=n;drawBots(lastState||{bots:{}});refreshWorld();if(ws&&ws.readyState===1)ws.send(JSON.stringify({subscribe:n,interval_ms:500}))}
function poll(){api('/api/state').then(r=>r.json()).then(d=>{drawBots(d);$('conn').textContent='live'}).catch(()=>$('conn').textContent='offline')}
function refreshWorld(){if(!selected)return;api('/api/world?bot='+encodeURIComponent(selected)).then(r=>r.json()).then(w=>{lastWorld=w;draw()}).catch(e=>$('log').textContent=e)}
function send(){if(!selected)return $('log').textContent='select a bot first';let c=$('cmd').value;api('/api/command',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({bot:selected,command:c})}).then(r=>r.text()).then(t=>{$('log').textContent=t;poll()})}
const canvas=$('map');canvas.addEventListener('pointerdown',e=>{drag={x:e.clientX,y:e.clientY};canvas.setPointerCapture(e.pointerId)});canvas.addEventListener('pointermove',e=>{if(!drag)return;cam.yaw+=(e.clientX-drag.x)*.5;cam.pitch=Math.max(-80,Math.min(80,cam.pitch+(e.clientY-drag.y)*.5));drag={x:e.clientX,y:e.clientY};draw()});canvas.addEventListener('pointerup',()=>drag=null);canvas.addEventListener('wheel',e=>{e.preventDefault();cam.zoom=Math.max(3,Math.min(40,cam.zoom-e.deltaY*.01));draw()},{passive:false});function connect(){try{ws=new WebSocket(`ws://${location.host}/ws${authQuery()}`);ws.onopen=()=>{if(selected)ws.send(JSON.stringify({subscribe:selected,interval_ms:500}))};ws.onmessage=e=>{try{let d=JSON.parse(e.data);if(d.bots)drawBots(d);else if(d.blocks){lastWorld=d;draw()}}catch{}};ws.onclose=()=>setTimeout(connect,1000)}}catch{setTimeout(connect,2000)}}$('layer').addEventListener('input',draw);setInterval(()=>{poll();refreshWorld()},1500);poll();connect();
</script>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn config(root: PathBuf) -> FleetConfig {
        FleetConfig {
            bots: Vec::new(),
            bind: "127.0.0.1:0".into(),
            textures: Some(root),
            auth_token: Some("secret".into()),
            auth_token_file: None,
            rate_limit_per_minute: 120,
            scheduler: None,
        }
    }

    #[test]
    fn protected_texture_endpoint_serves_pack_bytes() {
        let root = std::env::temp_dir().join(format!("sextant-pack-{}", std::process::id()));
        let file = root.join("assets/minecraft/textures/block/stone.png");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"png").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cfg = config(root.clone());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handle(&mut stream, &cfg).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        write!(
            client,
            "GET /textures/minecraft/stone.png?token=secret HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert!(response.ends_with(b"png"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn protected_api_rejects_missing_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cfg = config(std::env::temp_dir());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handle(&mut stream, &cfg).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        write!(client, "GET /api/state HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
    }

    #[test]
    fn token_file_rotation_and_rate_limit_are_enforced() {
        let path = std::env::temp_dir().join(format!("sextant-token-{}", std::process::id()));
        std::fs::write(&path, "old\n").unwrap();
        let mut cfg = config(std::env::temp_dir());
        cfg.auth_token = None;
        cfg.auth_token_file = Some(path.clone());
        let old = BTreeMap::from([(String::from("x-sextant-token"), String::from("old"))]);
        assert!(authorized(&cfg, "/api/state", &old));
        std::fs::write(&path, "new\n").unwrap();
        assert!(!authorized(&cfg, "/api/state", &old));
        assert!(rate_allowed(1, "unique-rate-test"));
        assert!(!rate_allowed(1, "unique-rate-test"));
        let _ = std::fs::remove_file(path);
    }
}
