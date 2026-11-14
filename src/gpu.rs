//! The Vulkan side: one full-screen draw of the pattern per frame into an 8-bit swapchain, so the pattern's bytes are
//! what's presented; and `dump`, the same draw into an image of its own, read back.

use std::sync::Arc;

use ash::{khr, vk};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use crate::{Mode, Options};

const FRAMES_IN_FLIGHT: usize = 2;
const VERT_SPV: &[u8] = include_bytes!("../shaders/fullscreen.vert.spv");
const FRAG_SPV: &[u8] = include_bytes!("../shaders/pattern.frag.spv");

const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
	aspect_mask: vk::ImageAspectFlags::COLOR,
	base_mip_level: 0,
	level_count: 1,
	base_array_layer: 0,
	layer_count: 1,
};

/// Matches the shader's push constants.
#[repr(C)]
#[derive(Clone, Copy)]
struct Params {
	width: u32,
	height: u32,
	frame: u32,
	mode: u32,
}

fn vk_err(what: &str) -> impl Fn(vk::Result) -> String + '_ {
	move |e| format!("{what}: {e}")
}

fn load_vulkan() -> Result<ash::Entry, String> {
	// SAFETY: loads the system's Vulkan loader, which stays loaded as long as
	// the returned entry.
	unsafe { ash::Entry::load() }.map_err(|e| format!("no Vulkan loader: {e}"))
}

fn create_instance(entry: &ash::Entry, extensions: &[*const std::ffi::c_char]) -> Result<ash::Instance, String> {
	let app = vk::ApplicationInfo::default()
		.application_name(c"framestamp")
		.api_version(vk::API_VERSION_1_3);
	// SAFETY: valid create info.
	unsafe {
		entry.create_instance(
			&vk::InstanceCreateInfo::default()
				.application_info(&app)
				.enabled_extension_names(extensions),
			None,
		)
	}
	.map_err(vk_err("create instance"))
}

/// A Vulkan 1.3 device with a queue family `fits`; a discrete GPU if there is one.
fn pick_device(
	instance: &ash::Instance,
	fits: impl Fn(vk::PhysicalDevice, u32, &vk::QueueFamilyProperties) -> bool,
) -> Result<(vk::PhysicalDevice, u32, String), String> {
	let mut best = None;
	// SAFETY: valid instance.
	for physical in unsafe { instance.enumerate_physical_devices() }.map_err(vk_err("devices"))? {
		// SAFETY: valid physical device.
		let props = unsafe { instance.get_physical_device_properties(physical) };
		if props.api_version < vk::API_VERSION_1_3 {
			continue;
		}
		// SAFETY: valid physical device.
		let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
		let family = (0..families.len() as u32).find(|&i| fits(physical, i, &families[i as usize]));
		if let Some(family) = family {
			let discrete = props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU;
			let name = props
				.device_name_as_c_str()
				.unwrap_or(c"?")
				.to_string_lossy()
				.into_owned();
			if best.is_none() || discrete {
				best = Some((physical, family, name));
			}
			if discrete {
				break;
			}
		}
	}
	best.ok_or_else(|| "no Vulkan 1.3 device that can draw".to_string())
}

fn create_device(
	instance: &ash::Instance,
	physical: vk::PhysicalDevice,
	family: u32,
	extensions: &[*const std::ffi::c_char],
) -> Result<ash::Device, String> {
	let mut features13 = vk::PhysicalDeviceVulkan13Features::default()
		.dynamic_rendering(true)
		.synchronization2(true);
	let priorities = [1.0];
	let queues = [vk::DeviceQueueCreateInfo::default()
		.queue_family_index(family)
		.queue_priorities(&priorities)];
	// SAFETY: valid physical device and create info.
	unsafe {
		instance.create_device(
			physical,
			&vk::DeviceCreateInfo::default()
				.queue_create_infos(&queues)
				.enabled_extension_names(extensions)
				.push_next(&mut features13),
			None,
		)
	}
	.map_err(vk_err("create device"))
}

/// The pipeline's layout and shaders, the same for a window and a dump.
struct Shaders {
	layout: vk::PipelineLayout,
	vert: vk::ShaderModule,
	frag: vk::ShaderModule,
}

