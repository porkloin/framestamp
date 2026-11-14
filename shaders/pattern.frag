#version 450

// framestamp's picture: a function of the pixel, the size, the frame number and the mode, in unsigned integer
// arithmetic only, so every GPU draws the same pixels and a client can draw any frame again to check what it decoded.
// PATTERN.md is the specification; gamestream-test-client's pattern.c is the same function in C. Change one, change
// both, and the test vectors in PATTERN.md.

layout(location = 0) out vec4 out_color;

layout(push_constant) uniform Params {
	uint width;
	uint height;
	uint frame;
	uint mode;
} p;

const uint MODE_STATIC = 0u;
const uint MODE_REGION = 1u;
const uint MODE_FULL = 2u;

// Every coordinate is in sixteenths of a virtual pixel, offset to stay positive.
const uint OFF = 1u << 24;
const uint SCENE_FRAMES = 600u;

uint hash(uint x) {
	x ^= x >> 16;
	x *= 0x7feb352du;
	x ^= x >> 15;
	x *= 0x846ca68bu;
	x ^= x >> 16;
	return x;
}

uint hash3(uint a, uint b, uint c) {
	return hash(a ^ hash(b ^ hash(c)));
}

// Value noise, one channel: a lattice every `l` virtual pixels, interpolated bilinearly in sixteenths.
uint noise(uint u, uint v, uint l, uint seed, uint ch) {
	uint lf = l * 16u;
	uint i = u / lf, j = v / lf, fx = u % lf, fy = v % lf;
	uint s = seed * 3u + ch;
	uint v00 = hash3(i, j, s) & 255u;
	uint v10 = hash3(i + 1u, j, s) & 255u;
	uint v01 = hash3(i, j + 1u, s) & 255u;
	uint v11 = hash3(i + 1u, j + 1u, s) & 255u;
	uint top = v00 * (lf - fx) + v10 * fx;
	uint bot = v01 * (lf - fx) + v11 * fx;
	return (top * (lf - fy) + bot * fy) / (lf * lf);
}

// The camera's position at frame `t` of a scene, in sixteenths of a virtual pixel: a calm pan, a fast turn, a fight,
// a still menu, a calm pan again.
ivec2 camera(uint t) {
	const int len[5] = int[5](240, 60, 120, 120, 60);
	const ivec2 vel[5] = ivec2[5](ivec2(24, 4), ivec2(384, 0), ivec2(48, -8), ivec2(0, 0), ivec2(24, 4));
	ivec2 c = ivec2(0);
	int start = 0;
	for (int k = 0; k < 5; k++) {
		int d = clamp(int(t) - start, 0, len[k]);
		c += vel[k] * d;
		start += len[k];
	}
	return c;
}

// 0 at 0, up to `per` at `per`, back to 0 at 2 × `per`.
uint tri(uint t, uint per) {
	int a = int((t + per) % (2u * per)) - int(per);
	return uint(abs(a));
}

uvec3 bytes3(uint h) {
	return uvec3(h & 255u, (h >> 8) & 255u, (h >> 16) & 255u);
}

uvec3 pattern(uint x, uint y) {
	uint s = max(1u, p.height / 270u);
	uint wv = p.width / s, hv = p.height / s;
	uint w16 = wv * 16u, h16 = hv * 16u;
	uint X = x * 16u / s, Y = y * 16u / s;

	uint scene = p.mode == MODE_FULL ? p.frame / SCENE_FRAMES : 0u;
	uint t = p.mode == MODE_STATIC ? 0u : p.frame % SCENE_FRAMES;
	uint sb = hash(scene + 1u);
	ivec2 cam = p.mode == MODE_FULL ? camera(t) : ivec2(0);
	uint cx = uint(cam.x), cy = uint(cam.y + 16384);

	// The background, panning with the camera.
	uint u = X + cx + OFF, v = Y + cy + OFF;
	uvec3 c;
	for (uint ch = 0u; ch < 3u; ch++)
		c[ch] = (3u * noise(u, v, 32u, sb, ch) + noise(u, v, 8u, sb + 1u, ch)) / 4u;

	// Hills in front, moving twice as fast: parallax.
	uint u1 = X + 2u * cx + OFF;
	uint horizon = h16 * 5u / 8u + noise(u1, OFF, 64u, sb + 2u, 0u) * (h16 / 4u) / 255u;
	if (Y > horizon)
		for (uint ch = 0u; ch < 3u; ch++)
			c[ch] = noise(u1, v, 16u, sb + 3u, ch) * 3u / 4u;

	// Sprites, on the screen: six in full, one in region.
	uint sprites = p.mode == MODE_FULL ? 6u : (p.mode == MODE_REGION ? 1u : 0u);
	for (uint k = 0u; k < sprites; k++) {
		uint hk = hash3(k, 77u, sb);
		int sz = int((24u + (hk & 31u)) * 16u);
		uint speed = 16u + ((hk >> 5) & 63u);
		uint span = w16 + 2u * uint(sz);
		int px = int(((hk >> 11) % span + speed * t) % span) - sz;
		uint per = 90u + ((hk >> 24) & 63u);
		int py = int(h16 / 8u + (hk >> 16) % (h16 / 2u) + tri(t, per) * (h16 / 8u) / per);
		int dx = int(X) - px, dy = int(Y) - py;
		if (dx >= 0 && dx < sz && dy >= 0 && dy < sz)
			c = (((uint(dx) >> 7) + (uint(dy) >> 7)) & 1u) == 0u ? bytes3(hash3(k, 1u, sb)) : bytes3(hash3(k, 2u, sb));
	}

	// Particles, new every frame, during the fight.
	if (p.mode == MODE_FULL && t >= 300u && t < 420u && X >= w16 * 5u / 8u && X < w16 * 7u / 8u && Y >= h16 / 8u &&
		Y < h16 * 3u / 8u)
		c = bytes3(hash3(X >> 5, Y >> 5, p.frame ^ sb));

	// The HUD, still: a bar along the bottom with a health bar in it.
	if (Y >= h16 - 384u) {
		c = uvec3(40u);
		if (X >= 128u && X < w16 / 4u && Y >= h16 - 256u && Y < h16 - 128u)
			c = uvec3(200u, 40u, 40u);
	}

	// The stamp: 36 cells of 4 virtual pixels in two rows, the second the first inverted: 1 and 0 to sync, the
	// frame number (most significant bit first), the mode.
	uint xc = x / s / 4u, yc = y / s / 4u;
	if (yc < 2u && xc < 36u) {
		uint bit = xc == 0u ? 1u : xc == 1u ? 0u : xc < 34u ? (p.frame >> (33u - xc)) & 1u : (p.mode >> (35u - xc)) & 1u;
		c = uvec3((bit ^ yc) == 1u ? 255u : 0u);
	}
	return c;
}

void main() {
	uvec3 c = pattern(uint(gl_FragCoord.x), uint(gl_FragCoord.y));
	out_color = vec4(vec3(c) / 255.0, 1.0);
}
