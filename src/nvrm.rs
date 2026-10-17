//! nvrm.rs - bootstrap du Resource Manager + page partagee RUSD, en Rust pur.
//!
//! Transcription de `src/nvrm.c` du depot
//! https://github.com/zagg/nvidia-smi-clone-RE (RUSD / NVML par retro-
//! ingenierie, aucun lien avec libnvidia-ml). Les quatre pieges qui bloquent
//! l'initialisation sont commentes in situ, cf. docs/REVERSE_ENGINEERING.md §3.
//!
//! Sequences :
//!   1. open /dev/nvidiactl
//!   2. 0xc8 CARD_INFO              (2304 o = 32 x 72)
//!   3. 0xd2 CHECK_VERSION_STR      QUERY ('2') puis STRICT (0)      <- piege 1
//!   4. 0x2b RM_ALLOC classe 0x0041 NV01_ROOT_CLIENT                 -> hClient
//!   5. 0x2a 0x214 GPU_GET_PROBED_IDS  (384 o = 3 x u32[32])
//!   6. 0x2a 0x215 GPU_ATTACH_IDS      (132 o, finit par 0xffffffff) <- piege 3
//!   7. open /dev/nvidiaN + 0xc9 REGISTER_FD(ctl_fd)                 <- piege 2
//!   8. 0x2b classes 0x0080 NV01_DEVICE_0 puis 0x2080 NV20_SUBDEVICE_0
//!   9. 0x2b classe 0x00de avec {u64 polledDataMask}                  <- piege 4
//!  10. 0x4e RM_MAP_MEMORY (fd = fd de CONTROLE, mmap offset = 0) + 0xde0001

use crate::ffi;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::raw::c_void;
use std::os::unix::io::AsRawFd;

pub const NV_OK: u32 = 0;

// --- numeros d'echappement (table complete dans le depot) ---
pub const ESC_CARD_INFO: u32 = 0xc8;
pub const ESC_REGISTER_FD: u32 = 0xc9;
pub const ESC_CHECK_VERSION: u32 = 0xd2;
pub const ESC_RM_CONTROL: u32 = 0x2a;
pub const ESC_RM_ALLOC: u32 = 0x2b;
pub const ESC_RM_MAP_MEMORY: u32 = 0x4e;

// --- classes d'objet ---
pub const CL_ROOT_CLIENT: u32 = 0x0041;
pub const CL_DEVICE: u32 = 0x0080;
pub const CL_SUBDEVICE: u32 = 0x2080;
pub const CL_RUSD: u32 = 0x00de;
pub const CL_NVKMS: u32 = 0x0073;

// --- commandes RM (NV0000 / NV0080 / NV00DE) ---
pub const C_SYS_BUILD_VERSION: u32 = 0x0000_0101;
pub const C_GPU_GET_ATTACHED_IDS: u32 = 0x0000_0201;
pub const C_GPU_GET_ID_INFO: u32 = 0x0000_0202;
pub const C_GPU_GET_PROBED_IDS: u32 = 0x0000_0214;
pub const C_GPU_ATTACH_IDS: u32 = 0x0000_0215;
pub const C_GPU_GET_ID_INFO_V2: u32 = 0x0000_0205;
pub const C_DEVICE_QUERY_PERSISTENCE: u32 = 0x0080_0288;
pub const C_DEVICE_GET_VIRT_MODE: u32 = 0x0080_0289;
pub const C_RUSD_REQUEST_DATA_POLL: u32 = 0x00de_0001;
pub const C_KMS_NUM_HEADS: u32 = 0x0073_0102;
pub const C_KMS_ACTIVE_HEAD: u32 = 0x0073_010c;

/// 3184 = taille exacte de NV00DE_SHARED_DATA ; c'est aussi le `length` que
/// NVML passe a mmap. Premier truc a reverifier sur un autre driver.
pub const RUSD_SIZE: usize = 3184;

