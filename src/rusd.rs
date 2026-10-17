//! rusd.rs - decodage de la page partagee NV00DE_SHARED_DATA (classee 0x00de).
//!
//! Transcription des deplaces de `include/rusd.h` du depot nvidia-smi-clone-RE,
//! recalculees depuis la structure C `NV00DE_SHARED_DATA` du SDK fournit avec le
//! module noyau open source (src/common/sdk/nvidia/inc/class/cl00de.h) et
//! verifiees contre le driver 615.71.09 : la somme fait exactement 3184 octets.
//!
//! Unites (documentees dans docs/REVERSE_ENGINEERING.md §5/§6) :
//!   temperature : SFXP24.8 (i32 en 1/256 de degre C)
//!   puissance   : mW
//!   horloges    : kHz (reel) / MHz (cible)
//!   fan duty    : UFXP16.16
//!   BAR1 / FB   : KiB  (NVML arrondit les MiB VERS LE HAUT)
//!   P-state     : MASQUE de bits, pas un index (4 => P2)

use crate::nvrm::Session;

pub const RUSD_BAR1: usize = 0;
pub const RUSD_PMA: usize = 16;
pub const RUSD_SHADOW_ERR: usize = 40;
pub const RUSD_GR_INFO: usize = 56;
pub const RUSD_CLK: usize = 72;
pub const RUSD_THROTTLE: usize = 112;
pub const RUSD_UTIL: usize = 1992;
pub const RUSD_ECC: usize = 2040;
pub const RUSD_PSTATE: usize = 2280;
pub const RUSD_POWER_LIMIT: usize = 2296;
pub const RUSD_TEMP: usize = 2312;
pub const RUSD_ROW_REMAP: usize = 2392;
pub const RUSD_BANK_REMAP: usize = 2440;
pub const RUSD_AVG_POWER: usize = 2472;
pub const RUSD_INST_POWER: usize = 2496;
pub const RUSD_POWER_POLICY: usize = 2520;
pub const RUSD_PCIE: usize = 2536;
pub const RUSD_FAN: usize = 2584;
pub const RUSD_PROC_UTIL: usize = 2656;

pub const TEMP_GPU: usize = 0;
pub const TEMP_MEM: usize = 1;
pub const TEMP_BOARD: usize = 2;
pub const TEMP_PSU: usize = 3;
pub const TEMP_HBM: usize = 4;
pub const TEMP_NAMES: [&str; 5] = ["GPU", "Memory (GDDR/HBM)", "Board", "Power supply", "HBM"];

pub const CLK_GRAPHICS: usize = 0;
pub const CLK_MEMORY: usize = 1;
pub const CLK_VIDEO: usize = 2;
pub const CLK_SM: usize = 3;
pub const CLK_NAMES: [&str; 4] = ["GRAPHICS", "MEMORY", "VIDEO", "SM"];

pub const VIOLATION_NAMES: [&str; 6] = [
    "max clock",
    "turbo boost",
    "3D boost",
    "rated TDP",
    "customer boost",
    "display clock",
];

/// RUSD_CLK_THROTTLE_REASON_* (cl00de.h) — le meme masue que
/// clocks_event_reasons.active de nvidia-smi.
pub const THROTTLE_REASONS: [(u32, &str, &str); 11] = [
    (1 << 0, "GPU Idle", "inactif"),
    (1 << 1, "Application Clocks Setting", "application clocks"),
    (1 << 2, "SW Power Cap", "software power cap"),
    (1 << 3, "HW Slowdown", "hardware slowdown"),
    (1 << 4, "Sync Boost", "sync boost"),
    (1 << 5, "SW Thermal Slowdown", "software thermal"),
    (1 << 6, "HW Thermal Slowdown", "hardware thermal"),
    (1 << 7, "HW Power Brake Slowdown", "hardware power brake"),
    (1 << 8, "Display Clock Setting", "display clock"),
    (1 << 9, "Board Limit", "board limit"),
    (1 << 10, "Reliability", "fiabilite (ECC/ratonnage)"),
];

#[derive(Default, Clone, Debug)]
pub struct EngUtil {
    pub busy: u32,
    pub period_us: u32,
}

#[derive(Default, Clone, Debug)]
pub struct ProcUtilSample {
    pub ts: u64,
    /// 6 canaux : FB, GR, NVENC, NVDEC, NVJPG, NVOFA
    pub eng: [(u32, u32); 6], // (util*100, pid)
}

