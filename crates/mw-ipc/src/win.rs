use crate::{Shared, LOCAL_NAME, NAME};
use mw_dsp::Params;
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, HANDLE, HLOCAL, INVALID_HANDLE_VALUE};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS,
    PAGE_READWRITE,
};

/// SYSTEM, LocalService, Administrators: full. Interactive users: read/write.
/// The trailing `S:` part sets a low mandatory label so audiodg's AppContainer-ish
/// token isn't blocked by integrity checks.
const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GRGW;;;IU)S:(ML;;NW;;;LW)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Open only; fail if nobody created it yet.
    OpenExisting,
    /// Open, or create (initialised with the given params) if missing.
    OpenOrCreate,
}

pub struct Mapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    pub created: bool,
    pub name: &'static str,
}

// The view is process-wide memory guarded by seqlocks/atomics.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Tries the `Global\` name first, then the session-local one.
    pub fn open(mode: OpenMode, defaults: &Params) -> windows::core::Result<Self> {
        match Self::open_named(NAME, mode, defaults) {
            Ok(m) => Ok(m),
            Err(e) => Self::open_named(LOCAL_NAME, mode, defaults).map_err(|_| e),
        }
    }

    fn open_named(name: &'static str, mode: OpenMode, defaults: &Params) -> windows::core::Result<Self> {
        let wname = HSTRING::from(name);
        let size = std::mem::size_of::<Shared>() as u32;
        unsafe {
            if let Ok(h) = OpenFileMappingW((FILE_MAP_READ | FILE_MAP_WRITE).0, false, PCWSTR(wname.as_ptr())) {
                return Self::map(h, false, name, defaults);
            }
            if mode == OpenMode::OpenExisting {
                return Err(windows::core::Error::from_thread());
            }

            let mut psd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut psd, None)?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: psd.0,
                bInheritHandle: false.into(),
            };
            let res = CreateFileMappingW(INVALID_HANDLE_VALUE, Some(&sa), PAGE_READWRITE, 0, size, PCWSTR(wname.as_ptr()));
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            let _ = LocalFree(Some(HLOCAL(psd.0)));
            let h = res?;
            Self::map(h, !already, name, defaults)
        }
    }

    unsafe fn map(h: HANDLE, created: bool, name: &'static str, defaults: &Params) -> windows::core::Result<Self> {
        let view = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, std::mem::size_of::<Shared>());
        if view.Value.is_null() {
            let e = windows::core::Error::from_thread();
            let _ = CloseHandle(h);
            return Err(e);
        }
        let m = Self { handle: h, view, created, name };
        let shared = &mut *(view.Value as *mut Shared);
        if created || !shared.is_valid() {
            shared.init(defaults);
        }
        Ok(m)
    }

    pub fn shared(&self) -> &Shared {
        unsafe { &*(self.view.Value as *const Shared) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}
