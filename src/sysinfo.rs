//! sysinfo.rs - les sources « hors ioctl » : config space PCI, sysfs, procfs.
//!
//! Le depot nvidia-smi-clone-RE utilise deja /sys/bus/pci/.../config (lien PCIe),
//! /proc/driver/nvidia/gpus/<bdf>/information (VBIOS) et /proc/<pid>/cmdline
//! (docs/REVERSE_ENGINEERING.md §10). On exploite ces canaux a fond : BARs, AER,
//! ASPM, NUMA, affinite CPU, pilotes, memoire systeme, comptes Unix.

use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default, Clone, Debug)]
pub struct Bar {
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub flags: u64,
}

impl Bar {
    pub fn size(&self) -> u64 {
        if self.end > self.start {
            self.end - self.start + 1
        } else {
            0
        }
    }
    pub fn kind(&self) -> &'static str {
        if self.flags & 0x1 != 0 {
            "IO"
        } else if self.flags & 0x8 != 0 {
            "mem64"
        } else {
            "mem32"
        }
    }
    pub fn prefetch(&self) -> bool {
        self.flags & 0x4000 != 0
    }
}

#[derive(Default, Clone, Debug)]
pub struct Pci {
    pub bdf: String,
    pub vendor: u16,
    pub device: u16,
    pub subsys_vendor: u16,
    pub subsys_device: u16,
    pub revision: u8,
    pub class: u32,
    pub hdr_type: u8,
    pub gen_cur: u32,
    pub width_cur: u32,
    pub gen_max: u32,
    pub width_max: u32,
    pub lnkctl_aspm: u32,
    pub max_payload_b: u32,
    pub max_read_req_b: u32,
    pub port_type: u32,
    pub driver: String,
    pub numa: String,
    pub cpulist: String,
    pub irq: String,
    pub msi: Vec<String>,
    pub iommu_group: String,
    pub current_link_speed: String,
    pub max_link_speed: String,
    pub power_state: String,
    pub boot_vga: String,
    pub bars: Vec<Bar>,
    pub aer_correctable: Vec<(String, u64)>,
    pub aer_nonfatal: Vec<(String, u64)>,
    pub aer_fatal: Vec<(String, u64)>,
    pub slots: u32,
    pub aux_power: String,
    pub consumer: Vec<String>,
}

fn read_file(p: &str) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

