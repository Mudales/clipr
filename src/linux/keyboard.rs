//! Typing on Wayland with the real keyboard layout: every character is sent as
//! the key (plus Shift / AltGr) that types it on the current layout, through a
//! virtual keyboard. `wtype` instead makes up a key for each character, which
//! apps that forward raw keys (Remmina and other remote-desktop clients, VMs)
//! turn into gibberish.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::thread::sleep;
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::{wl_registry::WlRegistry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon_dl::{
    XkbCommon, xkb_context_flags, xkb_keymap, xkb_keymap_compile_flags, xkb_keymap_format, xkbcommon_option,
};

/// xkb key codes are evdev codes + 8.
const EVDEV_OFFSET: u32 = 8;
const KEYSYM_SHIFT_L: u32 = 0xffe1;
const KEYSYM_ISO_LEVEL3_SHIFT: u32 = 0xfe03;

/// The keys that type one character.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stroke {
    /// xkb key code.
    key: u32,
    shift: bool,
    altgr: bool,
    /// Layout index (e.g. 1 = the second of "us,il").
    group: u32,
}

/// What a keymap can type.
struct Layout {
    strokes: HashMap<char, Stroke>,
    shift_key: u32,
    altgr_key: Option<u32>,
    shift_mask: u32,
    altgr_mask: u32,
}

