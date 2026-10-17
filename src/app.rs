//! app.rs - etat, echantillonnage, modeles de donnees du moniteur.
//!
//! Philosophie heritee du depot nvidia-smi-clone-RE :
//!   * le STATIQUE (identite, EEPROM plaque, caps, horloges max, BARs, config
//!     space PCI, procfs) est collecte UNE seule fois au demarrage ;
//!   * le DYNAMIQUE coute un ioctl de telemetry par GPU et par frame
//!     (0xde0001 REQUEST_DATA_POLL) plus quelques controles (memoire FB, duty
//!     ventilateur, limite de conso, pstate, liste des process) ;
//!   * tout le reste est de la lecture memoire dans la page RUSD mappee.
//! A 2 Hz, cela reste largement moins cher qu'un `fork()+exec` de nvidia-smi,
//! et il n'y a jamais de libnvidia-ml dans le processus.

use crate::gpu::{self, Proc, Static};
use crate::nvrm::{self, Session};
use crate::rusd::Rusd;
use crate::sysinfo::{self, Pci, ProcInfo};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Overview,
    Identity,
    Memory,
    Power,
    Pcie,
    Health,
    Driver,
}

pub const VIEWS: [View; 7] = [
    View::Overview,
    View::Identity,
    View::Memory,
    View::Power,
    View::Pcie,
    View::Health,
    View::Driver,
];

impl View {
    pub fn next(self) -> View {
        VIEWS[(self as usize + 1) % VIEWS.len()]
    }
    pub fn title(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Identity => "identity",
            View::Memory => "memory",
            View::Power => "power & thermal",
            View::Pcie => "PCI express bus",
            View::Health => "health & ECC",
            View::Driver => "driver & system",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Mem,
    Pid,
    Cpu,
    Name,
}

#[derive(Clone, Copy)]
pub struct HistPoint {
    pub util: f64,
    pub mem: f64,
    pub power: f64,
    pub temp: f64,
}

pub struct History {
    pub cap: usize,
    pub pts: VecDeque<HistPoint>,
}

impl History {
    pub fn new(cap: usize) -> History {
        History { cap, pts: VecDeque::with_capacity(cap) }
    }
    pub fn push(&mut self, p: HistPoint) {
        if self.pts.len() == self.cap {
            self.pts.pop_front();
        }
        self.pts.push_back(p);
    }
}

#[derive(Clone, Default)]
pub struct ProcRow {
    pub pid: u32,
    pub kind: String,
    pub mig: String,
    pub mem_mib: u64,
    pub mem_detail: String,
    pub cmd: String,
    pub exe: String,
    pub user: String,
    pub cpu: f64,
    pub rss_mib: u64,
    pub threads: u32,
    pub core: u32,
    pub engines: Vec<(String, f64)>,
}

pub struct GpuFrame {
    pub rusd: Rusd,
    /// KiB : (total_ram, heap_size, heap_free, plus_grande_region_libre)
    pub mem: Option<(u64, u64, u64, u64)>,
    pub fan_pct: Option<f64>,
    pub limit_mw: Option<u32>,
    pub pstate_direct: Option<u32>,
    pub powerstate: Option<u32>,
    pub rated: Option<(u32, u32)>,
    pub procs: Vec<ProcRow>,
    /// % du dernier intervalle passe plafonne a chaque point de performance
    pub viol_pct: [Option<f64>; 6],
    pub ioctls: u32,
    pub took_ms: f64,
    pub at: f64,
}

pub struct GpuState {
    pub st: Static,
    pub pci: Option<Pci>,
    pub information: Vec<(String, String)>,
    pub power_procfs: Vec<(String, String)>,
    pub hist: History,
    pub prev_viol: [u64; 6],
    pub frame: Option<GpuFrame>,
    pub rusd_ok: bool,
}

pub struct Host {
    pub name: String,
    pub kernel: String,
    pub cpu: String,
    pub nproc: usize,
    pub nvidia_version: String,
    pub params: Vec<(String, String)>,
    pub mem_total_kib: u64,
    pub mem_avail_kib: u64,
    pub swap_total_kib: u64,
}

pub struct App {
    pub s: Session,
    pub gpus: Vec<GpuState>,
    pub focus: usize,
    pub view: View,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub scroll: usize,
    pub paused: bool,
    pub mask: bool,
    pub cuda: Option<String>,
    pub users: HashMap<u32, String>,
    pub pci_ids: sysinfo::PciIds,
    pub prev_cpu: HashMap<u32, (u64, f64)>,
    pub clock: Instant,
    pub frames_done: u64,
    pub last_dt: f64,
    pub host: Host,
}

impl App {
    pub fn new(mask: bool, with_cuda: bool, cap: usize) -> std::io::Result<App> {
        let mut s = Session::open()?;
        let mut gpus = Vec::new();
        for i in 0..s.gpus.len() {
            let rusd_ok = s.enable_rusd(i, nvrm::RUSD_POLL_ALL);
            let st = s.gpu_static(i);
            let pci = sysinfo::read_pci(&st.bus_id);
            let information = sysinfo::gpu_information(&st.bus_id);
            let power_procfs = sysinfo::gpu_power_procfs(&st.bus_id);
            let st = fill_static(st.clone(), &information);
            gpus.push(GpuState {
                st,
                pci,
                information,
                power_procfs,
                hist: History::new(cap),
                prev_viol: [0; 6],
                frame: None,
                rusd_ok,
            });
        }
        let (mem_total, mem_avail, swap_total) = sysinfo::meminfo();
        let host = Host {
            name: sysinfo::hostname(),
            kernel: sysinfo::kernel_release(),
            cpu: sysinfo::cpu_model(),
            nproc: sysinfo::nproc(),
            nvidia_version: sysinfo::nvidia_version_line(),
            params: sysinfo::driver_params(),
            mem_total_kib: mem_total,
            mem_avail_kib: mem_avail,
            swap_total_kib: swap_total,
        };
        let mut app = App {
            s,
            gpus,
            focus: 0,
            view: View::Overview,
            sort: SortKey::Mem,
            sort_desc: true,
            filter: String::new(),
            scroll: 0,
            paused: false,
            mask,
            cuda: if with_cuda { crate::ffi::cuda_umd_version() } else { None },
            users: sysinfo::uid_names(),
            pci_ids: sysinfo::PciIds::load(),
            prev_cpu: HashMap::new(),
            clock: Instant::now(),
            frames_done: 0,
            last_dt: 0.0,
            host,
        };
        app.sample();
        app.sample(); // deuxieme passe : deltas de throttling + CPU% disponibles
        Ok(app)
    }

