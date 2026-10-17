//! gpu.rs - ce que le Resource Manager sait, commande par commande.
//!
//! Chaque valeur est annotee avec sa source reverse-engineerreee (id de commande
//! RM + deplace + unite), exactement comme dans src/fields.c du depot
//! nvidia-smi-clone-RE. Les commandes marquees UNDOCUMENTED n'apparaissent dans
//! aucun header publie ; elles ont ete decodees par sondage actif (docs §4).

use crate::nvrm::{
    bdf, CardInfo, Session, C_DEVICE_GET_VIRT_MODE, C_DEVICE_QUERY_PERSISTENCE, C_KMS_ACTIVE_HEAD,
    C_KMS_NUM_HEADS,
};
use crate::rusd::{self};

// ---- commandes NV20_SUBDEVICE_0 -------------------------------------------
pub const C_GPU_NAME_STRING: u32 = 0x2080_0110; // 68   {u32 flags, char[64]}
pub const C_GPU_GET_INFO_V2: u32 = 0x2080_0102; // 580  liste indexee (max 72)
pub const C_GPU_COMPUTE_MODE: u32 = 0x2080_0131; // 4
pub const C_GPU_ECC_CONFIG: u32 = 0x2080_0133; // 8   {bEnable, bEnabledOnNextBoot}
pub const C_GPU_OEM_BOARD_INFO: u32 = 0x2080_013f; // 136 eeprom plaque
pub const C_GPU_GID_INFO: u32 = 0x2080_014a; // 268 UUID
pub const C_GPU_INFOROM_VERSION: u32 = 0x2080_014b; // 8  {char[3],u8,u8}
pub const C_GPU_GET_ENGINES_V2: u32 = 0x2080_0170; // 340 {count, list[0x54]}
pub const C_GPU_GET_PIDS: u32 = 0x2080_018d; // 3812
pub const C_GPU_GET_PID_INFO: u32 = 0x2080_018e; // 14408 entrees de 72 o
pub const C_GPU_MC_ARCH_INFO: u32 = 0x2080_1701; // 16  {arch,impl,rev,subRev}
pub const C_FB_GET_INFO_V2: u32 = 0x2080_1303; // 1028 liste indexee (max 128)
pub const C_BUS_GET_INFO_V2: u32 = 0x2080_1823; // 420  liste indexee (max 52)
pub const C_ECC_CLIENT_EXPOSED: u32 = 0x2080_3400; // 96
pub const C_ECC_VOLATILE_COUNTS: u32 = 0x2080_3401; // 40
pub const C_PERF_GET_CURRENT_PSTATE: u32 = 0x2080_2068; // 4
pub const C_PERF_GET_POWERSTATE: u32 = 0x2080_205a; // 4
pub const C_PERF_RATED_TDP: u32 = 0x2080_206e; // 12
pub const C_THERM_SENSORS: u32 = 0x2080_852e; // 1432 UNDOCUMENTED table capteurs
pub const C_THERM_READING: u32 = 0x2080_852f; // 844  UNDOCUMENTED lecture (duty @32)
pub const C_PWR_INFO: u32 = 0x2080_a618; // 20512 UNDOCUMENTED default limit @736
pub const C_PWR_LIMIT: u32 = 0x2080_a61d; // 12   UNDOCUMENTED selecteur 0xfe01
pub const C_CLK_MAX: u32 = 0x2080_a0a7; // 17676 UNDOCUMENTED max graphics @36 (10 kHz)

const PID_CLASS_COMPUTE: u32 = 0xc7c0; // Ampere compute -> "C"
const PID_CLASS_GRAPHICS: u32 = 0xc797; // Ampere 3D      -> "G"

#[inline]
pub fn rd32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
#[inline]
pub fn rd64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
#[inline]
fn wr32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn cstr(b: &[u8], o: usize, len: usize) -> String {
    let s = &b[o..o + len];
    let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..e]).trim().to_string()
}

/// NVML arrondit les MiB VERS LE HAUT (308160 KiB -> 301 MiB, pas 300).
#[inline]
pub fn kib2mib(k: u64) -> u64 {
    (k + 1023) / 1024
}

