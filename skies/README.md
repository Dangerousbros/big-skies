# Big Skies

Regional, dynamic weather for Horizon, as a native mod (`big_skies`). The weather is a real place on the map rather than
a switch for the whole world: storms, showers, fronts and overcast decks are born, grow, drift with the wind and die,
so you can stand in sunshine and watch a thunderstorm on the horizon, see its rain shaft and lightning, and hear it
grumble long before it arrives.

Everything is at real scale, one block to one metre: fair-weather cumulus a few hundred metres across with bases at
1 to 1.5 km, showers towering to 4 to 5 km, thunderstorms reaching 12 km with an anvil spread downwind, and fronts and
decks as broad layers. Cloud heights follow the weather (dry, clear days lift the bases; damp, rainy days lower them).

The weather moves at about three times the game clock's own pace (a Minecraft day is only twenty minutes), so a
storm seen building on the horizon in the morning arrives by evening, and a day sees the changes a real one would.

## What you get

- **A physical sky.** Rayleigh, Mie and ozone scattering over a curved planet, so blue noon, golden sunsets and violet
  twilight come from the air rather than a gradient. Distant terrain hazes into the sky, which suits Horizon's LOD.
  Moonlight follows the moon's phase; stars come out as the sky darkens.
- **Soft volumetric clouds.** These are ray-marched through a tiling Perlin–Worley noise volume, with self-shadowing and
  multiple scattering. The clouds have flat grey bases and bright billowing tops, and they cast shadows on the ground.
- **Weather that hangs together.** Each day has a character (clear, fair, showery, thundery, overcast or rainy), drawn
  from the day before. Night carries on the day's weather. Overcast and rainy days are grey from end to end. The wind
  backs and veers with the weather. A rainbow follows a shower if the sun is low behind you.
- **Thunder that depends on distance.** Thunder is only heard within about 12 km of a storm. From far away it is a soft,
  low rumble. Close in it is louder and sharper, and directly under the storm you get the crack. The sound arrives
  after the flash at the speed of sound.
- **Morning valley fog.** On calm, damp mornings, fog pools in the valleys and lifts as the sun warms them.
- **The night sky.** Stars turn with the sky, and now and then a shooting star streaks across it. On about one clear
  night in four in cold lowlands (taiga, tundra, the frozen north; not a mountain's own cold) the northern lights hang
  low in the north: green curtains fading to violet, rays sliding along them, drifting and pulsing slowly.
- **Mist at waterfalls.** Wherever water comes down 10 blocks or more, straight through the air or cascading steeply
  from ledge to ledge, spray rises at its foot: a wisp for a ten-block trickle, a cloud billowing up the fall for a
  wide one forty blocks high. With the sun behind you, a spray rainbow shows in it, at 42°
  round the shadow of your head. Splashes, mist puffs and bubbles rise from the pool too. The mod finds the falls in the world's
  own blocks, about 140 blocks round you, so vanilla's falls count, and so do falls you build or a generator makes.
- **Clouds that suit the land.** The mod asks the world's generator for the biomes 50 km round (the engine's
  `world.read-climate`), and their temperature and rainfall decide what forms over them: jungles, swamps and the sea
  breed showers and thunderstorms, deserts and savannas keep high, fair skies, and cold country and the sea take grey
  decks. Thunderstorms need warm air.
- **Snow on the peaks, rain in the valleys.** The freezing level follows the air under it (low over cold land, high
  over warm, higher on a sultry day and lower behind a front, lower at night). Above it a shower's shaft is snow,
  whiter, softer and slower, leaning further in the wind, melting into rain through the 150 m below, so one storm
  snows on the summits and rains in the valleys. Near you, it snows above the freezing level and where snow lies;
  where vanilla would rain instead, the mod keeps vanilla's weather clear and draws the snow itself.
  `/skies` says where the snow line is.
- **Rain and snow** near you, falling in real time however fast the weather runs.

## Install

From Horizon's mod browser (launcher > Mods), or by hand: put `big_skies.hnmod` in the game home's
`horizon-native-mods/` folder (launcher > Mods > Open folder) and build. Native mods are compiled into the game and
are not sandboxed, so trusting one is your decision; all of the source is here.

