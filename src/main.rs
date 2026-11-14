//! framestamp: a picture a client can draw again. Each frame is a function of its number, its size and the mode
//! (PATTERN.md), stamped with its number, so a streaming client that decoded frame N can draw frame N itself and see
//! whether what it decoded is right.

mod gpu;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowId};

/// What moves (PATTERN.md). The numbers are the shader's, and the stamp's mode bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Mode {
	/// Only the stamp changes.
	Static = 0,
	/// One sprite moves over a still background.
	Region = 1,
	/// The camera pans and turns, layers move at their own speeds, sprites cross, particles flicker, scenes cut.
	Full = 2,
}

impl Mode {
	const ALL: [Self; 3] = [Self::Static, Self::Region, Self::Full];

	fn name(self) -> &'static str {
		match self {
			Self::Static => "static",
			Self::Region => "region",
			Self::Full => "full",
		}
	}
}

pub struct Options {
	pub mode: Mode,
	pub duration: Option<Duration>,
	pub fullscreen: bool,
	pub size: (u32, u32),
	pub vsync: bool,
	pub log: Option<String>,
	pub dump: Option<u32>,
	pub out: Option<String>,
}

const USAGE: &str = "\
usage: framestamp [options]

  --mode static|region|full  what moves (default full): only the stamp; one sprite over a still
                             background; or the whole scene, panning, turning and cutting
  --duration SECONDS         exit after SECONDS
  --fullscreen               borderless fullscreen
  --size WxH                 window size in pixels (default 1920x1080), or the dump's size
  --no-vsync                 present as fast as possible (mailbox or immediate)
  --log PATH                 append the mode and size presented, and each size change, as JSON lines to PATH
  --dump-frame N             draw frame N on the GPU without a window, write it as a PPM to --out (or stdout),
                             and print its FNV-1a hash (PATTERN.md's test vectors); then exit
  --out PATH                 where --dump-frame writes

keys: q or escape quits. frames presented and frame times are printed on exit.";

fn parse_args() -> Result<Options, String> {
	let mut o = Options {
		mode: Mode::Full,
		duration: None,
		fullscreen: false,
		size: (1920, 1080),
		vsync: true,
		log: None,
		dump: None,
		out: None,
	};
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		match arg.as_str() {
			"--mode" => {
				let v = args.next().unwrap_or_default();
				o.mode = Mode::ALL
					.into_iter()
					.find(|m| m.name() == v)
					.ok_or_else(|| format!("unknown mode {v:?}"))?;
			},
			"--duration" => {
				o.duration = Some(
					args.next()
						.and_then(|v| v.parse::<f64>().ok())
						.filter(|s| *s > 0.0)
						.map(Duration::from_secs_f64)
						.ok_or("--duration needs a number of seconds")?,
				)
			},
			"--fullscreen" => o.fullscreen = true,
			"--size" => {
				let v = args.next().unwrap_or_default();
				o.size = v
					.split_once('x')
					.and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
					.filter(|&(w, h): &(u32, u32)| w > 0 && h > 0)
					.ok_or_else(|| format!("--size needs WxH, got {v:?}"))?;
			},
			"--no-vsync" => o.vsync = false,
			"--log" => o.log = Some(args.next().ok_or("--log needs a path")?),
			"--dump-frame" => {
				o.dump = Some(
					args.next()
						.and_then(|v| v.parse().ok())
						.ok_or("--dump-frame needs a frame number")?,
				)
			},
			"--out" => o.out = Some(args.next().ok_or("--out needs a path")?),
			"-h" | "--help" => {
				println!("{USAGE}");
				std::process::exit(0);
			},
			_ => return Err(format!("unknown option {arg:?}")),
		}
	}
	Ok(o)
}

/// 64-bit FNV-1a, PATTERN.md's test vector hash.
fn fnv1a(bytes: &[u8]) -> u64 {
	bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
		(h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
	})
}

fn dump(options: &Options, frame: u32) -> Result<(), String> {
	let (w, h) = options.size;
	let rgb = gpu::dump(w, h, frame, options.mode)?;
	eprintln!(
		"framestamp: frame {frame} {} {w}x{h} fnv1a {:016x}",
		options.mode.name(),
		fnv1a(&rgb)
	);
	let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
	ppm.extend_from_slice(&rgb);
	match &options.out {
		Some(path) => std::fs::write(path, ppm).map_err(|e| format!("{path}: {e}")),
		None => std::io::stdout().write_all(&ppm).map_err(|e| e.to_string()),
	}
}

struct App {
	options: Options,
	gpu: Option<gpu::Gpu>,
	start: Instant,
	/// The next frame's number: frames presented so far.
	frame: u32,
	size: (u32, u32),
	last_frame: Option<Instant>,
	frame_times: Vec<f32>,
	error: Option<String>,
	/// --log, appended to so a reader following it keeps its place when framestamp starts again.
	log: Option<std::fs::File>,
}

/// Microseconds since the Unix epoch.
fn unix_us() -> u128 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_micros())
}

