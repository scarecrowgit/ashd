//! Non-GPUI rendering for surfaces GPUI cannot create: the ext-session-lock surfaces of
//! `ashd-lock` and the greeter. smithay-client-toolkit owns the Wayland objects; this
//! crate turns a `wl_surface` into a wgpu surface and draws a vello scene (text via
//! parley) into it.
//!
//! wgpu here is the **crates.io** wgpu 30 that vello 0.12 needs — not GPUI's git wgpu
//! (see docs/GPUI-NOTES.md "wgpu"); no binary links both.
//!
//! Pieces:
//! - [`Gfx`] — one per process: instance, adapter, device, queue, vello renderer.
//! - [`SurfaceTarget`] — a wgpu surface for one `wl_surface` (+ the vello target texture).
//! - [`Gfx::render`] / [`Gfx::render_offscreen`] — run a [`Draw`] into a surface / an RGBA image.
//! - [`Painter`] — rect / border / line / text / icon helpers in *logical* px with
//!   physical-pixel snapping (crisp 1 px lines at any scale).
//! - [`text`] (parley + fontique system fonts), [`icon`] (24-grid stroke SVG icons),
//!   [`anim`] (tweens / springs from `ashd_tokens::motion`), [`wl`] (fractional-scale +
//!   viewporter helpers for sctk clients).
//!
//! Owner: lock.

use std::{ffi::c_void, ptr::NonNull, time::Instant};

use anyhow::{Context as _, bail};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle,
    WaylandWindowHandle,
};
use vello::{AaConfig, RenderParams, RendererOptions, Scene};

pub mod anim;
pub mod icon;
mod paint;
pub mod text;
pub mod wl;

pub use paint::Painter;
pub use text::{Text, TextLayout, TextStyle};
pub use {parley, vello, wgpu};

/// Format of vello's storage target (vello only writes `Rgba8Unorm`).
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Shared GPU state (one per process).
pub struct Gfx {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: vello::Renderer,
}

/// `wl_display*` as a display handle for the wgpu instance (needed by the GLES backend
/// on Wayland; Vulkan ignores it).
#[derive(Debug)]
struct WaylandDisplay(NonNull<c_void>);

// SAFETY: a `wl_display*` is a plain pointer to a thread-safe libwayland object; wgpu only
// reads it to create EGL displays/surfaces.
unsafe impl Send for WaylandDisplay {}
// SAFETY: see above.
unsafe impl Sync for WaylandDisplay {}

impl HasDisplayHandle for WaylandDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: the caller of `Gfx::with_wayland_display` guarantees the display outlives `Gfx`.
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.0))) })
    }
}

impl Gfx {
    /// Pick an adapter (Vulkan preferred, GL fallback) and create the vello renderer.
    /// Without a display handle: fine for Vulkan surfaces and for offscreen rendering.
    pub fn new() -> anyhow::Result<Self> {
        Self::from_descriptor(wgpu::InstanceDescriptor::new_without_display_handle())
    }

