//! ffi.rs - les seuls symboles C dont nous avons besoin.
//!
//! Pas de crate `libc` : `std` lie deja la glibc, il suffit de declarer les
//! prototypes. Ainsi `rustc -O src/main.rs -o nvinfo` suffit a construire
//! l'outil, et `ldd nvinfo` ne montre que libc + libm + libgcc_s.
//!
//! Le projet est une transcription Rust de l'ABI reverse-engineequee dans
//! https://github.com/zagg/nvidia-smi-clone-RE (include/nvrm.h, docs/
//! REVERSE_ENGINEERING.md section 2).

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};

extern "C" {
    /// ioctl est variadique en C ; on respecte l'ABI (AL doit contenir le nombre
    /// de registres vectoriels utilises, le compilateur le remplit pour nous).
    pub fn ioctl(fd: c_int, request: u64, ...) -> c_int;
    pub fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    pub fn munmap(addr: *mut c_void, len: usize) -> c_int;
    fn __errno_location() -> *mut c_int;

    // Utilise uniquement pour recuperer la version du driver CUDA (UMD), le seul
    // champ que le noyau ne connait pas : nvidia-smi fait exactement la meme chose
    // (dlopen("libcuda.so.1")). Desactivable avec --no-cuda.
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> c_int;
    fn dlerror() -> *mut c_char;
}

pub const PROT_READ: i32 = 1;
pub const MAP_SHARED: i32 = 1;
pub const MAP_FAILED: *mut c_void = -1isize as *mut c_void;

// valeurs glibc/ELF : RTLD_LAZY == 1 (mode 0 est refuse : EINVAL)
const RTLD_LAZY: i32 = 1;
const RTLD_NOW: i32 = 2;
const RTLD_LOCAL: i32 = 0;
const _RTLD_GLOBAL: i32 = 0x100;
const _: i32 = RTLD_NOW;

pub fn errno() -> i32 {
    unsafe { *__errno_location() }
}

/// Numero d'ioctl du pilote NVIDIA : direction RW, magic 'F', nr et taille.
/// (verifie octet par octet dans la trace, cf. docs/REVERSE_ENGINEERING.md §2)
pub const fn nv_ioc(nr: u32, size: u32) -> u64 {
    ((3u32 << 30) as u64) | ((size as u64) << 16) | ((b'F' as u64) << 8) | (nr as u64)
}

fn last_err() -> std::io::Error {
    std::io::Error::from_raw_os_error(errno())
}

/// ioctl lecture/ecriture avec reprise sur EINTR.
/// Retourne Err(status RM) quand l'ioctl reussit mais que le pilote a rempli le
/// champ `status` a l'interieur du bloc de parametres : le fermoir appelle
/// `check` apres le coup.
pub fn iow(fd: i32, nr: u32, size: u32, argp: *mut c_void) -> i32 {
    let req = nv_ioc(nr, size);
    loop {
        let r = unsafe { ioctl(fd, req, argp) };
        if r >= 0 || errno() != 4 /* EINTR */ {
            return r;
        }
    }
}

pub fn ioctl_checked(fd: i32, nr: u32, size: u32, argp: *mut c_void) -> Result<(), std::io::Error> {
    if iow(fd, nr, size, argp) < 0 {
        Err(last_err())
    } else {
        Ok(())
    }
}

pub fn mmap_ro(fd: i32, len: usize) -> Result<*mut u8, std::io::Error> {
    let p = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_SHARED, fd, 0) };
    if p == MAP_FAILED {
        Err(last_err())
    } else {
        Ok(p as *mut u8)
    }
}

pub fn unmap(p: *mut c_void, len: usize) {
    unsafe {
        munmap(p, len);
    }
}

/// Charge libcuda.so.1 et appelle cuDriverGetVersion, comme le fait nvidia-smi.
/// Retourne "13.4" etc. Aucune erreur si la bibliotheque est absente.
pub fn cuda_umd_version() -> Option<String> {
    unsafe {
        let name = CString::new("libcuda.so.1").ok()?;
        let h = dlopen(name.as_ptr(), RTLD_LAZY | RTLD_LOCAL);
        if h.is_null() {
            if std::env::var_os("NVINFO_DBG").is_some() {
                let e = dlerror();
                let msg = if e.is_null() { "dlerror: null".to_string() } else { String::from_utf8_lossy(std::ffi::CStr::from_ptr(e).to_bytes()).into_owned() };
                eprintln!("ffi: dlopen(libcuda.so.1) a echoue : {msg}");
            }
            return None;
        }
        let sym = CString::new("cuDriverGetVersion").ok()?;
        let f = dlsym(h, sym.as_ptr());
        let mut out = None;
        if !f.is_null() {
            let f: extern "C" fn(*mut i32) -> i32 = std::mem::transmute(f);
            let mut v: i32 = 0;
            if f(&mut v) == 0 && v > 0 {
                out = Some(format!("{}.{}", v / 1000, (v / 10) % 100));
            } else {
                let init = CString::new("cuInit").ok()?;
                let gi = dlsym(h, init.as_ptr());
                if !gi.is_null() {
                    let g: extern "C" fn(u32) -> i32 = std::mem::transmute(gi);
                    if g(0) == 0 && v == 0 {
                        let mut v2: i32 = 0;
                        if f(&mut v2) == 0 && v2 > 0 {
                            out = Some(format!("{}.{}", v2 / 1000, (v2 / 10) % 100));
                        }
                    }
                }
            }
        }
        dlclose(h);
        out
    }
}