impl Shaders {
	fn new(device: &ash::Device) -> Result<Self, String> {
		let push = [vk::PushConstantRange::default()
			.stage_flags(vk::ShaderStageFlags::FRAGMENT)
			.size(size_of::<Params>() as u32)];
		// SAFETY: valid device and create infos.
		unsafe {
			let layout = device
				.create_pipeline_layout(
					&vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&push),
					None,
				)
				.map_err(vk_err("pipeline layout"))?;
			let module = |spv: &[u8]| {
				let code = ash::util::read_spv(&mut std::io::Cursor::new(spv)).map_err(|e| e.to_string())?;
				device
					.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None)
					.map_err(vk_err("shader module"))
			};
			Ok(Self {
				layout,
				vert: module(VERT_SPV)?,
				frag: module(FRAG_SPV)?,
			})
		}
	}

	fn pipeline(&self, device: &ash::Device, format: vk::Format) -> Result<vk::Pipeline, String> {
		let stages = [
			vk::PipelineShaderStageCreateInfo::default()
				.stage(vk::ShaderStageFlags::VERTEX)
				.module(self.vert)
				.name(c"main"),
			vk::PipelineShaderStageCreateInfo::default()
				.stage(vk::ShaderStageFlags::FRAGMENT)
				.module(self.frag)
				.name(c"main"),
		];
		let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
		let input_assembly =
			vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_LIST);
		let viewport = vk::PipelineViewportStateCreateInfo::default()
			.viewport_count(1)
			.scissor_count(1);
		let raster = vk::PipelineRasterizationStateCreateInfo::default()
			.polygon_mode(vk::PolygonMode::FILL)
			.cull_mode(vk::CullModeFlags::NONE)
			.line_width(1.0);
		let multisample =
			vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
		let attachments =
			[vk::PipelineColorBlendAttachmentState::default().color_write_mask(vk::ColorComponentFlags::RGBA)];
		let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
		let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
		let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
		let formats = [format];
		let mut rendering = vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
		let info = vk::GraphicsPipelineCreateInfo::default()
			.stages(&stages)
			.vertex_input_state(&vertex_input)
			.input_assembly_state(&input_assembly)
			.viewport_state(&viewport)
			.rasterization_state(&raster)
			.multisample_state(&multisample)
			.color_blend_state(&blend)
			.dynamic_state(&dynamic)
			.layout(self.layout)
			.push_next(&mut rendering);
		// SAFETY: every referenced object is this device's and alive.
		unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
			.map(|p| p[0])
			.map_err(|(_, e)| format!("create pipeline: {e}"))
	}

	/// Record the draw of `frame` into `target`'s view (in COLOR_ATTACHMENT_OPTIMAL) of its extent.
	///
	/// # Safety
	///
	/// `cmd` is recording, and `pipeline` and `view` are alive.
	unsafe fn record(
		&self,
		device: &ash::Device,
		cmd: vk::CommandBuffer,
		pipeline: vk::Pipeline,
		target: (vk::ImageView, vk::Extent2D),
		frame: u32,
		mode: Mode,
	) {
		let (view, extent) = target;
		let attachments = [vk::RenderingAttachmentInfo::default()
			.image_view(view)
			.image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
			.load_op(vk::AttachmentLoadOp::DONT_CARE)
			.store_op(vk::AttachmentStoreOp::STORE)];
		let area = vk::Rect2D {
			offset: vk::Offset2D::default(),
			extent,
		};
		unsafe {
			device.cmd_begin_rendering(
				cmd,
				&vk::RenderingInfo::default()
					.render_area(area)
					.layer_count(1)
					.color_attachments(&attachments),
			);
			device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
			device.cmd_set_viewport(
				cmd,
				0,
				&[vk::Viewport {
					x: 0.0,
					y: 0.0,
					width: extent.width as f32,
					height: extent.height as f32,
					min_depth: 0.0,
					max_depth: 1.0,
				}],
			);
			device.cmd_set_scissor(cmd, 0, &[area]);
			let params = Params {
				width: extent.width,
				height: extent.height,
				frame,
				mode: mode as u32,
			};
			let bytes = std::slice::from_raw_parts((&raw const params).cast::<u8>(), size_of::<Params>());
			device.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::FRAGMENT, 0, bytes);
			device.cmd_draw(cmd, 3, 1, 0, 0);
			device.cmd_end_rendering(cmd);
		}
	}

	/// # Safety
	///
	/// Nothing using them is in flight.
	unsafe fn destroy(&self, device: &ash::Device) {
		unsafe {
			device.destroy_shader_module(self.vert, None);
			device.destroy_shader_module(self.frag, None);
			device.destroy_pipeline_layout(self.layout, None);
		}
	}
}