///解析 de l'espace de configuration PCI (4 Ko) : identite, capacites, lien.
pub fn read_pci(bdf: &str) -> Option<Pci> {
    let base = format!("/sys/bus/pci/devices/{bdf}");
    let cfg = fs::read(format!("{base}/config")).ok()?;
    if cfg.len() < 64 {
        return None;
    }
    let mut p = Pci::default();
    p.bdf = bdf.to_string();
    p.vendor = le16(&cfg, 0x00);
    p.device = le16(&cfg, 0x02);
    p.class = le32(&cfg, 0x08) >> 8;
    p.revision = cfg[0x08];
    p.hdr_type = cfg[0x0e];
    if cfg.len() >= 0x3c {
        p.irq = le16(&cfg, 0x3c).to_string();
    }
    if cfg.len() >= 0x34 {
        p.subsys_vendor = le16(&cfg, 0x2c);
        p.subsys_device = le16(&cfg, 0x2e);
    }
    // capacites : chaine pointee par le offset @0x34
    let mut off = cfg[0x34] as usize;
    let mut seen = 0;
    let mut pcie_off = 0usize;
    let mut pm_off = 0usize;
    while off >= 0x40 && off + 8 <= cfg.len() && seen < 48 {
        let cid = cfg[off];
        let next = cfg[off + 1];
        match cid {
            0x01 => pm_off = off,
            0x10 => pcie_off = off,
            _ => {}
        }
        if next == 0 || next as usize == off {
            break;
        }
        off = next as usize;
        seen += 1;
    }
    if pcie_off != 0 && pcie_off + 0x20 <= cfg.len() {
        let cap = le32(&cfg, pcie_off + 0x0c);
        let ctl = le16(&cfg, pcie_off + 0x10);
        let sta = le16(&cfg, pcie_off + 0x12);
        p.gen_max = cap & 0xf;
        p.width_max = (cap >> 4) & 0x3f;
        p.gen_cur = (sta & 0xf) as u32;
        p.width_cur = ((sta >> 4) & 0x3f) as u32;
        p.port_type = (le32(&cfg, pcie_off + 0x02) >> 4) & 0xf;
        p.max_payload_b = 128 << (ctl & 7);
        p.lnkctl_aspm = ((ctl >> 6) & 3) as u32;
        p.max_read_req_b = 512 << ((ctl >> 12) & 7);
    }
    if pm_off != 0 && pm_off + 4 <= cfg.len() {
        p.power_state = match le16(&cfg, pm_off + 4) & 3 {
            0 => "D0 (actif)".into(),
            1 => "D1".into(),
            2 => "D2".into(),
            _ => "D3hot".into(),
        };
    }
    // sysfs
    p.driver = fs::read_link(format!("{base}/driver"))
        .ok()
        .and_then(|s| s.file_name().map(|f| f.to_string_lossy().into_owned()))
        .or_else(|| read_file(&format!("{base}/driver")))
        .unwrap_or_default();
    p.numa = read_file(&format!("{base}/numa_node")).unwrap_or_default();
    p.cpulist = read_file(&format!("{base}/local_cpulist")).unwrap_or_default();
    p.irq = read_file(&format!("{base}/irq")).unwrap_or_else(|| p.irq.clone());
    p.current_link_speed = read_file(&format!("{base}/current_link_speed")).unwrap_or_default();
    p.max_link_speed = read_file(&format!("{base}/max_link_speed")).unwrap_or_default();
    p.iommu_group = fs::read_link(format!("{base}/iommu_group"))
        .ok()
        .and_then(|s| s.file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "none (no IOMMU)".into());
    p.boot_vga = read_file(&format!("{base}/boot_vga")).unwrap_or_default();
    if p.power_state.is_empty() {
        p.power_state = read_file(&format!("{base}/power_state")).unwrap_or_default();
    }
    p.aux_power = read_file(&format!("{base}/ari_enabled")).unwrap_or_default();
    p.slots = fs::read_dir(format!("{base}/physical_slots"))
        .map(|d| d.count() as u32)
        .unwrap_or(0);
    if let Ok(d) = fs::read_dir(format!("{base}/msi_irqs")) {
        p.msi = d.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    }
    // BARs : triplets (start, end, flags) par ligne, les 6 premiers sont les BAR
    if let Ok(t) = fs::read_to_string(format!("{base}/resource")) {
        for (i, line) in t.lines().enumerate().take(6) {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 3 {
                continue;
            }
            let start = u64::from_str_radix(f[0].trim_start_matches("0x"), 16).unwrap_or(0);
            let end = u64::from_str_radix(f[1].trim_start_matches("0x"), 16).unwrap_or(0);
            let flags = u64::from_str_radix(f[2].trim_start_matches("0x"), 16).unwrap_or(0);
            if start == 0 && end == 0 {
                continue;
            }
            p.bars.push(Bar { name: format!("BAR{i}"), start, end, flags });
        }
    }
    // compteurs AER (PCIe advanced error reporting) exposes par le noyau
    for (file, dst) in [
        ("aer_dev_correctable", &mut p.aer_correctable),
        ("aer_dev_nonfatal", &mut p.aer_nonfatal),
        ("aer_dev_fatal", &mut p.aer_fatal),
    ] {
        if let Ok(t) = fs::read_to_string(format!("{base}/{file}")) {
            for line in t.lines() {
                let mut it = line.split_whitespace();
                if let (Some(k), Some(v)) = (it.next(), it.next()) {
                    dst.push((k.to_string(), v.parse().unwrap_or(0)));
                }
            }
        }
    }
    // fonctions filles (le GPU expose souvent une fonction audio 0x0403)
    if let Ok(t) = fs::read_to_string(format!("{base}/power/runtime_status")) {
        p.consumer.push(format!("runtime PM: {}", t.trim()));
    }
    for dir in fs::read_dir(&base).ok().into_iter().flatten().flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        if name.starts_with("i2c-") || name.starts_with("drm/") {
            p.consumer.push(name);
        }
    }
    Some(p)
}

