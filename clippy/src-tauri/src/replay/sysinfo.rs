//! Hardware probe for the resource calculator UI.
//!
//! GPU: D3D11 default adapter description (name + dedicated VRAM).
//! RAM: Win32 `GlobalMemoryStatusEx`.
//! HW encoders: Media Foundation transform enumeration with `MFT_FRIENDLY_NAME`.

#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct SystemInfo {
    pub gpu_name: String,
    pub gpu_vram_mb: u64,
    pub ram_total_mb: u64,
    pub hw_encoders: Vec<String>,
}

#[cfg(windows)]
pub fn collect() -> SystemInfo {
    SystemInfo {
        gpu_name: gpu().0,
        gpu_vram_mb: gpu().1,
        ram_total_mb: ram_total_mb(),
        hw_encoders: hw_encoders(),
    }
}

#[cfg(not(windows))]
pub fn collect() -> SystemInfo {
    SystemInfo::default()
}

#[cfg(windows)]
fn gpu() -> (String, u64) {
    use windows::core::Interface;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice, DXGI_ADAPTER_DESC};

    unsafe {
        let mut device = None;
        if D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .is_err()
        {
            return (String::new(), 0);
        }
        let Some(device) = device else {
            return (String::new(), 0);
        };
        let dxgi: IDXGIDevice = match device.cast() {
            Ok(d) => d,
            Err(_) => return (String::new(), 0),
        };
        let adapter: IDXGIAdapter = match dxgi.GetAdapter() {
            Ok(a) => a,
            Err(_) => return (String::new(), 0),
        };
        let desc: DXGI_ADAPTER_DESC = match adapter.GetDesc() {
            Ok(d) => d,
            Err(_) => return (String::new(), 0),
        };
        // desc.Description: [u16; 128] — null-terminated wide string.
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len]);
        let vram_mb = (desc.DedicatedVideoMemory as u64) / (1024 * 1024);
        (name, vram_mb)
    }
}

#[cfg(windows)]
fn ram_total_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m).is_err() {
            return 0;
        }
        m.ullTotalPhys / (1024 * 1024)
    }
}

#[cfg(windows)]
fn hw_encoders() -> Vec<String> {
    use windows::core::PWSTR;
    use windows::Win32::Media::MediaFoundation::{
        IMFActivate, MFMediaType_Video, MFTEnumEx, MFT_FRIENDLY_NAME_Attribute, MFVideoFormat_H264,
        MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
        MFT_REGISTER_TYPE_INFO,
    };
    use windows::Win32::System::Com::CoTaskMemFree;

    let mut out: Vec<String> = Vec::new();
    let output_info = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_H264,
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count: u32 = 0;

    unsafe {
        if MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            None,
            Some(&output_info),
            &mut activates,
            &mut count,
        )
        .is_err()
            || activates.is_null()
        {
            return out;
        }

        for i in 0..count as isize {
            let entry = &*activates.offset(i);
            if let Some(act) = entry {
                // Read MFT_FRIENDLY_NAME_Attribute (allocated by callee).
                let mut name_ptr = PWSTR::null();
                let mut name_len: u32 = 0;
                if act
                    .GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name_ptr, &mut name_len)
                    .is_ok()
                    && !name_ptr.is_null()
                {
                    let slice = std::slice::from_raw_parts(name_ptr.0, name_len as usize);
                    out.push(String::from_utf16_lossy(slice));
                    CoTaskMemFree(Some(name_ptr.0 as *const _));
                }
            }
        }
        CoTaskMemFree(Some(activates as *const _));
    }
    out
}
