//! eframe only sets the window icon while its window is the active one, so a
//! window that opens without focus shows a blank icon on the taskbar. Set it
//! directly from the icon embedded in the exe instead.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON,
    SendMessageW, WM_SETICON,
};

/// `set_icon` in build.rs embeds the icon under this resource id.
const ICON_RESOURCE: usize = 1;

pub fn apply(window: &impl HasWindowHandle) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(win) = handle.as_raw() else { return };
    let hwnd = win.hwnd.get() as _;
    for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
        // SAFETY: `hwnd` is this app's live window, the resource id is a valid
        // MAKEINTRESOURCE value, and a shared icon stays valid for the process.
        unsafe {
            let size = GetSystemMetrics(metric);
            let icon = LoadImageW(
                GetModuleHandleW(std::ptr::null()),
                ICON_RESOURCE as *const u16,
                IMAGE_ICON,
                size,
                size,
                LR_DEFAULTCOLOR,
            );
            if !icon.is_null() {
                SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize);
            }
        }
    }
}
