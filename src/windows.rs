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
    FindWindowW, GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
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

pub fn activate(handle: isize) {
    let hwnd = handle as HWND;
    unsafe {
        if IsWindow(hwnd) != 0 {
            SetForegroundWindow(hwnd);
        }
    }
}

fn picker_window() -> HWND {
    unsafe { FindWindowW(std::ptr::null(), wide("clipr").as_ptr()) }
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

/// Start at login: the per-user Run key (no admin rights needed).
pub mod login {
    use std::process::Command;
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn enabled() -> bool {
        Command::new("reg")
            .args(["query", KEY, "/v", "clipr"])
            .output()
            .is_ok_and(|o| o.status.success())
    }

    pub fn set(on: bool) -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        let status = if on {
            let value = format!("\"{}\"", exe.display());
            Command::new("reg").args(["add", KEY, "/v", "clipr", "/t", "REG_SZ", "/d", &value, "/f"]).status()?
        } else {
            Command::new("reg").args(["delete", KEY, "/v", "clipr", "/f"]).status()?
        };
        if status.success() { Ok(()) } else { Err(std::io::Error::other("reg.exe failed")) }
    }
}
