# Sextant

Pathing for [Horizon](https://github.com/horizon-rust/horizon) bots: walk a bot anywhere it can get to, mining
through what's in the way and building up and across where it must, in a style anywhere from *obviously a bot*
to *moves like a player*.

Sextant drives a bot through its `horizon-ctl` control socket, so it works on any Horizon bot, including every
bot of a [Flotilla](https://github.com/Solenopsisbot/flotilla) fleet.

```sh
sextant --socket .flotilla/ctl-Miner.sock go 120 64 -40 --style player-like
sextant --socket .flotilla/ctl-Miner.sock --build --tool iron_pickaxe go 31 101 30
sextant --socket ... plan 120 64 -40        # print the route without moving
sextant --socket ... mine diamond_ore --count 16
sextant --socket ... --cache world.sxrg eta 120 64 -40
sextant --socket ... waypoint save home
sextant --socket ... waypoint go home
sextant --socket ... craft sticks --times 4
sextant --socket ... farm wheat carrots --rounds 3
sextant --socket ... build house.json --origin 0 64 0
sextant --socket ... follow minecraft:pig --rounds 20
sextant --socket ... explore --legs 12 --leg-length 64
sextant --socket ... tunnel 120 64 -40
sextant --socket ... surface
sextant --socket ... axis east --distance 500
sextant --socket ... find diamond_ore --search-radius 128
sextant --socket ... portal 10 64 10
sextant --socket ... capabilities
sextant task validate outpost.toml
sextant --socket ... task run outpost.toml
sextant task status outpost.toml
sextant swarm --state fleet.json enqueue mine-iron iron.toml --priority 10
sextant swarm --state fleet.json claim Miner
sextant dashboard --fleet sextant-fleet.toml   # browser + WebSocket control plane
sextant styles                               # the presets and their knobs
```

## What it does

- **Plans** with A* over the moves a player on foot makes: walking, diagonals (only where both corners are open
  and floored), stepping up a block, dropping (up to 3 blocks, or any height into water), mining through
  (`--mine`, on by default) and mining down, and with `--build`, pillaring up and bridging gaps. Costs are game
  ticks from vanilla's movement rules (walking 4.317 blocks/s, sprinting 5.612, gravity 0.08 with 2% drag,
  breaking `hardness x 30 / tool speed`). Lava, fire and other hazards are never walked through; a block under a
  falling block or beside a liquid isn't mined. A route beyond what it has read ends at the best partial route,
  so long trips go in segments.
- **Reads** the world lazily across the bot's full render distance. A* asks for only the chunk tiles
  touched by its frontier, then continues the same search as replies arrive; it does not preload a
  small bubble or restart from scratch at every boundary.
- **Walks** a plan:
  - runs of walking are followed by pure pursuit along the line through the blocks' centres, so drift is
    corrected and corners are cut only inside their own block;
  - stops on a block sneak for the last stretch (Minecraft doesn't let a sneaking player walk off an edge),
    and count as stopped only once the bot is still;
  - a walk into a planned drop walks straight off the edge;
  - a step up taps jump once and lands sneaking;
  - a pillar first stands still on its block's centre (a jump off-centre can hit its head on a block beside it).
- **Recovers**: a step that fails (the bot slid off its level, something moved) replans from where the bot is.
  A dead bot ends the trip with an error, and every key is let go whatever happens.
- **Mines goals**: `mine` finds loaded matching blocks, walks to a reachable face, breaks them through
  Horizon's normal action, and repeats for a count.
- **Caches**: `--cache` stores complete terrain tiles in a compact versioned file for later planning.
- **Waypoints**: named feet positions can be saved, listed, and revisited.
- **Sprint-jumps**: long flat runs use two-block sprint jumps when the route and headroom allow it.
- **Blueprints**: the library validates JSON blueprints, orders supports before walls, and rotates them
  around Y for a future live builder.
- **Builds**: `build` accepts Sextant JSON blueprints and Sponge `.schem` files, places supports first,
  skips occupied targets, and reports missing materials.
- **Farms**: `farm` harvests and replants mature wheat, carrots, potatoes, and beetroot crops.
- **Navigation commands**: `tunnel`, `surface`, `axis`, `find`, `invert`, and `explore` cover common
  Baritone-style movement goals; `follow` tracks nearby mobs or items.
- **Tasks and fleets**: `pause` preserves a task for `resume`, and `--reservation` atomically claims a
  shared work-area file so multiple Flotilla bots do not take the same job.
- **Travel**: `portal` waits for a dimension change. `capabilities` reports swimming, portal, vehicle,
  and Elytra support exposed by the connected Horizon build.

## Multi-step tasks

Long jobs can be described once and run in order. Each step is announced in game chat with a
`[Sextant]` prefix, printed to the terminal, and can be mirrored by the dashboard:

```toml
name = "outpost"

[[steps]]
action = "go"
x = 120
y = 64
z = -40

[[steps]]
action = "mine"
blocks = ["iron_ore", "coal_ore"]
count = 16
search_radius = 96

[[steps]]
action = "craft"
recipe = "torches"
times = 4

[[steps]]
action = "chat"
text = "outpost supply run complete"
```

Validate it with `sextant task validate outpost.toml`, then run it with
`sextant --socket .flotilla/ctl-Miner.sock task run outpost.toml`. Supported steps are `go`,
`mine`, `craft`, `farm`, `chat`, `wait`, and `status`. A failed step stops the plan while completed
steps remain visible in chat. The state file is atomically updated after every step, so rerunning the
same command retries only the interrupted step; `task status` reports the durable checkpoint and
`--restart` intentionally starts over.

The `swarm` command manages a durable priority queue shared by workers. Jobs have expiring leases,
renewal, retry budgets, deterministic priority ordering, and reassignment when a worker disappears:
`enqueue`, `claim`, `renew`, `complete`, `fail`, and `list` are all safe to call from separate worker
processes using the same JSON or TOML state file.

## Headless fleet dashboard

The optional dashboard keeps Horizon responsible for world truth. Sextant connects to each bot's
existing control socket, so terminal, WebSocket, and in-game commands share the same permissions and
safety checks. Create a fleet file:

```toml
bind = "127.0.0.1:8765"
# Optional: a prepared Horizon resource root or resource-pack directory.
textures = ".local/horizon-client/26.3"
# Optional bearer token for API, texture, and WebSocket access.
auth_token = "change-me"
# Or point at a file that can be atomically replaced to rotate the token without restarting.
auth_token_file = "sextant.token"
# Requests per minute per remote peer (default 120).
rate_limit_per_minute = 120
# Optional shared swarm scheduler state surfaced in the dashboard.
scheduler = "fleet.json"

[[bots]]
name = "Miner"
socket = ".flotilla/ctl-Miner.sock"

[[bots]]
name = "Builder"
socket = ".flotilla/ctl-Builder.sock"
```

Start it with `sextant dashboard --fleet sextant-fleet.toml`. If `auth_token` is configured, the page
prompts once and stores it in browser local storage; all API, texture, and WebSocket requests then
carry the token. The page shows online state and
positions, accepts control-socket commands, and renders an isometric, layer-selectable spectator view
of the loaded world around a selected bot through `/api/world?bot=Miner`. When `textures` is set,
the dashboard serves matching `assets/<namespace>/textures/block/*.png` files and paints those tiles
with the actual resource-pack image; missing or custom textures fall back to deterministic material
colors, so the view remains useful while a pack is incomplete. `/api/state` is the fleet snapshot API.
`/ws` upgrades to a WebSocket: send
`{"bot":"Miner","command":"status"}` and receive the command reply followed by the latest fleet
snapshot. Keep it on a private LAN or behind an authenticated reverse proxy before exposing it beyond
a trusted network.

The WebSocket accepts `{"subscribe":"Miner","interval_ms":500}` and then sends the newest world
snapshot at the requested bounded interval. It keeps only the latest snapshot, caps frames at 512 KiB,
answers ping frames, reconnects the browser after a dropped connection, and rejects unmasked client
frames. A token file is reread for every HTTP/WebSocket handshake, so rotation is an atomic file replace.

Drag the canvas to orbit, use the wheel to zoom, and change the Y layer to inspect vertical slices.
Entity snapshots from Horizon are drawn as live markers and are refreshed with the loaded block view.
Dropped item stacks render as item squares, experience orbs as glowing markers, mobs as entity
triangles, and active player effects produce lightweight particle specks around the player. These
effects are presentation data; Horizon remains authoritative for positions and inventory state.

## Styles

| Preset | Turning | Reactions | Walk | Extras |
|---|---|---|---|---|
| `robot` | snaps instantly | none | exact centres | none |
| `tidy` | smooth, quick | 1-2 ticks | near the centre | none |
| `casual` | eased, 25°/tick | 2-6 ticks | wanders 0.2 blocks | pauses, glances, overshoot |
| `player-like` | eased, 18°/tick | 3-10 ticks | wanders 0.3 blocks | more pauses, glances, overshoot |

Any knob can be changed in a TOML file over a preset:

```toml
preset = "robot"
wobble = 0.15
reaction = [2, 5]
```

```sh
sextant --style my-style.toml --socket ... go 10 64 10
```

A style shapes how the movement looks. Overshooting a turn moves the view only: the feet follow the route. It
isn't tuned against any anticheat's checks.

## Tested

On an obstacle course built with commands, in the sky so its edges are deadly: a dip beside the route, a step
up, a 1- and a 2-block drop, a wall across the whole width to mine through, a gap across the whole width over
nothing to bridge, and a lone block three up to finish on by pillaring. Every style completes it with no
replans, ending on the goal block. Tracing a walk: `SEXTANT_TRACE=1 sextant -v ...`.

It needs a Horizon whose control socket answers commands sent together in one frame (`control/pipelined` on
[Solenopsisbot/horizon](https://github.com/Solenopsisbot/horizon)'s `main`): steering one tick at a time needs
it.

The control socket is the compatibility boundary. Sextant never writes directly to a game save or assumes a
particular renderer. It treats unread terrain as unknown, namespaces cache files per world/dimension in the
calling application, and releases movement keys when a command exits with an error.

## Building

```sh
cargo build --release   # target/release/sextant
cargo test
cargo clippy -- -D warnings
```

Clean-room: written from Minecraft's movement rules, not from another pathfinder's code.