    /// Un cycle de mesure pour tous les GPU.
    pub fn sample(&mut self) {
        let t0 = Instant::now();
        let span = self.last_dt.max(t0.elapsed().as_secs_f64());
        let now = self.clock.elapsed().as_secs_f64();
        for i in 0..self.gpus.len() {
            if self.gpus[i].rusd_ok {
                self.s.rusd_poll(i, nvrm::RUSD_POLL_ALL);
            }
            let mut ioctls = 1u32;
            let rusd = crate::rusd::read(&self.s, i);
            let mem = self.s.mem_state(i);
            let fan_pct = self.s.fan_duty(i);
            let limit_mw = self.s.power_limit_mw(i);
            let pstate_direct = self.s.pstate_direct(i);
            let powerstate = self.s.powerstate(i);
            let rated = self.s.rated_tdp(i);
            let raw: Vec<Proc> = self.s.processes(i);
            ioctls += 7 + 2; // controls + GET_PIDS/GET_PID_INFO (jusqu'a 2 classes)

            // Duree de throttling : le driver publie un cumul en ns ; on en tire
            // le pourcentage du dernier intervalle (docs/FINDINGS.md §3).
            let mut viol_pct = [None; 6];
            let prev = self.gpus[i].prev_viol;
            let cur: [u64; 6] = std::array::from_fn(|k| rusd.violation_ns[k].unwrap_or(0));
            if span > 0.05 {
                for k in 0..6 {
                    let d = cur[k].saturating_sub(prev[k]) as f64 / 1e9;
                    viol_pct[k] = Some((100.0 * d / span).clamp(0.0, 100.0));
                }
            }
            self.gpus[i].prev_viol = cur;

            // Processus : attribution memoire (RM) + CPU/RSS/user (/proc) +
            // occupation par moteur (RUSD POLL_PROC_UTIL).
            let mut rows: Vec<ProcRow> = Vec::new();
            for p in &raw {
                let info: ProcInfo = sysinfo::proc_info(p.pid, &self.users);
                let cpu = match self.prev_cpu.get(&p.pid) {
                    Some((pj, pt)) => {
                        let dd = now - pt;
                        if dd > 0.05 {
                            100.0 * (info.jiffies.saturating_sub(*pj) as f64 / 100.0) / dd
                        } else {
                            0.0
                        }
                    }
                    None => 0.0,
                };
                self.prev_cpu.insert(p.pid, (info.jiffies, now));
                // anneau POLL_PROC_UTIL : on garde le max par moteur pour ce PID
                let mut engines: Vec<(String, f64)> = Vec::new();
                for (e, name) in crate::rusd::PROC_ENGINES.iter().enumerate() {
                    let mut best = 0.0f64;
                    for sm in rusd.proc_util.iter() {
                        let (util, pid) = sm.eng[e];
                        if pid == p.pid {
                            best = best.max(util as f64 / 100.0);
                        }
                    }
                    if best > 0.0 {
                        engines.push((name.to_string(), best));
                    }
                }
                engines.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                rows.push(ProcRow {
                    pid: p.pid,
                    kind: p.kind(),
                    mig: p.mig(),
                    mem_mib: (p.total_mem() + 1048575) / 1048576,
                    mem_detail: format!(
                        "private {} MiB · shared {} MiB · duped {} MiB · protected {} MiB",
                        p.mem_private / 1048576,
                        p.mem_shared_owned / 1048576,
                        p.mem_shared_duped / 1048576,
                        p.mem_protected / 1048576
                    ),
                    cmd: info.cmdline.clone(),
                    exe: info.exe.clone(),
                    user: info.user.clone(),
                    cpu,
                    rss_mib: info.rss_kib / 1024,
                    threads: info.threads,
                    core: info.processor,
                    engines,
                });
            }
            self.sort_procs(&mut rows);

            // historique (sparklines)
            let total_kib = mem.map(|m| m.0).unwrap_or(0);
            let heap_kib = mem.map(|m| m.1).unwrap_or(0);
            let free_kib = mem.map(|m| m.2).unwrap_or(0);
            let used_mib = if heap_kib > free_kib { gpu::kib2mib(heap_kib - free_kib) } else { 0 };
            let tot_mib = gpu::kib2mib(total_kib);
            self.gpus[i].hist.push(crate::app::HistPoint {
                util: rusd.util_gpu.unwrap_or(0) as f64,
                mem: if tot_mib > 0 { used_mib as f64 / tot_mib as f64 * 100.0 } else { 0.0 },
                power: rusd.avg_power_mw.unwrap_or(0) as f64 / 1000.0,
                temp: rusd.temp_c[0].unwrap_or(0.0),
            });

            self.gpus[i].frame = Some(GpuFrame {
                rusd,
                mem,
                fan_pct,
                limit_mw,
                pstate_direct,
                powerstate,
                rated,
                procs: rows,
                viol_pct,
                ioctls,
                took_ms: t0.elapsed().as_secs_f64() * 1000.0,
                at: now,
            });
        }
        self.last_dt = t0.elapsed().as_secs_f64();
        self.frames_done += 1;
    }

