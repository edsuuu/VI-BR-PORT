# 27 November build on an 8 GB laptop: +1 fps, −400 MB VRAM, and two picture regressions

Three runs on one machine, one save, Series S profile, 1080p, prologue and the cell block, so the draw counts overlap (4,800 to 6,100). Same hardware as the earlier report (`docs/benchmarks-4060-laptop-8gb.md`): RTX 4060 **Laptop** 8 GB, i9-14900HX, 32 GB DDR5, hybrid graphics, driver 621.01.

- **Old build** — `main` as of this morning (the one the merged PR #7 measured).
- **New build, their defaults** — `94e5841`, every setting of `scripts/play.ps1` as published (Series S, `MEMORY_CARD_MB=1024`, `THIN=1`, `PORT_CONSTANTS=0`, the new ambient triple, and so on).
- **New build, `VICTORY_TEXTURE_NO_WAIT=0`** — the same run with only that one setting flipped.

| Run | Median fps | Mean | Lowest | VRAM observed | Picture |
|---|---|---|---|---|---|
| Old build | 13 | 12.6 | — | 7520–7644 MiB | as before |
| **New build, defaults** | **14** | 13.0 | 7 | **7134–7214 MiB** | two regressions, below |
| New build, `TEXTURE_NO_WAIT=0` | 13 | — | — | ~7429 MiB | both regressions still there |

So on this card the 27 November set is worth about one frame and 400 MB of VRAM that the old build did not have. That is much smaller than the desktop figures in `docs/BENCHMARKS.md` (24.0 → 26.6 in the prison walk), which may be the 8 GB card, or the laptop's power envelope, or both.

## The two picture regressions

Both are on the same screen: the cell-block corridor, a guard standing in a doorway, the camera behind the player. They are described here rather than shown because the repository's asset policy rules out game frames; I can put the three PNGs somewhere you can look at if that suits you.

1. **A guard's forearms and hands are covered in RGB noise.** With the new defaults the skin and the sleeve of both forearms are a coloured mosaic, the way VRAM looks before anything is written to it — the rest of the same character (face, shirt, badge) is correct. With `VICTORY_TEXTURE_NO_WAIT=0` the noise changes to yellow-and-black stripes but does not go away, so it does not look like a texture that arrives late and settles: it looks like the wrong texture or the wrong format for that material.
2. **The walls band.** The corridor wall goes from a smooth gradient in the old build to a handful of flat vertical steps in the new one, and the effect is stronger with `TEXTURE_NO_WAIT=0`. That is the sort of thing the new ambient triple (`VICTORY_AMBIENT=constant:0.1:0.5`, `VICTORY_AMBIENT_OUTSIDE=1:0.16`, `VICTORY_AMBIENT_KEY`, `VICTORY_AMBIENT_AIR`) would do if the stand-in light lands in the same few steps as the wall's own light, and it is the first thing I would suspect; the old run had `VICTORY_AMBIENT=constant:0.1` alone and no banding.

One thing that is **not** a regression, in case it comes up: two soft orange blobs on the corridor wall appear in the new build and in the old one, in the same place. Whatever draws them, the 27 November set did not change it.

## What I did not get to

- The `MEMORY_CARD_MB` question from PR #7 is unchanged by this set: on this card 1024 still costs a great deal and 2048 is what the old runs used, so the run above is not a like-for-like comparison of that switch alone. A second table with 2048 on the new build is what I would do next, and I will send it when I have it.
- All three runs were sampled every 5 s from the title's own counter (8 to 14 samples each) and `nvidia-smi` for VRAM; two runs of one build differ by more than the 1 fps above on this machine, so treat that as "no worse", not as a gain.
