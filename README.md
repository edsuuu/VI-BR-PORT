# VI-BR-PORT: the build for 8 GB cards

**English** | [Português](README.pt-BR.md)

## How to use

Two downloads, each its own release on the [Releases page](https://github.com/edsuuu/VI-BR-PORT/releases):

| What | Release | File | Inside |
|---|---|---|---|
| **Launcher** | `v<version>` | `VI-BR-PORT-<version>-windows-x64.zip` | `vi-launcher.exe` |
| **Engine** | `engine-v<version>` | `VIctory-<version>-windows-x64.zip` | `victory.exe` |

Releases whose names start with `dev-` (`dev-v<version>`, `dev-engine-v<version>`) are
test builds, marked as pre-releases; the plain ones are the stable builds.

1. Have **your own copy of the game** (the Xbox Series build): a folder holding
   `game_scarlett_master.exe`.
2. Download the latest **launcher** and **engine** releases and put `vi-launcher.exe` and
   `victory.exe` in the same folder (anywhere else works too: the launcher's settings choose
   the engine).
3. Open `vi-launcher.exe`.
4. Click **CHOOSE THE GAME FOLDER** and pick the folder of your copy.
5. If the engine is not next to the launcher, the button says **CHOOSE THE ENGINE**: pick
   `victory.exe`.
6. Click **PLAY**. The first time the shaders are translated and the pipelines warmed (a few
   minutes, with the per cent shown); then the game opens and the launcher waits in the tray.
7. **More options**: the saves folder, mouse and keyboard (Esc frees the cursor for the menus),
   extra flags, clearing the compiled shaders, checking for updates.

Needs Windows 10 or 11 (x64), a CPU with AVX2 and a Direct3D 12 card with shader model 6.6 and
mesh shaders (tested on an 8 GB NVIDIA card), and about 2 GB of disk for the shader cache.
Without the launcher, `victory.exe` runs with `scriptsplay.ps1` (see below).

A compatibility layer that runs, natively on Windows, the **Xbox Series X|S** build
(the *Scarlett* platform, titles made with the **GDK**) of a title, from the user's own
files. The console's x86-64 code runs unchanged inside our process; underneath it are the
console's system libraries rewritten (HLE) and a Direct3D 12 back end that recompiles the
console GPU's shaders (GFX10/RDNA2) to DXIL.

This fork has one target: an **8 GB NVIDIA card with waves of 32 lanes** (tested on an
RTX 4060 Ti) on a **1080p monitor**.

**This repository holds no files, code or assets of the game.** You need your own copy.
Nothing here is connected with, or endorsed by, the game's developer or publisher.

## Where it stands (2026-11-27)

**Playable from the opening through Lucia's prologue in the prison** (the cell, the
corridor, the common room, the counselor's office) **and on into the first open-world
mission (the boat)**, with the player in control, on the Series S profile.

**Video** (NVIDIA RTX 4060 Ti 8 GB, the prison walk with the frame rate on screen; recorded
on 26 Nov, before the gains below): <https://youtu.be/HI7cEpJPhP4>

### Benchmark

Release build of `main`, started with `scripts\play.ps1` (its settings are under
[How to test it with the frame rate we get](#how-to-test-it-with-the-frame-rate-we-get)),
Series S profile, 1920x1080 window, the title's own upscaler. PC: RTX 4060 Ti 8 GB (driver
621.40), i7-13700K, 64 GB of RAM, Windows 11. One machine. Each figure is the mean of the
run log's frame lines (one every 5 seconds). The column of 26 Nov is commit `8bb5142d` with
that day's settings.

| Scene | How it was measured | 26 Nov | 27 Nov |
|---|---|---|---|
| Prison, walking | a scripted 3-minute walk (keys through `VICTORY_KEYS_FILE`) from the cell's save | 24.0 fps (lowest line 16.3) | **26.6 to 27.9 fps** (lowest line 18.1 to 24.4; up to 31% of the lines at the title's cap of 30) |
| First open-world mission, the boat | 70 seconds driving the boat, from the save at the counselor's office | 20 to 22 fps | **28.7 to 29.1 fps** (lowest line 28.2) |
| Opening cutscene, 3D part | the first two minutes of a new game | 21.2 fps | **25.6 fps** (lowest line 18.5; the first scene runs at 30) |
| Prison, standing still in the cell | the log's frame lines, 35 s in a row | 30.0 fps | **30.0 fps** (33.3 ms, the title's cap) |
| Opening video | | 30 fps | 30 fps |

The goal is a steady 33 ms (30 fps) from the opening to the boat. It is not there yet. The
boat is one frame a second short and holds its rate. The prison walk and the opening reach
the cap in their lighter stretches and fall to 18 to 24 fps in the heavy ones. On the boat
the card is the limit (98% busy). In the prison and the opening two things are at the limit
together: the card (83 to 92% busy) and the layer's thread that prepares the submissions
(31 to 34 ms of work a frame).

What each change of this fork measured, before and after, with its commit:
[docs/BENCHMARKS.md](docs/BENCHMARKS.md).