    pub fn sort_procs(&self, rows: &mut Vec<ProcRow>) {
        sort_rows(rows, self.sort, self.sort_desc);
    }

    pub fn filtered(&self, rows: &[ProcRow]) -> Vec<ProcRow> {
        if self.filter.is_empty() {
            return rows.to_vec();
        }
        let f = self.filter.to_lowercase();
        rows.iter()
            .filter(|r| {
                r.cmd.to_lowercase().contains(&f)
                    || r.user.to_lowercase().contains(&f)
                    || r.pid.to_string().contains(&f)
            })
            .cloned()
            .collect()
    }

    // --- masquage (capture publique / logs partages) ------------------------
    pub fn m_serial(&self, v: &str) -> String {
        if !self.mask || v.is_empty() {
            return v.to_string();
        }
        "••••••••".to_string()
    }
    pub fn m_uuid(&self, v: &str) -> String {
        if !self.mask || v.len() < 12 {
            return v.to_string();
        }
        "GPU-••••••••-••••-••••-••••-••••••••••••".to_string()
    }
    pub fn m_host(&self, v: &str) -> String {
        if self.mask {
            "gpucalc-01".to_string()
        } else {
            v.to_string()
        }
    }
    pub fn m_cmd(&self, v: &str) -> String {
        if !self.mask {
            return v.to_string();
        }
        let base = v.split_whitespace().next().unwrap_or(v);
        let name = base.rsplit('/').next().unwrap_or(base);
        format!("/opt/inference/{name}")
    }
    pub fn m_user(&self, v: &str) -> String {
        if !self.mask {
            return v.to_string();
        }
        if v == "root" {
            "root".to_string()
        } else {
            format!("user{}", v.chars().count())
        }
    }

