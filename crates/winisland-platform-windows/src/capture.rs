use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use parking_lot::Mutex;
use windows::{
    Foundation::Metadata::ApiInformation,
    Foundation::TimeSpan,
    Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
        GraphicsCaptureAccessKind, GraphicsCaptureDirtyRegionMode, GraphicsCaptureItem,
        GraphicsCaptureSession,
    },
    Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    Graphics::SizeInt32,
    Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus,
    Win32::{
        Foundation::{HANDLE, HMODULE, HWND, LUID},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::{
                D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext,
                ID3D11DeviceContext4, ID3D11Fence, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ERROR_WAS_STILL_DRAWING, IDXGIAdapter, IDXGIDevice,
                IDXGIFactory4,
            },
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
            },
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
        UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_NONE},
    },
    core::{HSTRING, Interface},
};
use winisland_platform::{BackdropCaptureFrame, BackdropCaptureRequest, BackdropShare, Rect};

const FRAME_BUFFERS: i32 = 2;
const PROBE_STEP: usize = 2;
const PROBE_ALIGN: u32 = 64;
const BORDER_UNKNOWN: u8 = 0;
const BORDER_PENDING: u8 = 1;
const BORDER_OPTIONAL: u8 = 2;
const BORDER_REQUIRED: u8 = 3;

static BORDER_ACCESS: AtomicU8 = AtomicU8::new(BORDER_UNKNOWN);

#[derive(Default)]
struct Latest {
    frame: Option<Direct3D11CaptureFrame>,
    sequence: u64,
}

struct Shared {
    latest: Mutex<Latest>,
    interest: Mutex<Rect>,
    pending: AtomicBool,
    content: Mutex<SizeInt32>,
    device: AgileDevice,
    d3d: AgileContext,
    probe: Mutex<Probe>,
}

#[derive(Default)]
struct Probe {
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rect: Rect,
    samples: Vec<u32>,
    scratch: Vec<u32>,
}

struct Session {
    monitor: HMONITOR,
    origin: (i32, i32),
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    rate_hz: Cell<u32>,
    borderless: Cell<bool>,
}

struct Import {
    generation: u64,
    texture: ID3D11Texture2D,
    write_fence: ID3D11Fence,
    read_fence: ID3D11Fence,
    size: (u32, u32),
}

pub(crate) struct BackdropCapture {
    hwnd: HWND,
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    shared: Arc<Shared>,
    session: RefCell<Option<Session>>,
    import: RefCell<Option<Import>>,
    copied: Cell<Option<(u64, u64, i32, i32)>>,
    verified: Cell<Rect>,
    write_value: AtomicU64,
    serial: Cell<u64>,
}

