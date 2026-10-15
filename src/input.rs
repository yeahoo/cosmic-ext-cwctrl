//! Mouse and keyboard injection through the kernel `uinput` interface.
//!
//! Why uinput: Wayland deliberately offers no pointer-injection protocol, and
//! COSMIC exposes neither `zwlr_virtual_pointer_manager_v1` nor
//! `wp_virtual_pointer_manager_v1` (verified 2026-10). The only reliable channel
//! is the kernel uinput device, which the compositor treats as real hardware.
//! The keyboard could use `zwp_virtual_keyboard_manager_v1`, but that needs an
//! extra vendored XML plus xkb handling; routing both through uinput keeps a
//! single mechanism.
//!
//! Write access to `/dev/uinput` is required (the udev `uaccess` rule usually
//! grants it to the logged-in user, so root is normally not needed).

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------- kernel constants

const IOC_WRITE: u32 = 1;
const UINPUT_IOCTL_BASE: u32 = b'U' as u32;

/// Equivalent to C's `_IOC(dir, type, nr, size)`.
const fn ioc(dir: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (UINPUT_IOCTL_BASE << 8) | nr) as libc::c_ulong
}

const UI_DEV_CREATE: libc::c_ulong = ioc(0, 1, 0);
const UI_DEV_DESTROY: libc::c_ulong = ioc(0, 2, 0);
const UI_DEV_SETUP: libc::c_ulong = ioc(IOC_WRITE, 3, std::mem::size_of::<UinputSetup>() as u32);
const UI_ABS_SETUP: libc::c_ulong = ioc(IOC_WRITE, 4, std::mem::size_of::<UinputAbsSetup>() as u32);
const UI_SET_EVBIT: libc::c_ulong = ioc(IOC_WRITE, 100, 4);
const UI_SET_KEYBIT: libc::c_ulong = ioc(IOC_WRITE, 101, 4);
const UI_SET_RELBIT: libc::c_ulong = ioc(IOC_WRITE, 102, 4);
const UI_SET_ABSBIT: libc::c_ulong = ioc(IOC_WRITE, 103, 4);
const UI_SET_PROPBIT: libc::c_ulong = ioc(IOC_WRITE, 110, 4);

// linux/input-event-codes.h
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const INPUT_PROP_POINTER: u16 = 0;

const BUS_USB: u16 = 0x03;

/// Wait after creating the device so libinput/the compositor picks it up; too short drops events.
const DEVICE_SETTLE: Duration = Duration::from_millis(400);
/// Grace period before destroying the device; removing it too early can drop just-sent events.
const DEVICE_SETTLE_AFTER: Duration = Duration::from_millis(120);

// ---------------------------------------------------------------- C structures

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    pad: u16,
    absinfo: InputAbsInfo,
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

// ---------------------------------------------------------------- device wrapper

struct Uinput {
    /// Holds the File so the fd stays open.
    _file: File,
    fd: RawFd,
}