#[derive(Default, Clone, Debug)]
pub struct EccDetail {
    pub sram_corr_unique: u64,
    pub sram_unc_parity_unique: u64,
    pub sram_unc_secded_unique: u64,
    pub dram_corr_total: u64,
    pub dram_unc_total: u64,
    pub sram_bucket_l2: u64,
    pub sram_bucket_sm: u64,
    pub sram_bucket_pcie: u64,
    pub sram_bucket_fw: u64,
    pub sram_bucket_other: u64,
    pub last_cleared: u32,
    pub threshold_exceeded: bool,
}

#[derive(Default, Clone, Debug)]
pub struct Rusd {
    pub ok: bool,
    pub bar1_total_kib: Option<u32>,
    pub bar1_free_kib: Option<u32>,
    pub pma_total: Option<u64>,
    pub pma_free: Option<u64>,
    pub shadow_err_cont: Option<u32>,
    pub ctxsw_logging: Option<bool>,
    pub clk_target_mhz: [Option<u32>; 4],
    pub clk_actual_mhz: [Option<f64>; 4],
    pub reason_mask: Option<u32>,
    pub reference_time_ns: Option<u64>,
    pub supported_violation_mask: Option<u32>,
    /// temps cumulés (ns) par point de performance, pour le slot global
    pub violation_ns: [Option<u64>; 6],
    /// masque de perf-point du slot global + des 4 premiers slots par-engine
    pub violation_slots: Vec<(u32, [Option<u64>; 6])>,
    pub util_gpu: Option<u32>,
    pub util_mem: Option<u32>,
    pub eng: [Option<EngUtil>; 4],
    pub pstate_mask: Option<u32>,
    pub limit_requested_mw: Option<u32>,
    pub limit_enforced_mw: Option<u32>,
    pub temp_c: [Option<f64>; 5],
    pub row_hist: [Option<u32>; 5],
    pub row_corr: Option<u32>,
    pub row_uncorr: Option<u32>,
    pub row_corr_inactive: Option<u32>,
    pub row_uncorr_inactive: Option<u32>,
    pub row_pending: Option<bool>,
    pub row_failure_occurred: Option<bool>,
    pub bank_max_spare: Option<u32>,
    pub bank_no_spare: Option<u32>,
    pub bank_pending: Option<bool>,
    pub bank_active: Option<u32>,
    pub bank_inactive: Option<u32>,
    pub avg_power_mw: Option<u32>,
    pub avg_module_mw: Option<u32>,
    pub avg_memory_mw: Option<u32>,
    pub inst_power_mw: Option<u32>,
    pub inst_module_mw: Option<u32>,
    pub inst_cpu_mw: Option<u32>,
    pub tgpm_mw: Option<u32>,
    pub pcie: [Option<u32>; 9],
    pub fan_rpm: Vec<u32>,
    pub ecc_group: [[Option<u64>; 4]; 3],
    pub ecc_detail: Option<EccDetail>,
    pub proc_util: Vec<ProcUtilSample>,
}

/// SFXP24.8 -> degres C a vec une decimale (le depot C arrondit a l'entier).
#[inline]
pub fn sfxp24_8_to_c(raw: i32) -> f64 {
    raw as f64 / 256.0
}

/// P-state : le champ est un MASQUE (NV2080_CTRL_PERF_PSTATES_P0..P15).
#[inline]
pub fn pstate_from_mask(mask: u32) -> Option<u32> {
    if mask == 0 {
        return None;
    }
    Some(mask.trailing_zeros())
}

pub const PCIE_COUNTER_NAMES: [&str; 8] = [
    "line-code errors",
    "CRC errors",
    "NAK recus",
    "failed L0S exits",
    "correctable errors",
    "non-fatal errors",
    "fatal errors",
    "unsupported requests",
];

pub const PROC_ENGINES: [&str; 6] = ["FB", "GR", "NVENC", "NVDEC", "NVJPG", "NVOFA"];

