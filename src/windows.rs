//! Windows glue: clipboard change detection, focus handling and simulated
//! keystrokes (SendInput). No permission prompt is needed, but Windows won't
//! deliver our keystrokes to apps running as administrator unless clipr is too.

use anyhow::{Result, bail};
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, HWND};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::System::DataExchange::{
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RETURN, VK_RWIN, VK_SHIFT,
    VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

const VK_V: VIRTUAL_KEY = 0x56;

/// Clipboard formats that password managers set to mark secrets.
const SKIP_FORMATS: &[&str] = &["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Changes on every copy; reading it doesn't open the clipboard.
pub fn change_count() -> isize {
    unsafe { GetClipboardSequenceNumber() as isize }
}

pub fn should_skip_current() -> bool {
    SKIP_FORMATS.iter().any(|name| unsafe {
        let format = RegisterClipboardFormatW(wide(name).as_ptr());
        format != 0 && IsClipboardFormatAvailable(format) != 0
    })
}

fn window_exe(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return None;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// "1Password.exe" and "1Password" for the app in front (for Settings → Ignore apps).
pub fn frontmost_app_names() -> Vec<String> {
    let Some(path) = window_exe(unsafe { GetForegroundWindow() }) else { return Vec::new() };
    let file = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned();
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(&file).to_owned();
    vec![file, stem]
}

/// The focused window, to paste back into after the picker closes.
pub fn frontmost_app() -> Option<isize> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.is_null()).then_some(hwnd as isize)
}

/// Whether `handle` (from `frontmost_app`) is one of our own windows.
pub fn is_own(handle: isize) -> bool {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(handle as HWND, &mut pid) };
    pid == std::process::id()
}

/// Whether `handle` is the taskbar or its tray overflow (what's focused
/// right after clicking the tray icon).
pub fn is_shell(handle: isize) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(handle as HWND, buf.as_mut_ptr(), buf.len() as i32) };
    let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
    matches!(
        class.as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "NotifyIconOverflowWindow" | "TopLevelWindowForOverflowXamlIsland"
    )
}

pub fn activate(handle: isize) {
    let hwnd = handle as HWND;
    unsafe {
        if IsWindow(hwnd) != 0 {
            SetForegroundWindow(hwnd);
        }
    }
}

/// Our window titled "clipr" (not, say, an Explorer window on a folder of
/// that name).
fn picker_window() -> HWND {
    use windows_sys::Win32::Foundation::{LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> i32 {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == std::process::id() {
            let mut buf = [0u16; 16];
            let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
            if String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "clipr" {
                unsafe { *(found as *mut HWND) = hwnd };
                return 0; // stop
            }
        }
        TRUE
    }
    let mut found: HWND = std::ptr::null_mut();
    unsafe { EnumWindows(Some(each), &mut found as *mut HWND as LPARAM) };
    found
}

/// The window procedure we replaced in `hide_on_close`.
static WINDOW_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The title bar's ✕ hides the picker (clipr keeps running). Done here,
/// ahead of winit: the close request otherwise gets lost and the window is
/// destroyed under the app. Safe to call repeatedly.
pub fn hide_on_close() {
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallWindowProcW, GWLP_WNDPROC, SetWindowLongPtrW, WM_CLOSE, WNDPROC};

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_CLOSE {
            crate::ui::request_hide();
            return 0;
        }
        let prev: WNDPROC = unsafe { std::mem::transmute(WINDOW_PROC.load(Ordering::SeqCst)) };
        unsafe { CallWindowProcW(prev, hwnd, msg, wparam, lparam) }
    }

    if WINDOW_PROC.load(Ordering::SeqCst) != 0 {
        return;
    }
    let hwnd = picker_window();
    if hwnd.is_null() {
        return;
    }
    let prev = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, proc as *const () as isize) };
    WINDOW_PROC.store(prev, Ordering::SeqCst);
}

/// Brings the picker to the front. Windows only lets the app that received
/// the last input do that; if it refuses, tapping Alt is the standard way to
/// be allowed (it doesn't reach any app).
pub fn activate_self() {
    let hwnd = picker_window();
    if hwnd.is_null() {
        return;
    }
    unsafe {
        if SetForegroundWindow(hwnd) == 0 || GetForegroundWindow() != hwnd {
            let _ = send(&[key(VK_MENU, false), key(VK_MENU, true)]);
            SetForegroundWindow(hwnd);
        }
    }
}

/// Windows 11: rounded corners for our undecorated window.
pub fn round_corners() {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: u32 = 2;
    let hwnd = picker_window();
    if !hwnd.is_null() {
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&DWMWCP_ROUND as *const u32).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

fn input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    input(vk, 0, if up { KEYEVENTF_KEYUP } else { 0 })
}

fn send(inputs: &[INPUT]) -> Result<()> {
    let sent = unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("Windows blocked the keystrokes (is the target app running as administrator?)");
    }
    Ok(())
}

/// Waits (up to 2s) until the user lets go of Ctrl/Shift/Alt/Win, otherwise
/// our keystrokes would combine with the keys still held from the shortcut.
fn wait_for_modifiers_released() {
    let deadline = Instant::now() + Duration::from_secs(2);
    let held = || {
        [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0)
    };
    while held() && Instant::now() < deadline {
        sleep(Duration::from_millis(10));
    }
}

pub fn send_paste() -> Result<()> {
    wait_for_modifiers_released();
    send(&[key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)])
}

/// Types the text one character at a time (as Unicode, so any layout works).
pub fn type_text(text: &str) -> Result<()> {
    wait_for_modifiers_released();
    let mut buf = [0u16; 2];
    for ch in text.chars() {
        let inputs: Vec<INPUT> = match ch {
            '\r' => continue, // \r\n → one Enter
            '\n' => vec![key(VK_RETURN, false), key(VK_RETURN, true)],
            '\t' => vec![key(VK_TAB, false), key(VK_TAB, true)],
            _ => ch
                .encode_utf16(&mut buf)
                .iter()
                .flat_map(|&unit| {
                    [input(0, unit, KEYEVENTF_UNICODE), input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)]
                })
                .collect(),
        };
        send(&inputs)?;
        sleep(Duration::from_millis(3));
    }
    Ok(())
}

/// Start at login: the per-user Run key (no admin rights needed). Uses the
/// registry API directly: starting reg.exe is slow on machines with endpoint
/// security software, and this runs on the UI thread.
pub mod login {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn enabled() -> bool {
        let (key, name) = (wide(KEY), wide("clipr"));
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        status == ERROR_SUCCESS
    }

    pub fn set(on: bool) -> std::io::Result<()> {
        let (key, name) = (wide(KEY), wide("clipr"));
        let status = if on {
            let value = wide(&format!("\"{}\"", std::env::current_exe()?.display()));
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    value.as_ptr().cast(),
                    (value.len() * 2) as u32,
                )
            }
        } else {
            match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
                2 => ERROR_SUCCESS, // ERROR_FILE_NOT_FOUND: already off
                other => other,
            }
        };
        if status == ERROR_SUCCESS { Ok(()) } else { Err(std::io::Error::from_raw_os_error(status as i32)) }
    }
}