Then, in the game's options:

- turn **Clouds** off, or vanilla clouds float through the mod's;
- turn **Fog** off, or the mod hazes the distance and the game greys it again;
- turn **Weather** off, or vanilla rain streaks draw in front of the sky. The mod draws its own rain and snow nearby
  (straight streaks fixed in the world, slanted with the wind, and none under a roof, leaves or a cave's ceiling),
  and vanilla's rain sounds still play;
- turn **LOD Distance** up to at least 512 so the terrain reaches the haze.

The mod can be switched off on Options > Mods. With the [Natural Lighting](../natural-lighting/) shader pack, the
mod tells the pack how much sun gets through its clouds, so passing clouds dim the light and fade the shadows.

## Commands

`/skies` shows the forecast: today and tomorrow, what is overhead, the wind, and the systems nearby. The chat suggests
the sub-commands below as you type, as it does for vanilla's commands.

| Command | What it does |
| --- | --- |
| `/skies storm [cell\|cluster\|line\|pulse] [km\|here]` | Brings a thunderstorm, by default 16 km upwind: one great cell, a cluster of towers, a squall line or a lone pulse storm (any of them if you leave it out). `shower`, `front`, `overcast` and `fair` work the same way. |
| `/skies clear` | Clears the sky and holds it clear. |
| `/skies weather auto\|clear\|fair\|showery\|thundery\|overcast\|rainy` | Holds a kind of day, or `auto` lets the days run on. |
| `/skies wind auto\|<from> [m/s]` | Holds the wind, e.g. `/skies wind w 10`. `<from>` is n, e, s, w or degrees. |
| `/skies speed <x>` | Runs the weather faster (up to 60×) or slower, as a timelapse; 1 is the usual pace (about three times the clock's). |
| `/skies dawn` | Jumps to just before a foggy dawn, to watch the fog lift. |
| `/skies fog` | Makes the coming morning foggy. |
| `/skies aurora` | Brings the northern lights tonight, wherever you are. |
| `/skies rainbow` | Shows a rainbow, if the sun is out and low behind you. |
| `/skies falls` | Lists the waterfalls the mod has found near you. |
| `/skies bolt [m]` | Strikes lightning where you look (default 2,000 m), with its thunder. |
| `/skies sync on\|off` | Sets whether vanilla rain follows the sky overhead (default on). |
| `/skies quality low\|medium\|high` | Sets the number of cloud steps (default medium). |
| `/skies on\|off\|reset` | Pauses the mod, resumes it, or starts a fresh sky. |

## Performance

The whole sky costs about 2 ms of GPU time at 1440p on the development machine. It renders at half resolution in
a checkerboard and is joined with a depth-aware filter. The weather simulation stays inside the per-tick budget.

## Building

- `mod/src`: the weather simulation (Rust), and `mod/shaders/skies.wgsl`: the sky;
- `native/`: the glue that runs the mod's code natively, through Horizon's native hooks (`native/glue.rs`,
  `native/hooks/`), a native build of the mod SDK (`native/engine/horizon_sdk.rs`) and the sky's post pass on
  Horizon's render hooks (`native/post.rs`);
- `tools/build_native.py`: packages `dist/big_skies.hnmod` (`--bundle DIR` also writes the unpacked bundle, for
  `horizon-mixins apply --mods DIR`).

The mod's code goes in unchanged, the way Orbital Laser builds natively. The bundle is pinned to one Horizon commit
(`HORIZON` in `build_native.py`: Nightly 2026-11-08, `fdc87ae`), since it reads the game's own internals for the
climate and the sky's light.

## Credits

See [NOTICE](NOTICE). The techniques come from published work by Sébastien Hillaire, Christian Schüler, Andrew
Schneider and Nathan Vos, and Magnus Wrenninge, among others. The code is written afresh.

The native build's method, and the glue, native SDK and post pass it starts from, are from Orbital Laser
(github.com/Stink-O/orbital-laser, MIT License, Copyright (c) 2026 Stink-O).