/// /proc/driver/nvidia/gpus/<bdf>/information : model, IRQ, VBIOS, UUID,
/// Firmware, DMA mask, Device Minor, GPU Excluded.
pub fn gpu_information(bdf: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(t) = fs::read_to_string(format!("/proc/driver/nvidia/gpus/{bdf}/information")) {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim();
                if !v.is_empty() {
                    out.push((k.trim().to_string(), v.to_string()));
                }
            }
        }
    }
    out
}

pub fn gpu_power_procfs(bdf: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(t) = fs::read_to_string(format!("/proc/driver/nvidia/gpus/{bdf}/power")) {
        let mut section = String::new();
        for line in t.lines() {
            let l = line.trim_end();
            if l.is_empty() {
                continue;
            }
            if let Some((k, v)) = l.split_once(':') {
                let v = v.trim();
                if !v.is_empty() {
                    out.push((if section.is_empty() { k.trim().to_string() } else { format!("{section} {}", k.trim()) }, v.to_string()));
                }
            } else {
                section = l.trim_start().trim_end_matches(':').to_string();
            }
        }
    }
    out
}

/// /proc/driver/nvidia/params : les reglages du module noyau.
pub fn driver_params() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(t) = fs::read_to_string("/proc/driver/nvidia/params") {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once(':') {
                out.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

pub fn nvidia_version_line() -> String {
    fs::read_to_string("/proc/driver/nvidia/version")
        .ok()
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_default()
}

/// /proc/meminfo resume (le GPU preleve aussi de la RAM systeme).
pub fn meminfo() -> (u64, u64, u64) {
    let mut total = 0u64;
    let mut avail = 0u64;
    let mut swap_total = 0u64;
    if let Ok(t) = fs::read_to_string("/proc/meminfo") {
        for line in t.lines() {
            let mut it = line.split_whitespace();
            let k = it.next().unwrap_or("");
            let v: u64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            match k {
                "MemTotal:" => total = v,
                "MemAvailable:" => avail = v,
                "SwapTotal:" => swap_total = v,
                _ => {}
            }
        }
    }
    (total, avail, swap_total)
}

pub fn loadavg() -> (f64, f64, f64) {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .map(|t| {
            let f: Vec<&str> = t.split_whitespace().collect();
            (
                f.first().and_then(|s| s.parse().ok()).unwrap_or(0.0),
                f.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                f.get(2).and_then(|s| s.parse().ok()).unwrap_or(0.0),
            )
        })
        .unwrap_or_default()
}

pub fn uptime_s() -> f64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|t| t.split_whitespace().next().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".into())
}

pub fn kernel_release() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
        })
        .unwrap_or_else(|| "unknown CPU".into())
}

pub fn nproc() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

// ------------------------------------------------------------------ pid

#[derive(Default, Clone, Debug)]
pub struct ProcInfo {
    pub user: String,
    pub cmdline: String,
    pub exe: String,
    pub rss_kib: u64,
    pub jiffies: u64,
    pub threads: u32,
    pub state: char,
    pub nice: i32,
    pub processor: u32,
}

pub fn uid_names() -> HashMap<u32, String> {
    let mut m = HashMap::new();
    if let Ok(t) = fs::read_to_string("/etc/passwd") {
        for line in t.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 3 {
                if let Ok(uid) = f[2].parse::<u32>() {
                    m.insert(uid, f[0].to_string());
                }
            }
        }
    }
    m
}

