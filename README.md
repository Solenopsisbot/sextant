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

## Building

```sh
cargo build --release   # target/release/sextant
cargo test
```

Clean-room: written from Minecraft's movement rules, not from another pathfinder's code.