impl BackdropCapture {
    pub(crate) fn new(hwnd: HWND, adapter_luid: u64) -> Result<Self, String> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err("Windows.Graphics.Capture is unsupported".to_string());
        }
        let (device, context) = create_device(adapter_luid)?;
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|error| format!("IDXGIDevice is unavailable: {error}"))?;
        // SAFETY: The DXGI device is live; WinRT returns an owned inspectable wrapper.
        let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|device| device.cast())
            .map_err(|error| format!("WinRT Direct3D device creation failed: {error}"))?;
        // SAFETY: hwnd is the caller's live overlay window on this thread.
        unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
            .map_err(|error| format!("Capture exclusion failed: {error}"))?;
        let d3d = AgileContext(device.clone().into(), context.clone().into());
        let capture = Self {
            hwnd,
            device,
            context,
            shared: Arc::new(Shared {
                latest: Mutex::new(Latest::default()),
                interest: Mutex::new(Rect::default()),
                pending: AtomicBool::new(false),
                content: Mutex::new(SizeInt32::default()),
                device: AgileDevice(winrt_device),
                d3d,
                probe: Mutex::new(Probe::default()),
            }),
            session: RefCell::new(None),
            import: RefCell::new(None),
            copied: Cell::new(None),
            verified: Cell::new(Rect::default()),
            write_value: AtomicU64::new(0),
            serial: Cell::new(0),
        };
        capture.ensure_session(60)?;
        Ok(capture)
    }

    pub(crate) fn pending(&self) -> bool {
        self.shared.pending.load(Ordering::Acquire)
    }

    pub(crate) fn update(&self, request: BackdropCaptureRequest) -> Option<BackdropCaptureFrame> {
        if let Err(error) = self.ensure_session(request.max_rate_hz) {
            log::warn!("Backdrop capture restart failed: {error}");
            return None;
        }
        let origin = self.session.borrow().as_ref()?.origin;
        let interest = Rect {
            left: request.interest.left - origin.0,
            top: request.interest.top - origin.1,
            right: request.interest.right - origin.0,
            bottom: request.interest.bottom - origin.1,
        };
        *self.shared.interest.lock() = interest;
        self.shared.pending.store(false, Ordering::Release);
        self.ensure_import(request.share)?;
        let (frame, sequence) = {
            let latest = self.shared.latest.lock();
            (latest.frame.clone()?, latest.sequence)
        };
        let key = (
            request.share.generation,
            sequence,
            request.origin.x - origin.0,
            request.origin.y - origin.1,
        );
        let verified = intersection(self.verified.get(), interest);
        if self.copied.get() != Some(key) || verified != interest {
            self.copy(&frame, request, key)?;
            self.verified.set(interest);
        } else {
            self.verified.set(verified);
        }
        Some(BackdropCaptureFrame {
            write_value: self.write_value.load(Ordering::Acquire),
            serial: self.serial.get(),
        })
    }

    fn copy(
        &self,
        frame: &Direct3D11CaptureFrame,
        request: BackdropCaptureRequest,
        key: (u64, u64, i32, i32),
    ) -> Option<()> {
        let import = self.import.borrow();
        let import = import.as_ref()?;
        let source = frame_texture(frame).ok()?;
        let content = frame.ContentSize().ok()?;
        let (local_x, local_y) = (key.2, key.3);
        let left = local_x.max(0);
        let top = local_y.max(0);
        let right = (local_x + import.size.0 as i32).min(content.Width);
        let bottom = (local_y + import.size.1 as i32).min(content.Height);
        if right <= left || bottom <= top {
            return None;
        }
        let source_box = D3D11_BOX {
            left: left as u32,
            top: top as u32,
            front: 0,
            right: right as u32,
            bottom: bottom as u32,
            back: 1,
        };
        let value = self.write_value.load(Ordering::Acquire) + 1;
        // SAFETY: Every interface is live on this device; the box lies inside both textures, and the
        // GPU waits until the renderer finished reading before overwriting the shared texture.
        let result = unsafe {
            self.context
                .Wait(&import.read_fence, request.read_value)
                .map(|()| {
                    self.context.CopySubresourceRegion(
                        &import.texture,
                        0,
                        (left - local_x) as u32,
                        (top - local_y) as u32,
                        0,
                        &source,
                        0,
                        Some(&source_box),
                    );
                })
                .and_then(|()| self.context.Signal(&import.write_fence, value))
                .map(|()| self.context.Flush())
        };
        if let Err(error) = result {
            log::warn!("Backdrop capture copy failed: {error}");
            return None;
        }
        // SAFETY: This only queries the retained device's removal status.
        if let Err(error) = unsafe { self.device.GetDeviceRemovedReason() } {
            log::warn!("Backdrop capture device removed: {error}");
            return None;
        }
        self.write_value.store(value, Ordering::Release);
        self.serial.set(self.serial.get() + 1);
        self.copied.set(Some(key));
        Some(())
    }

    fn ensure_import(&self, share: BackdropShare) -> Option<()> {
        if self
            .import
            .borrow()
            .as_ref()
            .is_some_and(|import| import.generation == share.generation)
        {
            return Some(());
        }
        self.import.borrow_mut().take();
        let device1: ID3D11Device1 = self.device.cast().ok()?;
        // SAFETY: The renderer keeps every NT handle open while this generation is current;
        // opening duplicates the references, so this import outlives later handle closure.
        let import = (|| unsafe {
            let texture: ID3D11Texture2D =
                device1.OpenSharedResource1(HANDLE(share.texture as *mut _))?;
            let mut write_fence: Option<ID3D11Fence> = None;
            self.device
                .OpenSharedFence(HANDLE(share.write_fence as *mut _), &mut write_fence)?;
            let mut read_fence: Option<ID3D11Fence> = None;
            self.device
                .OpenSharedFence(HANDLE(share.read_fence as *mut _), &mut read_fence)?;
            windows::core::Result::Ok(Import {
                generation: share.generation,
                texture,
                write_fence: write_fence.ok_or_else(windows::core::Error::empty)?,
                read_fence: read_fence.ok_or_else(windows::core::Error::empty)?,
                size: (share.width, share.height),
            })
        })();
        match import {
            Ok(import) => {
                *self.import.borrow_mut() = Some(import);
                self.copied.set(None);
                Some(())
            }
            Err(error) => {
                log::warn!("Backdrop share import failed: {error}");
                None
            }
        }
    }

    fn ensure_session(&self, rate_hz: u32) -> Result<(), String> {
        // SAFETY: hwnd is the live overlay window; the call only reads its placement.
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        if let Some(session) = self.session.borrow().as_ref()
            && session.monitor == monitor
        {
            if session.rate_hz.get() != rate_hz {
                session.rate_hz.set(rate_hz);
                set_rate(&session.session, rate_hz);
            }
            if !session.borderless.get() && borderless_allowed() {
                session.borderless.set(true);
                hide_border(&session.session);
            }
            if border_access_required() {
                self.session.borrow_mut().take();
                return Err("Borderless screen capture access is unavailable".to_string());
            }
            return Ok(());
        }
        self.session.borrow_mut().take();
        *self.shared.latest.lock() = Latest::default();
        self.copied.set(None);
        let session = Session::start(monitor, &self.shared, rate_hz)?;
        *self.session.borrow_mut() = Some(session);
        Ok(())
    }
}