impl App {
	/// One line of --log, written whole and flushed: the mode and size now presented, from which frame.
	fn log_size(&mut self) {
		let (w, h) = self.size;
		if let Some(log) = &mut self.log
			&& let Err(e) = writeln!(
				log,
				r#"{{"event":"size","mode":"{}","width":{w},"height":{h},"frame":{},"unix_us":{}}}"#,
				self.options.mode.name(),
				self.frame,
				unix_us()
			) {
			eprintln!("framestamp: writing the log failed: {e}");
			self.log = None;
		}
	}

	fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
		self.error = Some(error);
		event_loop.exit();
	}

	fn frame(&mut self, event_loop: &ActiveEventLoop) {
		let now = Instant::now();
		if self.options.duration.is_some_and(|d| now - self.start >= d) {
			event_loop.exit();
			return;
		}
		let Some(gpu) = &mut self.gpu else { return };
		match gpu.draw(self.frame) {
			Ok(true) => self.frame = self.frame.wrapping_add(1),
			Ok(false) => return,
			Err(e) => return self.fail(event_loop, e),
		}
		let size = gpu.extent();
		if size != self.size {
			self.size = size;
			self.log_size();
		}
		if let Some(last) = self.last_frame.replace(now) {
			self.frame_times.push((now - last).as_secs_f32() * 1000.0);
		}
	}

	fn report(&mut self) {
		let t = &mut self.frame_times;
		let Some(&max) = t.iter().max_by(|a, b| a.total_cmp(b)) else {
			return;
		};
		t.sort_by(f32::total_cmp);
		let at = |q: f32| t[((t.len() - 1) as f32 * q) as usize];
		eprintln!(
			"framestamp: {} frames, {:.1} fps, frame time p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
			self.frame,
			t.len() as f32 * 1000.0 / t.iter().sum::<f32>(),
			at(0.5),
			at(0.99),
			max,
		);
	}
}

impl ApplicationHandler for App {
	fn resumed(&mut self, event_loop: &ActiveEventLoop) {
		if self.gpu.is_some() {
			return;
		}
		let (w, h) = self.options.size;
		let mut attributes = Window::default_attributes()
			.with_title("framestamp")
			.with_inner_size(PhysicalSize::new(w, h));
		if self.options.fullscreen {
			attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
		}
		let window = match event_loop.create_window(attributes) {
			Ok(window) => Arc::new(window),
			Err(e) => return self.fail(event_loop, format!("couldn't create a window: {e}")),
		};
		match gpu::Gpu::new(window, &self.options) {
			Ok(gpu) => {
				eprintln!("framestamp: {} on {}", self.options.mode.name(), gpu.device_name());
				self.gpu = Some(gpu);
				self.start = Instant::now();
			},
			Err(e) => self.fail(event_loop, e),
		}
	}

	fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
		match event {
			WindowEvent::CloseRequested => event_loop.exit(),
			WindowEvent::Resized(_) => {
				if let Some(gpu) = &mut self.gpu {
					gpu.resized();
				}
			},
			WindowEvent::KeyboardInput {
				event: KeyEvent {
					logical_key,
					state: ElementState::Pressed,
					repeat: false,
					..
				},
				..
			} => match logical_key.as_ref() {
				Key::Named(NamedKey::Escape) | Key::Character("q") => event_loop.exit(),
				_ => {},
			},
			WindowEvent::RedrawRequested => self.frame(event_loop),
			_ => {},
		}
	}

	fn about_to_wait(&mut self, _: &ActiveEventLoop) {
		if let Some(gpu) = &self.gpu {
			gpu.window().request_redraw();
		}
	}
}

fn main() -> ExitCode {
	let options = match parse_args() {
		Ok(o) => o,
		Err(e) => {
			eprintln!("framestamp: {e}\n\n{USAGE}");
			return ExitCode::from(2);
		},
	};
	if let Some(frame) = options.dump {
		return match dump(&options, frame) {
			Ok(()) => ExitCode::SUCCESS,
			Err(e) => {
				eprintln!("framestamp: {e}");
				ExitCode::FAILURE
			},
		};
	}
	let event_loop = match EventLoop::new() {
		Ok(l) => l,
		Err(e) => {
			eprintln!("framestamp: no display: {e}");
			return ExitCode::FAILURE;
		},
	};
	let mut app = App {
		options,
		gpu: None,
		start: Instant::now(),
		frame: 0,
		size: (0, 0),
		last_frame: None,
		frame_times: Vec::new(),
		error: None,
		log: None,
	};
	if let Some(path) = &app.options.log {
		match std::fs::OpenOptions::new().create(true).append(true).open(path) {
			Ok(f) => app.log = Some(f),
			Err(e) => {
				eprintln!("framestamp: {path}: {e}");
				return ExitCode::FAILURE;
			},
		}
	}
	if let Err(e) = event_loop.run_app(&mut app) {
		eprintln!("framestamp: {e}");
		return ExitCode::FAILURE;
	}
	app.report();
	match app.error {
		Some(e) => {
			eprintln!("framestamp: {e}");
			ExitCode::FAILURE
		},
		None => ExitCode::SUCCESS,
	}
}