pub const RUSD_POLL_CLOCK: u64 = 0x01;
pub const RUSD_POLL_PERF: u64 = 0x02;
pub const RUSD_POLL_MEMORY: u64 = 0x04;
pub const RUSD_POLL_POWER: u64 = 0x08;
pub const RUSD_POLL_THERMAL: u64 = 0x10;
pub const RUSD_POLL_PCI: u64 = 0x20;
pub const RUSD_POLL_FAN: u64 = 0x40;
pub const RUSD_POLL_PROC_UTIL: u64 = 0x80;
pub const RUSD_POLL_ALL: u64 = 0xff;

const TS_INVALID: u64 = 0;
const TS_IN_PROGRESS: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy, Default)]
#[derive(Debug)]
pub struct CardInfo {
    pub valid: u8,
    _p0: [u8; 3],
    pub domain: u32,
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
    _p1: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub gpu_id: u32,
    pub irq: u16,
    _p2: [u8; 2],
    pub reg_address: u64,
    pub reg_size: u64,
    pub fb_address: u64,
    pub fb_size: u64,
    pub minor: u32,
    pub dev_name: [u8; 10],
    _p3: [u8; 2],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Nvos64 {
    h_root: u32,
    h_object_parent: u32,
    h_object_new: u32,
    h_class: u32,
    p_alloc_parms: u64,
    p_rights_requested: u64,
    params_size: u32,
    flags: u32,
    status: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Nvos54 {
    h_client: u32,
    h_object: u32,
    cmd: u32,
    flags: u32,
    params: u64,
    params_size: u32,
    status: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Nvos33WithFd {
    h_client: u32,
    h_device: u32,
    h_memory: u32,
    _pad: u32,
    offset: u64,
    length: u64,
    p_linear_address: u64,
    status: u32,
    flags: u32,
    fd: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VersionReq {
    cmd: u32,
    reply: u32,
    version_string: [u8; 64],
}

impl Default for VersionReq {
    fn default() -> Self {
        VersionReq { cmd: 0, reply: 0, version_string: [0u8; 64] }
    }
}

const _: () = assert!(std::mem::size_of::<Nvos64>() == 48);
const _: () = assert!(std::mem::size_of::<Nvos54>() == 32);
const _: () = assert!(std::mem::size_of::<Nvos33WithFd>() == 56);
const _: () = assert!(std::mem::size_of::<VersionReq>() == 72);
const _: () = assert!(std::mem::size_of::<CardInfo>() == 72);

#[derive(Clone)]
pub struct GpuRef {
    pub index: usize,
    pub gpu_id: u32,
    pub card: CardInfo,
    pub h_device: u32,
    pub h_sub: u32,
    pub h_display: u32,
    pub h_rusd: u32,
    pub rusd: *mut u8,
    pub device_instance: u32,
    pub sub_device_instance: u32,
    pub board_id: u32,
    pub gpu_instance: u32,
    pub numa_id: i32,
    pub sli_status: u32,
    pub gpu_flags: u32,
    pub rm_name: String,
    pub node_open: bool,
}

pub struct Session {
    ctl: File,
    gpu_nodes: Vec<Option<File>>,
    pub h_client: u32,
    pub gpus: Vec<GpuRef>,
    pub cards: Vec<CardInfo>,
    pub excluded_ids: Vec<u32>,
    pub kmd_version: String,
    pub driver_version: String,
    pub rm_title: String,
    pub rm_branch: u32,
    pub rm_changelist: u32,
    pub rm_changelist_official: u32,
    pub version_validated: bool,
    pub debug: bool,
    pub last_status: std::cell::Cell<u32>,
}

pub fn bdf(d: &CardInfo) -> String {
    format!("{:04x}:{:02x}:{:02x}.{:x}", d.domain, d.bus, d.slot, d.function)
}

pub fn status_name(s: u32) -> &'static str {
    match s {
        0x00 => "NV_OK",
        0x1b => "NV_ERR_INSUFFICIENT_PERMISSIONS",
        0x1f => "NV_ERR_INVALID_ARGUMENT",
        0x56 => "NV_ERR_NOT_SUPPORTED",
        0x57 => "NV_ERR_TIMEOUT",
        0x9f => "NV_ERR_STATE_UNAVAILABLE",
        0xb0 => "NV_ERR_NOT_READY",
        0xffff_ffff => "ioctl error",
        _ => "NV_ERR_*",
    }
}

fn cstr(buf: &[u8]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).trim().to_string()
}

fn slice<T>(v: &mut T) -> &mut [u8] {
    unsafe { std::slice::from_raw_parts_mut(v as *mut T as *mut u8, std::mem::size_of::<T>()) }
}

fn open_rw(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

impl Session {
    pub fn open() -> io::Result<Session> {
        let ctl = match open_rw("/dev/nvidiactl") {
            Ok(f) => f,
            Err(e) => {
                return Err(io::Error::new(e.kind(), format!("/dev/nvidiactl: {e} — is the nvidia module loaded? (needs the same access as nvidia-smi, usually root)")));
            }
        };
        let fd = ctl.as_raw_fd();
        let debug = std::env::var_os("NVINFO_DBG").is_some();

        // ---- etape 2 : table des cartes -------------------------------------
        let mut cards = vec![CardInfo::default(); 32];
        let mut n_valid = 0usize;
        if ffi::ioctl_checked(fd, ESC_CARD_INFO, (32 * 72) as u32, cards.as_mut_ptr() as *mut c_void).is_ok() {
            n_valid = cards.iter().filter(|c| c.valid != 0).count();
        }
        cards.retain(|c| c.valid != 0);

        // ---- etape 3 : poignee de main de version --------------------------
        // PIEGE 1 : CHECK_VERSION_STR ne GET rien, il VALIDE la chaine fournie.
        // Sans fd valide, toute allocation d'un objet prive (NV01_DEVICE_0)
        // echoue en 0x1b et la seule trace est une ligne dans dmesg.
        let mut v = VersionReq::default();
        v.cmd = b'2' as u32; // QUERY : le noyau rend sa propre chaine
        let mut kmd = String::new();
        if ffi::ioctl_checked(fd, ESC_CHECK_VERSION, 72, &mut v as *mut _ as *mut c_void).is_ok() {
            kmd = cstr(&v.version_string);
        }
        let mut v2 = VersionReq::default();
        v2.cmd = 0; // STRICT
        let n = kmd.len().min(63);
        v2.version_string[..n].copy_from_slice(&kmd.as_bytes()[..n]);
        let mut validated = false;
        if ffi::ioctl_checked(fd, ESC_CHECK_VERSION, 72, &mut v2 as *mut _ as *mut c_void).is_ok() {
            validated = v2.reply == 1; // 1 = NV01_ROOT_*_VALID
        }
        if debug {
            eprintln!("nvrm: kmd={kmd:?} strict_reply={} validated={validated} cards={n_valid}", v2.reply);
        }

        // ---- etape 4 : client RM -------------------------------------------
        let fd = ctl.as_raw_fd();
        let mut h_client = 0u32;
        rm_alloc(fd, 0, 0, &mut h_client, CL_ROOT_CLIENT, std::ptr::null_mut(), 0).map_err(|st| {
            io::Error::new(ekind(st), format!("RM_ALLOC NV01_ROOT_CLIENT : {}", status_name(st)))
        })?;

        let mut s = Session {
            ctl,
            gpu_nodes: Vec::new(),
            h_client,
            gpus: Vec::new(),
            cards,
            excluded_ids: Vec::new(),
            kmd_version: kmd,
            driver_version: String::new(),
            rm_title: String::new(),
            rm_branch: 0,
            rm_changelist: 0,
            rm_changelist_official: 0,
            version_validated: validated,
            debug,
            last_status: std::cell::Cell::new(0),
        };

        // ---- etape 5 : GPU sondes ------------------------------------------
        // NV0000_CTRL_GPU_GET_PROBED_IDS_PARAMS = gpuIds[32], excludedGpuIds[32], gpuFlags[32]
        let mut pr = [0xffff_ffffu32; 96];
        let mut ids: Vec<u32> = Vec::new();
        let mut flags: Vec<u32> = Vec::new();
        if s.ctl(s.h_client, C_GPU_GET_PROBED_IDS, slice(&mut pr), 384).is_ok() {
            for i in 0..32 {
                if pr[i] != 0xffff_ffff {
                    ids.push(pr[i]);
                    flags.push(pr[64 + i]);
                }
                if pr[32 + i] != 0xffff_ffff {
                    s.excluded_ids.push(pr[32 + i]);
                }
            }
        }
        if ids.is_empty() {
            let mut a = [0xffff_ffffu32; 32];
            if s.ctl(s.h_client, C_GPU_GET_ATTACHED_IDS, slice(&mut a), 128).is_ok() {
                ids = a.iter().copied().take_while(|&i| i != 0xffff_ffff).collect();
                flags = vec![0; ids.len()];
            }
        }

        // ---- etape 6 : PIEGE 3 : la liste doit finir par NV_GPU_ID_INVALID --
        if !ids.is_empty() {
            let mut at = [0xffff_ffffu32; 33];
            for (i, id) in ids.iter().enumerate().take(32) {
                at[i] = *id;
            }
            match s.ctl(s.h_client, C_GPU_ATTACH_IDS, slice(&mut at), 132) {
                Ok(_) => if s.debug { eprintln!("nvrm: ATTACH_IDS ok, failedId={:#x}", at[32]); },
                Err(st) => if s.debug { eprintln!("nvrm: ATTACH_IDS -> {st:#x} ({})", status_name(st)); },
            }
        }
        if ids.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no GPU attached by the Resource Manager"));
        }

        // ---- version complete + changelist (0x101) -------------------------
        let mut drv = [0u8; 256];
        let mut ver = [0u8; 256];
        let mut title = [0u8; 256];
        let (dp, vp, tp) = (drv.as_mut_ptr(), ver.as_mut_ptr(), title.as_mut_ptr());
        #[repr(C)]
        struct Bv {
            size: u32,
            _pad: u32,
            p_drv: u64,
            p_ver: u64,
            p_title: u64,
            cl: u32,
            cl_official: u32,
        }
        let mut bv = Bv {
            size: 255,
            _pad: 0,
            p_drv: dp as u64,
            p_ver: vp as u64,
            p_title: tp as u64,
            cl: 0,
            cl_official: 0,
        };
        if s.ctl(s.h_client, C_SYS_BUILD_VERSION, slice(&mut bv), 40).is_ok() {
            s.driver_version = cstr(&drv);
            let vt = cstr(&ver);
            s.rm_branch = vt.trim().parse().unwrap_or(0);
            s.rm_changelist = bv.cl;
            s.rm_changelist_official = bv.cl_official;
            s.rm_title = cstr(&title);
            if s.driver_version.is_empty() {
                s.driver_version = vt;
            }
        }

        // ---- etapes 7-9 : device + subdevice par GPU -----------------------
        for (i, id) in ids.iter().enumerate() {
            let card = *s.cards.get(i).unwrap_or(&CardInfo::default());
            let mut g = GpuRef {
                index: i,
                gpu_id: *id,
                card,
                h_device: 0,
                h_sub: 0,
                h_display: 0,
                h_rusd: 0,
                rusd: std::ptr::null_mut(),
                device_instance: 0,
                sub_device_instance: 0,
                board_id: 0,
                gpu_instance: 0,
                numa_id: -1,
                sli_status: 0,
                gpu_flags: flags.get(i).copied().unwrap_or(0),
                rm_name: String::new(),
                node_open: false,
            };

            #[repr(C)]
            #[derive(Clone, Copy)]
            struct Ii {
                gpu_id: u32,
                gpu_flags: u32,
                device_instance: u32,
                sub_device_instance: u32,
                sz_name: u64,
                sli_status: u32,
                board_id: u32,
                gpu_instance: u32,
                numa_id: i32,
            }
            let mut nm = [0u8; 64];
            let np = nm.as_mut_ptr() as u64;
            let mut ii = Ii {
                gpu_id: *id,
                gpu_flags: 0,
                device_instance: 0,
                sub_device_instance: 0,
                sz_name: np,
                sli_status: 0,
                board_id: 0,
                gpu_instance: 0,
                numa_id: -1,
            };
            if let Err(st) = s.ctl(s.h_client, C_GPU_GET_ID_INFO, slice(&mut ii), 40) {
                if s.debug { eprintln!("nvrm: GET_ID_INFO gpu {id:#x} -> {st:#x} ({})", status_name(st)); }
                let mut b = ii;
                b.sz_name = 0;
                if s.ctl(s.h_client, C_GPU_GET_ID_INFO_V2, slice(&mut b), 32).is_ok() {
                    ii = b;
                } else if s.debug {
                    eprintln!("nvrm: GET_ID_INFO(gpu {id:#x}) echoue");
                }
            }
            g.device_instance = ii.device_instance;
            g.sub_device_instance = ii.sub_device_instance;
            g.board_id = ii.board_id;
            g.gpu_instance = ii.gpu_instance;
            g.numa_id = ii.numa_id;
            g.sli_status = ii.sli_status;
            g.gpu_flags = ii.gpu_flags;
            g.rm_name = cstr(&nm);
            if s.debug {
                eprintln!("nvrm: gpu{i} id={:#x} dev_instance={} sub_instance={} numa={} minor={} name={:?}",
                    *id, ii.device_instance, ii.sub_device_instance, ii.numa_id, card.minor, g.rm_name);
            }

            // PIEGE 2 : NV01_DEVICE_0 est RS_FLAGS_ALLOC_PRIVILEGED -> il faut
            // aussi ouvrir /dev/nvidiaN et l'enregistrer aupres du fd de controle.
            let node = format!("/dev/nvidia{}", card.minor);
            if let Ok(f) = open_rw(&node) {
                let mut arg = s.ctl.as_raw_fd() as u32;
                let r = ffi::iow(
                    f.as_raw_fd(),
                    ESC_REGISTER_FD,
                    4,
                    &mut arg as *mut _ as *mut c_void,
                );
                if s.debug {
                    eprintln!("nvrm: open {node} fd={} REGISTER_FD -> {} ({})", f.as_raw_fd(), r, if r < 0 { "ECHO?" } else { "ok" });
                }
                if r >= 0 {
                    g.node_open = true;
                    s.gpu_nodes.push(Some(f));
                } else {
                    s.gpu_nodes.push(None);
                }
            } else {
                s.gpu_nodes.push(None);
            }

            let fd = s.ctl.as_raw_fd();
            // NV0080_ALLOC_PARAMETERS (cl0080.h) fait 56 octets : deviceId a @0,
            // le reste nul. Une taille differente est refusee 0x1f/0x33 — la
            // taille du struct fait partie de l'API (docs §7.13).
            let mut dev_params = vec![0u8; 56];
            dev_params[0..4].copy_from_slice(&g.device_instance.to_le_bytes());
            let mut hd = 0u32;
            let mut r = if g.device_instance == 0 {
                // NVML passe paramsSize=0 quand l'instance vaut 0 (observation)
                rm_alloc(fd, h_client, h_client, &mut hd, CL_DEVICE, std::ptr::null_mut(), 0)
            } else {
                rm_alloc(
                    fd,
                    h_client,
                    h_client,
                    &mut hd,
                    CL_DEVICE,
                    dev_params.as_mut_ptr() as *mut c_void,
                    56,
                )
            };
            if r.is_err() && g.device_instance == 0 {
                r = rm_alloc(
                    fd,
                    h_client,
                    h_client,
                    &mut hd,
                    CL_DEVICE,
                    dev_params.as_mut_ptr() as *mut c_void,
                    56,
                );
            }
            match r {
                Ok(_) => g.h_device = hd,
                Err(st) => {
                    if s.debug {
                        eprintln!("nvrm: alloc NV01_DEVICE_0 gpu{i} -> {st:#x} {} (dev_instance={} node_open={})", status_name(st), g.device_instance, g.node_open);
                    }
                    s.gpus.push(g);
                    continue;
                }
            }
            let mut sub_params = g.sub_device_instance;
            let mut hs = 0u32;
            let mut r2 = if g.sub_device_instance == 0 {
                rm_alloc(fd, h_client, hd, &mut hs, CL_SUBDEVICE, std::ptr::null_mut(), 0)
            } else {
                rm_alloc(
                    fd,
                    h_client,
                    hd,
                    &mut hs,
                    CL_SUBDEVICE,
                    &mut sub_params as *mut _ as *mut c_void,
                    4,
                )
            };
            if r2.is_err() && g.sub_device_instance == 0 {
                r2 = rm_alloc(
                    fd,
                    h_client,
                    hd,
                    &mut hs,
                    CL_SUBDEVICE,
                    &mut sub_params as *mut _ as *mut c_void,
                    4,
                );
            }
            if let Ok(_) = r2 {
                g.h_sub = hs;
            }
            s.gpus.push(g);
        }
        Ok(s)
    }

    /// Un ioctl RM : le NVOS54 porte le pointeur vers les parametres.
    /// La taille est un test d'API cote noyau — toute autre taille renvoie 0x1f.
    pub fn ctl(&self, obj: u32, cmd: u32, params: &mut [u8], size: u32) -> Result<(), u32> {
        let mut c = Nvos54 {
            h_client: self.h_client,
            h_object: obj,
            cmd,
            flags: 0,
            params: params.as_mut_ptr() as u64,
            params_size: size,
            status: 0,
        };
        let r = ffi::ioctl_checked(
            self.ctl.as_raw_fd(),
            ESC_RM_CONTROL,
            32,
            &mut c as *mut _ as *mut c_void,
        );
        match r {
            Err(_) => {
                self.last_status.set(0xffff_ffff);
                Err(0xffff_ffff)
            }
            Ok(_) => {
                self.last_status.set(c.status);
                if c.status == NV_OK {
                    Ok(())
                } else {
                    Err(c.status)
                }
            }
        }
    }

    /// Controle sans parametre d'entree, reponse dans un buffer zero initialise.
    pub fn ctl_out(&self, obj: u32, cmd: u32, size: usize) -> Option<Vec<u8>> {
        let mut b = vec![0u8; size];
        self.ctl(obj, cmd, &mut b, size as u32).map(|_| b).ok()
    }

    /// Controle 4 octets en entree/sortie (les fameux { u32 } simples).
    pub fn ctl_u32(&self, obj: u32, cmd: u32, init: u32) -> Option<u32> {
        let mut v: u32 = init;
        self.ctl(obj, cmd, slice(&mut v), 4).ok().map(|_| v)
    }

    /// Idem sur 8 octets.
    pub fn ctl_u64(&self, obj: u32, cmd: u32, init: u64) -> Option<u64> {
        let mut v: u64 = init;
        self.ctl(obj, cmd, slice(&mut v), 8).ok().map(|_| v)
    }

    /// Table indexee a la NV2080 : { u32 count, { u32 index, u32 data }[] }.
    /// La reponse garde l'ordre de la requete. `max` = taille du tableau fixe
    /// declare dans le SDK (le noyau verifie params_size == sizeof(struct)).
    pub fn ctl_indexed(&self, obj: u32, cmd: u32, idx: &[u32], max: usize) -> Option<Vec<u32>> {
        if idx.len() > max {
            return None;
        }
        let mut b = vec![0u8; 4 + 8 * max];
        b[0..4].copy_from_slice(&(idx.len() as u32).to_le_bytes());
        for (i, &ix) in idx.iter().enumerate() {
            b[4 + 8 * i..8 + 8 * i].copy_from_slice(&ix.to_le_bytes());
        }
        self.ctl(obj, cmd, &mut b, (4 + 8 * max) as u32).ok()?;
        Some((0..idx.len()).map(|i| u32::from_le_bytes(b[8 + 8 * i..12 + 8 * i].try_into().unwrap())).collect())
    }

    pub fn has(&self, i: usize) -> bool {
        self.gpus.get(i).map(|g| g.h_sub != 0).unwrap_or(false)
    }

    pub fn sub(&self, i: usize) -> u32 {
        self.gpus.get(i).map(|g| g.h_sub).unwrap_or(0)
    }

    pub fn dev(&self, i: usize) -> u32 {
        self.gpus.get(i).map(|g| g.h_device).unwrap_or(0)
    }

    pub fn card(&self, i: usize) -> CardInfo {
        self.gpus.get(i).map(|g| g.card).unwrap_or_default()
    }

    /// Alloue 0x00de, mappe la page de telemetry, arme le polling.
    pub fn enable_rusd(&mut self, i: usize, mask: u64) -> bool {
        if self.gpus.get(i).map(|g| !g.rusd.is_null()).unwrap_or(false) {
            self.rusd_poll(i, mask);
            return true;
        }
        let (hs, hd, h_client, fd) = match self.gpus.get(i) {
            Some(g) if g.h_sub != 0 => (g.h_sub, g.h_device, self.h_client, self.ctl.as_raw_fd()),
            _ => return false,
        };
        let _ = hd;
        // PIEGE 4 : NV00DE_ALLOC_PARAMETERS = { u64 polledDataMask } est requis
        // (sans parametre le RM renvoie 0x1f).
        let mut ap = mask;
        let mut h_rusd = 0u32;
        if rm_alloc(
            fd,
            h_client,
            hs,
            &mut h_rusd,
            CL_RUSD,
            &mut ap as *mut _ as *mut c_void,
            8,
        )
        .is_err()
        {
            if self.debug {
                eprintln!("nvrm: class 0x00de alloc (mask {mask:#x}) refused");
            }
            return false;
        }
        // Le wrapper NVOS33 veut le fd de CONTROLE dans son champ `fd`, et
        // pLinearAddress n'est PAS un pointeur mais un cookie d'mmap ; le noyau
        // exige vm_pgoff == 0, donc mmap(offset = 0).
        let mut m = Nvos33WithFd {
            h_client,
            h_device: hd,
            h_memory: h_rusd,
            length: RUSD_SIZE as u64,
            flags: 1,
            fd: fd as u32,
            ..Default::default()
        };
        if ffi::ioctl_checked(fd, ESC_RM_MAP_MEMORY, 56, &mut m as *mut _ as *mut c_void).is_err()
            || m.status != NV_OK
        {
            return false;
        }
        let ptr = match ffi::mmap_ro(fd, RUSD_SIZE) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let g = &mut self.gpus[i];
        g.h_rusd = h_rusd;
        g.rusd = ptr;
        self.rusd_poll(i, mask);
        true
    }

    /// 0xde0001 : le pilote rafraichit les sections du masque demande.
    /// C'est LE seul ioctl de telemetry par cycle — d'ou un refresh bon marche.
    pub fn rusd_poll(&self, i: usize, mask: u64) {
        let h = match self.gpus.get(i) {
            Some(g) if g.h_rusd != 0 => g.h_rusd,
            _ => return,
        };
        let mut m = mask;
        let _ = self.ctl(h, C_RUSD_REQUEST_DATA_POLL, slice(&mut m), 8);
    }

    #[inline]
    fn base(&self, i: usize) -> *mut u8 {
        match self.gpus.get(i) {
            Some(g) => g.rusd,
            None => std::ptr::null_mut(),
        }
    }

    #[inline]
    fn word<T: Copy>(&self, i: usize, off: usize) -> Option<T> {
        let base = self.base(i);
        if base.is_null() || off + std::mem::size_of::<T>() > RUSD_SIZE {
            return None;
        }
        Some(unsafe { std::ptr::read_unaligned(base.add(off) as *const T) })
    }

    fn stamp(&self, i: usize, section: usize) -> u64 {
        self.word::<u64>(i, section).unwrap_or(TS_INVALID)
    }

    /// Lecture seqlock : timestamp, copie, timestamp ; valide si egaux et ni 0
    /// (invalide) ni ~0 (ecriture en cours).
    pub fn rusd<T: Copy>(&self, i: usize, section: usize, off: usize) -> Option<T> {
        for _ in 0..10 {
            let t0 = self.stamp(i, section);
            if t0 == TS_INVALID || t0 == TS_IN_PROGRESS {
                return None;
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            let v = self.word::<T>(i, off)?;
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            if self.stamp(i, section) == t0 {
                return Some(v);
            }
        }
        None
    }

    /// Lecture brute sans garde-fou : les sous-structures `violation` n'ont pas
    /// de timestamp propre et la section THROTTLE est publiee ts=0 par ce driver
    /// (docs/FINDINGS.md §3, « honest caveats »).
    pub fn rusd_raw<T: Copy>(&self, i: usize, off: usize) -> Option<T> {
        self.word::<T>(i, off)
    }

    /// Allocation de la classe NVKMS 0x0073 (colonne DispA).
    pub fn display_handle(&mut self, i: usize) -> Option<u32> {
        if let Some(g) = self.gpus.get(i) {
            if g.h_display != 0 {
                return Some(g.h_display);
            }
            if g.h_device == 0 {
                return None;
            }
        }
        let (h_client, hd, fd) = (self.h_client, self.dev(i), self.ctl.as_raw_fd());
        let mut h = 0u32;
        // la classe NVKMS 0x0073 s'alloue sous NV01_DEVICE_0 (observation NVML)
        rm_alloc(fd, h_client, hd, &mut h, CL_NVKMS, std::ptr::null_mut(), 0).ok()?;
        self.gpus[i].h_display = h;
        Some(h)
    }
}

fn ekind(st: u32) -> io::ErrorKind {
    match st {
        0x1b => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    }
}

/// NV_ESC_RM_ALLOC : hRoot est TOUJOURS le client, hObjectParent le pere de
/// l'objet cree (0 pour NV01_ROOT_CLIENT dont le pere est le root lui-meme).
fn rm_alloc(
    fd: i32,
    h_root: u32,
    parent: u32,
    out: &mut u32,
    class: u32,
    params: *mut c_void,
    size: u32,
) -> Result<(), u32> {
    let mut a = Nvos64 {
        h_root,
        h_object_parent: parent,
        h_object_new: 0, // 0 => le RM genere le handle
        h_class: class,
        p_alloc_parms: params as u64,
        params_size: size,
        ..Default::default()
    };
    let dbg = std::env::var_os("NVINFO_DBG").is_some();
    if dbg {
        eprintln!("nvrm:   RM_ALLOC hRoot={h_root} parent={parent} class={class:#x} params={params:p} size={size}");
    }
    let r = ffi::ioctl_checked(fd, ESC_RM_ALLOC, 48, &mut a as *mut _ as *mut c_void);
    if dbg {
        eprintln!(
            "nvrm:   -> ioctl_ok={} status={:#x} ({}) new_handle={:#x}",
            r.is_ok(),
            a.status,
            status_name(a.status),
            a.h_object_new
        );
    }
    r.map_err(|_| 0xffff_ffffu32)?;
    if a.status != NV_OK {
        return Err(a.status);
    }
    *out = a.h_object_new;
    Ok(())
}

impl Drop for Session {
    fn drop(&mut self) {
        for g in &self.gpus {
            if !g.rusd.is_null() {
                ffi::unmap(g.rusd as *mut c_void, RUSD_SIZE);
            }
        }
    }
}