/// architecture/implementation (0x20801701) -> compute capability + nom de puce
fn arch_cc(arch: u32, impl_: u32) -> (Option<&'static str>, &'static str) {
    match arch {
        0x120 => (Some("3.7"), "Kepler"),
        0x130 => (
            Some(if impl_ <= 1 { "5.0" } else { "5.2" }),
            "Maxwell",
        ),
        0x140 => (Some("6.1"), "Pascal"),
        0x150 => (Some("7.0"), "Volta"),
        0x160 => (Some("7.5"), "Turing"),
        0x170 => (
            Some(if impl_ == 0 { "8.0" } else { "8.6" }),
            match impl_ {
                0 => "Ampere GA100",
                1 => "Ampere GA10x",
                2 => "Ampere GA102",
                4 => "Ampere GA104",
                _ => "Ampere",
            },
        ),
        0x180 => (Some("9.0"), "Hopper"),
        0x190 => (Some("8.9"), "Ada"),
        0x1a0 => (Some("10.0"), "Blackwell GB1xx"),
        0x1b0 => (Some("10.0"), "GB200"),
        0x1c0 => (Some("10.0"), "GR100/Rubin-prep"),
        _ => (None, "inconnue"),
    }
}

fn engine_name(e: u32) -> &'static str {
    // NV2080_ENGINE_TYPE_* (class/cl2080_notification.h)
    match e {
        0 => "NULL",
        1..=8 => "GRAPHICS",
        9..=18 => "COPY (CE)",
        19 => "BSP/NVDEC",
        20..=26 => "NVDEC",
        27 => "MSENC",
        28..=29 => "NVENC",
        30 => "VP",
        31 => "ME",
        32 => "PPP",
        33 => "MPEG",
        34 => "SW",
        35 => "CIPHER",
        36 => "VIC",
        37 => "MP",
        38 => "SEC2",
        39 => "HOST",
        40 => "NVOFA",
        41 => "PVA0",
        _ => "OTHER",
    }
}

const FB_RAM_TYPE: [(u32, &str); 18] = [
    (0, "UNKNOWN"),
    (1, "SDRAM"),
    (2, "DDR1/GDDR1"),
    (3, "SDDR2"),
    (4, "GDDR2"),
    (5, "GDDR3"),
    (6, "GDDR4"),
    (7, "SDDR3"),
    (8, "GDDR5"),
    (9, "LPDDR2"),
    (12, "SDDR4"),
    (13, "LPDDR4"),
    (14, "HBM1"),
    (15, "HBM2"),
    (16, "GDDR5X"),
    (17, "GDDR6"),
    (18, "GDDR6X"),
    (19, "LPDDR5"),
];
pub fn ram_type(v: u32) -> String {
    match v {
        20 => "HBM3".into(),
        21 => "GDDR7".into(),
        22 => "HBM4".into(),
        _ => FB_RAM_TYPE
            .iter()
            .find(|(k, _)| *k == v)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| format!("#{v}")),
    }
}

pub fn mem_vendor(v: u32) -> &'static str {
    match v {
        0 => "UNKNOWN",
        1 => "Samsung",
        2 => "Qimonda",
        3 => "Elpida",
        4 => "Etron",
        5 => "Nanya",
        6 => "Hynix",
        7 => "Mosel",
        8 => "Winbond",
        9 => "ESMT",
        10 => "Micron",
        _ => "OTHER",
    }
}

/// Tout ce qui ne bouge pas (ou presque) : une seule passe au demarrage.
#[derive(Default, Clone, Debug)]
pub struct Static {
    pub index: usize,
    pub card: CardInfo,
    pub bus_id: String,
    pub name: String,
    pub marketing: String,
    pub rm_name: String,
    pub uuid: String,
    pub serial: String,
    pub product_part: String,
    pub board_699: String,
    pub board_965: String,
    pub mem_part: String,
    pub mem_date: String,
    pub mem_vendor: u8,
    pub board_type: u8,
    pub board_rev: String,
    pub build_date: u32,
    pub vbios: String,
    pub arch: u32,
    pub impl_: u32,
    pub chip_rev: u32,
    pub sub_rev: u32,
    pub compute_cap: String,
    pub codename: String,
    pub ecid: String,
    pub engines: Vec<u32>,
    pub caps: Vec<(String, String)>,
    pub compute_mode: Option<u32>,
    pub ecc_enabled: Option<bool>,
    pub ecc_next_boot: Option<bool>,
    pub persistence: Option<bool>,
    pub virt_mode: Option<u32>,
    pub heads: u32,
    pub display_active: bool,
    pub clk_max_gfx_mhz: Option<u32>,
    pub power_default_mw: Option<u32>,
    pub inforom: Vec<(String, String)>,
    pub fb: Vec<(String, String)>,
    pub fb_total_mib: Option<u64>,
    pub bus: Vec<(String, String)>,
    pub sensor_table_ok: bool,
}

