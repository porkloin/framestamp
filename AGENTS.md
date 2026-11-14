# framestamp: details

What README.md leaves out: the rules the pattern lives by, the log, and the layout. README.md is the short version
for people, and PATTERN.md specifies the picture.

## The pattern's rules

- Unsigned 32-bit integer arithmetic only in the shader's picture: no floats, no texture sampling, no derivatives.
  That makes every GPU draw the same bytes, and lets gamestream-test-client's `client/src/pattern.c` draw them on the
  CPU.
- A frame depends only on `(x, y, width, height, frame, mode)`: never the clock. A late frame is the same picture,
  just late.
- The output goes through an 8-bit UNORM swapchain in `SRGB_NONLINEAR`, so the shader's bytes are what's presented
  (no sRGB encoding on the way). `gpu::Gpu::pick_format` refuses anything else.
- Changing the picture means changing `pattern.c` the same way, the test vectors in PATTERN.md and in
  gamestream-test-client's `tests/test_pattern.c` (from `framestamp --dump-frame`), and releasing a new minor version.
  The two are pinned to each other in moonshine-test-headless's image.

## Behavior

- `frame` counts frames presented, from 0. It wraps after 2^32.
- It redraws continuously, so a capturing compositor always has a new frame.
- `--dump-frame N` draws frame N without a window (an image of its own, read back), writes it as a binary PPM to
  `--out` or stdout, prints its FNV-1a hash to stderr, and exits. It's how the test vectors are made.
- `--no-vsync` presents with mailbox or immediate instead of FIFO.

## Log

`--log PATH` appends one JSON object per line, flushed, when the presented size is first known and whenever it
changes:

```text
{"event":"size","mode":"full","width":1920,"height":1080,"frame":0,"unix_us":1793728374923981}
```

On exit it prints the frames presented and frame-time percentiles to stderr.

## Layout

- `src/main.rs`: options, the event loop, the frame counter, the log, `--dump-frame`.
- `src/gpu.rs`: Vulkan: the swapchain, the pipeline, each frame's draw, and `dump` (the same draw into an image of its
  own).
- `shaders/pattern.frag`: the picture. `shaders/fullscreen.vert`: one triangle over the screen. The `.spv` files are
  committed; `shaders/build.sh` remakes them with `glslc`.
- `.github/workflows/`: CI (fmt, clippy, build), and release-please with release binaries for x86_64 and aarch64
  Linux, plus SHA256SUMS.