    /// Like [`Gfx::new`], but also lets the GLES backend present on Wayland.
    ///
    /// # Safety
    /// `display` must be a valid `wl_display*` that outlives the returned `Gfx`.
    pub unsafe fn with_wayland_display(display: NonNull<c_void>) -> anyhow::Result<Self> {
        Self::from_descriptor(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(WaylandDisplay(display))))
    }

    fn from_descriptor(desc: wgpu::InstanceDescriptor) -> anyhow::Result<Self> {
        let mut desc = desc;
        desc.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        let desc = desc.with_env();
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context("no wgpu adapter (Vulkan/GL)")?;
        let info = adapter.get_info();
        log::info!("ashd-gfx: adapter {} ({:?})", info.name, info.backend);
        let features = adapter.features() & (wgpu::Features::CLEAR_TEXTURE | wgpu::Features::PIPELINE_CACHE);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("ashd-gfx"),
            required_features: features,
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .context("request wgpu device")?;
        let renderer = vello::Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|e| anyhow::anyhow!("vello renderer: {e}"))?;
        Ok(Self { instance, adapter, device, queue, renderer })
    }

    fn build_scene(text: &mut Text, draw: &mut dyn Draw, size: (f64, f64), scale: f64, now: Instant) -> Scene {
        let mut scene = Scene::new();
        draw.draw(&mut scene, text, size, scale, now);
        scene
    }

    /// Draw `draw` into `target` and present. Returns `Ok(false)` when the frame was
    /// skipped (surface outdated/timeout; the caller simply tries again on the next frame).
    pub fn render(&mut self, target: &mut SurfaceTarget, text: &mut Text, draw: &mut dyn Draw) -> anyhow::Result<bool> {
        let now = Instant::now();
        let scene = Self::build_scene(text, draw, target.logical, target.scale, now);
        let frame = match target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                // Present this one, reconfigure for the next.
                target.needs_configure = true;
                t
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(false),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                target.surface.configure(&self.device, &target.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => bail!("wgpu validation error acquiring the surface texture"),
        };
        let params = RenderParams {
            base_color: vello::peniko::Color::BLACK,
            width: target.config.width,
            height: target.config.height,
            antialiasing_method: AaConfig::Area,
        };
        self.renderer
            .render_to_texture(&self.device, &self.queue, &scene, &target.view, &params)
            .map_err(|e| anyhow::anyhow!("vello render: {e}"))?;
        let surface_view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ashd-gfx blit") });
        target.blitter.copy(&self.device, &mut encoder, &target.view, &surface_view);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        if std::mem::take(&mut target.needs_configure) {
            target.surface.configure(&self.device, &target.config);
        }
        Ok(true)
    }

    /// Render `draw` offscreen at `size` logical px × `scale`; returns tightly packed
    /// RGBA8 (sRGB-encoded, like the screen) of `width × height` physical px.
    pub fn render_offscreen(
        &mut self,
        text: &mut Text,
        draw: &mut dyn Draw,
        size: (f64, f64),
        scale: f64,
        now: Instant,
    ) -> anyhow::Result<Image> {
        let (width, height) = physical_size(size, scale);
        let scene = Self::build_scene(text, draw, size, scale, now);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ashd-gfx offscreen"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let params = RenderParams {
            base_color: vello::peniko::Color::BLACK,
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        self.renderer
            .render_to_texture(&self.device, &self.queue, &scene, &view, &params)
            .map_err(|e| anyhow::anyhow!("vello render: {e}"))?;
        let padded = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ashd-gfx readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ashd-gfx readback") });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| anyhow::anyhow!("wgpu poll: {e}"))?;
        rx.recv().context("readback callback dropped")?.context("map readback buffer")?;
        let mapped = slice.get_mapped_range().map_err(|e| anyhow::anyhow!("map readback: {e:?}"))?;
        let mut data = Vec::with_capacity((width * height * 4) as usize);
        for row in mapped.chunks(padded as usize) {
            data.extend_from_slice(&row[..(width * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        Ok(Image { width, height, data })
    }
}

/// RGBA8 pixels from [`Gfx::render_offscreen`].
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Image {
    /// RGBA of pixel (x, y).
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

/// Physical buffer size for a logical size and scale (never 0).
pub fn physical_size(logical: (f64, f64), scale: f64) -> (u32, u32) {
    let w = (logical.0 * scale).round().max(1.0) as u32;
    let h = (logical.1 * scale).round().max(1.0) as u32;
    (w, h)
}

/// A wgpu surface for one `wl_surface` (one per output while locked), plus vello's
/// intermediate target texture.
pub struct SurfaceTarget {
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    /// Output scale factor (logical → physical).
    pub scale: f64,
    /// Logical size (px).
    pub logical: (f64, f64),
    view: wgpu::TextureView,
    blitter: wgpu::util::TextureBlitter,
    needs_configure: bool,
}

impl SurfaceTarget {
    /// `width × height` are **logical** px; the buffer is `round(size × scale)`. The caller
    /// tells the compositor how to map it (buffer scale for integer scales, a viewport for
    /// fractional ones — see [`wl::SurfaceScale`]).
    ///
    /// # Safety
    /// `display` / `surface` must be valid `wl_display*` / `wl_surface*` pointers that
    /// outlive the returned target.
    pub unsafe fn from_wayland(
        gfx: &Gfx,
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
        width: u32,
        height: u32,
        scale: f64,
    ) -> anyhow::Result<Self> {
        // SAFETY: forwarded to the caller.
        let wgpu_surface = unsafe {
            gfx.instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
            })
        }
        .context("create wgpu surface")?;
        if !gfx.adapter.is_surface_supported(&wgpu_surface) {
            bail!("the wgpu adapter cannot present to this Wayland surface");
        }
        let caps = wgpu_surface.get_capabilities(&gfx.adapter);
        // Non-sRGB format: vello's output is already sRGB-encoded.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .context("no Bgra8Unorm/Rgba8Unorm surface format")?;
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let (pw, ph) = physical_size((f64::from(width), f64::from(height)), scale);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: pw,
            height: ph,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: vec![],
        };
        wgpu_surface.configure(&gfx.device, &config);
        let view = target_view(&gfx.device, pw, ph);
        Ok(Self {
            surface: wgpu_surface,
            config,
            scale,
            logical: (f64::from(width), f64::from(height)),
            view,
            blitter: wgpu::util::TextureBlitter::new(&gfx.device, format),
            needs_configure: false,
        })
    }

    /// Reconfigure after a `configure` with a new logical size or scale.
    pub fn resize(&mut self, gfx: &Gfx, width: u32, height: u32, scale: f64) {
        let logical = (f64::from(width), f64::from(height));
        let (pw, ph) = physical_size(logical, scale);
        self.logical = logical;
        self.scale = scale;
        if (pw, ph) != (self.config.width, self.config.height) {
            self.config.width = pw;
            self.config.height = ph;
            self.surface.configure(&gfx.device, &self.config);
            self.view = target_view(&gfx.device, pw, ph);
        }
    }

    /// Physical buffer size.
    pub fn physical(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }
}

fn target_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("ashd-gfx target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

/// Something that draws a frame. `size` is logical px.
pub trait Draw {
    fn draw(&mut self, scene: &mut vello::Scene, text: &mut Text, size: (f64, f64), scale: f64, now: Instant);
    /// Keep rendering frames (animations running).
    fn animating(&self) -> bool {
        false
    }
}

/// ashd token colour → vello colour.
pub fn color(c: ashd_tokens::Rgba) -> vello::peniko::Color {
    vello::peniko::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_size_rounds_and_never_zero() {
        assert_eq!(physical_size((1920.0, 1080.0), 1.0), (1920, 1080));
        assert_eq!(physical_size((1536.0, 864.0), 1.25), (1920, 1080));
        assert_eq!(physical_size((0.0, 0.0), 2.0), (1, 1));
    }

    #[test]
    fn token_colour_converts() {
        let c = color(ashd_tokens::Rgba { r: 0xE0, g: 0xA4, b: 0x58, a: 0xFF });
        assert_eq!(c.to_rgba8().to_u8_array(), [0xE0, 0xA4, 0x58, 0xFF]);
    }
}