/// /proc/<pid> : nom complet, utilisateur, RSS, temps CPU, coeur courant.
pub fn proc_info(pid: u32, users: &HashMap<u32, String>) -> ProcInfo {
    let mut p = ProcInfo::default();
    if let Ok(t) = fs::read(format!("/proc/{pid}/cmdline")) {
        let parts: Vec<String> = t
            .split(|&c| c == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        p.cmdline = parts.join(" ");
    }
    if let Ok(s) = fs::read_link(format!("/proc/{pid}/exe")) {
        p.exe = s.to_string_lossy().into_owned();
    }
    if p.cmdline.is_empty() {
        p.cmdline = fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
    }
    if let Ok(t) = fs::read_to_string(format!("/proc/{pid}/status")) {
        for line in t.lines() {
            if let Some((k, v)) = line.split_once(':') {
                let v = v.trim();
                match k {
                    "Uid" | "Gid" if k == "Uid" => {
                        let uid: u32 = v.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(u32::MAX);
                        p.user = users.get(&uid).cloned().unwrap_or_else(|| uid.to_string());
                    }
                    "VmRSS" => p.rss_kib = v.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0),
                    "Threads" => p.threads = v.parse().unwrap_or(0),
                    "State" => p.state = v.chars().next().unwrap_or('?'),
                    _ => {}
                }
            }
        }
    }
    if let Ok(t) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // le champ 2 (comm) peut contenir des espaces : on coupe apres le ')'
        if let Some(rest) = t.rsplit_once(')') {
            let f: Vec<&str> = rest.1.split_whitespace().collect();
            if f.len() > 34 {
                p.state = f[0].chars().next().unwrap_or('?');
                p.processor = f[36].parse().unwrap_or(0);
                let utime: u64 = f[11].parse().unwrap_or(0);
                let stime: u64 = f[12].parse().unwrap_or(0);
                p.jiffies = utime + stime;
            }
        }
    }
    p
}

// ------------------------------------------------------- noms PCI (pci.ids)

pub struct PciIds {
    vendors: HashMap<u16, (String, HashMap<u16, String>)>,
}

impl PciIds {
    pub fn load() -> PciIds {
        let mut vendors: HashMap<u16, (String, HashMap<u16, String>)> = HashMap::new();
        for path in ["/usr/share/misc/pci.ids", "/usr/share/hwdata/pci.ids", "/usr/share/pci.ids"] {
            let Ok(t) = fs::read_to_string(path) else { continue };
            let mut cur: Option<u16> = None;
            for line in t.lines() {
                if line.starts_with('#') || line.trim().is_empty() {
                    continue;
                }
                if !line.starts_with('\t') {
                    // "0001  Intergraph Corporation"
                    let mut it = line.split_once(' ');
                    if let Some((id, name)) = it.take() {
                        if let Ok(v) = u16::from_str_radix(id.trim(), 16) {
                            vendors.insert(v, (name.trim().to_string(), HashMap::new()));
                            cur = Some(v);
                        }
                    }
                } else if line.starts_with("\t\t") {
                    // "\t\t0001  Device"
                    if let Some(v) = cur {
                        let s = line.trim_start();
                        if let Some((id, name)) = s.split_once(' ') {
                            if let Ok(d) = u16::from_str_radix(id.trim(), 16) {
                                if let Some(e) = vendors.get_mut(&v) {
                                    e.1.insert(d, name.trim().to_string());
                                }
                            }
                        }
                    }
                }
            }
            break;
        }
        PciIds { vendors }
    }

    pub fn vendor(&self, v: u16) -> String {
        self.vendors.get(&v).map(|e| e.0.clone()).unwrap_or_else(|| format!("{v:04x}"))
    }

    pub fn device(&self, v: u16, d: u16) -> String {
        self.vendors
            .get(&v)
            .and_then(|e| e.1.get(&d))
            .cloned()
            .unwrap_or_else(|| format!("{d:04x}"))
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn human_bytes(b: f64, unit1024: bool) -> String {
    let base = if unit1024 { 1024.0 } else { 1000.0 };
    let units = if unit1024 {
        ["Ki", "Mi", "Gi", "Ti", "Pi"]
    } else {
        ["k", "M", "G", "T", "P"]
    };
    let mut v = b;
    let mut u = 0usize;
    while v >= base && u + 1 < units.len() {
        v /= base;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0}{}", units[u])
    } else {
        format!("{v:.1}{}", units[u])
    }
}

pub fn duration_hms(mut s: f64) -> String {
    let d = (s / 86400.0) as u64;
    s -= d as f64 * 86400.0;
    let h = (s / 3600.0) as u64;
    s -= h as f64 * 3600.0;
    let m = (s / 60.0) as u64;
    let sec = s - m as f64 * 60.0;
    if d > 0 {
        format!("{d}j {h:02}:{m:02}:{sec:04.1}")
    } else {
        format!("{h}:{m:02}:{sec:05.2}")
    }
}