    pub fn totals(&self) -> (f64, f64, f64, usize) {
        let (mut w, mut u, mut m, mut n) = (0.0, 0.0, 0.0, 0usize);
        let mut k = 0usize;
        for g in &self.gpus {
            let Some(f) = &g.frame else { continue };
            k += 1;
            w += f.rusd.avg_power_mw.unwrap_or(0) as f64 / 1000.0;
            u += f.rusd.util_gpu.unwrap_or(0) as f64;
            m += f.rusd.util_mem.unwrap_or(0) as f64;
            n += f.procs.len();
        }
        if k > 0 {
            (w, u / k as f64, m / k as f64, n)
        } else {
            (0.0, 0.0, 0.0, 0)
        }
    }
}

/// Le VBIOS n'est pas dans un controle RM simple (0x20800802 est base sur des
/// pointeurs) : le depot le lit dans le procfs du module noyau, on fait idem.
fn fill_static(mut st: Static, information: &[(String, String)]) -> Static {
    if st.vbios.is_empty() {
        if let Some((_, v)) = information.iter().find(|(k, _)| k == "Video BIOS") {
            st.vbios = v.clone();
        }
    }
    st
}

/// Etat d'une frame, vue par l'afficheur / le JSON.
pub fn pstate_of(g: &GpuState) -> Option<u32> {
    let f = g.frame.as_ref()?;
    rusd_pstate(&f.rusd).or(f.pstate_direct)
}

pub fn rusd_pstate(r: &Rusd) -> Option<u32> {
    crate::rusd::pstate_from_mask(r.pstate_mask.unwrap_or(0))
}

/// Tri explicite des lignes de processus (sans emprunt croise dans le TUI).
pub fn sort_rows(rows: &mut [ProcRow], key: SortKey, desc: bool) {
    rows.sort_by(|a, b| {
        let o = match key {
            SortKey::Mem => a.mem_mib.cmp(&b.mem_mib),
            SortKey::Pid => a.pid.cmp(&b.pid),
            SortKey::Cpu => a.cpu.partial_cmp(&b.cpu).unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Name => a.cmd.cmp(&b.cmd),
        };
        if desc {
            o.reverse()
        } else {
            o
        }
    });
}

/// Resume JSON machine-readable (`nvtop --json`, export Prometheus, jq, etc.)
pub fn json(app: &App) -> String {
    let mut o = String::new();
    let f2 = |v: Option<f64>| v.map(|x| format!("{:.1}", x)).unwrap_or("null".into());
    let u32s = |v: Option<u32>| v.map(|x| x.to_string()).unwrap_or("null".into());
    let u64s = |v: Option<u64>| v.map(|x| x.to_string()).unwrap_or("null".into());
    o.push('{');
    o.push_str(&format!(
        "\"tool\":\"nvtop-rs 0.1.0\",\"unix\":{},\"kmd_version\":{},\"driver_version\":{},\"changelist\":{},",
        sysinfo::now_unix(),
        js(&app.s.kmd_version),
        js(&app.s.driver_version),
        app.s.rm_changelist
    ));
    if let Some(c) = &app.cuda {
        o.push_str(&format!("\"cuda_umd\":{},", js(c)));
    }
    o.push_str(&format!("\"host\":{},\"gpus\":[", js(&app.m_host(&app.host.name))));
    for (i, g) in app.gpus.iter().enumerate() {
        let Some(fr) = &g.frame else { continue };
        if i > 0 {
            o.push(',');
        }
        let r = &fr.rusd;
        o.push('{');
        o.push_str(&format!(
            "\"index\":{},\"name\":{},\"uuid\":{},\"serial\":{},\"bus_id\":{},\"vbios\":{},\"compute_cap\":{},\"codename\":{},",
            i,
            js(&g.st.name),
            js(&g.st.uuid),
            js(&g.st.serial),
            js(&g.st.bus_id),
            js(&g.st.vbios),
            js(&g.st.compute_cap),
            js(&g.st.codename)
        ));
        o.push_str(&format!("\"pstate\":\"P{}\",", pstate_of(g).unwrap_or(0)));
        o.push_str(&format!(
            "\"utilization_pct\":{{\"gpu\":{},\"memory\":{},\"encoder\":{},\"decoder\":{},\"jpeg\":{},\"ofa\":{}}},",
            u32s(r.util_gpu),
            u32s(r.util_mem),
            r.eng[0].as_ref().map(|e| e.busy.to_string()).unwrap_or("null".into()),
            r.eng[1].as_ref().map(|e| e.busy.to_string()).unwrap_or("null".into()),
            r.eng[2].as_ref().map(|e| e.busy.to_string()).unwrap_or("null".into()),
            r.eng[3].as_ref().map(|e| e.busy.to_string()).unwrap_or("null".into())
        ));
        o.push_str(&format!(
            "\"temperature_c\":{{\"gpu\":{},\"memory\":{},\"board\":{},\"powersupply\":{},\"hbm\":{}}},",
            f2(r.temp_c[0]),
            f2(r.temp_c[1]),
            f2(r.temp_c[2]),
            f2(r.temp_c[3]),
            f2(r.temp_c[4])
        ));
        if let Some((tot, heap, free, lf)) = fr.mem {
            o.push_str(&format!(
                "\"memory_mib\":{{\"total\":{},\"used\":{},\"free\":{},\"reserved\":{},\"largest_free\":{}}},",
                gpu::kib2mib(tot),
                gpu::kib2mib(heap.saturating_sub(free)),
                gpu::kib2mib(free),
                gpu::kib2mib(tot.saturating_sub(heap)),
                gpu::kib2mib(lf)
            ));
        }
        o.push_str(&format!(
            "\"bar1_mib\":{{\"total\":{},\"free\":{}}},\"pma_bytes\":{{\"total\":{},\"free\":{}}},",
            u64s(r.bar1_total_kib.map(|k| gpu::kib2mib(k as u64))),
            u64s(r.bar1_free_kib.map(|k| gpu::kib2mib(k as u64))),
            u64s(r.pma_total),
            u64s(r.pma_free)
        ));
        o.push_str(&format!(
            "\"power_w\":{{\"average\":{},\"instant\":{},\"module\":{},\"memory\":{},\"tgpm\":{},\"limit\":{},\"default_limit\":{}}},",
            f2(r.avg_power_mw.map(|v| v as f64 / 1000.0)),
            f2(r.inst_power_mw.map(|v| v as f64 / 1000.0)),
            f2(r.avg_module_mw.map(|v| v as f64 / 1000.0)),
            f2(r.avg_memory_mw.map(|v| v as f64 / 1000.0)),
            f2(r.tgpm_mw.map(|v| v as f64 / 1000.0)),
            f2(fr.limit_mw.map(|v| v as f64 / 1000.0)),
            f2(g.st.power_default_mw.map(|v| v as f64 / 1000.0))
        ));
        o.push_str(&format!(
            "\"clocks_mhz\":{{\"graphics\":{},\"memory\":{},\"video\":{},\"sm\":{},\"max_graphics\":{},\"target_graphics\":{}}},",
            f2(r.clk_actual_mhz[0]),
            f2(r.clk_actual_mhz[1]),
            f2(r.clk_actual_mhz[2]),
            f2(r.clk_actual_mhz[3].filter(|v| *v > 0.0).or(r.clk_actual_mhz[0])),
            u32s(g.st.clk_max_gfx_mhz),
            u32s(r.clk_target_mhz[0])
        ));
        o.push_str(&format!(
            "\"fan\":{{\"pct\":{},\"rpm\":{}}},",
            f2(fr.fan_pct),
            if r.fan_rpm.is_empty() { "null".into() } else { format!("[{}]", r.fan_rpm.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")) }
        ));
        o.push_str(&format!(
            "\"pcie\":{{\"gen_current\":{},\"gen_max\":{},\"width_current\":{},\"width_max\":{},\"counters\":[{}]}},",
            g.pci.as_ref().map(|p| p.gen_cur).unwrap_or(0),
            g.pci.as_ref().map(|p| p.gen_max).unwrap_or(0),
            g.pci.as_ref().map(|p| p.width_cur).unwrap_or(0),
            g.pci.as_ref().map(|p| p.width_max).unwrap_or(0),
            r.pcie[1..]
                .iter()
                .map(|v| u32s(*v))
                .collect::<Vec<_>>()
                .join(",")
        ));
        o.push_str(&format!(
            "\"throttle\":{{\"mask\":\"{:#018x}\",\"reasons\":[{}],\"cumulative_s\":[{}],\"interval_pct\":[{}]}},",
            r.reason_mask.unwrap_or(0),
            r.reason_mask
                .map(|m| crate::rusd::THROTTLE_REASONS
                    .iter()
                    .filter(|(b, _, _)| m & b != 0)
                    .map(|(_, n, _)| js(n))
                    .collect::<Vec<_>>()
                    .join(","))
                .unwrap_or_default(),
            r.violation_ns
                .iter()
                .map(|v| format!("{:.3}", v.unwrap_or(0) as f64 / 1e9))
                .collect::<Vec<_>>()
                .join(","),
            fr.viol_pct
                .iter()
                .map(|v| f2(*v))
                .collect::<Vec<_>>()
                .join(",")
        ));
        o.push_str(&format!(
            "\"ecc\":{{\"mode\":{},\"groups\":[[{},{},{},{}],[{},{},{},{}],[{},{},{},{}]]}},",
            js(&g.st.ecc_enabled.map(|e| if e { "enabled" } else { "disabled" }).unwrap_or("unknown").to_string()),
            u64s(r.ecc_group[0][0]), u64s(r.ecc_group[0][1]), u64s(r.ecc_group[0][2]), u64s(r.ecc_group[0][3]),
            u64s(r.ecc_group[1][0]), u64s(r.ecc_group[1][1]), u64s(r.ecc_group[1][2]), u64s(r.ecc_group[1][3]),
            u64s(r.ecc_group[2][0]), u64s(r.ecc_group[2][1]), u64s(r.ecc_group[2][2]), u64s(r.ecc_group[2][3])
        ));
        o.push_str(&format!(
            "\"row_remap\":{{\"correctable\":{},\"uncorrectable\":{},\"pending\":{}}},",
            u32s(r.row_corr),
            u32s(r.row_uncorr),
            r.row_pending.map(|b| b.to_string()).unwrap_or("null".into())
        ));
        o.push_str("\"processes\":[");
        for (k, p) in fr.procs.iter().enumerate() {
            if k > 0 {
                o.push(',');
            }
            o.push_str(&format!(
                "{{\"pid\":{},\"type\":{},\"used_memory_mib\":{},\"user\":{},\"cpu_pct\":{:.2},\"rss_mib\":{},\"cmd\":{}}}",
                p.pid,
                js(&p.kind),
                p.mem_mib,
                js(&app.m_user(&p.user)),
                p.cpu,
                p.rss_mib,
                js(&app.m_cmd(&p.cmd))
            ));
        }
        o.push_str("],\"sample_cost_ms\":");
        o.push_str(&format!("{:.2}", fr.took_ms));
        o.push('}');
    }
    o.push_str("]}");
    o
}

fn js(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
