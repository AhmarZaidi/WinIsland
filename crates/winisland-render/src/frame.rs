use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use skia_safe::{Color, ImageInfo, Paint, Rect, Surface, gpu, image_filters};

use std::sync::atomic::{AtomicU64, Ordering};

use winisland_platform::{BackdropCaptureFrame, BackdropShare};

use crate::backend::{D3DDevice, RenderTarget, SharedBackdrop, check_surface_supported};
use crate::error::{RenderError, RenderResult};
use crate::image::Image;
use crate::liquid::Backdrop;
use crate::painter::Painter;
use crate::surface::NativeSurface;
use crate::types::Mipmapped;
use crate::types::{Sampling, TileMode};

const MAIN_TARGET: RendererTargetId = RendererTargetId(0);

static BACKDROP_GENERATION: AtomicU64 = AtomicU64::new(1);

/// One composited main-window frame captured while recording, as opaque RGBA8 pixels.
pub struct RecordedFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub at: Instant,
}

struct Recording {
    sender: Sender<RecordedFrame>,
    interval: Duration,
    next_due: Instant,
    surface: Option<Surface>,
    frame: Option<Surface>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RendererOptions {
    pub width: u32,
    pub height: u32,
}

impl RendererOptions {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

pub struct DrawingContext<'a> {
    direct_context: &'a mut gpu::DirectContext,
    backdrop: Option<&'a mut SharedBackdrop>,
}

impl DrawingContext<'_> {
    /// Returns the screen content captured beneath this frame's window, if any.
    pub fn backdrop(&mut self) -> Option<Backdrop> {
        let backdrop = self.backdrop.as_mut()?;
        let (_, blurred) = backdrop.images(self.direct_context)?;
        Some(Backdrop {
            blurred,
            blur_scale: backdrop.frost_scale(),
        })
    }

    pub fn prepare_image(&mut self, image: Image, mipmapped: Mipmapped) -> Option<Image> {
        let mipmapped = match mipmapped {
            Mipmapped::No => gpu::Mipmapped::No,
            Mipmapped::Yes => gpu::Mipmapped::Yes,
        };
        image
            .as_skia()
            .new_texture_image(self.direct_context, mipmapped)
            .map(Image::from_skia)
    }

    fn render_surface(&mut self, info: &ImageInfo) -> Option<Surface> {
        gpu::surfaces::render_target(
            self.direct_context,
            gpu::Budgeted::Yes,
            info,
            None,
            Some(gpu::SurfaceOrigin::TopLeft),
            None,
            Some(false),
            Some(false),
        )
    }

    fn finish_surface(&mut self, surface: &mut Surface) {
        self.direct_context.flush_surface(surface);
    }

