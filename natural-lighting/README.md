# Natural Lighting

A Horizon shader pack (shader API 0.1). It needs no fork and no native build, and it works in any Horizon from
Nightly 2026-11-08 on.

- **Light:** soft sun and moon shadows (PCSS: the shadow widens with the gap to whatever casts it), smooth torchlight,
  and sunlight dimming with rain and storms.
- **Water:** waves, reflections (the game's planar mirror where the surface is in view, and the sky otherwise),
  clear shallows you can see the bottom through, Fresnel, sun glints, and caustics on what lies under water.
- **Picture:** sunrays through leaves, past hills and between clouds, a soft bloom (more of it at night), a gentle
  contrast curve and a little more colour.

## Installing

Put the `natural-lighting` folder (or a zip of what is in it) in the game's `shaderpacks` folder. Then pick **Natural Lighting** in
Options > Video Settings > Shaders. Every option is on that screen, on four pages: Main, Light, Water and Picture.

For one start only, set `HORIZON_SHADER_PACK=<path to this folder>`.

## With Big Skies

When Big Skies (native, 0.3.0 or later) is running, it gives the pack what its sky lets through, as
`frame.mods`:

| slot | holds |
| --- | --- |
| `mods[0]` | x: how much of the sun's (or moon's) light gets through the clouds here, 0 to 1; yzw: that light's colour |
| `mods[1]` | xyz: the sky's colour at the horizon; w: 1, meaning Skies is here |
| `mods[2]` | xyz: the sky's colour overhead; w: 0 |

With those, a passing cloud dims the light and fades the shadows under it, and water away from the mirror shows
Skies' sky. Without Skies (`mods[1].w` is 0), Natural Lighting goes by vanilla rain and thunder and by the game's own sky and
fog colours.

## Checking

```
horizon-shader check natural-lighting
```

The pack stays inside the API's limits: 64 texture reads and 20000 expressions per stage. Its frame budget is 3 ms.