struct Frame {
	command_buffer: vk::CommandBuffer,
	fence: vk::Fence,
	acquired: vk::Semaphore,
}

pub struct Gpu {
	window: Arc<Window>,
	/// Keeps libvulkan loaded.
	_entry: ash::Entry,
	instance: ash::Instance,
	surface_fn: khr::surface::Instance,
	surface: vk::SurfaceKHR,
	physical: vk::PhysicalDevice,
	device_name: String,
	device: ash::Device,
	queue: vk::Queue,
	swapchain_fn: khr::swapchain::Device,
	pool: vk::CommandPool,
	frames: Vec<Frame>,
	frame: usize,
	shaders: Shaders,
	pipeline: vk::Pipeline,
	pipeline_format: vk::Format,
	swapchain: vk::SwapchainKHR,
	images: Vec<vk::Image>,
	views: Vec<vk::ImageView>,
	/// One per swapchain image: presentation waits on the image's own.
	rendered: Vec<vk::Semaphore>,
	extent: vk::Extent2D,
	mode: Mode,
	vsync: bool,
	/// The swapchain needs recreating before the next frame.
	stale: bool,
}

impl Gpu {
	pub fn new(window: Arc<Window>, options: &Options) -> Result<Self, String> {
		let display = window
			.display_handle()
			.map_err(|e| format!("display handle: {e}"))?
			.as_raw();
		let handle = window
			.window_handle()
			.map_err(|e| format!("window handle: {e}"))?
			.as_raw();

		// SAFETY: every call gets valid handles created here. If one fails
		// partway, what was made leaks; the caller exits.
		unsafe {
			let entry = load_vulkan()?;
			let extensions = ash_window::enumerate_required_extensions(display)
				.map_err(vk_err("surface extensions"))?
				.to_vec();
			let instance = create_instance(&entry, &extensions)?;
			let surface_fn = khr::surface::Instance::new(&entry, &instance);
			let surface = ash_window::create_surface(&entry, &instance, display, handle, None)
				.map_err(vk_err("create surface"))?;
			let (physical, family, device_name) = pick_device(&instance, |physical, i, family| {
				family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
					&& surface_fn
						.get_physical_device_surface_support(physical, i, surface)
						.unwrap_or(false)
			})?;
			let device = create_device(&instance, physical, family, &[khr::swapchain::NAME.as_ptr()])?;
			let queue = device.get_device_queue(family, 0);
			let swapchain_fn = khr::swapchain::Device::new(&instance, &device);

			let pool = device
				.create_command_pool(
					&vk::CommandPoolCreateInfo::default()
						.queue_family_index(family)
						.flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
					None,
				)
				.map_err(vk_err("command pool"))?;
			let buffers = device
				.allocate_command_buffers(
					&vk::CommandBufferAllocateInfo::default()
						.command_pool(pool)
						.level(vk::CommandBufferLevel::PRIMARY)
						.command_buffer_count(FRAMES_IN_FLIGHT as u32),
				)
				.map_err(vk_err("command buffers"))?;
			let mut frames = Vec::new();
			for command_buffer in buffers {
				frames.push(Frame {
					command_buffer,
					fence: device
						.create_fence(
							&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
							None,
						)
						.map_err(vk_err("fence"))?,
					acquired: device
						.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
						.map_err(vk_err("semaphore"))?,
				});
			}
			let shaders = Shaders::new(&device)?;

			Ok(Self {
				window,
				_entry: entry,
				instance,
				surface_fn,
				surface,
				physical,
				device_name,
				device,
				queue,
				swapchain_fn,
				pool,
				frames,
				frame: 0,
				shaders,
				pipeline: vk::Pipeline::null(),
				pipeline_format: vk::Format::UNDEFINED,
				swapchain: vk::SwapchainKHR::null(),
				images: Vec::new(),
				views: Vec::new(),
				rendered: Vec::new(),
				extent: vk::Extent2D::default(),
				mode: options.mode,
				vsync: options.vsync,
				stale: true,
			})
		}
	}