impl Uinput {
    fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")?;
        let fd = file.as_raw_fd();
        Ok(Self { _file: file, fd })
    }

    /// `UI_SET_*BIT` ioctls take an integer value, not a pointer.
    fn set_bit(&self, request: libc::c_ulong, bit: u16) -> io::Result<()> {
        let r = unsafe { libc::ioctl(self.fd, request, bit as libc::c_ulong) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn set_ptr<T>(&self, request: libc::c_ulong, ptr: &T) -> io::Result<()> {
        let r = unsafe { libc::ioctl(self.fd, request, ptr as *const T) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn emit(&self, type_: u16, code: u16, value: i32) {
        let ev = InputEvent {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };
        unsafe {
            libc::write(
                self.fd,
                &ev as *const InputEvent as *const libc::c_void,
                std::mem::size_of::<InputEvent>(),
            );
        }
    }

    fn syn(&self) {
        self.emit(EV_SYN, SYN_REPORT, 0);
    }

    /// Only performs `UI_DEV_SETUP`; absolute axes must be configured before
    /// `UI_DEV_CREATE`.
    fn dev_setup(&self, name: &str, product: u16) -> io::Result<()> {
        let mut setup = UinputSetup {
            id: InputId {
                bustype: BUS_USB,
                vendor: 0xbeef,
                product,
                version: 1,
            },
            name: [0u8; 80],
            ff_effects_max: 0,
        };
        let bytes = name.as_bytes();
        let n = bytes.len().min(setup.name.len() - 1);
        setup.name[..n].copy_from_slice(&bytes[..n]);
        self.set_ptr(UI_DEV_SETUP, &setup)
    }

    fn dev_create(&self) -> io::Result<()> {
        self.set_ptr_raw(UI_DEV_CREATE)
    }

    fn set_ptr_raw(&self, request: libc::c_ulong) -> io::Result<()> {
        let r = unsafe { libc::ioctl(self.fd, request) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        thread::sleep(DEVICE_SETTLE_AFTER);
        unsafe {
            libc::ioctl(self.fd, UI_DEV_DESTROY);
        }
    }
}

// ---------------------------------------------------------------- mouse

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "left" | "l" | "1" => Some(Self::Left),
            "right" | "r" | "3" => Some(Self::Right),
            "middle" | "m" | "2" => Some(Self::Middle),
            _ => None,
        }
    }

    fn code(self) -> u16 {
        match self {
            Self::Left => BTN_LEFT,
            Self::Right => BTN_RIGHT,
            Self::Middle => BTN_MIDDLE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxis {
    Up,
    Down,
    Left,
    Right,
}

impl ScrollAxis {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "left" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    fn rel(self) -> (u16, i32) {
        match self {
            Self::Up => (REL_WHEEL, 1),
            Self::Down => (REL_WHEEL, -1),
            Self::Left => (REL_HWHEEL, -1),
            Self::Right => (REL_HWHEEL, 1),
        }
    }
}

/// Easing curve for animated pointer movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ease {
    Linear,
    /// Cubic ease-in-out: slow, fast, slow.
    InOut,
}

impl Ease {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "linear" | "lin" => Some(Self::Linear),
            "in-out" | "inout" | "smooth" | "ease" => Some(Self::InOut),
            _ => None,
        }
    }

    fn apply(self, t: f64) -> f64 {
        match self {
            Self::Linear => t,
            Self::InOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
        }
    }
}

pub struct Mouse {
    u: Uinput,
}

impl Mouse {
    /// `screen_w`/`screen_h` are **logical pixel** sizes, used as the absolute axis range.
    pub fn create(screen_w: i32, screen_h: i32) -> io::Result<Self> {
        let u = Uinput::open()?;
        for ev in [EV_SYN, EV_KEY, EV_REL, EV_ABS] {
            u.set_bit(UI_SET_EVBIT, ev)?;
        }
        for k in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
            u.set_bit(UI_SET_KEYBIT, k)?;
        }
        for r in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
            u.set_bit(UI_SET_RELBIT, r)?;
        }
        for a in [ABS_X, ABS_Y] {
            u.set_bit(UI_SET_ABSBIT, a)?;
        }
        u.set_bit(UI_SET_PROPBIT, INPUT_PROP_POINTER)?;
        u.dev_setup("cwctrl-mouse", 0x0001)?;

        for (code, max) in [(ABS_X, screen_w - 1), (ABS_Y, screen_h - 1)] {
            let abs = UinputAbsSetup {
                code,
                pad: 0,
                absinfo: InputAbsInfo {
                    value: 0,
                    minimum: 0,
                    maximum: max.max(1),
                    fuzz: 0,
                    flat: 0,
                    resolution: 1,
                },
            };
            u.set_ptr(UI_ABS_SETUP, &abs)?;
        }
        u.dev_create()?;
        thread::sleep(DEVICE_SETTLE);
        Ok(Self { u })
    }

    /// Absolute positioning (logical pixels, same space as `cwctrl list` geometry).
    pub fn move_abs(&self, x: i32, y: i32, screen_w: i32, screen_h: i32) {
        let cx = x.clamp(0, screen_w - 1);
        let cy = y.clamp(0, screen_h - 1);
        self.u.emit(EV_ABS, ABS_X, cx);
        self.u.emit(EV_ABS, ABS_Y, cy);
        self.u.syn();
    }

    /// Relative motion, sent step by step so the compositor handles each frame.
    pub fn move_rel(&self, dx: i32, dy: i32) {
        for _ in 0..dx.unsigned_abs() {
            self.u.emit(EV_REL, REL_X, if dx > 0 { 1 } else { -1 });
            self.u.syn();
        }
        for _ in 0..dy.unsigned_abs() {
            self.u.emit(EV_REL, REL_Y, if dy > 0 { 1 } else { -1 });
            self.u.syn();
        }
    }

    pub fn button(&self, b: Button, down: bool) {
        self.u.emit(EV_KEY, b.code(), down as i32);
        self.u.syn();
    }

    pub fn click(&self, b: Button, count: u32) {
        for _ in 0..count.max(1) {
            self.button(b, true);
            thread::sleep(Duration::from_millis(15));
            self.button(b, false);
            thread::sleep(Duration::from_millis(15));
        }
    }

    pub fn scroll(&self, axis: ScrollAxis, n: i32) {
        let (code, step) = axis.rel();
        for _ in 0..n.max(1) {
            self.u.emit(EV_REL, code, step);
            self.u.syn();
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Animated absolute move from `from` to `to` over `duration_ms`.
    ///
    /// The start point must be supplied by the caller: Wayland exposes no way to
    /// read the global pointer position, so it cannot be discovered. A wrong
    /// `from` makes the pointer jump on the first frame.
    pub fn move_smooth_abs(
        &self,
        from: (i32, i32),
        to: (i32, i32),
        screen_w: i32,
        screen_h: i32,
        duration_ms: u64,
        ease: Ease,
    ) {
        let (steps, frame) = animation_steps(duration_ms);
        let mut last: Option<(i32, i32)> = None;
        for i in 1..=steps {
            let e = ease.apply(f64::from(i) / f64::from(steps));
            let x = (f64::from(from.0) + f64::from(to.0 - from.0) * e).round() as i32;
            let y = (f64::from(from.1) + f64::from(to.1 - from.1) * e).round() as i32;
            if last != Some((x, y)) {
                self.move_abs(x, y, screen_w, screen_h);
                last = Some((x, y));
            }
            thread::sleep(frame);
        }
    }

    /// Animated relative move. Unlike the absolute variant this needs no start
    /// point, since only the delta is interpolated.
    pub fn move_smooth_rel(&self, dx: i32, dy: i32, duration_ms: u64, ease: Ease) {
        let (steps, frame) = animation_steps(duration_ms);
        let mut sent = (0.0f64, 0.0f64);
        for i in 1..=steps {
            let e = ease.apply(f64::from(i) / f64::from(steps));
            let cur = (f64::from(dx) * e, f64::from(dy) * e);
            let sx = (cur.0 - sent.0).round() as i32;
            let sy = (cur.1 - sent.1).round() as i32;
            sent = cur;
            if sx != 0 || sy != 0 {
                self.move_rel(sx, sy);
            }
            thread::sleep(frame);
        }
    }
}

/// Split a duration into a frame count (about 60 fps) and a per-frame sleep.
fn animation_steps(duration_ms: u64) -> (u32, Duration) {
    let duration_ms = duration_ms.clamp(1, 60_000);
    let steps = (duration_ms / 16).max(1) as u32;
    let frame = Duration::from_millis((duration_ms / u64::from(steps)).max(1));
    (steps, frame)
}

// ---------------------------------------------------------------- keyboard

pub struct Keyboard {
    u: Uinput,
}

impl Keyboard {
    pub fn create() -> io::Result<Self> {
        let u = Uinput::open()?;
        u.set_bit(UI_SET_EVBIT, EV_SYN)?;
        u.set_bit(UI_SET_EVBIT, EV_KEY)?;
        for k in 1..=255u16 {
            u.set_bit(UI_SET_KEYBIT, k)?;
        }
        u.dev_setup("cwctrl-keyboard", 0x0002)?;
        u.dev_create()?;
        thread::sleep(DEVICE_SETTLE);
        Ok(Self { u })
    }

    pub fn key(&self, code: u16, down: bool) {
        self.u.emit(EV_KEY, code, down as i32);
        self.u.syn();
    }

    /// A chord such as `ctrl+shift+t`: press the modifiers, tap the main key,
    /// then release in reverse.
    pub fn chord(&self, mods: &[u16], code: u16) {
        for &m in mods {
            self.key(m, true);
        }
        self.key(code, true);
        thread::sleep(Duration::from_millis(20));
        self.key(code, false);
        for &m in mods.iter().rev() {
            self.key(m, false);
        }
    }

    /// Type text as key presses, assuming a US layout. Returns the first
    /// character that cannot be produced; nothing is typed in that case.
    pub fn type_text(&self, text: &str, delay_ms: u64) -> Result<(), char> {
        for c in text.chars() {
            if char_to_key(c).is_none() {
                return Err(c);
            }
        }
        let delay = Duration::from_millis(delay_ms.min(1000));
        for c in text.chars() {
            let (code, shift) = char_to_key(c).expect("checked above");
            if shift {
                self.key(KEY_LEFTSHIFT, true);
            }
            self.key(code, true);
            thread::sleep(Duration::from_millis(8));
            self.key(code, false);
            if shift {
                self.key(KEY_LEFTSHIFT, false);
            }
            if !delay.is_zero() {
                thread::sleep(delay);
            }
        }
        Ok(())
    }
}

/// Map a printable US-layout character to (evdev keycode, needs Shift).
///
/// Only the US layout is assumed. Non-ASCII characters cannot be produced at
/// the evdev layer at all -- the clipboard path exists for those.
pub fn char_to_key(c: char) -> Option<(u16, bool)> {
    let lower = c.to_ascii_lowercase();
    if lower.is_ascii_lowercase() {
        return Some((keycode(&lower.to_string())?, c.is_ascii_uppercase()));
    }
    if c.is_ascii_digit() {
        return Some((keycode(&c.to_string())?, false));
    }
    let (code, shift) = match c {
        ' ' => (57, false),  // KEY_SPACE
        '\t' => (15, false), // KEY_TAB
        '\n' => (28, false), // KEY_ENTER
        '!' => (2, true),    // Shift+1
        '@' => (3, true),
        '#' => (4, true),
        '$' => (5, true),
        '%' => (6, true),
        '^' => (7, true),
        '&' => (8, true),
        '*' => (9, true),
        '(' => (10, true),
        ')' => (11, true),
        '-' => (12, false), // KEY_MINUS
        '_' => (12, true),
        '=' => (13, false), // KEY_EQUAL
        '+' => (13, true),
        '[' => (26, false), // KEY_LEFTBRACE
        '{' => (26, true),
        ']' => (27, false), // KEY_RIGHTBRACE
        '}' => (27, true),
        '\\' => (43, false), // KEY_BACKSLASH
        '|' => (43, true),
        ';' => (39, false), // KEY_SEMICOLON
        ':' => (39, true),
        '\'' => (40, false), // KEY_APOSTROPHE
        '"' => (40, true),
        '`' => (41, false), // KEY_GRAVE
        '~' => (41, true),
        ',' => (51, false), // KEY_COMMA
        '<' => (51, true),
        '.' => (52, false), // KEY_DOT
        '>' => (52, true),
        '/' => (53, false), // KEY_SLASH
        '?' => (53, true),
        _ => return None,
    };
    Some((code, shift))
}

/// Put `text` on the Wayland clipboard using `wl-copy`.
pub fn clipboard_copy(text: &str) -> io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    if !child.wait()?.success() {
        return Err(io::Error::other("wl-copy exited with an error"));
    }
    Ok(())
}

