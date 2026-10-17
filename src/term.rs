//! term.rs - terminal brut, sans crate : mode raw, taille, touches, souris.
//!
//! Aucune dependance (ni `libc`, ni `crossterm`) : termios passe par ioctl
//! (TCGETS/TCSETS), la taille par TIOCGWINSZ, la lecture est non bloquante via
//! O_NONBLOCK sur le fd 0. Pas de gestion de SIGWINCH : la taille est relue a
//! chaque frame (un ioctl bon marche), le redimensionnement devient transparent.

use crate::ffi;
use std::io::{self, Write};
use std::os::raw::c_void;

pub const TCGETS: u32 = 0x5401;
pub const TCSETS: u32 = 0x5402;
pub const TIOCGWINSZ: u32 = 0x5413;

const IGNBRK: u32 = 0o0001;
const BRKINT: u32 = 0o0002;
const PARMRK: u32 = 0o0010;
const ISTRIP: u32 = 0o0040;
const INLCR: u32 = 0o0100;
const IGNCR: u32 = 0o0200;
const ICRNL: u32 = 0o0400;
const IXON: u32 = 0o2000;
const OPOST: u32 = 0o000001;
const ECHO: u32 = 0o000010;
const ECHONL: u32 = 0o000100;
const ICANON: u32 = 0o000002;
const ISIG: u32 = 0o000001;
const IEXTEN: u32 = 0o100000;
const CSIZE: u32 = 0o000060;
const PARENB: u32 = 0o000400;
const CS8: u32 = 0o000060;
const VMIN: usize = 6;
const VTIME: usize = 5;

/// struct termios (glibc / x86-64) : 4 tcflag_t, c_line, c_cc[NCCS=32], 2 speed_t
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 32],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

const _: () = assert!(std::mem::size_of::<Termios>() == 60);

/// ioctl « normal » du noyau (TCGETS/TCSETS/TIOCGWINSZ) : ces numeros sont des
/// constantes litterales, il ne faut PAS leur appliquer l'encodage _IOWR 'F'
/// propre aux ioctls NVIDIA de ffi::iow (c'etait le bug qui empechait le mode
/// raw : l'ioctl echouait, read() restait bloquant en mode canonique).
#[inline]
fn io(fd: i32, nr: u32, _size: u32, p: *mut c_void) -> i32 {
    loop {
        let r = unsafe { ffi::ioctl(fd, nr as u64, p) };
        if r >= 0 || ffi::errno() != 4 /* EINTR */ {
            return r;
        }
    }
}

pub fn get_termios(fd: i32) -> Option<Termios> {
    let mut t = Termios::default();
    if io(fd, TCGETS, 60, &mut t as *mut _ as *mut c_void) == 0 {
        Some(t)
    } else {
        None
    }
}

/// Entre en mode raw ; le Drop restaure l'etat sauvegarde.
pub struct RawGuard {
    fd: i32,
    saved: Option<Termios>,
}

impl RawGuard {
    pub fn enter(fd: i32) -> RawGuard {
        let saved = get_termios(fd);
        if let Some(mut t) = saved {
            t.c_iflag &= !(IGNBRK | BRKINT | PARMRK | ISTRIP | INLCR | IGNCR | ICRNL | IXON);
            t.c_oflag &= !OPOST;
            // ISIG coupe : Ctrl-C nous arrive comme octet 0x03, on le traite comme q
            t.c_lflag &= !(ECHO | ECHONL | ICANON | ISIG | IEXTEN);
            t.c_cflag &= !(CSIZE | PARENB);
            t.c_cflag |= CS8;
            t.c_cc[VMIN] = 0;
            t.c_cc[VTIME] = 0;
            let _ = io(fd, TCSETS, 60, &mut t as *mut _ as *mut c_void);
        }
        RawGuard { fd, saved }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if let Some(mut t) = self.saved {
            t.c_lflag |= ECHO | ICANON | ISIG | IEXTEN;
            t.c_iflag |= ICRNL | IXON;
            t.c_oflag |= OPOST;
            let _ = io(self.fd, TCSETS, 60, &mut t as *mut _ as *mut c_void);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    PgUp,
    PgDn,
    Home,
    End,
    Del,
    Enter,
    Esc,
    WheelUp,
    WheelDown,
}

pub struct Term {
    pub color: bool,
    pub mouse: bool,
    pub alt: bool,
    buf: Vec<u8>,
}

/// (rows, cols) en caracteres
pub fn winsize(fd: i32) -> (u16, u16) {
    #[repr(C)]
    #[derive(Default)]
    struct WinSize {
        rows: u16,
        cols: u16,
        x: u16,
        y: u16,
    }
    let mut w = WinSize::default();
    if io(fd, TIOCGWINSZ, 8, &mut w as *mut _ as *mut c_void) == 0 && w.rows > 0 && w.cols > 0 {
        (w.rows, w.cols)
    } else {
        (
            std::env::var("LINES").ok().and_then(|v| v.parse().ok()).unwrap_or(40),
            std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok()).unwrap_or(120),
        )
    }
}

impl Term {
    pub fn new(color: bool) -> Term {
        Term { color, mouse: false, alt: false, buf: Vec::with_capacity(32 * 1024) }
    }

    pub fn enter(&mut self) {
        self.buf.clear();
        self.buf.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l");
        self.alt = true;
    }

    fn write_sync(&mut self, s: &[u8]) {
        if self.color {
            self.buf.extend_from_slice(s);
        }
    }

