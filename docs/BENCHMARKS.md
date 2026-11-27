# Measurements of this fork's own changes

What each change of this fork measured on one PC, with the commit that made it. Self-reported;
nobody else has reproduced these yet.

- **PC:** RTX 4060 Ti 8 GB (driver 621.40), i7-13700K, 64 GB, Windows 11.
- **Build and settings:** release, `--console series-s`, 1920x1080 window, the settings of
  `scripts/play.ps1` ([README](../README.md), "How to test it with the frame rate we get").
- **Scenes:**
  - *opening cutscene*: the 3D part of the opening, 40 to 75 s after the window opens; the mean of
    the frame lines the run log writes every 5 s;
  - *prison walk*: Lucia's prologue; a script holds the movement and camera keys for three minutes
    through `VICTORY_KEYS_FILE` (no focus, no mouse), the same route every run; the mean of the
    frame lines while the walk goes on, and the lowest of them.
- *the boat*: the first open-world mission, from the save made at the counselor's office; a
    script holds the throttle for 70 seconds; the mean of the frame lines while it drives.
- **Noise:** two runs of the same build differ by about 1.5 fps, so a difference smaller than
  that says nothing. Most rows are one run each.

## Frame rate, before and after

| Change (commit) | Scene | Before | After |
|---|---|---|---|
| The ray-tracing skinning passes (30 `CSComputeSkinning*` programs) are left out while every ray is answered as a miss ([ab4571df](https://github.com/edsuuu/VI-BR-PORT/commit/ab4571df), [1556de82](https://github.com/edsuuu/VI-BR-PORT/commit/1556de82)) | opening cutscene | 6.8 fps | 10.9 fps |
| Card steps at stage 2 with the relay, as the default ([46f467a0](https://github.com/edsuuu/VI-BR-PORT/commit/46f467a0)) | opening cutscene | 12.4 fps | 16.3 fps (stage 1: 14.8) |
| Title buffer copies kept in the card's memory from 1 MiB up to 1 GiB on an 8-12 GB card ([389b9747](https://github.com/edsuuu/VI-BR-PORT/commit/389b9747)) | opening cutscene | 17 fps | 18-19 fps |
| The title's 32-bit graphics heap used in place, no host copy ([6349f8c0](https://github.com/edsuuu/VI-BR-PORT/commit/6349f8c0), [2aa1895d](https://github.com/edsuuu/VI-BR-PORT/commit/2aa1895d)) | opening cutscene | 18.2 fps | 21.6 and 24.4 fps (two runs) |
| The same | prison walk | 20.7 fps | 22.6 fps |
| The air's light bounded with the constant ambient ([557d2f03](https://github.com/edsuuu/VI-BR-PORT/commit/557d2f03), [31b9a82f](https://github.com/edsuuu/VI-BR-PORT/commit/31b9a82f)) | prison walk | 22.6 fps | 24.8 fps |
| Submissions prepared by a thread of the layer's own (`VICTORY_ASYNC_SUBMIT=1`, not the default) | prison walk | 22.9 fps (lowest line 14.4) | 24.3 fps (lowest line 18.6) |
| The title's 182 MB buffer of constants as a copy in the card's memory on NVIDIA, not read in place (`VICTORY_PORT_CONSTANTS=0`, what `scripts/play.ps1` sets) | prison walk | 23.0 fps (lowest line 14.9) | 25.4 fps (lowest line 20.9) |
| The same | the boat | 23.8 to 24.3 fps | 27.7 fps (lowest line 27.1) |
| The guards' lock shared, and the heaps' records in a journal (`VICTORY_PORT_GUARD=1`, `VICTORY_PORT_JOURNAL=1`; [accf24b1](https://github.com/edsuuu/VI-BR-PORT/commit/accf24b1b), [43afb5a2](https://github.com/edsuuu/VI-BR-PORT/commit/43afb5a27)) | prison walk | 29.4 ms preparing a frame, 22.6 fps | 26.6 ms preparing a frame, 25.6 fps |
| The room kept for render targets follows what they take, 1.4 GB, not a fixed 3 GB ([7e818069](https://github.com/edsuuu/VI-BR-PORT/commit/7e8180697)) | prison walk | 122 "no room" warnings a run; 3,041 texture swaps lost, 2,790 of them with no copy to make them from again | 12 to 13 warnings; 62 to 86 swaps lost, every one made again |
| Thin lists on NVIDIA, with the decoration bake translated at the title's call (`VICTORY_THIN=1`; [c354528b](https://github.com/edsuuu/VI-BR-PORT/commit/c354528b0)) | prison walk | 25.3 fps | 27.5 fps (lowest line 24.2) |
| The same | the boat | 27.7 fps | 29.1 fps |
| The copies whose bytes changed taken from a list (`VICTORY_CARD_TOUCHED=1`; [11135bfe](https://github.com/edsuuu/VI-BR-PORT/commit/11135bfec)) | prison walk, thin lists | 27.5 fps (lowest line 24.2) | 27.9 fps (lowest line 24.4), 21.2 ms preparing a frame |
| A declared range read whole once in 30 submissions, its lately changed pieces between (`VICTORY_CARD_COMPARE_EVERY=30`; [bd9a382f](https://github.com/edsuuu/VI-BR-PORT/commit/bd9a382fa), [b817ae6f](https://github.com/edsuuu/VI-BR-PORT/commit/b817ae6f5)) | opening cutscene | 2,373 MB read a frame in 36,217 pieces, 9.88 ms of card time | about 470 MB, 1.83 ms of card time |
| All of the above together, 27 Nov against 26 Nov | opening cutscene (the first two minutes) | 21.2 fps | 25.6 fps (lowest line 18.5) |
| The same | prison walk | 24.0 fps (lowest line 16.3) | 26.6 fps (lowest line 18.1) |
| The same | the boat | 20 to 22 fps | 28.7 fps (lowest line 28.2) |

The prison walk went from about 14 fps on 25 Nov to 24.0 fps on 26 Nov (lowest line 16.3) and
26.6 to 27.9 fps on 27 Nov; standing still in the cell the title holds its cap, 30.0 fps
(33.3 ms). The whole set of 27 Nov measured 26.6 fps in the prison where thin lists and the
list of changed copies alone measured 27.9: one run each, and the difference is within what two
runs of one build differ by.

## Tried and not kept (measured, no gain or a worse picture)

| Change | Scene | Result |
|---|---|---|
| Half render scale (`VICTORY_RENDER_SCALE=0.5`) | opening cutscene | 12.6 to 13.3 fps: the card is not limited by pixels |
| The ray-traced shadow passes left out (`VICTORY_SKIP_RT_SHADOW=1`) | prison walk | 21.9 fps against 22.6: nothing |
| More room for textures on the card (`VICTORY_TEXTURE_ROOM_PERCENT=97`) | prison walk | 20.8 fps: nothing, and textures still wait |
| The BVH builder with real ray queries | prison walk | 12.7 fps against 20.7 |
| Indirect light from the probes without rays (`VICTORY_AMBIENT=<number>`) | prison | posterized walls and skin: replaced by a constant, faint ambient |
| Compares of exactly the pieces found changed, with a book per piece (`VICTORY_CARD_COMPARE_HOT`) | prison walk | 23.4 fps against 25.3: the book costs 8 ms preparing a frame |
| The same | opening cutscene | 11 to 14 fps against 21.7: 7 to 8 ms less card time, 30 to 50 ms more preparing |
| In-place indirect records clamped one by one (`VICTORY_PORT_CONSTANTS_ORDER=1`) | prison walk | 19.2 fps against 23.0: the wait for the card went from 2 to 11.7 ms a frame |
| The heaps' records not checked again (`VICTORY_HEAP_RECHECK=0`) | opening cutscene | no difference here |
| Submissions prepared by a thread of the layer's own, with the shared lock and the journal (`VICTORY_ASYNC_SUBMIT=1`) | prison walk | 25.7 fps against 25.6: the frame is not bound by one thread there |

## Picture fixes of this fork (no frame-rate claim)

- Faces' mouths and expressions: 16-bit buffers of the facial network were read as zeros
  through descriptor tables.
- The opening video no longer steps back between frames: each list has its own upload buffer for
  movie planes written by the CPU ([fd36f4a4](https://github.com/edsuuu/VI-BR-PORT/commit/fd36f4a4)).
- A new texture shows in a slot only after its upload ([44ead3c5](https://github.com/edsuuu/VI-BR-PORT/commit/44ead3c5)):
  a reused pool piece showed old bytes (a green and blue checkerboard on the car seat of the opening).
- The Series S present of several planes (world and interface) is composed as the console's
  display does ([57a8a722](https://github.com/edsuuu/VI-BR-PORT/commit/57a8a722)), sent ahead without
  waiting for the card ([511f629a](https://github.com/edsuuu/VI-BR-PORT/commit/511f629a)).
- The decoration bake's 51 programs are known by name ([10ac68f9](https://github.com/edsuuu/VI-BR-PORT/commit/10ac68f9)):
  faces of the cutscene came out black.
- Tracing against the ray structures and the ray-hit shading are left out while every ray misses
  ([24f8d2ed](https://github.com/edsuuu/VI-BR-PORT/commit/24f8d2ed), [f1e5e454](https://github.com/edsuuu/VI-BR-PORT/commit/f1e5e454)): same picture.
- The prison's window glass: the pass that prefilters transmission asked for 64-lane waves and
  32-lane cards refused it every frame; it translates for waves of 32.
- The guard's tattooed arm, and other textures left with old or wrong bytes: the card's memory
  was called full while 1.6 GB of it sat kept for render targets that never used it
  ([7e818069](https://github.com/edsuuu/VI-BR-PORT/commit/7e8180697)), and a slot whose swap was lost is made again from its new view
  ([7b0084ab](https://github.com/edsuuu/VI-BR-PORT/commit/7b0084abc)).
- Blocks of noise on clothes, less often (not gone: about one launch in three still has coloured
  squares on a character's clothes or tattoos): with thin lists the decoration bake's upload pass ran
  out of the title's order; the list that binds a bake pipeline is translated at the title's call
  ([c354528b](https://github.com/edsuuu/VI-BR-PORT/commit/c354528b0)).
- The prison's common room is readable: the constant stand-in for indirect light is raised in
  rooms darker than a key, measured on the rooms on screen only ([7656395d](https://github.com/edsuuu/VI-BR-PORT/commit/7656395dd),
  [249c9b57](https://github.com/edsuuu/VI-BR-PORT/commit/249c9b577)); outside rooms the game's own light stays, with a floor
  ([849a20b3](https://github.com/edsuuu/VI-BR-PORT/commit/849a20b34)).
- The sea is lit in the open world: the water's tiled light runs on 32-lane cards
  ([6d5079cf](https://github.com/edsuuu/VI-BR-PORT/commit/6d5079cfa)).

## What stands between this and a steady 33 ms (27 Nov)

- On the boat the card is 98% busy at 28.7 to 29.1 fps: about 1 ms of card time a frame.
- In the prison walk and the opening two things are at the limit together. The layer's thread
  that prepares the submissions works 31 to 34 ms a frame (most of it in the passes that keep
  the copies in the card's memory equal to the title's bytes), and the card is 83 to 92% busy
  (in the opening, 28.5 ms a frame of the title's own commands, 2.4 ms of our sends and 1.8 ms of
  our compares).
- 35 compute programs of lighting, shadows and fog are translated approximately on 32-lane
  cards (among them the one that builds the light lists per cluster); the exact wave-wide form
  can be asked for program by program (`VICTORY_WAVE_WIDE_FOR`) and is under test.
- The opening's night scenes are darker than on the console: the stand-in for the ray-traced
  indirect light is a constant, not the light of the place.

## Community playthrough on an RTX 5070 Ti Laptop GPU

- **Build** [`da55992`](https://github.com/edsuuu/VI-BR-PORT/commit/da559920c3f4a11b5c30acb45c7e905370c773a3), release. The Xbox package manifest reports version `1.0.2.1`.
- **Test system** AMD Ryzen 9 8945HX, NVIDIA GeForce RTX 5070 Ti Laptop GPU (12 GB VRAM, driver 595.79), 32 GB RAM, Windows 11 build 26200.
- **Runtime** Xbox Series S profile, 1920×1080 display mode reported to the title, and the title's upscaler (`VICTORY_UPSCALER=6`). The log also confirms ray-miss mode, relay stage 2, and card steps stage 2.
- **Coverage** Parts of the prologue cutscenes, then the first mission's boat section. After going ashore, the tester continued on foot to look for Donnie and ended the session there. The log records the title suspending when the window closed.

The run lasted about 12 minutes and 18 seconds. Across 135 FPS readings from the runtime log, the arithmetic mean was **9.6 FPS**, the median was **7.9 FPS**, and the lowest and highest readings were **2.1 FPS** and **30.9 FPS**. These figures cover the whole session, including cutscenes and gameplay. The log has no scene timestamps for calculating a separate boat-section average.