// ---------------------------------------------------------------- key names

pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_LEFTMETA: u16 = 125;

/// Resolve a key name, or a single `a`-`z` / `0`-`9` character.
pub fn keycode(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    let c = match n.as_str() {
        "esc" | "escape" => 1,
        "1" => 2,
        "2" => 3,
        "3" => 4,
        "4" => 5,
        "5" => 6,
        "6" => 7,
        "7" => 8,
        "8" => 9,
        "9" => 10,
        "0" => 11,
        "minus" | "-" => 12,
        "equal" | "=" => 13,
        "backspace" => 14,
        "tab" => 15,
        "q" => 16,
        "w" => 17,
        "e" => 18,
        "r" => 19,
        "t" => 20,
        "y" => 21,
        "u" => 22,
        "i" => 23,
        "o" => 24,
        "p" => 25,
        "leftbrace" | "[" => 26,
        "rightbrace" | "]" => 27,
        "enter" | "return" => 28,
        "leftctrl" | "ctrl" | "control" => KEY_LEFTCTRL,
        "a" => 30,
        "s" => 31,
        "d" => 32,
        "f" => 33,
        "g" => 34,
        "h" => 35,
        "j" => 36,
        "k" => 37,
        "l" => 38,
        "semicolon" | ";" => 39,
        "apostrophe" | "'" => 40,
        "grave" | "`" => 41,
        "leftshift" | "shift" => KEY_LEFTSHIFT,
        "backslash" | "\\" => 43,
        "z" => 44,
        "x" => 45,
        "c" => 46,
        "v" => 47,
        "b" => 48,
        "n" => 49,
        "m" => 50,
        "comma" | "," => 51,
        "dot" | "period" | "." => 52,
        "slash" | "/" => 53,
        "rightshift" => 54,
        "kpasterisk" => 55,
        "leftalt" | "alt" => KEY_LEFTALT,
        "space" => 57,
        "capslock" => 58,
        "f1" => 59,
        "f2" => 60,
        "f3" => 61,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "numlock" => 69,
        "scrolllock" => 70,
        "f11" => 87,
        "f12" => 88,
        "rightctrl" => 97,
        "rightalt" => 100,
        "home" => 102,
        "up" => 103,
        "pageup" => 104,
        "left" => 105,
        "right" => 106,
        "end" => 107,
        "down" => 108,
        "pagedown" => 109,
        "insert" => 110,
        "delete" | "del" => 111,
        "leftmeta" | "super" | "meta" | "win" => KEY_LEFTMETA,
        "rightmeta" => 126,
        _ => return None,
    };
    Some(c)
}