    pub fn scale_image(&mut self, image: &Image, width: i32, height: i32) -> Option<Image> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let info = ImageInfo::new_n32_premul((width, height), None);
        let mut surface = self.render_surface(&info)?;
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        surface.canvas().draw_image_rect_with_sampling_options(
            image.as_skia(),
            None,
            Rect::from_xywh(0.0, 0.0, width as f32, height as f32),
            crate::convert::to_skia_sampling(Sampling::LinearNone),
            &paint,
        );
        self.finish_surface(&mut surface);
        Some(Image::from_skia(surface.image_snapshot()))
    }

    pub fn blur_image(
        &mut self,
        image: &Image,
        width: i32,
        height: i32,
        sigma: (f32, f32),
        tile: Option<TileMode>,
    ) -> Option<Image> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let info = ImageInfo::new_n32_premul((width, height), None);
        let mut surface = self.render_surface(&info)?;
        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        if let Some(filter) = image_filters::blur(
            sigma,
            tile.map(crate::convert::to_skia_tile_mode),
            None,
            None,
        ) {
            paint.set_image_filter(filter);
        }
        surface
            .canvas()
            .draw_image(image.as_skia(), (0, 0), Some(&paint));
        self.finish_surface(&mut surface);
        Some(Image::from_skia(surface.image_snapshot()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RendererTargetId(u64);

pub struct Renderer {
    targets: HashMap<RendererTargetId, RenderTarget>,
    device: D3DDevice,
    next_target_id: u64,
    failure: Option<String>,
    backdrop: Option<SharedBackdrop>,
    recording: Option<Recording>,
}

impl Renderer {
    pub fn new(surface: NativeSurface, options: RendererOptions) -> RenderResult<Self> {
        check_surface_supported(&surface)?;
        let mut device = D3DDevice::new()?;
        let target = device.create_target(surface, options.width, options.height)?;
        Self::prewarm_expansion_effects(&mut device);
        Ok(Self {
            targets: HashMap::from([(MAIN_TARGET, target)]),
            device,
            next_target_id: 1,
            failure: None,
            backdrop: None,
            recording: None,
        })
    }

    pub fn create_target(
        &mut self,
        surface: NativeSurface,
        width: u32,
        height: u32,
    ) -> RenderResult<RendererTargetId> {
        check_surface_supported(&surface)?;
        self.check_ready()?;
        let target = self.device.create_target(surface, width, height);
        let target = self.record_result(target)?;
        let id = RendererTargetId(self.next_target_id);
        self.next_target_id += 1;
        self.targets.insert(id, target);
        Ok(id)
    }

    fn prewarm_expansion_effects(device: &mut D3DDevice) {
        let info = ImageInfo::new_n32_premul((96, 96), None);
        let mut drawing_context = DrawingContext {
            direct_context: &mut device.context,
            backdrop: None,
        };
        let Some(mut surface) = drawing_context.render_surface(&info) else {
            return;
        };
        if let Some(filter) = image_filters::blur((12.0, 10.0), None, None, None) {
            let mut paint = Paint::default();
            paint.set_image_filter(filter);
            let canvas = surface.canvas();
            canvas.save_layer(&skia_safe::canvas::SaveLayerRec::default().paint(&paint));
            canvas.draw_rect(Rect::from_xywh(8.0, 8.0, 80.0, 80.0), &Paint::default());
            canvas.restore();
            drawing_context.finish_surface(&mut surface);
            drop(surface);
            if let Err(error) = device.submit(gpu::SyncCpu::Yes) {
                log::warn!("Expanded effect warm-up failed: {error}");
            }
        }
    }

    pub fn main_target(&self) -> RendererTargetId {
        MAIN_TARGET
    }

    pub fn adapter_luid(&self) -> u64 {
        self.device.adapter_luid()
    }

    /// Returns a shared texture covering at least `width` x `height` pixels for backdrop capture.
    pub fn backdrop_share(&mut self, width: u32, height: u32) -> RenderResult<BackdropShare> {
        self.check_ready()?;
        if let Some(backdrop) = &self.backdrop
            && backdrop.fits(width, height)
        {
            return Ok(backdrop.share());
        }
        self.release_backdrop();
        let generation = BACKDROP_GENERATION.fetch_add(1, Ordering::Relaxed);
        let backdrop = self.device.create_backdrop(width, height, generation)?;
        let share = backdrop.share();
        self.backdrop = Some(backdrop);
        Ok(share)
    }

    pub fn backdrop_read_value(&self) -> u64 {
        self.backdrop.as_ref().map_or(0, SharedBackdrop::read_value)
    }

    /// Makes the next frames sample `frame`; the GPU waits for the capture copy before reading.
    pub fn set_backdrop_frame(
        &mut self,
        frame: Option<BackdropCaptureFrame>,
        frost_sigma: f32,
    ) -> RenderResult<()> {
        let Some(backdrop) = self.backdrop.as_mut() else {
            return Ok(());
        };
        let result = backdrop.set_frame(self.device.queue(), frame, frost_sigma);
        self.record_result(result)
    }

    /// Sends the main window composited over the captured screen behind it to `sender`, at most
    /// `fps` times per second, until stopped or the receiver is dropped.
    pub fn start_recording(&mut self, sender: Sender<RecordedFrame>, fps: u32) {
        self.recording = Some(Recording {
            sender,
            interval: Duration::from_secs_f64(1.0 / f64::from(fps.max(1))),
            next_due: Instant::now(),
            surface: None,
            frame: None,
        });
    }

    pub fn stop_recording(&mut self) {
        self.recording = None;
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    pub fn release_backdrop(&mut self) {
        if self.backdrop.is_none() {
            return;
        }
        if let Err(error) = self.device.synchronize() {
            log::warn!("Backdrop release synchronization failed: {error}");
        }
        self.backdrop = None;
    }

    fn draw<T>(
        &mut self,
        target_id: RendererTargetId,
        draw: impl FnOnce(&mut DrawingContext<'_>, &mut Surface) -> T,
    ) -> RenderResult<T> {
        let result = self.draw_inner(target_id, draw);
        self.record_result(result)
    }

    pub fn frame<T>(
        &mut self,
        target_id: RendererTargetId,
        draw: impl FnOnce(&mut DrawingContext<'_>, Painter<'_>) -> T,
    ) -> RenderResult<T> {
        self.draw(target_id, |drawing_context, surface| {
            draw(
                drawing_context,
                Painter {
                    canvas: surface.canvas(),
                },
            )
        })
    }

    fn draw_inner<T>(
        &mut self,
        target_id: RendererTargetId,
        draw: impl FnOnce(&mut DrawingContext<'_>, &mut Surface) -> T,
    ) -> RenderResult<T> {
        self.check_ready()?;
        let target = self.targets.get_mut(&target_id).ok_or_else(|| {
            RenderError::Backend("D3D12 render target is unavailable".to_string())
        })?;
        let present = target.current_surface()?;
        let mut offscreen = if target_id == MAIN_TARGET
            && let Some(recording) = self.recording.as_mut()
        {
            recording.take_frame(&mut self.device.context, present.width(), present.height())
        } else {
            None
        };
        let surface = match offscreen.as_mut() {
            Some(surface) => surface,
            None => &mut *present,
        };
        let canvas = surface.canvas();
        canvas.restore_to_count(1);
        canvas.reset_matrix();
        canvas.clear(Color::TRANSPARENT);
        canvas.save();
        let output = draw(
            &mut DrawingContext {
                direct_context: &mut self.device.context,
                backdrop: self.backdrop.as_mut(),
            },
            surface,
        );
        surface.canvas().restore_to_count(1);
        if let Some(mut frame) = offscreen {
            let image = frame.image_snapshot();
            let canvas = present.canvas();
            canvas.restore_to_count(1);
            canvas.reset_matrix();
            let mut paint = Paint::default();
            paint.set_blend_mode(skia_safe::BlendMode::Src);
            canvas.draw_image(&image, (0, 0), Some(&paint));
            if let Some(recording) = self.recording.as_mut() {
                let alive = record_frame(
                    recording,
                    &image,
                    &mut self.device.context,
                    self.backdrop.as_mut(),
                );
                recording.frame = Some(frame);
                if !alive {
                    self.recording = None;
                }
            }
        }
        self.device.context.flush_surface_with_access(
            present,
            skia_safe::surfaces::BackendSurfaceAccess::Present,
            &gpu::FlushInfo::default(),
        );
        self.device.submit(gpu::SyncCpu::No)?;
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.finish_frame(self.device.queue())?;
        }
        let present_result = target.present();
        self.device.check_health()?;
        present_result?;
        Ok(output)
    }

    pub fn resize(
        &mut self,
        target_id: RendererTargetId,
        width: u32,
        height: u32,
    ) -> RenderResult<()> {
        let result = self.resize_inner(target_id, width, height);
        self.record_result(result)
    }

    fn resize_inner(
        &mut self,
        target_id: RendererTargetId,
        width: u32,
        height: u32,
    ) -> RenderResult<()> {
        self.check_ready()?;
        let target = self.targets.get_mut(&target_id).ok_or_else(|| {
            RenderError::Backend("D3D12 render target is unavailable".to_string())
        })?;
        self.device.resize_target(target, width, height)
    }

    pub fn take_failure(&mut self) -> Option<String> {
        self.failure.take()
    }

    pub fn remove_target(&mut self, target_id: RendererTargetId) {
        if !self.targets.contains_key(&target_id) {
            return;
        }
        let result = self.device.synchronize();
        if let Err(error) = self.record_result(result) {
            log::warn!("D3D12 target cleanup failed: {error}");
            self.device.context.abandon();
        }
        self.targets.remove(&target_id);
        self.device
            .context
            .purge_unlocked_resources(gpu::PurgeResourceOptions::AllResources);
    }

    fn check_ready(&mut self) -> RenderResult<()> {
        if let Some(error) = &self.failure {
            return Err(RenderError::Backend(error.clone()));
        }
        self.device.check_health()
    }

    fn record_result<T>(&mut self, result: RenderResult<T>) -> RenderResult<T> {
        if let Err(error) = &result {
            let message = error.to_string();
            self.failure.get_or_insert(message);
        }
        result
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if self.device.synchronize().is_err() {
            self.device.context.abandon();
        }
        self.targets.clear();
    }
}

impl Recording {
    fn take_frame(
        &mut self,
        context: &mut gpu::DirectContext,
        width: i32,
        height: i32,
    ) -> Option<Surface> {
        match self.frame.take() {
            Some(surface) if surface.width() == width && surface.height() == height => {
                Some(surface)
            }
            _ => gpu::surfaces::render_target(
                context,
                gpu::Budgeted::Yes,
                &ImageInfo::new_n32_premul((width, height), None),
                None,
                Some(gpu::SurfaceOrigin::TopLeft),
                None,
                Some(false),
                Some(false),
            ),
        }
    }
}

fn record_frame(
    recording: &mut Recording,
    island: &skia_safe::Image,
    context: &mut gpu::DirectContext,
    backdrop: Option<&mut SharedBackdrop>,
) -> bool {
    let now = Instant::now();
    if now < recording.next_due {
        return true;
    }
    recording.next_due = now + recording.interval.mul_f32(0.75);
    let (width, height) = (island.width(), island.height());
    let info = ImageInfo::new(
        (width, height),
        skia_safe::ColorType::RGBA8888,
        skia_safe::AlphaType::Premul,
        None,
    );
    if recording
        .surface
        .as_ref()
        .is_none_or(|export| export.width() != width || export.height() != height)
    {
        recording.surface = gpu::surfaces::render_target(
            context,
            gpu::Budgeted::Yes,
            &info,
            None,
            Some(gpu::SurfaceOrigin::TopLeft),
            None,
            Some(false),
            Some(false),
        );
    }
    let Some(export) = recording.surface.as_mut() else {
        return true;
    };
    let canvas = export.canvas();
    canvas.clear(Color::BLACK);
    if let Some((behind, _)) = backdrop.and_then(|backdrop| backdrop.images(context)) {
        canvas.draw_image(&behind, (0, 0), None);
    }
    canvas.draw_image(island, (0, 0), None);
    let mut pixels = vec![0_u8; info.compute_min_byte_size()];
    if !export.read_pixels(&info, &mut pixels, info.min_row_bytes(), (0, 0)) {
        return true;
    }
    recording
        .sender
        .send(RecordedFrame {
            width: width as u32,
            height: height as u32,
            pixels,
            at: now,
        })
        .is_ok()
}
