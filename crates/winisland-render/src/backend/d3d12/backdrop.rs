use std::cell::OnceCell;

use skia_safe::{
    AlphaType, BlendMode, ColorType, Data, FilterMode, ISize, Image, ImageInfo, MipmapMode, Paint,
    Rect, RuntimeEffect, SamplingOptions, Surface, TileMode,
    gpu::{self, DirectContext, SurfaceOrigin, d3d::TextureResourceInfo},
    runtime_effect::ChildPtr,
};
use windows::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Graphics::{
        Direct3D12::{
            D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES,
            D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET, D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN, ID3D12CommandQueue,
            ID3D12Device, ID3D12Fence, ID3D12Resource,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
    },
};
use winisland_platform::{BackdropCaptureFrame, BackdropShare};

use crate::error::RenderResult;

const SIZE_STEP: u32 = 64;
const MIN_FROST_SCALE: f32 = 2.0;
const MAX_FROST_SCALE: f32 = 24.0;
const MAX_WORK_SIGMA: f32 = 4.0;
const MIN_WORK_SIGMA: f32 = 0.3;
const MAX_WORK_SPAN: f32 = 12.0;

thread_local! {
    static FROST: OnceCell<Option<RuntimeEffect>> = const { OnceCell::new() };
}

struct OwnedHandle(HANDLE);

struct Fence {
    fence: ID3D12Fence,
    handle: OwnedHandle,
}

pub(crate) struct SharedBackdrop {
    resource: ID3D12Resource,
    texture_handle: OwnedHandle,
    write: Fence,
    read: Fence,
    width: u32,
    height: u32,
    generation: u64,
    read_value: u64,
    frame: Option<BackdropCaptureFrame>,
    frost_sigma: f32,
    frost_surfaces: [Option<Surface>; 3],
    frost_scale: f32,
    images: Option<(u64, u32, Image, Image)>,
    used: bool,
}

impl SharedBackdrop {
    pub(crate) fn new(
        device: &ID3D12Device,
        width: u32,
        height: u32,
        generation: u64,
    ) -> RenderResult<Self> {
        let width = width.max(1).div_ceil(SIZE_STEP) * SIZE_STEP;
        let height = height.max(1).div_ceil(SIZE_STEP) * SIZE_STEP;
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
            ..Default::default()
        };
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: Both descriptors are initialized and the output Option is owned by this call.
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
        }
        .map_err(|error| format!("Shared backdrop texture creation failed: {error}"))?;
        let resource = resource.ok_or_else(|| "Shared backdrop texture is missing".to_string())?;
        // SAFETY: The resource was created on a shared heap; the returned NT handle is owned here.
        let texture_handle = OwnedHandle(
            unsafe { device.CreateSharedHandle(&resource, None, GENERIC_ALL.0, None) }
                .map_err(|error| format!("Shared backdrop handle failed: {error}"))?,
        );
        let write = Fence::new(device)?;
        let read = Fence::new(device)?;
        Ok(Self {
            resource,
            texture_handle,
            write,
            read,
            width,
            height,
            generation,
            read_value: 0,
            frame: None,
            frost_sigma: 0.0,
            frost_surfaces: Default::default(),
            frost_scale: MIN_FROST_SCALE,
            images: None,
            used: false,
        })
    }

    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        width <= self.width
            && height <= self.height
            && self.width - width < SIZE_STEP * 4
            && self.height - height < SIZE_STEP * 4
    }

    pub(crate) fn share(&self) -> BackdropShare {
        BackdropShare {
            texture: self.texture_handle.0.0 as usize,
            write_fence: self.write.handle.0.0 as usize,
            read_fence: self.read.handle.0.0 as usize,
            width: self.width,
            height: self.height,
            generation: self.generation,
        }
    }

    pub(crate) fn read_value(&self) -> u64 {
        self.read_value
    }

    pub(crate) fn set_frame(
        &mut self,
        queue: &ID3D12CommandQueue,
        frame: Option<BackdropCaptureFrame>,
        frost_sigma: f32,
    ) -> RenderResult<()> {
        if let Some(frame) = frame
            && self
                .frame
                .is_none_or(|previous| previous.write_value != frame.write_value)
        {
            // SAFETY: The fence is live; the queue waits on the GPU for the capture copy.
            unsafe { queue.Wait(&self.write.fence, frame.write_value) }
                .map_err(|error| format!("Backdrop fence wait failed: {error}"))?;
        }
        self.frame = frame;
        self.frost_sigma = frost_sigma;
        Ok(())
    }

    pub(crate) fn finish_frame(&mut self, queue: &ID3D12CommandQueue) -> RenderResult<()> {
        if !std::mem::take(&mut self.used) {
            return Ok(());
        }
        let value = self.read_value + 1;
        // SAFETY: The fence is live; the signal follows all submitted reads of the texture.
        unsafe { queue.Signal(&self.read.fence, value) }
            .map_err(|error| format!("Backdrop fence signal failed: {error}"))?;
        self.read_value = value;
        Ok(())
    }

    /// Returns the captured screen and its blurred copy for the current frame.
    pub(crate) fn images(&mut self, context: &mut DirectContext) -> Option<(Image, Image)> {
        let serial = self.frame?.serial;
        let sigma_key = (self.frost_sigma * 16.0).round() as u32;
        if let Some((cached, key, sharp, blurred)) = &self.images
            && *cached == serial
            && *key == sigma_key
        {
            self.used = true;
            return Some((sharp.clone(), blurred.clone()));
        }
        let info = TextureResourceInfo {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            level_count: 1,
            ..TextureResourceInfo::from_resource(self.resource.clone())
                .with_state(D3D12_RESOURCE_STATE_COMMON)
        };
        let backend = gpu::backend_textures::make_d3d(
            (self.width as i32, self.height as i32),
            &info,
            "WinIsland backdrop",
        );
        let sharp = gpu::images::borrow_texture_from(
            context,
            &backend,
            SurfaceOrigin::TopLeft,
            ColorType::BGRA8888,
            AlphaType::Opaque,
            None,
        )?;
        self.used = true;
        self.images = None;
        let blurred = self.render_frost(context, &sharp)?;
        self.images = Some((serial, sigma_key, sharp.clone(), blurred.clone()));
        Some((sharp, blurred))
    }

    pub(crate) fn frost_scale(&self) -> f32 {
        self.frost_scale
    }

    fn render_frost(&mut self, context: &mut DirectContext, sharp: &Image) -> Option<Image> {
        if self.frost_sigma < MIN_WORK_SIGMA {
            self.frost_surfaces = Default::default();
            self.frost_scale = 1.0;
            return Some(sharp.clone());
        }
        let effect = FROST.with(|cell| {
            cell.get_or_init(|| {
                RuntimeEffect::make_for_shader(include_str!("../../frost.sksl"), None)
                    .map_err(|error| log::error!("Backdrop blur shader failed: {error}"))
                    .ok()
            })
            .clone()
        })?;
        let scale = (self.frost_sigma / MAX_WORK_SIGMA)
            .ceil()
            .clamp(MIN_FROST_SCALE, MAX_FROST_SCALE);
        let size = ISize::new(
            (sharp.width() as f32 / scale).ceil() as i32,
            (sharp.height() as f32 / scale).ceil() as i32,
        );
        for surface in &mut self.frost_surfaces {
            if surface.as_ref().is_none_or(|surface| {
                surface.width() != size.width || surface.height() != size.height
            }) {
                let info = ImageInfo::new(size, ColorType::RGBA8888, AlphaType::Premul, None);
                *surface = gpu::surfaces::render_target(
                    context,
                    gpu::Budgeted::Yes,
                    &info,
                    None,
                    Some(SurfaceOrigin::TopLeft),
                    None,
                    Some(false),
                    Some(false),
                );
            }
        }
        self.frost_scale = scale;
        let sigma = self.frost_sigma / scale;
        let reduced = frost_pass(
            context,
            &effect,
            sharp,
            self.frost_surfaces[0].as_mut()?,
            FrostPass::Reduce(scale),
        )?;
        if sigma < MIN_WORK_SIGMA {
            return Some(reduced);
        }
        let horizontal = frost_pass(
            context,
            &effect,
            &reduced,
            self.frost_surfaces[1].as_mut()?,
            FrostPass::Blur([1.0, 0.0], sigma),
        )?;
        drop(reduced);
        frost_pass(
            context,
            &effect,
            &horizontal,
            self.frost_surfaces[2].as_mut()?,
            FrostPass::Blur([0.0, 1.0], sigma),
        )
    }
}