impl Layout {
    /// From a keymap in the xkb text format (as the compositor sends it).
    fn parse(text: &str) -> Result<Layout> {
        let xkb = xkbcommon_option().ok_or_else(|| anyhow!("libxkbcommon not found"))?;
        let text = CString::new(text.trim_end_matches('\0'))?;
        unsafe {
            let ctx = (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if ctx.is_null() {
                bail!("cannot create an xkb context");
            }
            let keymap = (xkb.xkb_keymap_new_from_string)(
                ctx,
                text.as_ptr(),
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            let layout = if keymap.is_null() {
                Err(anyhow!("cannot read the keyboard layout"))
            } else {
                let layout = Self::from_keymap(xkb, keymap);
                (xkb.xkb_keymap_unref)(keymap);
                layout
            };
            (xkb.xkb_context_unref)(ctx);
            layout
        }
    }

    unsafe fn from_keymap(xkb: &XkbCommon, keymap: *mut xkb_keymap) -> Result<Layout> {
        unsafe {
            let (min, max) = ((xkb.xkb_keymap_min_keycode)(keymap), (xkb.xkb_keymap_max_keycode)(keymap));
            let mod_mask = |name: &str| {
                let name = CString::new(name).unwrap();
                match (xkb.xkb_keymap_mod_get_index)(keymap, name.as_ptr()) {
                    index if index < 32 => 1 << index,
                    _ => 0, // XKB_MOD_INVALID
                }
            };
            // The key to hold for Shift / AltGr: whichever one types that keysym.
            let key_with = |keysym: u32| {
                (min..=max).find(|&key| {
                    let mut syms = std::ptr::null();
                    let n = (xkb.xkb_keymap_key_get_syms_by_level)(keymap, key, 0, 0, &mut syms);
                    n > 0 && std::slice::from_raw_parts(syms, n as usize).contains(&keysym)
                })
            };
            let shift_key = key_with(KEYSYM_SHIFT_L).ok_or_else(|| anyhow!("the keyboard layout has no Shift key"))?;
            let (shift_mask, altgr_mask) = (mod_mask("Shift"), mod_mask("Mod5"));
            let altgr_key = key_with(KEYSYM_ISO_LEVEL3_SHIFT).filter(|_| altgr_mask != 0);

            let state = (xkb.xkb_state_new)(keymap);
            if state.is_null() {
                bail!("cannot create an xkb state");
            }
            let mut strokes = HashMap::new();
            let mut combos = vec![(false, false), (true, false)];
            if altgr_key.is_some() {
                combos.extend([(false, true), (true, true)]);
            }
            // Fewest modifiers and the first layout win.
            for group in 0..(xkb.xkb_keymap_num_layouts)(keymap) {
                for &(shift, altgr) in &combos {
                    let mods = if shift { shift_mask } else { 0 } | if altgr { altgr_mask } else { 0 };
                    (xkb.xkb_state_update_mask)(state, mods, 0, 0, 0, 0, group);
                    for key in min..=max {
                        let code = (xkb.xkb_state_key_get_utf32)(state, key);
                        if let Some(ch) = char::from_u32(code).filter(|_| code != 0) {
                            strokes.entry(ch).or_insert(Stroke { key, shift, altgr, group });
                        }
                    }
                }
            }
            (xkb.xkb_state_unref)(state);
            Ok(Layout { strokes, shift_key, altgr_key, shift_mask, altgr_mask })
        }
    }

    fn stroke(&self, ch: char) -> Option<Stroke> {
        // Enter is "\r" in a keymap.
        self.strokes.get(&if ch == '\n' { '\r' } else { ch }).copied()
    }
}

/// Types `text` with the real keys of the current layout. Characters the
/// layout can't type, or all of it if there's no virtual keyboard, go to
/// `fallback` (wtype).
pub fn type_text(text: &str, fallback: impl Fn(&str) -> Result<()>) -> Result<()> {
    let mut keyboard = match Keyboard::new() {
        Ok(keyboard) => keyboard,
        Err(e) => {
            eprintln!("clipr: typing with wtype instead ({e:#})");
            return fallback(text);
        }
    };
    let mut rest = String::new();
    for ch in text.chars().filter(|&ch| ch != '\r') {
        match keyboard.layout.stroke(ch) {
            Some(stroke) => {
                if !rest.is_empty() {
                    keyboard.sync()?; // ours first
                    fallback(&std::mem::take(&mut rest))?;
                }
                keyboard.press(stroke)?;
            }
            None => rest.push(ch),
        }
    }
    keyboard.sync()?;
    if !rest.is_empty() {
        fallback(&rest)?;
    }
    Ok(())
}

struct Keyboard {
    conn: Connection,
    queue: wayland_client::EventQueue<State>,
    state: State,
    device: ZwpVirtualKeyboardV1,
    layout: Layout,
    start: Instant,
}

impl Keyboard {
    fn new() -> Result<Keyboard> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).context("no seat")?;
        let manager: ZwpVirtualKeyboardManagerV1 =
            globals.bind(&qh, 1..=1, ()).context("the compositor has no virtual keyboards")?;
        // The compositor sends the current layout to every keyboard client.
        let mut state = State::default();
        let keyboard = seat.get_keyboard(&qh, ());
        queue.roundtrip(&mut state)?;
        if state.keymap.is_none() {
            queue.roundtrip(&mut state)?;
        }
        if keyboard.version() >= 3 {
            keyboard.release();
        }
        let keymap = state.keymap.take().ok_or_else(|| anyhow!("the compositor sent no keyboard layout"))?;
        let layout = Layout::parse(&keymap)?;

        let device = manager.create_virtual_keyboard(&seat, &qh, ());
        let (fd, size) = keymap_fd(&keymap)?;
        device.keymap(KeymapFormat::XkbV1.into(), fd.as_fd(), size);
        queue.roundtrip(&mut state)?;
        Ok(Keyboard { conn, queue, state, device, layout, start: Instant::now() })
    }

    fn key(&self, key: u32, down: bool) {
        let time = self.start.elapsed().as_millis() as u32;
        self.device.key(time, key - EVDEV_OFFSET, down as u32);
    }

    fn press(&mut self, stroke: Stroke) -> Result<()> {
        let layout = &self.layout;
        let altgr_key = layout.altgr_key.filter(|_| stroke.altgr);
        let mut mods = 0;
        if stroke.shift {
            self.key(layout.shift_key, true);
            mods |= layout.shift_mask;
        }
        if let Some(key) = altgr_key {
            self.key(key, true);
            mods |= layout.altgr_mask;
        }
        self.device.modifiers(mods, 0, 0, stroke.group);
        self.key(stroke.key, true);
        self.key(stroke.key, false);
        if let Some(key) = altgr_key {
            self.key(key, false);
        }
        if stroke.shift {
            self.key(layout.shift_key, false);
        }
        self.device.modifiers(0, 0, 0, stroke.group);
        self.conn.flush()?;
        sleep(Duration::from_millis(3));
        Ok(())
    }

