# RTX 4060 Laptop (8 GB) — 47-sample session and switch A/B

One machine, one title build (`1.0.2.1`), Series S profile, 1080p.

| | |
|---|---|
| GPU | RTX 4060 **Laptop**, 8188 MiB (D3D12 budget 6489–6569 MB), driver 621.01 |
| CPU / RAM | i9-14900HX / 32 GB DDR5-5600 |
| OS | Windows 11 build 26300, hybrid graphics (Intel UHD + NVIDIA) |
| Build | MSVC 14.44 + Windows SDK 10.0.26100, rust 1.98.1, `cargo build --locked --release -p victory` in 46 s |

## Frame rate

| Scene | draws | fps |
|---|---|---|
| Menus / first boot | < 100 | 30.0 (title cap) |
| Cell, standing | ~1.5k | 17–24 |
| Cell / corridor, walking | 3–5k | 12–17 |
| Street / promenade | 8.5k | 9–12 |

Long session (6 min 11 s, 47 samples at 5 s, read from the title bar): median **12 fps**, mean 12.2, best 30, worst 3; median frame 84 ms, worst 356 ms. GPU median 85%, peak 99%, 53 W (peak 67 W), 62–64 °C, no throttling.

Frame rate is almost independent of draw count here (3k → 13 fps, 8.5k → 12 fps), so the cost on this card is per-frame fixed overhead rather than scene scale.

## VRAM grows during play

7342 → 7921 MiB (+579 MB in 6 minutes), i.e. 96% of 8192. At the ceiling the frame rate drops to 3–4 fps and textures blur. Restarting the title resets it. Sessions of 20–30 minutes are the practical limit on 8 GB.

## Switch A/B

| Change | Before | After |
|---|---|---|
| `VICTORY_MEMORY_CARD_MB=1024` → `2048` | 1–3 fps (GPU 3–25% busy, frames 300–1200 ms) | **13–17 fps** (GPU 85%) |
| `VICTORY_HEAP_RECHECK=0` | prepare 44 ms/frame, copies 12 ms / 70 MB | prepare **29 ms**, copies **10 ms / 55 MB** |
| `VICTORY_DISPLAY_MODE=1280x720` | 13–17 fps | 13–14 fps (not pixel-bound) |
| `VICTORY_ASYNC_SUBMIT=1` | 13–17 fps | **7–12 fps** (no gain on a 32 GB laptop; the extra submit thread contends) |

No visual difference from `HEAP_RECHECK=0`; the "texture picked by index changed after its upload" warnings go to zero.

## Prewarm

`shaders` → 30,418 entries (1.19 GB). `warm` → 88,573 pipelines in 1093 s, 0 failures (median 3.1 ms, p99 4989 ms, slowest single pipeline 25,866 ms). After that the longest frame in a 10-minute run was **243 ms**, with 4461 pipelines started early and 1 made at runtime.

## Shaders that still do not translate

`002e93a6cc2aa355`, `0e64f14fae060d04`, `dbdbf36298727135`, `96ccb14ef981fd85` — `a lane-read VGPR written without a dword value`;
`77360df21e12ea43`, `9fc111d3be79364f`, `39321a95223001ed` — `a first-lane read where not every lane…`;
`07c93bfb77fecebb` — `a descriptor read only as data`.
Plus 7 compute pipelines refused by the driver with `E_INVALIDARG (0x80070057)`.

`ID3D12CommandQueue` slot 23 (`0xb8`) is unimplemented and is called on the shutdown path; returning `S_OK` there locally (3 lines in `object.rs`) lets the title exit cleanly with no observed side effects.

For reference, driver-level frame generation works on top of this layer: at 9 fps rendered in an 8.5k-draw scene the NVIDIA overlay reports 20 fps displayed (2.2×). The app's OFA engine counter stays at 0%, so it cannot be used as a signal.