impl Session {
    pub fn gpu_static(&mut self, i: usize) -> Static {
        let mut st = Static::default();
        st.index = i;
        st.card = self.card(i);
        st.bus_id = bdf(&st.card);
        st.rm_name = self.gpus[i].rm_name.clone();
        let sub = self.sub(i);
        if sub == 0 {
            return st;
        }

        // nom (0x20800110) : { u32 flags, char name[64] }
        if let Some(b) = self.ctl_out(sub, C_GPU_NAME_STRING, 68) {
            st.name = cstr(&b, 4, 64);
        }

        // EEPROM plaque (0x2080013f, 136 o) — offsets issus de
        // NV2080_CTRL_GPU_GET_OEM_BOARD_INFO_PARAMS :
        //   buildDate@0 marketing[24]@4 serial[16]@28 memMfr@44 memPart[20]@45
        //   memDate[6]@65 productPart[20]@71 boardRev[3]@91 boardType@94
        //   699Part[20]@95 965Part[20]@115
        if let Some(b) = self.ctl_out(sub, C_GPU_OEM_BOARD_INFO, 136) {
            st.build_date = rd32(&b, 0);
            st.marketing = cstr(&b, 4, 24);
            st.serial = cstr(&b, 28, 16);
            st.mem_vendor = b[44];
            st.mem_part = cstr(&b, 45, 20);
            st.mem_date = cstr(&b, 65, 6);
            st.product_part = cstr(&b, 71, 20);
            st.board_rev = format!("{:02X}{:02X}{:02X}", b[91], b[92], b[93]);
            st.board_type = b[94];
            st.board_699 = cstr(&b, 95, 20);
            st.board_965 = cstr(&b, 115, 20);
        }

        // UUID (0x2080014a) : IN {index=0, flags=2 (binaire)} -> 16 octets a @12
        let mut g = vec![0u8; 268];
        wr32(&mut g, 4, 0x2);
        if let Ok(()) = self.ctl(sub, C_GPU_GID_INFO, &mut g, 268) {
            let len = rd32(&g, 8) as usize;
            if len == 16 {
                let d = &g[12..28];
                st.uuid = format!(
                    "GPU-{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7], d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]
                );
            } else if len > 0 && len < 80 {
                st.uuid = cstr(&g, 12, len.min(79));
            }
        }

        // architecture (0x20801701)
        if let Some(b) = self.ctl_out(sub, C_GPU_MC_ARCH_INFO, 16) {
            st.arch = rd32(&b, 0);
            st.impl_ = rd32(&b, 4);
            st.chip_rev = rd32(&b, 8);
            st.sub_rev = rd32(&b, 12);
            let (cc, code) = arch_cc(st.arch, st.impl_);
            st.compute_cap = cc.unwrap_or("?").to_string();
            st.codename = code.to_string();
        }

        // GPU_GET_INFO_V2 : caps / ECID / particularites
        let idx: Vec<u32> = vec![1, 2, 4, 0x1f, 0x25, 0x27, 0x28, 0x29, 0x2a, 0x2c, 0x2d, 0x2f, 0x30, 0x34, 0x36, 0x37, 0x3a, 0x3b, 0x3c, 0x3d, 0x3f, 0x41, 0x42, 0x44, 0x45, 0x46, 0x47];
        let names: Vec<&str> = vec![
            "ECID_LO32", "ECID_HI32", "MINOR_REVISION_EXT", "SYSMEM_ACCESS",
            "SURPRISE_REMOVAL_POSSIBLE", "GLOBAL_POISON_FUSE", "NVSWITCH_PROXY",
            "GPU_SR_SUPPORT", "GPU_SMC_MODE", "SM_VERSION", "FLA_CAPABILITY",
            "PER_RUNLIST_CHANNEL_RAM", "ATS_CAPABILITY", "DISPLAY_ENABLED",
            "PROFILING_CAPABILITY", "DEBUGGING_CAPABILITY", "LOCAL_EGM_CAPABILITY",
            "SELF_HOSTED_CAPABILITY", "CMP_SKU", "DMABUF_CAPABILITY",
            "RESETLESS_MIG_SUPPORTED", "LOCALIZATION_SUPPORTED", "NON_PASID_ATS",
            "FLA_SYSMEM_CAPABILITY", "COHERENT_GPU_MEMORY_MODE",
            "COMPR_BIT_BACKING_COPY_TYPE", "CPU_COHERENT_CACHING_GPU_MEM",
            "GPU_COHERENT_CACHING_CPU_MEM",
        ];
        if let Some(vals) = self.ctl_indexed(sub, C_GPU_GET_INFO_V2, &idx, 72) {
            let mut ecid_lo = 0u32;
            let mut ecid_hi = 0u32;
            for (k, v) in vals.iter().enumerate() {
                if k >= names.len() {
                    break;
                }
                match names[k] {
                    "ECID_LO32" => ecid_lo = *v,
                    "ECID_HI32" => ecid_hi = *v,
                    n => {
                        st.caps.push((n.to_string(), format!("{:#x}", v)));
                    }
                }
            }
            if ecid_lo != 0 || ecid_hi != 0 {
                st.ecid = format!("{:08x}{:08x}", ecid_hi, ecid_lo);
            }
        }

        // moteurs (0x20800170)
        if let Some(b) = self.ctl_out(sub, C_GPU_GET_ENGINES_V2, 340) {
            let n = (rd32(&b, 0) as usize).min(0x54);
            st.engines = (0..n).map(|k| rd32(&b, 4 + k * 4)).collect();
        }

        // modes
        st.compute_mode = self.ctl_u32(sub, C_GPU_COMPUTE_MODE, 0);
        if let Some(b) = self.ctl_out(sub, C_GPU_ECC_CONFIG, 8) {
            st.ecc_enabled = Some(rd32(&b, 0) != 0);
            st.ecc_next_boot = Some(rd32(&b, 4) != 0);
        }
        let dev = self.dev(i);
        // PIEGE (docs §7.4) : NV0080_CTRL_GPU_SW_STATE_PERSISTENCE_ENABLED == 0
        st.persistence = self.ctl_u32(dev, C_DEVICE_QUERY_PERSISTENCE, 1).map(|v| v == 0);
        if let Some(b) = self.ctl_out(dev, C_DEVICE_GET_VIRT_MODE, 8) {
            st.virt_mode = Some(rd32(&b, 0));
        }

        // tete d'affichage : classe NVKMS 0x0073 (colonne DispA)
        if let Some(h) = self.display_handle(i) {
            let mut b = vec![0u8; 12];
            if self.ctl(h, C_KMS_NUM_HEADS, &mut b, 12).is_ok() {
                st.heads = rd32(&b, 8);
                let mut active = 0u32;
                for head in 0..st.heads.min(8) {
                    let mut c = vec![0u8; 20];
                    wr32(&mut c, 0, 0);
                    wr32(&mut c, 4, head);
                    if self.ctl(h, C_KMS_ACTIVE_HEAD, &mut c, 20).is_ok() {
                        if rd32(&c, 12) != 0 {
                            active = 1;
                        }
                    }
                }
                st.display_active = active != 0;
            }
        }

        // InfoROM : objets "img", "cap", "rsd" (taille reelle = 8)
        for (tag, label) in [([b'i', b'm', b'g'], "image"), ([b'c', b'a', b'p'], "capabilities"), ([b'r', b's', b'd'], "reserved")] {
            let mut b = vec![0u8; 8];
            b[0..3].copy_from_slice(&tag);
            if self.ctl(sub, C_GPU_INFOROM_VERSION, &mut b, 8).is_ok() {
                st.inforom.push((label.to_string(), format!("{}.{}", b[3], b[4])));
            }
        }

        // memoire : tags FB statiques
        let fbidx: Vec<u32> = vec![
            0x01, 0x04, 0x05, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x10, 0x11, 0x17,
            0x18, 0x19, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x22, 0x23, 0x27, 0x2a, 0x33, 0x34,
            0x36, 0x3b, 0x44,
        ];
        if let Some(vals) = self.ctl_indexed(sub, C_FB_GET_INFO_V2, &fbidx, 128) {
            let push = |k: usize, name: &str, unit: &str, vals: &Vec<u32>, v: &mut Vec<(String, String)>| {
                if let Some(&x) = vals.get(k) {
                    v.push((name.to_string(), if unit.is_empty() { x.to_string() } else if unit == "type" { ram_type(x) } else if unit == "vendor" { mem_vendor(x).to_string() } else { format!("{x} {unit}") }));
                }
            };
            push(0, "compression size", "KiB", &vals, &mut st.fb);
            push(1, "partitions (FBPA)", "", &vals, &mut st.fb);
            push(2, "BAR1 size", "KiB", &vals, &mut st.fb);
            push(3, "RAM size (physical)", "KiB", &vals, &mut st.fb);
            if let Some(&t) = vals.get(4) {
                st.fb_total_mib = Some(kib2mib(t as u64));
                st.fb.push(("total RAM".into(), format!("{} MiB", kib2mib(t as u64))));
            }
            push(5, "heap size", "KiB", &vals, &mut st.fb);
            push(6, "mappable heap", "KiB", &vals, &mut st.fb);
            push(7, "bus width", "bits", &vals, &mut st.fb);
            push(8, "RAM cfg", "", &vals, &mut st.fb);
            push(9, "RAM type", "type", &vals, &mut st.fb);
            push(10, "banks", "", &vals, &mut st.fb);
            push(11, "FB tax", "KiB", &vals, &mut st.fb);
            push(12, "heap base", "KiB", &vals, &mut st.fb);
            push(13, "RAM location", "", &vals, &mut st.fb);
            push(14, "FB is broken", "", &vals, &mut st.fb);
            push(15, "FBP count", "", &vals, &mut st.fb);
            push(16, "L2 cache", "KiB", &vals, &mut st.fb);
            push(17, "RAM vendor", "vendor", &vals, &mut st.fb);
            push(18, "BAR1 available", "KiB", &vals, &mut st.fb);
            push(19, "heap start", "KiB", &vals, &mut st.fb);
            push(20, "BAR1 max contiguous", "KiB", &vals, &mut st.fb);
            push(21, "RAM utilisable", "KiB", &vals, &mut st.fb);
            push(22, "LTC count", "", &vals, &mut st.fb);
            push(23, "LTS count", "", &vals, &mut st.fb);
            push(24, "offline heap", "KiB", &vals, &mut st.fb);
            push(25, "page retirement", "", &vals, &mut st.fb);
            push(26, "protected mem total", "KiB", &vals, &mut st.fb);
            push(27, "protected mem free", "KiB", &vals, &mut st.fb);
            push(28, "zero FB", "", &vals, &mut st.fb);
            push(29, "NUMA node id", "", &vals, &mut st.fb);
            push(30, "reclaimable heap", "KiB", &vals, &mut st.fb);
        }

        // bus : tags BUS_GET_INFO_V2 (0x20801823)
        let bidx: Vec<u32> = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x07, 0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x13, 0x15, 0x18, 0x2c, 0x2d, 0x2e, 0x2f, 0x31, 0x33];
        if let Some(vals) = self.ctl_indexed(sub, C_BUS_GET_INFO_V2, &bidx, 52) {
            let bn = [
                "bus type",
                "IRQ",
                "bus caps",
                "PCIe LINK_CAPS (GPU)",
                "PCIe LINK_CAPS (root)",
                "PCIe LINK_CTRL_STATUS",
                "GART size",
                "GART flags",
                "bus number",
                "device number",
                "PCIe link errors",
                "interface type",
                "PCIe AER",
                "ASLM status",
                "domain",
                "PCIE_GEN_INFO",
                "GPU interface type",
                "UPSTREAM_GEN_INFO",
                "MSI info",
                "sysmem connection",
            ];
            for (k, name) in bn.iter().enumerate() {
                if let Some(&v) = vals.get(k) {
                    st.bus.push((name.to_string(), format!("{v:#010x}")));
                }
            }
        }