    /// Une frame = une seule emission, encadree en sortie synchronisee (pas de
    /// dechirure), terminee par la remise a zero des attributs.
    pub fn flush_frame(&mut self) {
        if self.color {
            self.buf.splice(0..0, b"\x1b[?2026h".iter().copied());
            self.buf.extend_from_slice(b"\x1b[?2026l");
        }
        let mut out = io::stdout();
        let _ = out.write_all(&self.buf);
        let _ = out.flush();
        self.buf.clear();
    }

    pub fn leave(&mut self) {
        self.disable_mouse();
        self.write_sync(b"\x1b[?2026l");
        self.buf.extend_from_slice(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let mut out = io::stdout();
        let _ = out.write_all(&self.buf);
        let _ = out.flush();
        self.buf.clear();
        self.alt = false;
    }

    pub fn enable_mouse(&mut self) {
        if !self.mouse {
            self.mouse = true;
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1b[?1000h\x1b[?1006h");
            let _ = out.flush();
        }
    }

    pub fn disable_mouse(&mut self) {
        if self.mouse {
            self.mouse = false;
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1b[?1006l\x1b[?1000l");
            let _ = out.flush();
        }
    }

    #[inline]
    pub fn w(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
    }

    #[inline]
    pub fn wc(&mut self, s: &[u8]) {
        self.buf.extend_from_slice(s);
    }

    /// Clavier + souris deja disponibles (non bloquant).
    pub fn poll_keys(&mut self) -> Vec<Key> {
        extern "C" {
            fn read(fd: i32, buf: *mut c_void, n: usize) -> isize;
        }
        let mut chunk = [0u8; 1024];
        let mut all: Vec<u8> = Vec::new();
        loop {
            let n = unsafe { read(0, chunk.as_mut_ptr() as *mut c_void, chunk.len()) };
            if n <= 0 {
                break;
            }
            all.extend_from_slice(&chunk[..n as usize]);
            if all.len() > 4096 {
                break;
            }
        }
        parse_keys(&all)
    }
}

/// Parse les sequences ANSI/XT courantes (fleches, Home/End, SGR mouse).
pub fn parse_keys(b: &[u8]) -> Vec<Key> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == 0x1b {
            if i + 1 >= b.len() {
                out.push(Key::Esc);
                i += 1;
                continue;
            }
            if b[i + 1] == b'[' {
                let mut j = i + 2;
                let mut raw = String::new();
                while j < b.len() && !((b[j] as char).is_ascii_alphabetic() || b[j] == b'~') {
                    raw.push(b[j] as char);
                    j += 1;
                }
                let last = b.get(j).copied().unwrap_or(b'?');
                let nums: Vec<i32> = raw
                    .trim_start_matches('<')
                    .split(';')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
                match last {
                    b'A' => out.push(Key::Up),
                    b'B' => out.push(Key::Down),
                    b'C' => out.push(Key::Right),
                    b'D' => out.push(Key::Left),
                    b'H' => out.push(Key::Home),
                    b'F' => out.push(Key::End),
                    b'M' if raw.starts_with('<') => {
                        let btn = nums.first().copied().unwrap_or(0);
                        if btn & 64 != 0 {
                            out.push(if btn & 1 == 0 { Key::WheelUp } else { Key::WheelDown });
                        }
                    }
                    b'~' => match nums.first().copied().unwrap_or(0) {
                        1 | 7 => out.push(Key::Home),
                        2 => out.push(Key::Enter),
                        3 => out.push(Key::Del),
                        4 | 8 => out.push(Key::End),
                        5 => out.push(Key::PgUp),
                        6 => out.push(Key::PgDn),
                        _ => {}
                    },
                    _ => {}
                }
                i = j + 1;
                continue;
            }
            if b[i + 1] == b'O' {
                match b.get(i + 2) {
                    Some(b'H') => out.push(Key::Home),
                    Some(b'F') => out.push(Key::End),
                    _ => {}
                }
                i += 3;
                continue;
            }
            out.push(Key::Esc);
            i += 1;
            continue;
        }
        match c {
            b'\r' | b'\n' => out.push(Key::Enter),
            0x7f | 0x08 => out.push(Key::Del),
            b'\t' => out.push(Key::Char('\t')),
            0..=0x1a => out.push(Key::Ctrl((b'a' + c) as char)),
            _ => {
                let len = if c < 0x80 {
                    1
                } else if c >> 5 == 0b110 {
                    2
                } else if c >> 4 == 0b1110 {
                    3
                } else {
                    4
                };
                let s = String::from_utf8_lossy(&b[i..(i + len).min(b.len())]);
                if let Some(ch) = s.chars().next() {
                    out.push(Key::Char(ch));
                }
                i += len;
                continue;
            }
        }
        i += 1; // les cas simples (Entree, Tab, Del, Ctrl-x) avancent d'un octet
    }
    out
}

/// O_NONBLOCK sur stdin : le polling des touches ne bloque jamais le refresh.
pub fn set_nonblocking_stdin() {
    extern "C" {
        fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    }
    const F_GETFL: i32 = 3;
    const F_SETFL: i32 = 2;
    const O_NONBLOCK: i32 = 0o4000;
    unsafe {
        let fl = fcntl(0, F_GETFL);
        if fl >= 0 {
            fcntl(0, F_SETFL, fl | O_NONBLOCK);
        }
    }
}

pub fn is_tty(fd: i32) -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(fd) == 1 }
}