	pub fn device_name(&self) -> &str {
		&self.device_name
	}

	pub fn window(&self) -> &Window {
		&self.window
	}

	pub fn extent(&self) -> (u32, u32) {
		(self.extent.width, self.extent.height)
	}

	pub fn resized(&mut self) {
		self.stale = true;
	}

	/// An 8-bit UNORM format, so the pattern's bytes reach the screen as they are (no sRGB encoding on the way).
	fn pick_format(&self) -> Result<vk::SurfaceFormatKHR, String> {
		use vk::{ColorSpaceKHR as Cs, Format as F};
		// SAFETY: valid physical device and surface.
		let offered = unsafe {
			self.surface_fn
				.get_physical_device_surface_formats(self.physical, self.surface)
		}
		.map_err(vk_err("surface formats"))?;
		[F::B8G8R8A8_UNORM, F::R8G8B8A8_UNORM]
			.iter()
			.find_map(|&format| {
				offered
					.iter()
					.find(|o| o.format == format && o.color_space == Cs::SRGB_NONLINEAR)
					.copied()
			})
			.ok_or_else(|| "no 8-bit UNORM surface format".to_string())
	}

	fn create_swapchain(&mut self) -> Result<(), String> {
		let format = self.pick_format()?;
		// SAFETY: the device is idle before anything the old swapchain uses is
		// destroyed; all handles are this device's.
		unsafe {
			let caps = self
				.surface_fn
				.get_physical_device_surface_capabilities(self.physical, self.surface)
				.map_err(vk_err("surface capabilities"))?;
			let size = self.window.inner_size();
			let extent = if caps.current_extent.width != u32::MAX {
				caps.current_extent
			} else {
				vk::Extent2D {
					width: size
						.width
						.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
					height: size
						.height
						.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
				}
			};
			if extent.width == 0 || extent.height == 0 {
				// Minimised; try again on the next frame.
				self.stale = true;
				return Ok(());
			}
			let modes = self
				.surface_fn
				.get_physical_device_surface_present_modes(self.physical, self.surface)
				.map_err(vk_err("present modes"))?;
			let present_mode = if self.vsync {
				vk::PresentModeKHR::FIFO
			} else {
				[vk::PresentModeKHR::MAILBOX, vk::PresentModeKHR::IMMEDIATE]
					.into_iter()
					.find(|m| modes.contains(m))
					.unwrap_or(vk::PresentModeKHR::FIFO)
			};
			let mut count = caps.min_image_count + 1;
			if caps.max_image_count > 0 {
				count = count.min(caps.max_image_count);
			}
			let alpha = [
				vk::CompositeAlphaFlagsKHR::OPAQUE,
				vk::CompositeAlphaFlagsKHR::INHERIT,
				vk::CompositeAlphaFlagsKHR::PRE_MULTIPLIED,
				vk::CompositeAlphaFlagsKHR::POST_MULTIPLIED,
			]
			.into_iter()
			.find(|a| caps.supported_composite_alpha.contains(*a))
			.unwrap_or(vk::CompositeAlphaFlagsKHR::OPAQUE);

			self.device.device_wait_idle().map_err(vk_err("wait idle"))?;
			let old = self.swapchain;
			let swapchain = self
				.swapchain_fn
				.create_swapchain(
					&vk::SwapchainCreateInfoKHR::default()
						.surface(self.surface)
						.min_image_count(count)
						.image_format(format.format)
						.image_color_space(format.color_space)
						.image_extent(extent)
						.image_array_layers(1)
						.image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
						.image_sharing_mode(vk::SharingMode::EXCLUSIVE)
						.pre_transform(caps.current_transform)
						.composite_alpha(alpha)
						.present_mode(present_mode)
						.clipped(true)
						.old_swapchain(old),
					None,
				)
				.map_err(vk_err("create swapchain"))?;
			self.destroy_swapchain_resources();
			self.swapchain_fn.destroy_swapchain(old, None);
			self.swapchain = swapchain;
			self.images = self
				.swapchain_fn
				.get_swapchain_images(swapchain)
				.map_err(vk_err("swapchain images"))?;
			for &image in &self.images {
				let view = self
					.device
					.create_image_view(
						&vk::ImageViewCreateInfo::default()
							.image(image)
							.view_type(vk::ImageViewType::TYPE_2D)
							.format(format.format)
							.subresource_range(COLOR),
						None,
					)
					.map_err(vk_err("image view"))?;
				self.views.push(view);
				let semaphore = self
					.device
					.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
					.map_err(vk_err("semaphore"))?;
				self.rendered.push(semaphore);
			}
			if format.format != self.pipeline_format {
				self.device.destroy_pipeline(self.pipeline, None);
				self.pipeline = self.shaders.pipeline(&self.device, format.format)?;
				self.pipeline_format = format.format;
			}
			self.extent = extent;
			self.stale = false;
		}
		Ok(())
	}