        // horloges max (0x2080a0a7 UNDOCUMENTED) : graphics @36, en 10 kHz
        if let Some(b) = self.ctl_out(sub, C_CLK_MAX, 17676) {
            let v = rd32(&b, 36);
            if v != 0 {
                st.clk_max_gfx_mhz = Some(v / 100);
            }
        }

        // limite par defaut (0x2080a618 UNDOCUMENTED) : mW @736
        if let Some(b) = self.ctl_out(sub, C_PWR_INFO, 20512) {
            let v = rd32(&b, 736);
            if v != 0 {
                st.power_default_mw = Some(v);
            }
        }

        // la table des capteurs/refroidisseurs doit etre lue une fois pour que
        // la lecture 0x2080852f (duty cycle) soit armee
        st.sensor_table_ok = self.ctl_out(sub, C_THERM_SENSORS, 1432).is_some();
        st
    }

    /// Duty cycle du ventilateur en % : 0x2080852f UNDOCUMENTED, champ @32 en
    /// UFXP16.16 (0xa88b -> 65.8 %, coherent avec les 2646 RPM lus ailleurs).
    pub fn fan_duty(&self, i: usize) -> Option<f64> {
        let mut b = vec![0u8; 844];
        wr32(&mut b, 0, 0);
        wr32(&mut b, 4, 1);
        self.ctl(self.sub(i), C_THERM_READING, &mut b, 844).ok()?;
        let duty = rd32(&b, 32);
        if duty == 0 || duty >= 16 << 16 {
            return None;
        }
        Some(duty as f64 * 100.0 / 65536.0)
    }

    /// Limites de conso : selecteur 0xfe01 -> mW a @4 (0x2080a61d UNDOCUMENTED).
    pub fn power_limit_mw(&self, i: usize) -> Option<u32> {
        let mut b = vec![0u8; 12];
        wr32(&mut b, 0, 0xfe01);
        self.ctl(self.sub(i), C_PWR_LIMIT, &mut b, 12).ok()?;
        let v = rd32(&b, 4);
        if v == 0 {
            None
        } else {
            Some(v)
        }
    }

    /// Compteurs ECC « live » lus direct dans le RM (pas via la page RUSD) :
    /// 0x20803401 volatile (40 o) et 0x20803400 client-exposed (96 o).
    pub fn ecc_live(&self, i: usize) -> (Option<[u64; 5]>, Option<Vec<u64>>, Option<u32>, Option<bool>) {
        let sub = self.sub(i);
        let vol = self
            .ctl_out(sub, C_ECC_VOLATILE_COUNTS, 40)
            .map(|b| std::array::from_fn(|k| rd64(&b, k * 8)));
        let (exp, cleared, thr) = match self.ctl_out(sub, C_ECC_CLIENT_EXPOSED, 96) {
            Some(b) => {
                let mut v = Vec::new();
                for k in 0..5 {
                    v.push(rd64(&b, k * 8));
                }
                for k in 0..5 {
                    v.push(rd64(&b, 48 + k * 8));
                }
                (Some(v), Some(rd32(&b, 40)), Some(b[88] != 0))
            }
            None => (None, None, None),
        };
        (vol, exp, cleared, thr)
    }

    /// P-state par la commande dediee (0x20802068) — recoupe le masue RUSD.
    pub fn pstate_direct(&self, i: usize) -> Option<u32> {
        rusd::pstate_from_mask(self.ctl_u32(self.sub(i), C_PERF_GET_CURRENT_PSTATE, 0)?)
    }

    /// Etat d'alimentation (0x2080205a) : 0 = on, 1 = standby...
    pub fn powerstate(&self, i: usize) -> Option<u32> {
        self.ctl_u32(self.sub(i), C_PERF_GET_POWERSTATE, 0)
    }

    /// Rated TDP : statut + niveau (0x2080206e, 12 o).
    pub fn rated_tdp(&self, i: usize) -> Option<(u32, u32)> {
        let mut b = vec![0u8; 12];
        wr32(&mut b, 0, 0);
        self.ctl(self.sub(i), C_PERF_RATED_TDP, &mut b, 12).ok()?;
        Some((rd32(&b, 4), rd32(&b, 8)))
    }

    /// Etat memoire courant : NVML fait `used = HEAP_SIZE - HEAP_FREE`
    /// (et NON total - free), `free = HEAP_FREE`, `reserved = TOTAL - HEAP`.
    pub fn mem_state(&self, i: usize) -> Option<(u64, u64, u64, u64)> {
        let idx = [22u32, 8, 9, 0x12, 0x1f];
        let v = self.ctl_indexed(self.sub(i), C_FB_GET_INFO_V2, &idx, 128)?;
        let free = v[0] as u64;
        let total = v[1] as u64;
        let heap = v[2] as u64;
        Some((total, heap, free, v[3] as u64))
    }

    /// Processes : 0x2080018d (liste par classe de canal) puis 0x2080018e
    /// (entries de 72 o ; memPrivate@16, memSharedOwned@24, memSharedDuped@32,
    /// protected@40/48/56, GI@64, CI@68).
    pub fn processes(&self, i: usize) -> Vec<Proc> {
        let sub = self.sub(i);
        let mut out: Vec<Proc> = Vec::new();
        for (k, class) in [PID_CLASS_COMPUTE, PID_CLASS_GRAPHICS].iter().enumerate() {
            let mut pb = vec![0u8; 3812];
            wr32(&mut pb, 0, 0); // idType = CLASS
            wr32(&mut pb, 4, *class);
            if self.ctl(sub, C_GPU_GET_PIDS, &mut pb, 3812).is_err() {
                continue;
            }
            let mut np = rd32(&pb, 8) as usize;
            if np > 950 {
                np = 950;
            }
            if np == 0 {
                continue;
            }
            let pids: Vec<u32> = (0..np).map(|j| rd32(&pb, 12 + j * 4)).collect();
            let mut ib = vec![0u8; 14408];
            wr32(&mut ib, 0, np as u32);
            for (j, pid) in pids.iter().enumerate() {
                wr32(&mut ib, 8 + 72 * j, *pid);
                wr32(&mut ib, 12 + 72 * j, 0); // index = VIDEO_MEMORY_USAGE
            }
            if self.ctl(sub, C_GPU_GET_PID_INFO, &mut ib, 14408).is_err() {
                continue;
            }
            for (j, &pid) in pids.iter().enumerate() {
                let e = 8 + 72 * j;
                if rd32(&ib, e + 8) != 0 {
                    continue; // result != NV_OK
                }
                let priv_m = rd64(&ib, e + 16);
                let shared = rd64(&ib, e + 24);
                let duped = rd64(&ib, e + 32);
                let prot = rd64(&ib, e + 40) + rd64(&ib, e + 48) + rd64(&ib, e + 56);
                let gi = rd32(&ib, e + 64);
                let ci = rd32(&ib, e + 68);
                if let Some(p) = out.iter_mut().find(|p| p.pid == pid) {
                    if k == 0 {
                        p.compute = true;
                    } else {
                        p.graphics = true;
                    }
                    p.mem_private += priv_m;
                    p.mem_shared_owned += shared;
                    continue;
                }
                out.push(Proc {
                    pid,
                    compute: k == 0,
                    graphics: k == 1,
                    mem_private: priv_m,
                    mem_shared_owned: shared,
                    mem_shared_duped: duped,
                    mem_protected: prot,
                    gi,
                    ci,
                });
            }
        }
        out.sort_by_key(|p| std::cmp::Reverse(p.mem_private + p.mem_shared_owned));
        out
    }
}

#[derive(Default, Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub compute: bool,
    pub graphics: bool,
    pub mem_private: u64,
    pub mem_shared_owned: u64,
    pub mem_shared_duped: u64,
    pub mem_protected: u64,
    pub gi: u32,
    pub ci: u32,
}

impl Proc {
    pub fn total_mem(&self) -> u64 {
        self.mem_private + self.mem_shared_owned
    }
    pub fn kind(&self) -> String {
        match (self.compute, self.graphics) {
            (true, true) => "C+G".into(),
            (true, false) => "C".into(),
            (false, true) => "G".into(),
            _ => "?".into(),
        }
    }
    pub fn mig(&self) -> String {
        if self.gi == 0xffff_ffff && self.ci == 0xffff_ffff {
            "-".into()
        } else {
            format!("{}/{:#x}", self.gi, self.ci)
        }
    }
}
