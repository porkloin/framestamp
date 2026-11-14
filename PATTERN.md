# The pattern

Every pixel of every frame is a function of `(x, y, width, height, frame, mode)`, in unsigned 32-bit integer
arithmetic that wraps, with no floating point and no texture sampling. Every GPU draws the same bytes, and a client
can draw any frame on the CPU to check what it decoded. `shaders/pattern.frag` is the reference;
[gamestream-test-client](https://github.com/porkloin/gamestream-test-client)'s `client/src/pattern.c` is the same
function in C. A change to one is a change to both, plus new test vectors, and a new minor version (a client that
checks against the old pattern calls every frame corrupt).

`x` and `y` are the pixel (0, 0 at the top left), `frame` counts the frames framestamp has presented (from 0), and
`mode` is 0 (static), 1 (region) or 2 (full). The output is 8-bit RGB, presented through an 8-bit UNORM swapchain, so
the bytes reach the compositor as they are.

## Building blocks

```text
hash(x):  x ^= x >> 16; x *= 0x7feb352d; x ^= x >> 15; x *= 0x846ca68b; x ^= x >> 16
hash3(a, b, c) = hash(a ^ hash(b ^ hash(c)))

noise(u, v, l, seed, ch): value noise, a lattice every l virtual pixels, coordinates in sixteenths
  lf = 16 l;  i = u / lf, j = v / lf, fx = u % lf, fy = v % lf;  s = 3 seed + ch
  vIJ = hash3(i + I, j + J, s) & 255 for I, J in {0, 1}
  top = v00 (lf - fx) + v10 fx;  bot = v01 (lf - fx) + v11 fx
  noise = (top (lf - fy) + bot fy) / (lf lf)
```

## Space

- The scale `s = max(1, height / 270)`: a virtual pixel is `s` real ones, so the scene looks the same at any size.
- Virtual size `wv = width / s`, `hv = height / s`; in sixteenths `w16 = 16 wv`, `h16 = 16 hv`.
- A pixel's position in sixteenths of a virtual pixel: `X = 16 x / s`, `Y = 16 y / s`.
- `OFF = 2^24` keeps every coordinate positive.

## Time

- A scene is 600 frames. In full mode `scene = frame / 600` and `t = frame % 600`. In region mode the scene is 0 and
  `t = frame % 600`. In static mode both are 0.
- `sb = hash(scene + 1)` seeds everything in the scene, so a new scene is a cut.
- The camera, full mode only (else at 0), in sixteenths, by segment of `t`. Its position is the sum of each
  segment's velocity times the frames of that segment so far:

| frames of t | velocity (x, y) per frame | |
|---|---|---|
| 0–239 | (24, 4) | a calm pan, 1.5 virtual pixels a frame |
| 240–299 | (384, 0) | a fast turn, 24 a frame |
| 300–419 | (48, −8) | the fight |
| 420–539 | (0, 0) | a still menu |
| 540–599 | (24, 4) | calm again |

- `cx = cam.x`, `cy = cam.y + 16384` (as unsigned).

## Layers, back to front

1. **Background**, panning with the camera: `u = X + cx + OFF`, `v = Y + cy + OFF`, and per channel
   `(3 noise(u, v, 32, sb, ch) + noise(u, v, 8, sb + 1, ch)) / 4`.
2. **Hills**, at twice the camera's speed across (parallax): `u1 = X + 2 cx + OFF`,
   `horizon = 5 h16 / 8 + noise(u1, OFF, 64, sb + 2, 0) (h16 / 4) / 255`. Below it (`Y > horizon`), per channel
   `3 noise(u1, v, 16, sb + 3, ch) / 4`.
3. **Sprites**: 6 in full mode, 1 in region, none in static. Sprite `k`, in signed arithmetic where it says so:
   - `hk = hash3(k, 77, sb)`
   - side `sz = 16 (24 + (hk & 31))`
   - speed `16 + ((hk >> 5) & 63)`
   - `span = w16 + 2 sz`
   - `px = ((hk >> 11) % span + speed t) % span − sz` (signed)
   - `per = 90 + ((hk >> 24) & 63)`
   - `py = h16 / 8 + (hk >> 16) % (h16 / 2) + tri(t, per) (h16 / 8) / per`, where
     `tri(t, per) = |((t + per) % 2per) − per|`
   - It covers `0 ≤ X − px < sz`, `0 ≤ Y − py < sz`, checkered in cells of 8 virtual pixels:
     `((dx >> 7) + (dy >> 7)) & 1`, colour bytes of `hash3(k, 1, sb)` when 0, else `hash3(k, 2, sb)`. The bytes are
     the low three of the hash, R first. Later sprites cover earlier ones.
4. **Particles**: full mode, `300 ≤ t < 420`, in `5 w16 / 8 ≤ X < 7 w16 / 8` and `h16 / 8 ≤ Y < 3 h16 / 8`.
   Colour bytes of `hash3(X >> 5, Y >> 5, frame ^ sb)`, new every frame. Noise like this doesn't survive encoding,
   by design, so the client leaves it out of its check.
5. **HUD**, still: `Y ≥ h16 − 384` is grey 40; within it, `128 ≤ X < w16 / 4` and `h16 − 256 ≤ Y < h16 − 128` is
   (200, 40, 40).
6. **The stamp**: cells of 4 virtual pixels, `xc = x / s / 4`, `yc = y / s / 4`, for `yc < 2` and `xc < 36`.
   - Cell 0 is bit 1 and cell 1 is bit 0 (sync).
   - Cells 2–33 are `frame`, most significant bit first.
   - Cells 34–35 are `mode`.
   - Row 0 is white (255) for a 1 and black (0) for a 0; row 1 is the inverse.

   A reader compares each cell's two rows: whichever is brighter says the bit, with no threshold to tune.

## Test vectors

64-bit FNV-1a over the RGB bytes, rows top to bottom, at 1920×1080: what `framestamp --dump-frame N --mode M --size
1920x1080` prints, drawn on the GPU.

| frame | static | region | full |
|---:|---|---|---|
| 0 | `6e61a791df72274f` | `4b1fc5395acf8107` | `cf0e60c667b105d6` |
| 1 | `14cca2c04835188f` | `9ad40ed0a27da5cb` | `d396acb5dee1efc7` |
| 330 | `c0d95a66433db4cf` | `932360b1ef06b6fc` | `8b5cc90ee1ffae23` |
| 1234 | `8ab37e9fc80c790f` | `54b48805bbd46666` | `4ac759a21062c013` |
