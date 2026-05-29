//! WGC screen capture session backed by a D3D11 device.
//!
//! Supports two capture targets via the same session machinery:
//!   - Window (`capture_item_for_hwnd`) — used for per-window mode
//!   - Monitor (`capture_item_for_monitor`) — used for full-screen mode

#[derive(Debug, Clone, serde::Serialize)]
pub struct MonitorInfo {
    /// HMONITOR as a stringified isize so it survives the JS bigint boundary.
    pub hmonitor: String,
    /// Display index (1-based) — what users see in Windows display settings.
    pub index: u32,
    /// Friendly label including primary marker, e.g. "Display 1 (Primary)".
    pub label: String,
    /// Device path like `\\.\DISPLAY1`.
    pub device: String,
    pub primary: bool,
    pub width: u32,
    pub height: u32,
}

#[cfg(windows)]
pub fn list_monitors() -> Vec<MonitorInfo> {
    windows_impl::list_monitors()
}

#[cfg(not(windows))]
pub fn list_monitors() -> Vec<MonitorInfo> {
    Vec::new()
}

#[cfg(windows)]
pub mod windows_impl {
    use super::MonitorInfo;
    use windows::{
        core::{Interface, Result},
        Graphics::{
            Capture::{
                Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
                GraphicsCaptureSession,
            },
            DirectX::DirectXPixelFormat,
        },
        Win32::{
            Foundation::{BOOL, HWND, LPARAM, RECT},
            Graphics::{
                Direct3D::D3D_DRIVER_TYPE_HARDWARE,
                Direct3D11::{
                    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
                },
                Dxgi::IDXGIDevice,
                Gdi::{
                    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
                    MONITORINFOEXW,
                },
            },
            System::WinRT::{
                Direct3D11::CreateDirect3D11DeviceFromDXGIDevice,
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
    };

    pub struct D3D11Bundle {
        pub device: ID3D11Device,
        pub context: ID3D11DeviceContext,
    }

    pub fn create_d3d11_device() -> Result<D3D11Bundle> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        Ok(D3D11Bundle {
            device: device.unwrap(),
            context: context.unwrap(),
        })
    }

    fn d3d11_to_winrt_device(
        device: &ID3D11Device,
    ) -> Result<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice> {
        let dxgi: IDXGIDevice = device.cast()?;
        let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
        inspectable.cast()
    }

    pub fn capture_item_for_hwnd(hwnd: HWND) -> Result<GraphicsCaptureItem> {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        unsafe { interop.CreateForWindow(hwnd) }
    }

    pub fn capture_item_for_monitor(hmon: HMONITOR) -> Result<GraphicsCaptureItem> {
        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        unsafe { interop.CreateForMonitor(hmon) }
    }

    pub fn extract_texture_from_frame(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D> {
        use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        unsafe { access.GetInterface() }
    }

    pub struct CaptureSession {
        pub frame_pool: Direct3D11CaptureFramePool,
        pub session: GraphicsCaptureSession,
        pub item_size: windows::Graphics::SizeInt32,
    }

    /// Open a WGC capture session on the given item. Used by both window and
    /// monitor capture paths.
    pub fn open_capture_session_for(
        item: &GraphicsCaptureItem,
        d3d_device: &ID3D11Device,
    ) -> Result<CaptureSession> {
        let size = item.Size()?;
        let winrt_device = d3d11_to_winrt_device(d3d_device)?;

        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;

        let session = frame_pool.CreateCaptureSession(item)?;

        // Suppress the yellow Windows Graphics Capture indicator border.
        // Win11 22H2+: SetIsBorderRequired(false) silently succeeds.
        // Older Win10/11: the method either doesn't exist or errors; we
        // ignore the result so capture still works without the toggle.
        let _ = session.SetIsBorderRequired(false);

        session.StartCapture()?;

        Ok(CaptureSession {
            frame_pool,
            session,
            item_size: size,
        })
    }

    /// Convenience wrapper for window capture (per-window mode).
    pub fn open_capture_session(hwnd: HWND, d3d_device: &ID3D11Device) -> Result<CaptureSession> {
        let item = capture_item_for_hwnd(hwnd)?;
        open_capture_session_for(&item, d3d_device)
    }

    // ---------- monitor enumeration ----------

    pub fn list_monitors() -> Vec<MonitorInfo> {
        let mut out: Vec<MonitorInfo> = Vec::new();
        unsafe {
            let _ = EnumDisplayMonitors(
                HDC::default(),
                None,
                Some(enum_proc),
                LPARAM(&mut out as *mut _ as isize),
            );
        }
        out
    }

    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        let out = lparam.0 as *mut Vec<MonitorInfo>;
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info.monitorInfo as *mut MONITORINFO).as_bool() {
            let device_chars: Vec<u16> = info
                .szDevice
                .iter()
                .copied()
                .take_while(|c| *c != 0)
                .collect();
            let device = String::from_utf16_lossy(&device_chars);
            // MONITORINFOF_PRIMARY = 1 in Windows headers; not re-exported by name
            // in this windows-rs version, so use the literal.
            let primary = (info.monitorInfo.dwFlags & 1) != 0;
            let r = info.monitorInfo.rcMonitor;
            let index = ((*out).len() as u32) + 1;
            let label = if primary {
                format!("Display {index} (Primary)")
            } else {
                format!("Display {index}")
            };
            (*out).push(MonitorInfo {
                hmonitor: format!("{}", hmon.0 as isize),
                index,
                label,
                device,
                primary,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
            });
        }
        BOOL(1) // continue enumeration
    }
}