	/// # Safety
	///
	/// The device must be idle.
	unsafe fn destroy_swapchain_resources(&mut self) {
		unsafe {
			for view in self.views.drain(..) {
				self.device.destroy_image_view(view, None);
			}
			for semaphore in self.rendered.drain(..) {
				self.device.destroy_semaphore(semaphore, None);
			}
		}
		self.images.clear();
	}

	/// Draw and present `frame`. Returns whether it was presented (false while the swapchain is being remade).
	pub fn draw(&mut self, frame_number: u32) -> Result<bool, String> {
		if self.stale {
			self.create_swapchain()?;
			if self.stale {
				return Ok(false);
			}
		}
		let frame = &self.frames[self.frame];
		// SAFETY: the frame's fence guards its command buffer and semaphore; the
		// image's `rendered` semaphore is only reused after its present.
		unsafe {
			self.device
				.wait_for_fences(&[frame.fence], true, u64::MAX)
				.map_err(vk_err("wait for frame"))?;
			let index =
				match self
					.swapchain_fn
					.acquire_next_image(self.swapchain, u64::MAX, frame.acquired, vk::Fence::null())
				{
					Ok((index, _)) => index,
					Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
						self.stale = true;
						return Ok(false);
					},
					Err(e) => return Err(format!("acquire: {e}")),
				};
			let (image, view, rendered) = (
				self.images[index as usize],
				self.views[index as usize],
				self.rendered[index as usize],
			);
			self.device
				.reset_fences(&[frame.fence])
				.map_err(vk_err("reset fence"))?;

			let cmd = frame.command_buffer;
			self.device
				.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
				.map_err(vk_err("reset command buffer"))?;
			self.device
				.begin_command_buffer(
					cmd,
					&vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
				)
				.map_err(vk_err("begin command buffer"))?;
			let to_attachment = vk::ImageMemoryBarrier2::default()
				.src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
				.dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
				.dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
				.old_layout(vk::ImageLayout::UNDEFINED)
				.new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
				.image(image)
				.subresource_range(COLOR);
			self.device.cmd_pipeline_barrier2(
				cmd,
				&vk::DependencyInfo::default().image_memory_barriers(&[to_attachment]),
			);
			self.shaders.record(
				&self.device,
				cmd,
				self.pipeline,
				(view, self.extent),
				frame_number,
				self.mode,
			);
			let to_present = vk::ImageMemoryBarrier2::default()
				.src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
				.src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
				.dst_stage_mask(vk::PipelineStageFlags2::NONE)
				.old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
				.new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
				.image(image)
				.subresource_range(COLOR);
			self.device
				.cmd_pipeline_barrier2(cmd, &vk::DependencyInfo::default().image_memory_barriers(&[to_present]));
			self.device
				.end_command_buffer(cmd)
				.map_err(vk_err("end command buffer"))?;

			let waits = [vk::SemaphoreSubmitInfo::default()
				.semaphore(frame.acquired)
				.stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
			let signals = [vk::SemaphoreSubmitInfo::default()
				.semaphore(rendered)
				.stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)];
			let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
			self.device
				.queue_submit2(
					self.queue,
					&[vk::SubmitInfo2::default()
						.wait_semaphore_infos(&waits)
						.command_buffer_infos(&buffers)
						.signal_semaphore_infos(&signals)],
					frame.fence,
				)
				.map_err(vk_err("submit"))?;

			let swapchains = [self.swapchain];
			let indices = [index];
			let wait = [rendered];
			match self.swapchain_fn.queue_present(
				self.queue,
				&vk::PresentInfoKHR::default()
					.wait_semaphores(&wait)
					.swapchains(&swapchains)
					.image_indices(&indices),
			) {
				Ok(false) => {},
				Ok(true) | Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.stale = true,
				Err(e) => return Err(format!("present: {e}")),
			}
		}
		self.frame = (self.frame + 1) % FRAMES_IN_FLIGHT;
		Ok(true)
	}
}