The frame rate over a whole run, from the log's own frame lines (the blue band is the
scripted walk; these are runs of 26 Nov):

![fps in the prison, main](docs/img/fps-prisao-main.svg)

![fps in the prison with VICTORY_ASYNC_SUBMIT=1](docs/img/fps-prisao-async.svg)

The charts are drawn from the logs; this repository keeps no pictures of the game
([docs/asset-policy.md](docs/asset-policy.md)).

**Stability:** no device removal with these settings in the runs of 26 and 27 Nov (more
than a hundred runs of 4 to 12 minutes), and one crash of the title itself, four minutes
into a run on 27 Nov, whose cause is not found yet. Long sessions have not been measured.

**Known picture problems:**

- **Decorations baked onto characters come out wrong in some launches**: coloured squares
  on clothes (the man's shorts in the opening's first shot) or on tattooed skin (the
  guard's arms in the prison). The title bakes them once, from textures it picks by index,
  and in some launches a source is not on the card yet. About one launch in three with
  thin lists, fewer without (`-More @{VICTORY_THIN='0'}`, at about 2 fps less in the
  prison and on the boat).
- **Textures are soft in the first half minute of a scene**: the title brings a texture's
  small levels first and the large ones as it streams, and its streaming runs slower under
  the layer than on the console (in the opening, more than ten thousand texture views
  still wait for their memory 25 seconds in).
- The opening's night scenes are lit by a stand-in for the ray-traced indirect light (a
  constant with a floor), flatter than on the console.
- In an occasional frame the walls vanish or the interior light goes out.
- 35 programs of lighting, shadows and fog are still translated approximately on 32-lane
  cards.

Fixed since 26 Nov: the prison's common room (it was dark under the fog; it is readable
now), textures refused for want of card memory that was in fact free, the window glass,
the unlit boat and the black rectangles on the sea.

## What this fork adds

- **Thin lists on NVIDIA** (`VICTORY_THIN=1`): a list call writes its arguments down and
  eight threads of the layer translate them beside the title. The pass that bakes
  decorations onto clothes is translated at the title's own call, in the title's order
  (`VICTORY_THIN_BAKE`); out of order it left blocks of noise on clothes in most launches
  (it still does in some: see the known problems).
- **Compares on the card that read less** (`VICTORY_CARD_COMPARE_EVERY=30`): a range of a
  title buffer a shader may have written is read whole once in 30 submissions and only its
  lately changed pieces between (the opening read 2.4 GB of buffers a frame to find some
  30 changed pieces; it reads 0.5 GB).
- **The room kept for render targets follows what they take** (1.4 GB on Series S with
  the title's upscaler) instead of a fixed 3 GB of an 8 GB card: textures were refused
  for want of memory that was free.
- **The copies whose bytes changed come from a list** (`VICTORY_CARD_TOUCHED=1`) instead
  of a look at every one of 3,700 copies at each submission.
- **The stand-in for indirect light tells rooms from outdoors** (the word `room` of
  `VICTORY_AMBIENT_KEY`, and `VICTORY_AMBIENT_OUTSIDE`): a faint constant inside rooms,
  raised where a room is dark; outdoors the game's own light with a floor.
- **`VICTORY_RT_MISS=1`**: the ray test hits nothing; without the ray-tracing structure
  built, the shadows of local lights came out as garbage (black or "glittering" faces).
- **The ray-tracing skinning is left out** with `RT_MISS=1`: the 30 `CSComputeSkinning*`
  programs (`bvh_skinning`) redid every character's pose only for the ray-tracing
  structure, which nobody uses that way, and were most of the card's time (about 6.8 to
  10.9 fps on Series S). `VICTORY_RT_SKIN=1` runs them again.
- **The face decoration bake is known by name** (`PS_DecorationBlend_*`,
  `PS_BakeDecorations_*`, 51 programs): the list of 4 hashes left out the ones that make
  the cutscene's faces, and the faces came out black.
- **Lists of changed memory are compacted, not cleared,** when they fill up: clearing
  left textures with old bytes after the memory changed.
- **The present's planes are composed** (Series S: the world and the interface in separate
  planes), sent ahead like a single-plane present, without waiting for the card.
- **Mouths and facial expressions**: the network that animates faces keeps its values in
  16-bit buffers, which the translation read as zeros through descriptor tables; they are
  now moved as 16-bit integers and mouths open and close with the lines.
- **Exact cascade shadows on 32-lane cards** (NVIDIA), and **texture views written by the
  card** copied from the texture itself.
- **A "Brazil | VIctory" window** with the fork's logo (`VICTORY_ICON`), and
  **`VICTORY_KEYS_FILE`**: a key written to a file is pressed in the game without focus
  (for scripts that get past the terms and the account picker, and for scripted walks).
- **The opening video no longer steps back**: each list has its own upload buffer for the
  video planes the CPU writes; with several frames in flight one frame overwrote the
  one before, and the opening's titles came back in the middle of a fade.
- **A new texture shows only after its upload** (`VICTORY_SHOW_AFTER`, now on; `0` turns
  it off): a reused piece of the pool showed old bytes, and the car seat of the opening
  became a green and blue checkerboard.