impl Session {
    fn start(monitor: HMONITOR, shared: &Arc<Shared>, rate_hz: u32) -> Result<Self, String> {
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: monitor came from MonitorFromWindow and info is a correctly sized output.
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err("Monitor information is unavailable".to_string());
        }
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|error| format!("Capture interop is unavailable: {error}"))?;
        // SAFETY: monitor is a live display handle; WinRT returns an owned capture item.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }
            .map_err(|error| format!("Monitor capture item failed: {error}"))?;
        let size = item
            .Size()
            .map_err(|error| format!("Capture size is unavailable: {error}"))?;
        *shared.content.lock() = size;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &shared.device.0,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            FRAME_BUFFERS,
            size,
        )
        .map_err(|error| format!("Capture frame pool failed: {error}"))?;
        let weak = Arc::downgrade(shared);
        let token = pool
            .FrameArrived(&windows::Foundation::TypedEventHandler::new(
                move |pool: windows::core::Ref<'_, Direct3D11CaptureFramePool>, _| {
                    if let (Some(pool), Some(shared)) = (pool.as_ref(), weak.upgrade()) {
                        shared.receive(pool);
                    }
                    Ok(())
                },
            ))
            .map_err(|error| format!("Capture frame handler failed: {error}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|error| format!("Capture session failed: {error}"))?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let borderless = borderless_allowed();
        if border_access_required() {
            return Err("Borderless screen capture access is unavailable".to_string());
        }
        if borderless {
            hide_border(&session);
        }
        let _ = session.SetDirtyRegionMode(GraphicsCaptureDirtyRegionMode::ReportOnly);
        set_rate(&session, rate_hz);
        session
            .StartCapture()
            .map_err(|error| format!("Capture start failed: {error}"))?;
        Ok(Self {
            monitor,
            origin: (info.rcMonitor.left, info.rcMonitor.top),
            pool,
            session,
            token,
            rate_hz: Cell::new(rate_hz),
            borderless: Cell::new(borderless),
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl Shared {
    fn probe_changed(&self, frame: &Direct3D11CaptureFrame, interest: Rect) -> bool {
        let content = *self.content.lock();
        let rect = Rect {
            left: interest.left.max(0),
            top: interest.top.max(0),
            right: interest.right.min(content.Width),
            bottom: interest.bottom.min(content.Height),
        };
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return false;
        }
        let mut probe = self.probe.lock();
        if let Err(error) = self.sample(&mut probe, frame, rect) {
            log::debug!("Backdrop change probe failed: {error}");
            return true;
        }
        let changed = probe.rect != rect || probe.samples != probe.scratch;
        let probe = &mut *probe;
        std::mem::swap(&mut probe.samples, &mut probe.scratch);
        probe.rect = rect;
        changed
    }

    fn sample(
        &self,
        probe: &mut Probe,
        frame: &Direct3D11CaptureFrame,
        rect: Rect,
    ) -> windows::core::Result<()> {
        let AgileContext(device, context) = &self.d3d;
        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;
        if probe
            .staging
            .as_ref()
            .is_none_or(|(_, w, h)| *w < width || *h < height)
        {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width.div_ceil(PROBE_ALIGN) * PROBE_ALIGN,
                Height: height.div_ceil(PROBE_ALIGN) * PROBE_ALIGN,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            // SAFETY: The descriptor is initialized and the output Option is owned by this call.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
            probe.staging = Some((
                texture.ok_or_else(windows::core::Error::empty)?,
                desc.Width,
                desc.Height,
            ));
        }
        let Some((staging, _, _)) = probe.staging.clone() else {
            return Err(windows::core::Error::empty());
        };
        let source = frame_texture(frame)?;
        let source_box = D3D11_BOX {
            left: rect.left as u32,
            top: rect.top as u32,
            front: 0,
            right: rect.right as u32,
            bottom: rect.bottom as u32,
            back: 1,
        };
        probe.scratch.clear();
        // SAFETY: The box lies inside the frame, the staging texture is at least as large, and the
        // mapped rows are read only within the copied width and height before unmapping.
        unsafe {
            context.CopySubresourceRegion(&staging, 0, 0, 0, 0, &source, 0, Some(&source_box));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let mut attempts = 0;
            loop {
                match context.Map(
                    &staging,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                ) {
                    Ok(()) => break,
                    Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING && attempts < 50 => {
                        attempts += 1;
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => return Err(error),
                }
            }
            let base = mapped.pData as *const u8;
            for y in (0..height as usize).step_by(PROBE_STEP) {
                let row = base.add(y * mapped.RowPitch as usize).cast::<u32>();
                for x in (0..width as usize).step_by(PROBE_STEP) {
                    probe.scratch.push(row.add(x).read_unaligned());
                }
            }
            context.Unmap(&staging, 0);
        }
        Ok(())
    }

    fn receive(&self, pool: &Direct3D11CaptureFramePool) {
        let mut newest = None;
        let mut dirty: Option<Rect> = None;
        let mut whole = false;
        let mut unknown = false;
        while let Ok(frame) = pool.TryGetNextFrame() {
            match frame.DirtyRegions().and_then(|regions| {
                let mut rects = Vec::new();
                for index in 0..regions.Size()? {
                    rects.push(regions.GetAt(index)?);
                }
                Ok(rects)
            }) {
                Ok(rects) => {
                    for rect in rects {
                        let rect = Rect {
                            left: rect.X,
                            top: rect.Y,
                            right: rect.X + rect.Width,
                            bottom: rect.Y + rect.Height,
                        };
                        dirty = Some(dirty.map_or(rect, |acc| union(acc, rect)));
                    }
                }
                Err(_) => unknown = true,
            }
            newest = Some(frame);
        }
        let Some(frame) = newest else {
            return;
        };
        if let Ok(size) = frame.ContentSize() {
            let mut content = self.content.lock();
            if size != *content {
                *content = size;
                let _ = pool.Recreate(
                    &self.device.0,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    FRAME_BUFFERS,
                    size,
                );
                whole = true;
            }
        }
        let interest = *self.interest.lock();
        let relevant = whole
            || dirty.is_some_and(|dirty| {
                intersects(dirty, interest) && self.probe_changed(&frame, interest)
            })
            || (unknown && self.probe_changed(&frame, interest));
        {
            let mut latest = self.latest.lock();
            latest.frame = Some(frame);
            if relevant {
                latest.sequence += 1;
            }
        }
        if relevant && !self.pending.swap(true, Ordering::AcqRel) {
            crate::window::wake();
        }
    }
}

impl Drop for BackdropCapture {
    fn drop(&mut self) {
        self.session.borrow_mut().take();
        *self.shared.latest.lock() = Latest::default();
        self.import.borrow_mut().take();
        // SAFETY: hwnd may already be destroyed; the call fails harmlessly in that case.
        let _ = unsafe { SetWindowDisplayAffinity(self.hwnd, WDA_NONE) };
    }
}

fn set_rate(session: &GraphicsCaptureSession, rate_hz: u32) {
    let interval = 10_000_000 / i64::from(rate_hz.clamp(1, 1000));
    let _ = session.SetMinUpdateInterval(TimeSpan { Duration: interval });
}

fn frame_texture(frame: &Direct3D11CaptureFrame) -> windows::core::Result<ID3D11Texture2D> {
    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
    // SAFETY: The surface wraps a live D3D11 texture owned by the frame.
    unsafe { access.GetInterface() }
}

fn create_device(adapter_luid: u64) -> Result<(ID3D11Device5, ID3D11DeviceContext4), String> {
    // SAFETY: DXGI returns an owned factory and adapter without retaining caller pointers.
    let adapter: IDXGIAdapter = unsafe {
        CreateDXGIFactory1::<IDXGIFactory4>().and_then(|factory| {
            factory.EnumAdapterByLuid(LUID {
                LowPart: adapter_luid as u32,
                HighPart: (adapter_luid >> 32) as i32,
            })
        })
    }
    .map_err(|error| format!("Capture adapter lookup failed: {error}"))?;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: The adapter is live and both outputs are initialized Options owned by this call.
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|error| format!("Capture D3D11 device failed: {error}"))?;
    let device = device.ok_or("Capture D3D11 device is missing")?;
    let context = context.ok_or("Capture D3D11 context is missing")?;
    if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
        // SAFETY: Enabling protection only serializes this device's immediate context.
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
    }
    Ok((
        device
            .cast()
            .map_err(|error| format!("ID3D11Device5 is unavailable: {error}"))?,
        context
            .cast()
            .map_err(|error| format!("ID3D11DeviceContext4 is unavailable: {error}"))?,
    ))
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn intersection(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right).max(a.left.max(b.left)),
        bottom: a.bottom.min(b.bottom).max(a.top.max(b.top)),
    }
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

struct AgileDevice(IDirect3DDevice);

struct AgileContext(ID3D11Device, ID3D11DeviceContext);

// SAFETY: WinRT Direct3D devices are agile, and the wrapped D3D11 device has multithread
// protection enabled before it is shared with the capture callback thread.
unsafe impl Send for AgileDevice {}
// SAFETY: See the Send implementation; the device is only used through thread-safe calls.
unsafe impl Sync for AgileDevice {}

// SAFETY: The device has multithread protection enabled, which serializes the immediate context
// between the render thread and the capture callback thread.
unsafe impl Send for AgileContext {}
// SAFETY: See the Send implementation.
unsafe impl Sync for AgileContext {}

fn borderless_allowed() -> bool {
    if !borderless_api_supported() {
        BORDER_ACCESS.store(BORDER_REQUIRED, Ordering::Release);
        return false;
    }
    if BORDER_ACCESS
        .compare_exchange(
            BORDER_UNKNOWN,
            BORDER_PENDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        let requested =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
                .and_then(|operation| {
                    operation.when(|status| {
                        let allowed = status
                            .as_ref()
                            .is_ok_and(|status| *status == AppCapabilityAccessStatus::Allowed);
                        if !allowed {
                            log::info!("Borderless screen capture is not allowed: {status:?}");
                        }
                        BORDER_ACCESS.store(
                            if allowed {
                                BORDER_OPTIONAL
                            } else {
                                BORDER_REQUIRED
                            },
                            Ordering::Release,
                        );
                        crate::window::wake();
                    })
                });
        if let Err(error) = requested {
            log::info!("Borderless screen capture is unavailable: {error}");
            BORDER_ACCESS.store(BORDER_REQUIRED, Ordering::Release);
        }
    }
    BORDER_ACCESS.load(Ordering::Acquire) == BORDER_OPTIONAL
}

fn border_access_required() -> bool {
    BORDER_ACCESS.load(Ordering::Acquire) == BORDER_REQUIRED
}

fn borderless_api_supported() -> bool {
    ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    )
    .unwrap_or(false)
}

fn hide_border(session: &GraphicsCaptureSession) {
    if let Err(error) = session.SetIsBorderRequired(false) {
        log::warn!("Screen capture border could not be hidden: {error}");
    }
}