/// Extraction complete d'une frame RUSD. Un seul ioctl (0xde0001) a precede :
/// tout le reste est de la lecture memoire.
pub fn read(s: &Session, i: usize) -> Rusd {
    let mut r = Rusd::default();
    if s.gpus.get(i).map(|g| g.rusd.is_null()).unwrap_or(true) {
        return r;
    }
    r.ok = true;

    r.bar1_total_kib = s.rusd::<u32>(i, RUSD_BAR1, RUSD_BAR1 + 8);
    r.bar1_free_kib = s.rusd::<u32>(i, RUSD_BAR1, RUSD_BAR1 + 12);
    r.pma_total = s.rusd::<u64>(i, RUSD_PMA, RUSD_PMA + 8);
    r.pma_free = s.rusd::<u64>(i, RUSD_PMA, RUSD_PMA + 16);
    r.shadow_err_cont = s.rusd::<u32>(i, RUSD_SHADOW_ERR, RUSD_SHADOW_ERR + 8);
    r.ctxsw_logging = s.rusd::<u8>(i, RUSD_GR_INFO, RUSD_GR_INFO + 8).map(|v| v != 0);

    for d in 0..4 {
        let base = RUSD_CLK + 8 + d * 8;
        r.clk_target_mhz[d] = s.rusd::<u32>(i, RUSD_CLK, base);
        r.clk_actual_mhz[d] = s.rusd::<u32>(i, RUSD_CLK, base + 4).map(|khz| khz as f64 / 1000.0);
    }

    r.reason_mask = s.rusd::<u32>(i, RUSD_THROTTLE, RUSD_THROTTLE + 8);
    r.reference_time_ns = s.rusd::<u64>(i, RUSD_THROTTLE, RUSD_THROTTLE + 16);
    r.supported_violation_mask = s.rusd::<u32>(i, RUSD_THROTTLE, RUSD_THROTTLE + 24);
    // RUSD_CLK_VIOLATION_STATUS = { u32 perfPointMask; pad; NvU64 timeNs[6] } = 56 o
    // globalViolation @112+1824. Sans garde-fou seqlock (cf. commentaire module).
    let gv = RUSD_THROTTLE + 1824;
    if let Some(mask) = s.rusd_raw::<u32>(i, gv) {
        r.violation_slots.push((mask, std::array::from_fn(|k| s.rusd_raw::<u64>(i, gv + 8 + k * 8))));
    }
    r.violation_ns = match r.violation_slots.first() {
        Some((_, t)) => t.clone(),
        None => Default::default(),
    };
    for slot in 0..8usize {
        let o = RUSD_THROTTLE + 32 + slot * 56;
        if let Some(mask) = s.rusd_raw::<u32>(i, o) {
            if mask != 0 {
                r.violation_slots.push((mask, std::array::from_fn(|k| s.rusd_raw::<u64>(i, o + 8 + k * 8))));
            }
        }
    }

    r.util_gpu = s.rusd::<u32>(i, RUSD_UTIL, RUSD_UTIL + 8);
    r.util_mem = s.rusd::<u32>(i, RUSD_UTIL, RUSD_UTIL + 12);
    for e in 0..4 {
        let o = RUSD_UTIL + 16 + e * 8;
        if let (Some(b), Some(p)) = (
            s.rusd::<u32>(i, RUSD_UTIL, o),
            s.rusd::<u32>(i, RUSD_UTIL, o + 4),
        ) {
            r.eng[e] = Some(EngUtil { busy: b, period_us: p });
        }
    }

    r.pstate_mask = s.rusd::<u32>(i, RUSD_PSTATE, RUSD_PSTATE + 8);
    r.limit_requested_mw = s.rusd::<u32>(i, RUSD_POWER_LIMIT, RUSD_POWER_LIMIT + 8);
    r.limit_enforced_mw = s.rusd::<u32>(i, RUSD_POWER_LIMIT, RUSD_POWER_LIMIT + 12);

    for t in 0..5 {
        r.temp_c[t] = s
            .rusd::<i32>(i, RUSD_TEMP + t * 16, RUSD_TEMP + t * 16 + 8)
            .map(sfxp24_8_to_c);
    }

    for h in 0..5 {
        r.row_hist[h] = s.rusd::<u32>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 8 + h * 4);
    }
    r.row_corr = s.rusd::<u32>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 28);
    r.row_uncorr = s.rusd::<u32>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 32);
    r.row_corr_inactive = s.rusd::<u32>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 36);
    r.row_uncorr_inactive = s.rusd::<u32>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 40);
    r.row_pending = s.rusd::<u8>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 44).map(|v| v != 0);
    r.row_failure_occurred = s.rusd::<u8>(i, RUSD_ROW_REMAP, RUSD_ROW_REMAP + 45).map(|v| v != 0);

    r.bank_max_spare = s.rusd::<u32>(i, RUSD_BANK_REMAP, RUSD_BANK_REMAP + 8);
    r.bank_no_spare = s.rusd::<u32>(i, RUSD_BANK_REMAP, RUSD_BANK_REMAP + 12);
    r.bank_pending = s.rusd::<u8>(i, RUSD_BANK_REMAP, RUSD_BANK_REMAP + 16).map(|v| v != 0);
    r.bank_active = s.rusd::<u32>(i, RUSD_BANK_REMAP, RUSD_BANK_REMAP + 20);
    r.bank_inactive = s.rusd::<u32>(i, RUSD_BANK_REMAP, RUSD_BANK_REMAP + 24);

    r.avg_power_mw = s.rusd::<u32>(i, RUSD_AVG_POWER, RUSD_AVG_POWER + 8);
    r.avg_module_mw = s.rusd::<u32>(i, RUSD_AVG_POWER, RUSD_AVG_POWER + 12);
    r.avg_memory_mw = s.rusd::<u32>(i, RUSD_AVG_POWER, RUSD_AVG_POWER + 16);
    r.inst_power_mw = s.rusd::<u32>(i, RUSD_INST_POWER, RUSD_INST_POWER + 8);
    r.inst_module_mw = s.rusd::<u32>(i, RUSD_INST_POWER, RUSD_INST_POWER + 12);
    r.inst_cpu_mw = s.rusd::<u32>(i, RUSD_INST_POWER, RUSD_INST_POWER + 16);
    r.tgpm_mw = s.rusd::<u32>(i, RUSD_POWER_POLICY, RUSD_POWER_POLICY + 8);

    for p in 0..9 {
        r.pcie[p] = s.rusd::<u32>(i, RUSD_PCIE, RUSD_PCIE + 8 + p * 4);
    }
    r.fan_rpm = (0..16)
        .map(|f| s.rusd::<u32>(i, RUSD_FAN, RUSD_FAN + 8 + f * 4))
        .collect::<Option<Vec<u32>>>()
        .unwrap_or_default();

    for g in 0..3 {
        for c in 0..4 {
            r.ecc_group[g][c] = s.rusd::<u64>(i, RUSD_ECC, RUSD_ECC + 8 + g * 32 + c * 8);
        }
    }
    // RUSD_ECC_INFO : volatileCounts (5 x u64) puis clientExposedCounts
    if s.rusd::<u64>(i, RUSD_ECC, RUSD_ECC + 8 + 96).is_some()
        && s.rusd::<u64>(i, RUSD_ECC, RUSD_ECC + 8 + 96 + 40).is_some()
    {
        let base = RUSD_ECC + 8 + 96;
        let g64 = |k: usize| s.rusd::<u64>(i, RUSD_ECC, base + k * 8).unwrap_or(0);
        r.ecc_detail = Some(EccDetail {
            sram_corr_unique: g64(0),
            sram_unc_parity_unique: g64(1),
            sram_unc_secded_unique: g64(2),
            dram_corr_total: g64(3),
            dram_unc_total: g64(4),
            // NV2080_CTRL_ECC_GET_CLIENT_EXPOSED_COUNTERS_PARAMS @ base+40 :
            // 5 x u64, u32 lastClearedTimestamp, 5 x u64 (buckets), NvBool
            sram_bucket_l2: g64(11),
            sram_bucket_sm: g64(12),
            sram_bucket_pcie: g64(13),
            sram_bucket_fw: g64(14),
            sram_bucket_other: g64(15),
            last_cleared: s.rusd::<u32>(i, RUSD_ECC, base + 8 * 5 + 40).unwrap_or(0),
            threshold_exceeded: s
                .rusd::<u8>(i, RUSD_ECC, base + 8 * 5 + 40 + 4 + 4 + 8 * 5)
                .unwrap_or(0)
                != 0,
        });
    }

    // Anneau de 5 samples (POLL_PROC_UTIL) : 104 o par echantillon
    for k in 0..5usize {
        let base = RUSD_PROC_UTIL + 8 + k * 104;
        if let Some(ts) = s.rusd::<u64>(i, RUSD_PROC_UTIL, base) {
            let mut eng = [(0u32, 0u32); 6];
            for e in 0..6 {
                let o = base + 8 + e * 16;
                let util = s.rusd::<u32>(i, RUSD_PROC_UTIL, o).unwrap_or(0);
                let pid = s.rusd::<u32>(i, RUSD_PROC_UTIL, o + 8).unwrap_or(0xffff_ffff);
                eng[e] = (util, pid);
            }
            r.proc_util.push(ProcUtilSample { ts, eng });
        }
    }
    r
}
