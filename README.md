# framestamp

A test pattern with gamelike movement, rendered with explicit frame counts on each frame, allowing streaming
applications to verify picture accuracy in motion.

Every frame is a function of its number, which is stamped in the top left. A client that decodes frame 4127 can draw
frame 4127 itself and compare it with the decoded frame. That catches the corruption a decoder doesn't report, such
as a picture built on a reference frame the client threw away.
[gamestream-test-client](https://github.com/porkloin/gamestream-test-client) does this with `--check-picture`, and
[moonshine-test-headless](https://github.com/porkloin/moonshine-test-headless) streams it as a workload.

    framestamp --fullscreen                 # the whole scene
    framestamp --mode static                # only the stamp changes
    framestamp --mode region                # one sprite over a still background
    framestamp --dump-frame 330 --out f.ppm # one frame, drawn without a window

`framestamp --help` for the rest. Q quits.

## Modes

| Mode | What moves |
|---|---|
| `full` (default) | A camera that pans, turns fast, stops and pans again; hills in parallax; six sprites; a burst of particles; a cut to a new scene every 600 frames |
| `region` | One sprite crossing a still background |
| `static` | Only the stamp |

A HUD bar along the bottom never moves. [PATTERN.md](PATTERN.md) specifies every pixel.

## Prereqs

- Vulkan 1.3
- A compositor offering an 8-bit UNORM swapchain (any does)

## Installing

On Linux, grab the release tarball, check it against its checksum, and put the binary in `~/.local/bin`:

    name=framestamp-linux-$(uname -m)
    curl -fLO https://github.com/porkloin/framestamp/releases/latest/download/$name.tar.gz
    curl -fLO https://github.com/porkloin/framestamp/releases/latest/download/SHA256SUMS
    sha256sum -c --ignore-missing SHA256SUMS
    tar xzf $name.tar.gz
    install -Dm755 $name/framestamp ~/.local/bin/framestamp

Or with Homebrew:

    brew install porkloin/tap/framestamp

Note: on older Linux distros where Homebrew uses its own glibc, it may not be able to load your GPU
driver. In that case, use a binary from the [releases page](https://github.com/porkloin/framestamp/releases) instead.

## Building

    cargo build --release

The shaders' SPIR-V is committed. After changing a shader, run `shaders/build.sh` (needs `glslc`).

## License

GPL-3.0-or-later.