enum FrostPass {
    Reduce(f32),
    Blur([f32; 2], f32),
}

fn frost_pass(
    context: &mut DirectContext,
    effect: &RuntimeEffect,
    input: &Image,
    target: &mut Surface,
    pass: FrostPass,
) -> Option<Image> {
    let source = input.to_shader(
        (TileMode::Clamp, TileMode::Clamp),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        None,
    )?;
    let mut uniforms = vec![0_u8; effect.uniform_size()];
    let mut set = |name: &str, values: &[f32]| {
        if let Some(uniform) = effect.find_uniform(name) {
            for (index, value) in values.iter().enumerate() {
                let offset = uniform.offset() + index * 4;
                uniforms[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
    };
    match pass {
        FrostPass::Reduce(scale) => set("boxSize", &[scale]),
        FrostPass::Blur(direction, sigma) => {
            set("direction", &direction);
            set("sigma", &[sigma]);
            set("span", &[(sigma * 3.0).ceil().min(MAX_WORK_SPAN)]);
        }
    }
    let shader = effect.make_shader(Data::new_copy(&uniforms), &[ChildPtr::from(source)], None)?;
    let mut paint = Paint::default();
    paint.set_shader(shader);
    paint.set_blend_mode(BlendMode::Src);
    let bounds = Rect::from_wh(target.width() as f32, target.height() as f32);
    target.canvas().draw_rect(bounds, &paint);
    context.flush_surface(target);
    Some(target.image_snapshot())
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: The NT handle was created for this owner and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

impl Fence {
    fn new(device: &ID3D12Device) -> Result<Self, String> {
        // SAFETY: The device is live and returns an owned fence created on a shared heap.
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }
            .map_err(|error| format!("Shared backdrop fence failed: {error}"))?;
        // SAFETY: The fence is shareable; the returned NT handle is owned by this value.
        let handle = OwnedHandle(
            unsafe { device.CreateSharedHandle(&fence, None, GENERIC_ALL.0, None) }
                .map_err(|error| format!("Shared backdrop fence handle failed: {error}"))?,
        );
        Ok(Self { fence, handle })
    }
}
