# Sextant native client mod

This bundle adds an in-game `/sextant` command to Horizon. It runs in the client process and
delegates movement to Horizon's existing agent pathfinder and input tasks, so it does not need a
separate socket process or a server mod.

Examples:

```text
/sextant go 120 64 -40
/sextant mine diamond_ore 16 64
/sextant explore 12 64
/sextant follow minecraft:pig 20
/sextant stop
/sextant jump
/sextant break 120 64 -40
/sextant place 120 64 -40 --item cobblestone
```

Build and install it from the Sextant checkout with:

```sh
horizon-launcher native-build native/sextant --instance Ladybug --trust-native-code
```

The bundle is pinned to Horizon commit `3bc251ea5af23d1ea4e800e722a6617630f098af` and must be
rebuilt when the client instance moves to a different incompatible revision.