- **Title patches in release builds too** (`VICTORY_UPSCALER=6`: the title's own upscaler
  on Series S, since the FidelityFX passes have no DXIL here).
- **Pipelines**: a 64 MB stack for the NVIDIA driver's compiler, compute pipelines started
  at creation, a 500 ms wait only in the first 60 frames (8 ms after), and a late
  pipeline holding up only the first frame that asks for it.
- **wave64 on 32-lane cards**: programs that would ask for 64-lane waves take the
  wave-wide form.
- **DirectStorage** writes to read-only pages as the console's engine does.
- `VICTORY_NO_WATCHDOG`, `VICTORY_NULL_MAGENTA` (diagnostics) and the cap of 262,144
  remapped ranges.

## How to test it with the frame rate we get

From a normal PowerShell window (not as administrator: the driver then keeps its shader
cache elsewhere), in the repository's folder:

```powershell
copy victory.example.toml victory.toml      # and point game_dir at your copy
cargo build --locked --release -p victory
$env:VICTORY_RT_MISS = '1'                  # a translation setting: the shader cache is keyed on it
target\release\victory.exe shaders <the game's shader files>
scripts\play.ps1 -Exe <the game's executable> -Build release
```

The `shaders` step is needed once for each build of the translator (a few minutes).
After the first run, `target\release\victory.exe warm --used` makes ahead the pipelines
that run drew with (about 4,000 here), so that later runs do not stop to make them; the
first minutes in a new place are slower until then. The window's title shows the fps, the
ms a frame and the draws a frame; the log has a frame line every 5 seconds
(`... fps, ... ms/frame (... preparing ...)`).

`scripts\play.ps1` sets what the benchmark above was measured with. The settings that
make the frame rate, with what each measured on the RTX 4060 Ti (before → after):

| Setting | What it does | Measured here |
|---|---|---|
| `VICTORY_PORT_CONSTANTS=0` | the title's 182 MB buffer of constants is a copy in the card's memory, not read in place from the PC's memory | boat 23.8 → 27.7 fps; prison walk 23.0 → 25.4 |
| `VICTORY_THIN=1` | a list call only writes its arguments down; eight threads of the layer translate the calls beside the title | prison walk 25.3 → 27.5 fps (lowest line 24.2); boat 27.7 → 29.1 |
| `VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1` | the guards' lock is shared, and what the heaps hold is kept in a journal | prison walk: 29.4 → 26.6 ms preparing a frame |
| `VICTORY_CARD_TOUCHED=1` | the copies whose bytes changed come from a list, not from a look at all 3,700 at every submission | about 3 ms less preparing a frame |
| `VICTORY_CARD_COMPARE_EVERY=30` | a range a shader may have written is read whole on the card once in 30 submissions, only its lately changed pieces between | opening: 2.4 → 0.5 GB read a frame, 9.9 → 1.8 ms of card time; with thin lists, 21.7 → 25.6 fps |
| (no setting; `VICTORY_CARD_ROOM_FOR_TARGETS_MB` fixes it by hand) | the room kept for render targets follows what they take (1.4 GB) instead of a fixed 3 GB | "no room" for a texture 122 → 12 times a run; textures left with wrong bytes 2,790 → 0 |
| `VICTORY_AMBIENT=constant:0.1:0.5`, `VICTORY_AMBIENT_OUTSIDE=1:0.16`, `VICTORY_AMBIENT_KEY=0.2:8:room:air:geo`, `VICTORY_AMBIENT_AIR=0.8` | the stand-in for the ray-traced indirect light: a faint constant inside rooms, raised where a room is dark; outdoors the game's own light with a floor | picture only: the common room is readable and the night street keeps the glow of its lamps |
| `VICTORY_RT_MISS=1`, `VICTORY_UPSCALER=6`, `VICTORY_MEMORY_CARD_MB=1024`, `VICTORY_DISPLAY_MODE=1920x1080`, `VICTORY_PIPELINE_BOOK=1`, `--console series-s` | every ray test answers "nothing hit"; the title's own upscaler; the Series S profile at 1080p; the pipelines a run draws with are written down for `warm` | the base of every measurement |

Any of them can be changed for one run:
`scripts\play.ps1 -Exe ... -Build release -More @{VICTORY_THIN='0'}`. `-Safe` runs without
the newer ways (the first thing to try when something is wrong). On another card try
`-More @{VICTORY_PORT_CONSTANTS='1'}` as well: which of the two is faster depends on the
card and its driver.

Tried and left off, because they measured slower or no better here:
`VICTORY_CARD_COMPARE_HOT` (the exact form of the compare above: 8 to 50 ms more preparing
a frame), `VICTORY_ASYNC_SUBMIT=1` (25.7 fps against 25.6), `VICTORY_HEAP_RECHECK=0`,
`VICTORY_PORT_CONSTANTS_ORDER=1` (19.2 fps against 23.0).

## License

GPL-3.0-or-later, like VIctory. See [LICENSE](LICENSE).