/// Split `ctrl+shift+t` into (modifiers, main key).
pub fn parse_chord(spec: &str) -> Option<(Vec<u16>, u16)> {
    let mut parts: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let main = parts.pop()?;
    let code = keycode(main)?;
    let mut mods = Vec::new();
    for m in parts {
        let mc = keycode(m)?;
        if !is_modifier(mc) {
            return None;
        }
        mods.push(mc);
    }
    Some((mods, code))
}

fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        KEY_LEFTCTRL | KEY_LEFTSHIFT | KEY_LEFTALT | KEY_LEFTMETA | 97 | 54 | 100 | 126
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing() {
        assert_eq!(Ease::Linear.apply(0.0), 0.0);
        assert_eq!(Ease::Linear.apply(1.0), 1.0);
        assert_eq!(Ease::Linear.apply(0.5), 0.5);
        assert!((Ease::InOut.apply(0.0) - 0.0).abs() < 1e-9);
        assert!((Ease::InOut.apply(1.0) - 1.0).abs() < 1e-9);
        assert!((Ease::InOut.apply(0.5) - 0.5).abs() < 1e-9);
        assert!(Ease::InOut.apply(0.25) < 0.25, "eases in at the start");
        assert!(Ease::InOut.apply(0.75) > 0.75, "eases out at the end");
        assert_eq!(Ease::parse("linear"), Some(Ease::Linear));
        assert_eq!(Ease::parse("in-out"), Some(Ease::InOut));
        assert_eq!(Ease::parse("nope"), None);
    }

    #[test]
    fn animation_frames() {
        let (steps, frame) = animation_steps(400);
        assert_eq!(steps, 25);
        assert_eq!(frame, Duration::from_millis(16));
        assert_eq!(animation_steps(0).0, 1, "at least one frame");
    }

    #[test]
    fn char_mapping() {
        assert_eq!(char_to_key('a'), Some((30, false)));
        assert_eq!(char_to_key('A'), Some((30, true)));
        assert_eq!(char_to_key('1'), Some((2, false)));
        assert_eq!(char_to_key('!'), Some((2, true)));
        assert_eq!(char_to_key(' '), Some((57, false)));
        assert_eq!(char_to_key('?'), Some((53, true)));
        assert_eq!(char_to_key('\n'), Some((28, false)));
        assert_eq!(char_to_key('中'), None, "non-ASCII is refused");
        assert_eq!(char_to_key('€'), None);
    }

    #[test]
    fn keycodes() {
        assert_eq!(keycode("a"), Some(30));
        assert_eq!(keycode("A"), Some(30));
        assert_eq!(keycode("Return"), Some(28));
        assert_eq!(keycode("ctrl"), Some(29));
        assert_eq!(keycode("F12"), Some(88));
        assert_eq!(keycode("bogus"), None);
    }

    #[test]
    fn chords() {
        assert_eq!(parse_chord("t"), Some((vec![], 20)));
        assert_eq!(parse_chord("ctrl+t"), Some((vec![29], 20)));
        assert_eq!(parse_chord("ctrl+shift+t"), Some((vec![29, 42], 20)));
        assert_eq!(
            parse_chord("ctrl+shift"),
            Some((vec![29], 42)),
            "a modifier may also be the main key"
        );
        assert_eq!(parse_chord("ctrl+nope"), None);
    }

    #[test]
    fn button_and_axis_parsing() {
        assert_eq!(Button::parse("left"), Some(Button::Left));
        assert_eq!(Button::parse("2"), Some(Button::Middle));
        assert_eq!(Button::parse("x"), None);
        assert_eq!(ScrollAxis::parse("up"), Some(ScrollAxis::Up));
        assert_eq!(ScrollAxis::parse("nope"), None);
    }

    #[test]
    fn ioctl_encoding_matches_kernel() {
        // Cross-check against C's _IOW('U', 100, int) etc. to catch manual encoding errors.
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_SET_RELBIT, 0x4004_5566);
        assert_eq!(UI_SET_ABSBIT, 0x4004_5567);
        assert_eq!(UI_SET_PROPBIT, 0x4004_556e);
        assert_eq!(UI_DEV_CREATE, 0x0000_5501);
        assert_eq!(UI_DEV_DESTROY, 0x0000_5502);
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<UinputAbsSetup>(), 28);
    }
}