    /// Waits until the compositor has handled everything sent so far.
    fn sync(&mut self) -> Result<()> {
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.device.destroy();
        let _ = self.conn.flush();
    }
}

/// The keymap in a memory file, to hand to the compositor.
fn keymap_fd(keymap: &str) -> Result<(OwnedFd, u32)> {
    let fd = unsafe { libc::memfd_create(c"clipr-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let mut bytes = keymap.trim_end_matches('\0').as_bytes().to_vec();
    bytes.push(0);
    file.write_all_at(&bytes, 0)?;
    Ok((file.into(), bytes.len() as u32))
}

#[derive(Default)]
struct State {
    keymap: Option<String>,
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_keyboard::Event::Keymap { format: WEnum::Value(KeymapFormat::XkbV1), fd, size } = event {
            let mut buf = vec![0; size as usize];
            if File::from(fd).read_exact_at(&mut buf, 0).is_ok() {
                state.keymap = Some(String::from_utf8_lossy(&buf).into_owned());
            }
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwpVirtualKeyboardManagerV1, _: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(_: &mut Self, _: &ZwpVirtualKeyboardV1, _: <ZwpVirtualKeyboardV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use xkbcommon_dl::xkb_rule_names;

    /// A keymap for `layouts` (e.g. "us,il"), as the compositor would send it.
    fn keymap_text(layouts: &str) -> String {
        let xkb = xkbcommon_option().expect("libxkbcommon");
        let layout = CString::new(layouts).unwrap();
        let names = xkb_rule_names {
            rules: std::ptr::null(),
            model: std::ptr::null(),
            layout: layout.as_ptr(),
            variant: std::ptr::null(),
            options: std::ptr::null(),
        };
        unsafe {
            let ctx = (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            let keymap = (xkb.xkb_keymap_new_from_names)(ctx, &names, xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS);
            assert!(!keymap.is_null());
            let text = (xkb.xkb_keymap_get_as_string)(keymap, xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1);
            let owned = std::ffi::CStr::from_ptr(text).to_string_lossy().into_owned();
            libc::free(text as *mut _);
            (xkb.xkb_keymap_unref)(keymap);
            (xkb.xkb_context_unref)(ctx);
            owned
        }
    }

    fn stroke(key: u32, shift: bool, altgr: bool, group: u32) -> Option<Stroke> {
        Some(Stroke { key, shift, altgr, group })
    }

    #[test]
    fn us_and_hebrew() {
        let layout = Layout::parse(&keymap_text("us,il")).unwrap();
        assert_eq!(layout.shift_key, 50);
        assert_eq!(layout.stroke('a'), stroke(38, false, false, 0));
        assert_eq!(layout.stroke('A'), stroke(38, true, false, 0));
        assert_eq!(layout.stroke('!'), stroke(10, true, false, 0));
        assert_eq!(layout.stroke(' '), stroke(65, false, false, 0));
        assert_eq!(layout.stroke('\n'), stroke(36, false, false, 0));
        assert_eq!(layout.stroke('\t'), stroke(23, false, false, 0));
        assert_eq!(layout.stroke('ש'), stroke(38, false, false, 1));
        assert_eq!(layout.stroke('😀'), None);
    }

    #[test]
    fn altgr() {
        let layout = Layout::parse(&keymap_text("de")).unwrap();
        assert!(layout.altgr_key.is_some());
        assert_eq!(layout.stroke('z'), stroke(29, false, false, 0));
        assert_eq!(layout.stroke('ü'), stroke(34, false, false, 0));
        assert_eq!(layout.stroke('@'), stroke(24, false, true, 0));
    }
}