impl Drop for Gpu {
	fn drop(&mut self) {
		// SAFETY: nothing is in flight after the wait, and each object is
		// destroyed once, children before their parents.
		unsafe {
			let _ = self.device.device_wait_idle();
			self.destroy_swapchain_resources();
			self.swapchain_fn.destroy_swapchain(self.swapchain, None);
			self.device.destroy_pipeline(self.pipeline, None);
			self.shaders.destroy(&self.device);
			for frame in &self.frames {
				self.device.destroy_fence(frame.fence, None);
				self.device.destroy_semaphore(frame.acquired, None);
			}
			self.device.destroy_command_pool(self.pool, None);
			self.device.destroy_device(None);
			self.surface_fn.destroy_surface(self.surface, None);
			self.instance.destroy_instance(None);
		}
	}
}

/// Frame `frame` in `mode` at `width` × `height`, drawn on the GPU without a window and read back: RGB, 3 bytes a
/// pixel, rows top to bottom.
pub fn dump(width: u32, height: u32, frame: u32, mode: Mode) -> Result<Vec<u8>, String> {
	// SAFETY: every call gets valid handles created here, and the GPU is idle before anything is read or destroyed.
	// On an error what was made leaks; the caller exits.
	unsafe {
		let entry = load_vulkan()?;
		let instance = create_instance(&entry, &[])?;
		let (physical, family, _) = pick_device(&instance, |_, _, family| {
			family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
		})?;
		let device = create_device(&instance, physical, family, &[])?;
		let queue = device.get_device_queue(family, 0);
		let memory_props = instance.get_physical_device_memory_properties(physical);
		let memory_type = |bits: u32, flags: vk::MemoryPropertyFlags| {
			(0..memory_props.memory_type_count)
				.find(|&i| bits & (1 << i) != 0 && memory_props.memory_types[i as usize].property_flags.contains(flags))
				.ok_or_else(|| format!("no {flags:?} memory"))
		};

		let format = vk::Format::R8G8B8A8_UNORM;
		let extent = vk::Extent2D { width, height };
		let image = device
			.create_image(
				&vk::ImageCreateInfo::default()
					.image_type(vk::ImageType::TYPE_2D)
					.format(format)
					.extent(vk::Extent3D {
						width,
						height,
						depth: 1,
					})
					.mip_levels(1)
					.array_layers(1)
					.samples(vk::SampleCountFlags::TYPE_1)
					.tiling(vk::ImageTiling::OPTIMAL)
					.usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC),
				None,
			)
			.map_err(vk_err("create image"))?;
		let req = device.get_image_memory_requirements(image);
		let image_memory = device
			.allocate_memory(
				&vk::MemoryAllocateInfo::default()
					.allocation_size(req.size)
					.memory_type_index(memory_type(
						req.memory_type_bits,
						vk::MemoryPropertyFlags::DEVICE_LOCAL,
					)?),
				None,
			)
			.map_err(vk_err("image memory"))?;
		device
			.bind_image_memory(image, image_memory, 0)
			.map_err(vk_err("bind image memory"))?;
		let view = device
			.create_image_view(
				&vk::ImageViewCreateInfo::default()
					.image(image)
					.view_type(vk::ImageViewType::TYPE_2D)
					.format(format)
					.subresource_range(COLOR),
				None,
			)
			.map_err(vk_err("image view"))?;
		let size = u64::from(width) * u64::from(height) * 4;
		let buffer = device
			.create_buffer(
				&vk::BufferCreateInfo::default()
					.size(size)
					.usage(vk::BufferUsageFlags::TRANSFER_DST),
				None,
			)
			.map_err(vk_err("create buffer"))?;
		let req = device.get_buffer_memory_requirements(buffer);
		let buffer_memory = device
			.allocate_memory(
				&vk::MemoryAllocateInfo::default()
					.allocation_size(req.size)
					.memory_type_index(memory_type(
						req.memory_type_bits,
						vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
					)?),
				None,
			)
			.map_err(vk_err("buffer memory"))?;
		device
			.bind_buffer_memory(buffer, buffer_memory, 0)
			.map_err(vk_err("bind buffer memory"))?;

		let shaders = Shaders::new(&device)?;
		let pipeline = shaders.pipeline(&device, format)?;
		let pool = device
			.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(family), None)
			.map_err(vk_err("command pool"))?;
		let cmd = device
			.allocate_command_buffers(
				&vk::CommandBufferAllocateInfo::default()
					.command_pool(pool)
					.level(vk::CommandBufferLevel::PRIMARY)
					.command_buffer_count(1),
			)
			.map_err(vk_err("command buffer"))?[0];
		device
			.begin_command_buffer(
				cmd,
				&vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
			)
			.map_err(vk_err("begin command buffer"))?;
		let to_attachment = vk::ImageMemoryBarrier2::default()
			.dst_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
			.dst_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
			.old_layout(vk::ImageLayout::UNDEFINED)
			.new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
			.image(image)
			.subresource_range(COLOR);
		device.cmd_pipeline_barrier2(
			cmd,
			&vk::DependencyInfo::default().image_memory_barriers(&[to_attachment]),
		);
		shaders.record(&device, cmd, pipeline, (view, extent), frame, mode);
		let to_transfer = vk::ImageMemoryBarrier2::default()
			.src_stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
			.src_access_mask(vk::AccessFlags2::COLOR_ATTACHMENT_WRITE)
			.dst_stage_mask(vk::PipelineStageFlags2::COPY)
			.dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
			.old_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
			.new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
			.image(image)
			.subresource_range(COLOR);
		device.cmd_pipeline_barrier2(
			cmd,
			&vk::DependencyInfo::default().image_memory_barriers(&[to_transfer]),
		);
		device.cmd_copy_image_to_buffer(
			cmd,
			image,
			vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
			buffer,
			&[vk::BufferImageCopy::default()
				.image_subresource(vk::ImageSubresourceLayers {
					aspect_mask: vk::ImageAspectFlags::COLOR,
					mip_level: 0,
					base_array_layer: 0,
					layer_count: 1,
				})
				.image_extent(vk::Extent3D {
					width,
					height,
					depth: 1,
				})],
		);
		device.end_command_buffer(cmd).map_err(vk_err("end command buffer"))?;
		let buffers = [vk::CommandBufferSubmitInfo::default().command_buffer(cmd)];
		device
			.queue_submit2(
				queue,
				&[vk::SubmitInfo2::default().command_buffer_infos(&buffers)],
				vk::Fence::null(),
			)
			.map_err(vk_err("submit"))?;
		device.queue_wait_idle(queue).map_err(vk_err("wait"))?;

		let mapped = device
			.map_memory(buffer_memory, 0, size, vk::MemoryMapFlags::empty())
			.map_err(vk_err("map"))?;
		let rgba = std::slice::from_raw_parts(mapped.cast::<u8>(), size as usize);
		let rgb = rgba
			.as_chunks::<4>()
			.0
			.iter()
			.flat_map(|p| [p[0], p[1], p[2]])
			.collect();
		device.unmap_memory(buffer_memory);

		device.destroy_command_pool(pool, None);
		device.destroy_pipeline(pipeline, None);
		shaders.destroy(&device);
		device.destroy_buffer(buffer, None);
		device.free_memory(buffer_memory, None);
		device.destroy_image_view(view, None);
		device.destroy_image(image, None);
		device.free_memory(image_memory, None);
		device.destroy_device(None);
		instance.destroy_instance(None);
		Ok(rgb)
	}
}
