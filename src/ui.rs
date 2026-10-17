//! ui.rs - rendu du moniteur.
//!
//! Toute frame est construite comme une liste de lignes de spans (texte +
//! couleur + gras) puis serialisee, soit en ANSI pour le TUI, soit en texte pur
//! pour `--once`. Le troncage et les gouttieres ne sont calcules qu'une fois.

use crate::app::*;
use crate::gpu::kib2mib;
use crate::rusd::{self, THROTTLE_REASONS, VIOLATION_NAMES};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq)]
pub struct Col(pub u8, pub u8, pub u8);

// Palette Catppuccin Mocha — pensee pour un fond sombre, sans imposer de theme.
pub const TEXT: Col = Col(205, 214, 244);
pub const SUB: Col = Col(166, 173, 200);
pub const MUTED: Col = Col(108, 112, 134);
pub const BLUE: Col = Col(137, 180, 250);
pub const LAV: Col = Col(180, 190, 254);
pub const GREEN: Col = Col(166, 227, 161);
pub const YELLOW: Col = Col(249, 226, 175);
pub const RED: Col = Col(243, 139, 168);
pub const PEACH: Col = Col(250, 179, 135);
pub const MAUVE: Col = Col(203, 166, 247);
pub const TEAL: Col = Col(148, 226, 213);
pub const SKY: Col = Col(137, 220, 235);
pub const PINK: Col = Col(245, 194, 231);
pub const BAR_BG: Col = Col(69, 71, 90);

static COLOR_ON: AtomicBool = AtomicBool::new(true);

pub fn set_color(on: bool) {
    COLOR_ON.store(on, Ordering::Relaxed);
}
pub fn toggle_color() {
    COLOR_ON.store(!COLOR_ON.load(Ordering::Relaxed), Ordering::Relaxed);
}
pub fn color_on() -> bool {
    COLOR_ON.load(Ordering::Relaxed)
}

#[derive(Clone)]
pub struct Span {
    pub t: String,
    pub c: Col,
    pub b: bool,
}

pub type Line = Vec<Span>;

pub fn sp(t: impl Into<String>, c: Col) -> Span {
    Span { t: t.into(), c, b: false }
}
pub fn sbb(t: impl Into<String>, c: Col) -> Span {
    Span { t: t.into(), c, b: true }
}
pub fn txt(t: impl Into<String>) -> Line {
    vec![sp(t, TEXT)]
}

fn width(s: &str) -> usize {
    s.chars().count()
}

pub fn trim_to(s: &str, w: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= w {
        return s.to_string();
    }
    if w <= 1 {
        return "…".into();
    }
    let mut out: String = c[..w - 1].iter().collect();
    out.push('…');
    out
}

pub fn load_color(pct: f64) -> Col {
    if pct < 45.0 {
        GREEN
    } else if pct < 72.0 {
        YELLOW
    } else if pct < 88.0 {
        PEACH
    } else {
        RED
    }
}

pub fn temp_color(t: f64) -> Col {
    if t >= 88.0 {
        RED
    } else if t >= 78.0 {
        PEACH
    } else if t >= 65.0 {
        YELLOW
    } else {
        TEAL
    }
}

/// Jauge en blocs Unicode : partie pleine colorée, reste discret.
pub fn bar(pct: f64, w: usize) -> Line {
    if w == 0 {
        return Vec::new();
    }
    let p = pct.clamp(0.0, 100.0);
    let mut filled = ((p / 100.0) * w as f64).round() as usize;
    if filled == 0 && p > 0.4 {
        filled = 1;
    }
    if filled > w {
        filled = w;
    }
    let mut out = Vec::new();
    if filled > 0 {
        out.push(sp("█".repeat(filled), load_color(p)));
    }
    if w - filled > 0 {
        out.push(sp("░".repeat(w - filled), BAR_BG));
    }
    out
}

pub fn pct_label(p: f64, w: usize) -> Span {
    let t = if p >= 99.95 {
        "100%".into()
    } else if p >= 10.0 {
        format!("{p:.0}%")
    } else {
        format!("{p:.1}%")
    };
    let pad = w.saturating_sub(width(&t));
    sbb(format!("{}{}", " ".repeat(pad), t), load_color(p))
}

/// Sparkline en blocs 1/8 : lisible, monotone, sans dependance graphique.
pub fn spark(vals: &[f64], w: usize, max: f64) -> Line {
    const RAMP: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if vals.is_empty() || w == 0 {
        return vec![sp("-".repeat(w), BAR_BG)];
    }
    let start = vals.len().saturating_sub(w);
    let data = &vals[start..];
    let mx = max.max(data.iter().cloned().fold(0.0f64, f64::max)).max(1e-6);
    let mut out: Vec<Span> = Vec::new();
    for &v in data {
        let idx = (((v / mx) * 7.999).floor() as usize).min(7);
        let c = load_color(v / mx * 100.0);
        if out.last().map(|s| s.c) == Some(c) {
            let n = out.last().unwrap().t.clone();
            let last = out.last_mut().unwrap();
            last.t = n;
            last.t.push(RAMP[idx]);
        } else {
            out.push(sp(RAMP[idx].to_string(), c));
        }
    }
    let used: usize = out.iter().map(|s| width(&s.t)).sum();
    if used < w {
        out.push(sp(" ".repeat(w - used), BAR_BG));
    }
    out
}

/// Graphe en caracteres braille (2 colonnes x 4 rangees de pixels par cellulle),
/// aire remplie coloree selon la charge. Aucun crate graphique.
pub fn chart(vals: &[f64], w: usize, h: usize, max: f64, labels: bool) -> Vec<Line> {
    let px_h = h * 4;
    let mut grid = vec![vec![0u8; w * 2]; px_h]; // 1 = pixel allume
    let n = vals.len().min(w);
    if n == 0 || w == 0 || h == 0 {
        return (0..h).map(|_| vec![sp(" ".repeat(w), TEXT)]).collect();
    }
    let start = vals.len() - n;
    let mx = max.max(vals[start..].iter().cloned().fold(0.0f64, f64::max)).max(1e-6);
    for (x, &v) in vals[start..].iter().enumerate() {
        let top = px_h - ((v / mx) * (px_h - 1) as f64).round() as usize;
        let top = top.min(px_h - 1);
        for y in top..px_h {
            grid[y][x * 2] = 1;
            grid[y][x * 2 + 1] = 1;
        }
    }
    let mut out: Vec<Line> = Vec::new();
    for cy in 0..h {
        let mut l: Line = Vec::new();
        let mut run = String::new();
        let mut runv = 0.0f64;
        for cx in 0..w {
            let mut bits: u16 = 0;
            let mut vmax = 0.0f64;
            for dy in 0..4 {
                for dx in 0..2 {
                    let y = cy * 4 + dy;
                    let x = cx * 2 + dx;
                    if grid[y][x] != 0 {
                        let bit: u16 = match (dx, dy) {
                            (0, 0) => 0x01,
                            (0, 1) => 0x02,
                            (0, 2) => 0x04,
                            (0, 3) => 0x40,
                            (1, 0) => 0x08,
                            (1, 1) => 0x10,
                            (1, 2) => 0x20,
                            _ => 0x80,
                        };
                        bits |= bit;
                        // valeur approximative de la colonne, pour la couleur
                        let frac = 1.0 - (y as f64 / (px_h - 1) as f64);
                        vmax = vmax.max(frac);
                    }
                }
            }
            let ch = if bits == 0 { ' ' } else { char::from_u32(0x2800 + bits as u32).unwrap_or(' ') };
            let val = vmax * 100.0;
            if !run.is_empty() && (load_color(val) != load_color(runv)) {
                l.push(sp(std::mem::take(&mut run), load_color(runv)));
            }
            if run.is_empty() {
                runv = val;
            }
            run.push(ch);
        }
        if !run.is_empty() {
            l.push(sp(run, load_color(runv)));
        }
        if l.is_empty() {
            l.push(sp(" ".repeat(w), BAR_BG));
        }
        out.push(l);
    }
    // rangee la plus haute en premier (convention ecran)
    out.reverse();
    let _ = labels;
    out
}

/// Barre de duree de bridage par point de performance.
pub fn violation_bars(app: &App, w: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let Some(g) = app.gpus.get(app.focus) else { return out };
    let Some(f) = &g.frame else { return out };
    let r = &f.rusd;
    let total = crate::sysinfo::uptime_s().max(1.0);
    let barw = ((w as f64) * 0.42) as usize;
    for (k, name) in VIOLATION_NAMES.iter().enumerate() {
        let cum = r.violation_ns[k].unwrap_or(0) as f64 / 1e9;
        let pct_int = (cum / total * 100.0).clamp(0.0, 100.0);
        let pct_iv = f.viol_pct[k].unwrap_or(0.0);
        let mut l = vec![sp(format!(" {name:<22}"), MUTED)];
        l.extend(bar(pct_int, barw));
        l.push(sp(format!("  total {:>11}  ·  interval {}", crate::sysinfo::duration_hms(cum), {
            let mut t = format!("{pct_iv:.1}%");
            if pct_iv < 10.0 {
                t = format!("{:>5}", t);
            }
            t
        }), if pct_iv > 1.0 { PEACH } else { MUTED }));
        out.push(l);
    }
    out
}

/// Bande systeme : ce que le GPU prend a la machine.
pub fn host_strip(app: &App, w: usize) -> Line {
    let (mt, ma) = (app.host.mem_total_kib as f64 / 1024.0, app.host.mem_avail_kib as f64 / 1024.0);
    let used = mt - ma;
    let pct = if mt > 0.0 { used / mt * 100.0 } else { 0.0 };
    let (l1, l5, l15) = crate::sysinfo::loadavg();
    let gpu_sysmem: u64 = app
        .gpus
        .iter()
        .filter_map(|g| g.frame.as_ref())
        .flat_map(|f| f.procs.iter())
        .map(|p| p.rss_mib)
        .sum();
    let bw = ((w as f64) * 0.2) as usize;
    let mut l = vec![
        sp("  host ", MUTED),
        sbb(app.m_host(&app.host.name), BLUE),
        sp(format!(" · {} threads", app.host.nproc), SUB),
        sp("  load ", MUTED),
        sbb(format!("{l1:.2} {l5:.2} {l15:.2}"), load_color(l1 / app.host.nproc as f64 * 100.0)),
        sp("  RAM ", MUTED),
    ];
    l.extend(bar(pct, bw));
    l.push(sp(format!(" {}/{}", gib(used), gib(mt)), SUB));
    l.push(sp("  ·  GPU RSS ", MUTED));
    l.push(sbb(gib(gpu_sysmem as f64 / 1024.0), TEAL));
    l.push(sp(format!("  ·  up {}", crate::sysinfo::duration_hms(crate::sysinfo::uptime_s())), SUB));
    let usedw: usize = l.iter().map(|s| width(&s.t)).sum();
    if usedw < w {
        l.push(sp(" ".repeat(w - usedw), TEXT));
    }
    l
}

fn gib(mib: f64) -> String {
    if mib >= 1024.0 {
        format!("{:.2} GiB", mib / 1024.0)
    } else {
        format!("{mib:.0} MiB")
    }
}
fn watts(mw: Option<u32>) -> String {
    mw.map(|v| format!("{:.1} W", v as f64 / 1000.0))
        .unwrap_or("—".into())
}
fn f1(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or("—".into())
}
fn f2(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or("—".into())
}
fn mhz(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.0}")).unwrap_or("—".into())
}
fn u(v: Option<u32>) -> String {
    v.map(|x| x.to_string()).unwrap_or("—".into())
}
/// 0 = capteur absent de cette carte, pas une mesure a zero watt.
fn watts_nz(mw: Option<u32>) -> String {
    mw.filter(|v| *v != 0).map(|v| format!("{:.1} W", v as f64 / 1000.0))
        .unwrap_or("not published".into())
}

fn u32o(v: Option<u32>) -> String {
    v.map(|x| x.to_string()).unwrap_or("—".into())
}
fn u64o(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or("—".into())
}
fn merge(v: &[Span]) -> String {
    v.iter().map(|s| s.t.as_str()).collect()
}

// ------------------------------------------------------------------ header

pub fn header(app: &App, w: usize, interval: f64) -> Line {
    let (pw, util, _, nproc) = app.totals();
    let mut v: Vec<Span> = vec![
        sbb(" nvtop-rs", MAUVE),
        sp("  ", MUTED),
        sbb(app.m_host(&app.host.name), BLUE),
        sp(" │ ", MUTED),
        sp(format!("{} GPU", app.gpus.len()), TEXT),
        sp(" │ ", MUTED),
        sp(format!("KMD {}", app.s.kmd_version), SUB),
        sp(" │ ", MUTED),
        sp(
            app.cuda
                .clone()
                .map(|c| format!("CUDA UMD {c}"))
                .unwrap_or_else(|| "CUDA UMD —".into()),
            SUB,
        ),
        sp(" │ ", MUTED),
        sbb(format!("{:.0} W", pw), if pw > 1.0 { PEACH } else { MUTED }),
        sp(" │ ", MUTED),
        sbb(format!("{util:.0}%"), load_color(util)),
        sp(" │ ", MUTED),
        sp(format!("{nproc} proc"), SUB),
        sp(" │ ", MUTED),
        sp(format!("{:.1} Hz", 1.0 / interval), MUTED),
    ];
    if app.paused {
        v.push(sp(" │ ", MUTED));
        v.push(sbb("PAUSE", RED));
    }
    if app.mask {
        v.push(sp(" │ ", MUTED));
        v.push(sp("MASKED", TEAL));
    }
    // fond legerement marque sur toute la largeur
    let s = trim_to(&merge(&v), w);
    vec![sp(format!("{s:<w$}", w = w), TEXT)]
}

// --------------------------------------------------------------- GPU panel

fn panel_top(title: &str, right: &str, w: usize, c: Col) -> Line {
    let t = trim_to(title, w.saturating_sub(8));
    let r = trim_to(right, w / 2);
    let used = 2 + width(&t) + width(&r) + 2;
    let fill = w.saturating_sub(used).max(1);
    vec![
        sp("╭─", c),
        sbb(t, c),
        sp("─".repeat(fill), c),
        sbb(r, MUTED),
        sp("─╮", c),
    ]
}

fn panel_row(v: Vec<Span>, inner: usize) -> Line {
    let mut out = vec![sp("│", BAR_BG)];
    let mut used = 0usize;
    for s in v {
        if used >= inner {
            break;
        }
        let t = trim_to(&s.t, inner - used);
        used += width(&t);
        out.push(Span { t, c: s.c, b: s.b });
    }
    if used < inner {
        out.push(sp(" ".repeat(inner - used), TEXT));
    }
    out.push(sp("│", BAR_BG));
    out
}

fn panel_bottom(w: usize, c: Col) -> Line {
    vec![sp(format!("╰{}╯", "─".repeat(w.saturating_sub(2))), c)]
}

fn label(t: &str) -> Span {
    sp(format!("{t:<8}"), MUTED)
}

/// Panel complet d'un GPU. `big` ajoute les lignes PCIe / sante / systeme.
pub fn gpu_panel(app: &App, i: usize, w: usize, big: bool) -> Vec<Line> {
    let Some(g) = app.gpus.get(i) else { return Vec::new() };
    let Some(f) = &g.frame else {
        return vec![txt("no data")];
    };
    let r = &f.rusd;
    let inner = w.saturating_sub(2);
    let accent = if app.focus == i { MAUVE } else { BLUE };
    let mut out: Vec<Line> = Vec::new();

    let pstate = pstate_of(g).unwrap_or(0);
    let title = format!(
        " GPU{} · {} ",
        i,
        trim_to(&g.st.name, inner.saturating_sub(46).max(8))
    );
    let uuid_short = if app.mask {
        "••••-••••".to_string()
    } else {
        g.st.uuid.chars().skip(4).take(13).collect::<String>()
    };
    let right = format!("{} · P{} · {} · {}", g.st.bus_id, pstate, g.st.codename, uuid_short);
    out.push(panel_top(&title, &right, w, accent));

    // --- GPU / VRAM
    let ugpu = r.util_gpu.unwrap_or(0) as f64;
    let (tot, heap, free, lf) = f.mem.unwrap_or((0, 0, 0, 0));
    let used_mib = kib2mib(heap.saturating_sub(free)) as f64;
    let tot_mib = kib2mib(tot) as f64;
    let mem_pct = if tot_mib > 0.0 { used_mib / tot_mib * 100.0 } else { 0.0 };
    let bw = ((inner as f64) * 0.26) as usize;
    let mut l = vec![label("util")];
    l.extend(bar(ugpu, bw));
    l.push(sp(" ", TEXT));
    l.push(pct_label(ugpu, 5));
    l.push(sp("  vram ", MUTED));
    l.extend(bar(mem_pct, bw));
    l.push(sp(" ", TEXT));
    l.push(pct_label(mem_pct, 5));
    l.push(sp(format!("  {}/{}", gib(used_mib), gib(tot_mib)), SUB));
    out.push(panel_row(l, inner));

    // --- memoire detaillee
    let b1t = r.bar1_total_kib.map(|k| kib2mib(k as u64)).unwrap_or(0);
    let b1f = r.bar1_free_kib.map(|k| kib2mib(k as u64)).unwrap_or(0);
    let mut l = vec![label("mem")];
    l.push(sp(
        format!(
            "heap {} · free {} · reserved {} · largest free block {} · BAR1 {}/{} MiB",
            gib(kib2mib(heap) as f64),
            gib(kib2mib(free) as f64),
            gib(kib2mib(tot.saturating_sub(heap)) as f64),
            gib(kib2mib(lf) as f64),
            b1f,
            b1t
        ),
        SUB,
    ));
    out.push(panel_row(l, inner));

    if let (Some(pt), Some(pf)) = (r.pma_total, r.pma_free) {
        let mut l = vec![label("pma")];
        l.push(sp(
            format!(
                "physical pool {:.2} GiB used of {:.2} GiB ({:.2} GiB free)",
                (pt - pf) as f64 / 1073741824.0,
                pt as f64 / 1073741824.0,
                pf as f64 / 1073741824.0
            ),
            SUB,
        ));
        out.push(panel_row(l, inner));
    }

    // --- temperature
    let tgpu = r.temp_c[0].unwrap_or(0.0);
    let mut l = vec![label("temp")];
    l.extend(bar(tgpu, bw));
    l.push(sp(" ", TEXT));
    l.push(sbb(format!("{:>5.1}°C", tgpu), temp_color(tgpu)));
    l.push(sp("  ", TEXT));
    for (k, name) in [(1usize, "mem"), (2, "plaque"), (3, "psu"), (4, "hbm")] {
        if let Some(t) = r.temp_c[k] {
            l.push(sp(format!("{name} {t:.1}° "), temp_color(t)));
        }
    }
    out.push(panel_row(l, inner));

    // --- puissance
    let lim = f.limit_mw.unwrap_or(0) as f64 / 1000.0;
    let avg = r.avg_power_mw.unwrap_or(0) as f64 / 1000.0;
    let ppct = if lim > 0.0 { avg / lim * 100.0 } else { 0.0 };
    let mut l = vec![label("power")];
    l.extend(bar(ppct, bw));
    l.push(sp(" ", TEXT));
    l.push(sbb(format!("{avg:>6.1} W"), PEACH));
    l.push(sp(
        format!(
            " / {:.0} W · instant {} · module {} · vram {} · TGPM {}",
            lim,
            f1(r.inst_power_mw.map(|v| v as f64 / 1000.0)),
            f1(r.avg_module_mw.filter(|v| *v != 0).map(|v| v as f64 / 1000.0)),
            f1(r.avg_memory_mw.filter(|v| *v != 0).map(|v| v as f64 / 1000.0)),
            f1(r.tgpm_mw.map(|v| v as f64 / 1000.0)),
        ),
        SUB,
    ));
    out.push(panel_row(l, inner));

    // --- refroidissement
    let fan = f.fan_pct.unwrap_or(0.0);
    let rpm = r.fan_rpm.first().copied().unwrap_or(0);
    let mut l = vec![label("fan")];
    l.extend(bar(fan, bw));
    l.push(sp(" ", TEXT));
    l.push(sp(
        format!("{fan:>4.0}% · {rpm} rpm"),
        if fan > 0.0 || rpm > 0 { SKY } else { MUTED },
    ));
    let others: Vec<String> = r
        .fan_rpm
        .iter()
        .enumerate()
        .filter(|(k, v)| *k > 0 && **v > 0)
        .map(|(k, v)| format!("F{k} {v}"))
        .collect();
    if !others.is_empty() {
        l.push(sp(format!("  ·  others {}", others.join(" ")), MUTED));
    }
    out.push(panel_row(l, inner));

    // --- horloges
    // PIEGE (docs §7.10) : le domaine SM n'est pas publie par ce driver (0) ;
    // NVML retombe alors sur l'horloge GRAPHICS. On fait idem.
    let sm = r.clk_actual_mhz[3].filter(|v| *v > 0.0).or(r.clk_actual_mhz[0]);
    let mut l = vec![label("clock")];
    l.push(sp(
        format!(
            "gfx {}/{} MHz · SM {} · mem {} · video {} · target {} · {}",
            mhz(r.clk_actual_mhz[0]),
            u(g.st.clk_max_gfx_mhz),
            mhz(sm),
            mhz(r.clk_actual_mhz[1]),
            mhz(r.clk_actual_mhz[2]),
            u(r.clk_target_mhz[0]),
            if g.st.clk_max_gfx_mhz.map(|m| r.clk_actual_mhz[0].unwrap_or(0.0) >= m as f64 * 0.98).unwrap_or(false) { "at max" } else { "" },
        ),
        SUB,
    ));
    out.push(panel_row(l, inner));

    // --- moteurs
    let mut l = vec![label("engines")];
    for (name, k) in [("ENC", 0usize), ("DEC", 1), ("JPG", 2), ("OFA", 3)] {
        let e = r.eng[k].as_ref().map(|x| x.busy as f64);
        l.push(sp(format!("{name} "), MUTED));
        l.push(sp(
            format!("{:>4}", e.map(|v| format!("{v:.0}%")).unwrap_or("—".into())),
            e.map(load_color).unwrap_or(MUTED),
        ));
        l.push(sp(" ", TEXT));
    }
    if let Some(e) = r.eng[0].as_ref() {
        l.push(sp(format!("(sampling period {} µs)", e.period_us), MUTED));
    }
    out.push(panel_row(l, inner));

    // --- bridage
    let mask = r.reason_mask.unwrap_or(0);
    let active: Vec<&str> = THROTTLE_REASONS
        .iter()
        .filter(|(b, _, _)| mask & *b != 0)
        .map(|(_, n, _)| *n)
        .collect();
    let worst = f
        .viol_pct
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(k, v)| v.map(|v| (v, k)))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut l = vec![label("perf")];
    l.push(sp(format!("0x{mask:016x} "), if mask == 0 { MUTED } else { YELLOW }));
    let active_txt = if active.is_empty() { "no throttling".to_string() } else { active.join(" + ") };
    l.push(sbb(active_txt, if active.is_empty() { GREEN } else { RED }));
    if let Some((v, k)) = worst {
        if v > 0.5 {
            l.push(sp(
                format!(
                    " · {} : {:.1}% of interval, {:.1} s total",
                    VIOLATION_NAMES[k],
                    v,
                    r.violation_ns[k].unwrap_or(0) as f64 / 1e9
                ),
                SUB,
            ));
        }
    }
    out.push(panel_row(l, inner));

    if big {
        let mut l = vec![label("pcie")];
        match &g.pci {
            Some(p) => {
                let err: u32 = r.pcie[1..].iter().filter_map(|v| *v).sum();
                l.push(sp(
                    format!(
                        "Gen{} x{} negotiated · max Gen{} x{} · {} · payload {} B · ASPM {} · IRQ {} · NUMA {} · IOMMU {}",
                        p.gen_cur,
                        p.width_cur,
                        p.gen_max,
                        p.width_max,
                        p.current_link_speed,
                        p.max_payload_b,
                        aspm(p.lnkctl_aspm),
                        p.irq,
                        p.numa,
                        p.iommu_group,
                    ),
                    SUB,
                ));
                l.push(sp(
                    format!(" · link errors {}", err),
                    if err == 0 { GREEN } else { RED },
                ));
            }
            None => l.push(sp("configuration space unavailable", MUTED)),
        }
        out.push(panel_row(l, inner));

        let mut l = vec![label("health")];
        l.push(sp(
            format!(
                "ECC {} · DRAM corr/unc {} / {} · SRAM {} / {} · row-remap {}{} · BAR1 usage {}% · ERR_CONT {:#x}",
                g.st.ecc_enabled.map(|e| if e { "ON" } else { "OFF" }).unwrap_or("?"),
                u64o(r.ecc_group[1][0]),
                u64o(r.ecc_group[1][2]),
                u64o(r.ecc_group[2][0]),
                u64o(r.ecc_group[2][2]),
                r.row_corr.unwrap_or(0) + r.row_uncorr.unwrap_or(0),
                if r.row_pending.unwrap_or(false) { " PENDING" } else { "" },
                if b1t > 0 { (100 * (b1t - b1f)) / b1t.max(1) } else { 0 },
                r.shadow_err_cont.unwrap_or(0),
            ),
            SUB,
        ));
        out.push(panel_row(l, inner));

        let mut l = vec![label("system")];
        l.push(sp(
            format!(
                "/dev/nvidia{} · VBIOS {} · SN {} · driver {} · cpus {} · host RAM {} · load {:.2}",
                g.st.card.minor,
                if g.st.vbios.is_empty() { "—" } else { &g.st.vbios },
                app.m_serial(&g.st.serial),
                g.pci.as_ref().map(|p| if p.driver.is_empty() { "—".into() } else { p.driver.clone() }).unwrap_or("—".into()),
                g.pci.as_ref().map(|p| p.cpulist.clone()).unwrap_or_default(),
                gib((app.host.mem_total_kib - app.host.mem_avail_kib) as f64 / 1024.0),
                crate::sysinfo::loadavg().0,
            ),
            SUB,
        ));
        out.push(panel_row(l, inner));

        let mut l = vec![label("cost")];
        l.push(sp(
            format!(
                "{} ioctl/frame · {:.2} ms · {} frames · page RUSD {} ({} bytes)",
                f.ioctls,
                f.took_ms,
                app.frames_done,
                if g.rusd_ok { "mapped" } else { "UNAVAILABLE" },
                crate::nvrm::RUSD_SIZE,
            ),
            if g.rusd_ok { GREEN } else { RED },
        ));
        out.push(panel_row(l, inner));
    }

    out.push(panel_bottom(w, accent));
    out
}

fn aspm(v: u32) -> &'static str {
    match v {
        0 => "off",
        1 => "L0s",
        2 => "L1",
        _ => "L0s+L1",
    }
}

/// Ligne resume pour les GPU non focus (mode multi-carte).
pub fn gpu_compact(app: &App, i: usize, w: usize) -> Line {
    let mut o: Line = Vec::new();
    let Some(g) = app.gpus.get(i) else { return o };
    let Some(f) = &g.frame else { return vec![sp("—", MUTED)] };
    let r = &f.rusd;
    let u = r.util_gpu.unwrap_or(0) as f64;
    let (tot, heap, free, _) = f.mem.unwrap_or((0, 0, 0, 0));
    let used = kib2mib(heap.saturating_sub(free)) as f64;
    let totf = kib2mib(tot) as f64;
    let mem_pct = if totf > 0.0 { used / totf * 100.0 } else { 0.0 };
    let t = r.temp_c[0].unwrap_or(0.0);
    let p = r.avg_power_mw.unwrap_or(0) as f64 / 1000.0;
    o.push(sp(format!(" GPU{i}"), if app.focus == i { MAUVE } else { MUTED }));
    o.push(sbb(trim_to(&g.st.name, 20), TEXT));
    o.push(sp(" ", TEXT));
    o.extend(bar(u, 12));
    o.push(pct_label(u, 5));
    o.push(sp("  ", TEXT));
    o.extend(bar(mem_pct, 12));
    o.push(sp(format!(" {}", gib(used)), SUB));
    o.push(sp(format!("  {:>5.1}°C", t), temp_color(t)));
    o.push(sp(format!(" {:>6.1}W", p), PEACH));
    o.push(sp(format!("  P{}", pstate_of(g).unwrap_or(0)), SKY));
    let usedw: usize = o.iter().map(|s| width(&s.t)).sum();
    if usedw < w {
        o.push(sp(" ".repeat(w - usedw), TEXT));
    }
    o
}

// ------------------------------------------------------------ process table

pub fn proc_table(app: &App, w: usize, rows_avail: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let Some(g) = app.gpus.get(app.focus) else { return out };
    let rows = app.filtered(&g.frame.as_ref().map(|f| f.procs.clone()).unwrap_or_default());
    let total: u64 = rows.iter().map(|r| r.mem_mib).sum();
    let head = format!(
        " GPU{} processes · {} shown · {} MiB total · sort {}{} ",
        app.focus,
        rows.len(),
        total,
        match app.sort {
            SortKey::Mem => "memory",
            SortKey::Pid => "pid",
            SortKey::Cpu => "cpu",
            SortKey::Name => "name",
        },
        if app.sort_desc { "▼" } else { "▲" },
    );
    out.push(sub_header(&head, w, TEAL));
    if rows.is_empty() {
        let msg = if app.filter.is_empty() {
            "no process holds GPU memory on this card — the driver tells an open file descriptor apart from real usage"
        } else {
            "no process matches the filter — Esc to clear it"
        };
        out.push(vec![sp(format!("  {msg}"), MUTED)]);
        return out;
    }
    let fixed = 7 + 1 + 7 + 1 + 5 + 1 + 10 + 1 + 6 + 1 + 8 + 1 + 4 + 1 + 4 + 1 + 19;
    let cw_cmd = w.saturating_sub(fixed).max(14);
    out.push(vec![
        sp("    PID", MUTED),
        sp("   USER", MUTED),
        sp(" TYPE", MUTED),
        sp("  GPU-MEM", MUTED),
        sp("  CPU%", MUTED),
        sp("     RSS", MUTED),
        sp(" THR", MUTED),
        sp(" CPU", MUTED),
        sp(" ENGINES           ", MUTED),
        sp("COMMAND", MUTED),
    ]);
    let body = rows_avail.saturating_sub(2);
    let start = app.scroll.min(rows.len().saturating_sub(1));
    for row in rows.iter().skip(start).take(body) {
        let eng: Vec<String> = row.engines.iter().map(|(e, v)| format!("{e} {v:.0}%")).collect();
        let cmd = app.m_cmd(&row.cmd);
        let l = vec![
            sbb(format!("{:>6}", row.pid), TEXT),
            sp(format!("{:>6}", app.m_user(&row.user)), SUB),
            sp(
                format!(" {:>4}", row.kind),
                if row.kind.contains('C') { SKY } else { PINK },
            ),
            sbb(format!("{:>8} Mi", row.mem_mib), load_color((row.mem_mib as f64 / (163.84 * 102.4)).max(1.0))),
            sp(format!("{:>6.1}", row.cpu), if row.cpu > 1.0 { YELLOW } else { MUTED }),
            sp(format!("{:>7} Mi", row.rss_mib), SUB),
            sp(format!("{:>4}", row.threads), MUTED),
            sp(format!("{:>4}", row.core), MUTED),
            sp(format!(" {:<18}", trim_to(&eng.join(","), 18)), TEAL),
            sp(trim_to(&cmd, cw_cmd), TEXT),
        ];
        out.push(l);
    }
    if rows.len() > start + body {
        out.push(vec![sp(
            format!(
                "  … {} more row(s) — wheel, j/k, PgUp/PgDn",
                rows.len() - start - body
            ),
            MUTED,
        )]);
    }
    out
}

pub fn sub_header(t: &str, w: usize, c: Col) -> Line {
    let tt = trim_to(t, w.saturating_sub(2));
    let n = width(&tt);
    vec![
        sp("─", c),
        sbb(tt, c),
        sp("─".repeat(w.saturating_sub(n + 1)), c),
    ]
}

// ----------------------------------------------------------- detail views

pub fn detail(app: &App, w: usize, rows: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    let Some(g) = app.gpus.get(app.focus) else { return out };
    let pairs: Vec<(String, String)> = match app.view {
        View::Overview => Vec::new(),
        View::Identity => identity(app, g),
        View::Memory => memory(app, g),
        View::Power => power(app, g),
        View::Pcie => pcie(app, g),
        View::Health => health(app, g),
        View::Driver => driver(app, g),
    };
    out.push(sub_header(
        &format!(
            " GPU{} · {} · {} entrees ",
            app.focus,
            app.view.title(),
            pairs.len()
        ),
        w,
        LAV,
    ));
    let kw = (w / 4).clamp(18, 40);
    let vw = w.saturating_sub(kw + 2);
    for (i, (key, val)) in pairs.iter().enumerate() {
        if i >= rows.saturating_sub(1) {
            out.push(vec![sp(
                format!("  … {} more rows (wheel)", pairs.len() - i),
                MUTED,
            )]);
            break;
        }
        out.push(vec![
            sp(format!(" {:<kw$}", trim_to(key, kw), kw = kw), MUTED),
            sp("│", BAR_BG),
            sp(trim_to(val, vw), TEXT),
        ]);
    }
    out
}

fn push(v: &mut Vec<(String, String)>, k: &str, val: impl Into<String>) {
    let val: String = val.into();
    if !val.is_empty() && val != "—" && val != "0" {
        v.push((k.to_string(), val));
    }
}

fn engine_name(e: u32) -> &'static str {
    match e {
        0 => "NULL",
        1..=8 => "GRAPHICS",
        9..=18 => "COPY",
        19 => "BSP",
        20..=26 => "NVDEC",
        27 => "MSENC",
        28..=29 => "NVENC",
        30 => "VP",
        36 => "VIC",
        38 => "SEC2",
        40 => "NVOFA",
        _ => "other",
    }
}

fn compute_mode(m: Option<u32>) -> &'static str {
    match m {
        Some(0) => "Default",
        Some(1) => "Exclusive_Thread",
        Some(2) => "Prohibited",
        Some(3) => "Exclusive_Process",
        _ => "unknown",
    }
}

fn yesno(v: Option<bool>) -> String {
    match v {
        Some(true) => "On".into(),
        Some(false) => "Off".into(),
        None => "—".into(),
    }
}

fn identity(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let st = &g.st;
    let sg = &app.s.gpus[st.index];
    let mut v: Vec<(String, String)> = Vec::new();
    v.push(("model (0x20800110)".into(), st.name.clone()));
    push(&mut v, "marketing name (EEPROM)", st.marketing.clone());
    push(&mut v, "RM name (0x202 szName)", st.rm_name.clone());
    v.push(("UUID (0x2080014a)".into(), app.m_uuid(&st.uuid)));
    v.push(("serial number".into(), app.m_serial(&st.serial)));
    push(&mut v, "product part", st.product_part.clone());
    push(&mut v, "board part 699", st.board_699.clone());
    push(&mut v, "board part 965", st.board_965.clone());
    push(&mut v, "memory part", st.mem_part.clone());
    push(&mut v, "memory date code", st.mem_date.clone());
    push(&mut v, "memory vendor", crate::gpu::mem_vendor(st.mem_vendor as u32));
    push(&mut v, "board revision", format!("{} (type {})", st.board_rev, st.board_type));
    push(&mut v, "EEPROM build date", if st.build_date > 0 { format!("{}", st.build_date) } else { String::new() });
    v.push(("VBIOS (procfs module)".into(), if st.vbios.is_empty() { "—".into() } else { st.vbios.clone() }));
    v.push(("gpuId (CARD_INFO 0xc8)".into(), format!("{:#010x}", st.card.gpu_id)));
    v.push(("device / subdevice".into(), format!("{} / {}", sg.device_instance, sg.sub_device_instance)));
    v.push(("boardId · gpuInstance · sli".into(), format!("{} · {} · {}", sg.board_id, sg.gpu_instance, sg.sli_status)));
    v.push(("numa (RM)".into(), sg.numa_id.to_string()));
    for (k, val) in &g.information {
        if k == "GPU UUID" {
            continue;
        }
        v.push((format!("procfs · {k}"), val.clone()));
    }
    v.push(("architecture / impl".into(), format!("{:#x} / {:#x}", st.arch, st.impl_)));
    v.push(("puce".into(), format!("{} · rev {:#x}.{:#x}", st.codename, st.chip_rev, st.sub_rev)));
    v.push(("compute capability".into(), st.compute_cap.clone()));
    push(&mut v, "ECID (0x20800102)", st.ecid.clone());
    for (label, ver) in &st.inforom {
        v.push((format!("InfoROM {label}"), ver.clone()));
    }
    push(&mut v, "engines (0x20800170)", st.engines.iter().map(|e| format!("{}:{}", e, engine_name(*e))).collect::<Vec<_>>().join(" "));
    v.push(("display heads".into(), format!("{} · screen {}", st.heads, if st.display_active { "connected" } else { "none" })));
    v.push(("compute mode".into(), compute_mode(st.compute_mode).into()));
    v.push(("persistence (0x800288)".into(), yesno(st.persistence)));
    v.push(("virtualisation".into(), st.virt_mode.map(virt_name).unwrap_or("unknown").to_string()));
    for (k, val) in &st.caps {
        v.push((format!("capacite {k}"), val.clone()));
    }
    v
}

fn virt_name(m: u32) -> &'static str {
    match m {
        0 => "none",
        1 => "vGPU passthrough",
        2 => "vGPU",
        3 => "vESXi",
        4 => "HGX passthrough",
        _ => "?",
    }
}

fn memory(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let Some(f) = &g.frame else { return v };
    let r = &f.rusd;
    if let Some((tot, heap, free, lf)) = f.mem {
        v.push(("FB total (tag 8)".into(), format!("{} KiB = {}", tot, gib(kib2mib(tot) as f64))));
        v.push(("RM heap (tag 9)".into(), format!("{} KiB = {}", heap, gib(kib2mib(heap) as f64))));
        v.push(("free heap (tag 22)".into(), format!("{} KiB = {}", free, gib(kib2mib(free) as f64))));
        v.push(("used = heap - free".into(), gib(kib2mib(heap.saturating_sub(free)) as f64)));
        v.push(("reserved = total - heap".into(), gib(kib2mib(tot.saturating_sub(heap)) as f64)));
        v.push(("largest free block (18)".into(), format!("{} KiB", lf)));
    }
    v.push((
        "BAR1 free / total".into(),
        format!(
            "{} / {} MiB",
            r.bar1_total_kib
                .map(|t| kib2mib(t as u64).saturating_sub(r.bar1_free_kib.map(|f| kib2mib(f as u64)).unwrap_or(0)))
                .unwrap_or(0),
            r.bar1_total_kib.map(|k| kib2mib(k as u64)).unwrap_or(0)
        ),
    ));
    v.push(("PMA total / free".into(), match (r.pma_total, r.pma_free) {
        (Some(t), Some(fr)) => format!("{:.2} / {:.2} GiB", t as f64 / 1073741824.0, fr as f64 / 1073741824.0),
        _ => "—".into(),
    }));
    for (k, val) in &g.st.fb {
        // certains tags renvoient u32::MAX quand la carte ne les publie pas
        if val.starts_with("42949") || val.starts_with("4294967295") {
            continue;
        }
        v.push((format!("FB · {k}"), val.clone()));
    }
    for (k, val) in &g.st.bus {
        if k.contains("GART") {
            v.push((format!("BUS · {k}"), val.clone()));
        }
    }
    if let Some(p) = &g.pci {
        for b in &p.bars {
            v.push((
                format!("{} ({})", b.name, b.kind()),
                format!(
                    "{:#018x}–{:#018x} · {}{}",
                    b.start,
                    b.end,
                    gib(b.size() as f64 / 1048576.0),
                    if b.prefetch() { " prefetch" } else { "" }
                ),
            ));
        }
    }
    v.push(("unites".into(), "the RM speaks KiB; NVML converts to MiB ROUNDED UP (docs §7.7)".into()));
    let _ = app;
    v
}

fn power(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let _ = app;
    let mut v: Vec<(String, String)> = Vec::new();
    let Some(f) = &g.frame else { return v };
    let r = &f.rusd;
    v.push(("average power (RUSD)".into(), watts_nz(r.avg_power_mw)));
    v.push(("instant power".into(), watts(r.inst_power_mw)));
    v.push(("module (GPU+memory)".into(), watts_nz(r.avg_module_mw)));
    v.push(("VRAM power".into(), watts_nz(r.avg_memory_mw)));
    v.push(("Module CPU".into(), watts(r.inst_cpu_mw)));
    v.push(("TGPM (power policy)".into(), watts(r.tgpm_mw)));
    v.push(("requested limit".into(), watts(r.limit_requested_mw)));
    v.push(("enforced limit".into(), watts(r.limit_enforced_mw)));
    v.push(("current limit (0x2080a61d)".into(), watts(f.limit_mw)));
    v.push(("default limit (0x2080a618)".into(), watts(g.st.power_default_mw)));
    if let Some((st, lvl)) = f.rated {
        v.push(("rated TDP (0x2080206e)".into(), format!("state {st} · level {lvl}")));
    }
    if let Some(p) = f.powerstate {
        v.push((
            "power state (0x2080205a)".into(),
            match p {
                0 => "on".to_string(),
                1 => "standby".to_string(),
                2 => "off".to_string(),
                other => format!("code {other}"),
            },
        ));
    }
    if let (Some(lim), Some(avg)) = (f.limit_mw, r.avg_power_mw) {
        v.push((
            "headroom under the limit".into(),
            format!(
                "{:.1} % used · {:.1} W free",
                avg as f64 / lim as f64 * 100.0,
                lim.saturating_sub(avg) as f64 / 1000.0
            ),
        ));
    }
    push(&mut v, "fan duty (0x2080852f+32)", f.fan_pct.map(|x| format!("{x:.2} %")).unwrap_or_default());
    for (i, rpm) in r.fan_rpm.iter().enumerate() {
        if *rpm > 0 {
            v.push((format!("ventilateur {i}"), format!("{rpm} RPM")));
        }
    }
    for (k, sensor) in r.temp_c.iter().enumerate() {
        v.push((
            format!("sensor {} (SFXP24.8)", rusd::TEMP_NAMES[k]),
            sensor.map(|x| format!("{x:.2} °C")).unwrap_or_else(|| "not published by GSP".into()),
        ));
    }
    for (k, val) in &g.power_procfs {
        v.push((format!("procfs · {k}"), val.clone()));
    }
    v
}

fn pcie(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let Some(f) = &g.frame else { return v };
    let r = &f.rusd;
    if let Some(p) = &g.pci {
        v.push(("adresse".into(), p.bdf.clone()));
        v.push((
            "vendor:device".into(),
            format!(
                "{:04x}:{:04x} · {} · {}",
                p.vendor,
                p.device,
                app.pci_ids.vendor(p.vendor),
                app.pci_ids.device(p.vendor, p.device)
            ),
        ));
        v.push((
            "subsystem".into(),
            format!(
                "{:04x}:{:04x} · {}",
                p.subsys_vendor,
                p.subsys_device,
                app.pci_ids.vendor(p.subsys_vendor)
            ),
        ));
        v.push(("PCI class".into(), format!("{:06x}", p.class)));
        v.push(("revision".into(), format!("{:#02x}", p.revision)));
        v.push(("negotiated link".into(), format!("Gen{} x{} · {}", p.gen_cur, p.width_cur, p.current_link_speed)));
        v.push(("max link".into(), format!("Gen{} x{} · {}", p.gen_max, p.width_max, p.max_link_speed)));
        v.push(("max payload / read request".into(), format!("{} B / {} B", p.max_payload_b, p.max_read_req_b)));
        v.push(("ASPM".into(), aspm(p.lnkctl_aspm).into()));
        push(&mut v, "power state", p.power_state.clone());
        v.push(("IRQ / MSI".into(), format!("{} · {}", p.irq, if p.msi.is_empty() { "—".into() } else { p.msi.join(",") })));
        v.push(("NUMA / cpulist".into(), format!("{} · {}", p.numa, p.cpulist)));
        v.push(("IOMMU group".into(), p.iommu_group.clone()));
        v.push(("driver".into(), if p.driver.is_empty() { "—".into() } else { p.driver.clone() }));
        for (k, n) in p.aer_correctable.iter().chain(p.aer_nonfatal.iter()).chain(p.aer_fatal.iter()) {
            if *n != 0 || k.contains("TOTAL") {
                v.push((format!("AER · {k}"), n.to_string()));
            }
        }
        for c in &p.consumer {
            v.push(("sub-device".into(), c.clone()));
        }
    }
    v.push((
        "RUSD pciBusData[0]".into(),
        r.pcie[0]
            .map(|x| format!("{x:#x} · max Gen {}, current Gen {}", ((x >> 12) & 0xf) + 1, ((x >> 16) & 0xf) + 1))
            .unwrap_or("—".into()),
    ));
    for (k, name) in rusd::PCIE_COUNTER_NAMES.iter().enumerate() {
        v.push((format!("counter · {name}"), r.pcie[k + 1].map(|x| x.to_string()).unwrap_or("—".into())));
    }
    for (k, val) in &g.st.bus {
        v.push((format!("RM BUS · {k}"), val.clone()));
    }
    v
}

fn health(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let Some(f) = &g.frame else { return v };
    let r = &f.rusd;
    v.push((
        "ECC (0x20800133)".into(),
        g.st.ecc_enabled.map(|e| if e { "actif" } else { "inactif" }).unwrap_or("?").to_string(),
    ));
    v.push((
        "ECC next boot".into(),
        g.st.ecc_next_boot.map(|e| if e { "oui" } else { "non" }).unwrap_or("?").to_string(),
    ));
    for (gi, name) in ["TOTAL", "DRAM", "SRAM"].iter().enumerate() {
        v.push((
            format!("{name} · corrected volatile / aggregate"),
            format!("{} / {}", u64o(r.ecc_group[gi][0]), u64o(r.ecc_group[gi][1])),
        ));
        v.push((
            format!("{name} · uncorrected volatile / aggregate"),
            format!("{} / {}", u64o(r.ecc_group[gi][2]), u64o(r.ecc_group[gi][3])),
        ));
    }
    if let Some(d) = &r.ecc_detail {
        v.push(("SRAM corrige (unique)".into(), d.sram_corr_unique.to_string()));
        v.push(("SRAM non corrige parity".into(), d.sram_unc_parity_unique.to_string()));
        v.push(("SRAM non corrige SEC-DED".into(), d.sram_unc_secded_unique.to_string()));
        v.push(("DRAM corrige / non corrige".into(), format!("{} / {}", d.dram_corr_total, d.dram_unc_total)));
        v.push((
            "SRAM buckets L2/SM/PCIe/FW/others".into(),
            format!("{} / {} / {} / {} / {}", d.sram_bucket_l2, d.sram_bucket_sm, d.sram_bucket_pcie, d.sram_bucket_fw, d.sram_bucket_other),
        ));
        v.push(("SRAM threshold exceeded".into(), if d.threshold_exceeded { "OUI" } else { "non" }.into()));
        if d.last_cleared != 0 {
            v.push(("counters last cleared".into(), format!("{} (unix epoch)", d.last_cleared)));
        }
    }
    let hist = [
        "no row remapped",
        "one row remapped",
        "several rows",
        "one row available",
        "all rows used",
    ];
    for (k, name) in hist.iter().enumerate() {
        if let Some(x) = r.row_hist[k] {
            if x != 0 || k == 0 {
                v.push((format!("row-remap · {name}"), x.to_string()));
            }
        }
    }
    v.push(("row-remap correctable / uncorrectable".into(), format!("{} / {}", u32o(r.row_corr), u32o(r.row_uncorr))));
    v.push((
        "row-remap inactive".into(),
        format!("{} / {}", u32o(r.row_corr_inactive), u32o(r.row_uncorr_inactive)),
    ));
    v.push((
        "remapping pending".into(),
        r.row_pending
            .map(|b| if b { "OUI — reboot requis" } else { "non" })
            .unwrap_or("?")
            .to_string(),
    ));
    v.push((
        "memory failure occurred".into(),
        r.row_failure_occurred.map(|b| if b { "oui" } else { "non" }).unwrap_or("?").to_string(),
    ));
    v.push(("bank-remap spare max / without spare".into(), format!("{} / {}", u32o(r.bank_max_spare), u32o(r.bank_no_spare))));
    v.push((
        "bank-remap active / inactive / pending".into(),
        format!("{} / {} / {}", u32o(r.bank_active), u32o(r.bank_inactive), r.bank_pending.map(|b| b.to_string()).unwrap_or("?".into())),
    ));
    v.push(("ERR_CONT register (shadow)".into(), r.shadow_err_cont.map(|x| format!("{x:#010x}")).unwrap_or("—".into())));
    v.push(("context-switch logging".into(), r.ctxsw_logging.map(|b| b.to_string()).unwrap_or("—".into())));
    v.push((
        "reference timestamp".into(),
        r.reference_time_ns.map(|x| format!("{} ms", x / 1_000_000)).unwrap_or("—".into()),
    ));
    v.push(("reason mask".into(), format!("{:#018x}", r.reason_mask.unwrap_or(0))));
    v.push((
        "accounted performance points".into(),
        r.supported_violation_mask.map(|m| format!("{m:#x}")).unwrap_or("—".into()),
    ));
    for (k, name) in VIOLATION_NAMES.iter().enumerate() {
        v.push((
            format!("cumul · {name}"),
            format!(
                "{} s · interval {}",
                r.violation_ns[k].map(|x| format!("{:.3}", x as f64 / 1e9)).unwrap_or("—".into()),
                f.viol_pct[k].map(|x| format!("{x:.1} %")).unwrap_or("—".into())
            ),
        ));
    }
    v.push((
        "per-engine, per-PID occupancy".into(),
        if r.proc_util.is_empty() {
            "not published by GSP on this card".into()
        } else {
            format!("{} sample(s) in the ring", r.proc_util.len())
        },
    ));
    for (i, sm) in r.proc_util.iter().take(3).enumerate() {
        let parts: Vec<String> = sm
            .eng
            .iter()
            .enumerate()
            .filter(|(_, (u, _))| *u > 0)
            .map(|(e, (u, pid))| format!("{} {:.2}% pid{:#x}", rusd::PROC_ENGINES[e], *u as f64 / 100.0, pid))
            .collect();
        if !parts.is_empty() {
            v.push((format!("echantillon {i} · ts {}", sm.ts), parts.join("  ")));
        }
    }
    let _ = app;
    v
}

fn driver(app: &App, g: &GpuState) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let s = &app.s;
    v.push(("KMD (0xd2 QUERY)".into(), s.kmd_version.clone()));
    v.push(("driver (0x101)".into(), s.driver_version.clone()));
    v.push(("changelist / officiel".into(), format!("{} / {}", s.rm_changelist, s.rm_changelist_official)));
    v.push(("RM branch".into(), if s.rm_branch == 0 { "—".into() } else { s.rm_branch.to_string() }));
    push(&mut v, "build title", s.rm_title.clone());
    v.push((
        "fd validated (privileged objects)".into(),
        if s.version_validated { "yes (STRICT reply=1)".into() } else { "NON — 0x1b attendu".into() },
    ));
    if let Some(c) = &app.cuda {
        v.push(("CUDA UMD (dlopen libcuda)".into(), c.clone()));
    }
    v.push(("kernel module".into(), app.host.nvidia_version.clone()));
    v.push((
        "GPUs probed / attached".into(),
        format!(
            "{} attached{}",
            s.gpus.len(),
            if s.excluded_ids.is_empty() { String::new() } else { format!(", excluded {:?}", s.excluded_ids) }
        ),
    ));
    v.push(("gpuId".into(), s.gpus.iter().map(|x| format!("{:#010x}", x.gpu_id)).collect::<Vec<_>>().join(" ")));
    v.push(("device node".into(), format!("/dev/nvidia{} (minor {}, IRQ {})", g.st.card.minor, g.st.card.minor, g.st.card.irq)));
    v.push(("BAR reg / FB (CARD_INFO)".into(), format!("{:#x} ({} Mo) · {:#x} ({} Mo)",
        g.st.card.reg_address, g.st.card.reg_size / 1048576,
        g.st.card.fb_address, g.st.card.fb_size / 1048576)));
    v.push(("access".into(), "ioctl /dev/nvidiactl + /dev/nvidiaN · no libnvidia-ml, no nvidia-smi".into()));
    v.push(("securite".into(), "read-only: GET/QUERY controls and ATTACH_IDS only".into()));
    v.push(("host".into(), app.m_host(&app.host.name)));
    v.push(("kernel".into(), app.host.kernel.clone()));
    v.push(("CPU".into(), format!("{} · {} threads", app.host.cpu, app.host.nproc)));
    v.push((
        "host RAM".into(),
        format!(
            "{} libres / {} (swap {})",
            gib(app.host.mem_avail_kib as f64 / 1024.0),
            gib(app.host.mem_total_kib as f64 / 1024.0),
            gib(app.host.swap_total_kib as f64 / 1024.0)
        ),
    ));
    v.push(("uptime".into(), crate::sysinfo::duration_hms(crate::sysinfo::uptime_s())));
    for (k, val) in &app.host.params {
        v.push((format!("module param · {k}"), val.clone()));
    }
    v
}

// ----------------------------------------------------------------- keybar

pub fn keybar(app: &App, w: usize) -> Line {
    let keys: [(&str, &str); 11] = [
        ("q", "quit"),
        ("␣", "pause"),
        ("+/-", "speed"),
        ("⇥", "gpu"),
        ("h l", "views"),
        ("i m p x e d", ""),
        ("s", "sort"),
        ("/", "filter"),
        ("wheel", ""),
        ("z", "mask"),
        ("?", "help"),
    ];
    let parts: Vec<String> = keys
        .iter()
        .map(|(k, d)| {
            let d = d.trim();
            if d.is_empty() { k.to_string() } else { format!("{k} {d}") }
        })
        .collect();
    let view = format!(" {} {} ", app.view.title(), if app.paused { "❚❚" } else { "▶" });
    let room = w.saturating_sub(width(&view) + 2);
    let mut line = String::new();
    for p in parts {
        let cand = if line.is_empty() { p.clone() } else { format!("{line}  ·  {p}") };
        if width(&cand) + 1 > room {
            break;
        }
        line = cand;
    }
    let pad = w.saturating_sub(width(&line) + width(&view) + 1);
    vec![
        sp(format!(" {line}"), MUTED),
        sp(" ".repeat(pad), MUTED),
        sbb(view, LAV),
    ]
}

pub fn help(w: usize, h: usize) -> Vec<Line> {
    let rows: [(&str, &str); 16] = [
        ("q · Ctrl-C", "quit and hand the terminal back"),
        ("Space", "freeze / resume refreshing"),
        ("+ · -", "sampling interval (0.05 s to 20 s)"),
        ("Tab · 1..8", "pick the GPU to watch"),
        ("h · l · ←/→", "cycle the views"),
        ("i m p x e d", "identity · memory · power · PCIe · health · driver"),
        ("s · S", "sort processes · reverse order"),
        ("/", "filter (pid, user, command) · Enter applies, Esc clears"),
        ("j k · wheel", "scroll · PgUp PgDn · Home"),
        ("z", "mask mode (serial, UUID, users, commands)"),
        ("c", "colours on/off (NO_COLOR honoured)"),
        ("", ""),
        ("0x20800110", "GPU name — 0x2080013f serial and board part numbers"),
        ("RUSD 0x00de", "shared telemetry page: 1 ioctl per frame, everything else is a memory read"),
        ("0x2080018d/e", "process list and memory held per PID and per channel class"),
        ("", ""),
    ];
    let bw = w.clamp(40, 92);
    let mut out: Vec<Line> = Vec::new();
    out.push(panel_top(" nvtop-rs — help ", " q or ? to close ", bw, MAUVE));
    for (k, d) in rows {
        if k.is_empty() {
            out.push(panel_row(vec![sp("", MUTED)], bw - 2));
        } else {
            out.push(panel_row(
                vec![sp(format!("{:<22}", trim_to(k, 22)), TEAL), sp(trim_to(d, bw - 26), TEXT)],
                bw - 2,
            ));
        }
    }
    out.push(panel_bottom(bw, MAUVE));
    let _ = h;
    out
}

// ------------------------------------------------------------------ draw

/// Assemble une frame complete (lignes pretes a etre serialisees).
pub fn render(app: &App, w: usize, h: usize, color: bool, interval: f64) -> Vec<String> {
    let color = color && color_on();
    let mut lines: Vec<Line> = Vec::new();
    lines.push(header(app, w, interval));

    let multi = app.gpus.len() > 1;
    if app.view == View::Overview {
        if multi {
            for i in 0..app.gpus.len() {
                lines.push(gpu_compact(app, i, w));
            }
        }
        lines.extend(gpu_panel(app, app.focus, w, true));

        // blocs optionnels, empiles par ordre d'interet, tronques si manque de place
        let hu: Vec<f64> = app.gpus[app.focus].hist.pts.iter().map(|p| p.util).collect();
        let hm: Vec<f64> = app.gpus[app.focus].hist.pts.iter().map(|p| p.mem).collect();
        let hp: Vec<f64> = app.gpus[app.focus].hist.pts.iter().map(|p| p.power).collect();
        let charts_on = h >= 30 && !hu.is_empty();
        let viols_on = h >= 26;
        let host_on = h >= 22;

        let mut rest: Vec<Line> = Vec::new();
        if charts_on {
            let gh = 3usize;
            rest.push(sub_header(
                &format!(
                    " historique · {} samples · {} · GPU% / VRAM% / watts ",
                    hu.len(),
                    format!("~{:.0} s", hu.len() as f64 * interval)
                ),
                w,
                BLUE,
            ));
            for (vals, mx, tag) in [(&hu, 100.0, " gpu "), (&hm, 100.0, " vram"), (&hp, 120.0, "   W ")] {
                let cur = *vals.iter().last().unwrap_or(&0.0);
                let mut g = chart(vals, w.saturating_sub(44), gh, mx, false);
                for (i, row) in g.iter_mut().enumerate() {
                    let mut l = vec![sp(
                        format!("{tag}"),
                        if i == 0 { TEAL } else { BAR_BG },
                    )];
                    l.append(row);
                    if i == 0 {
                        l.push(sp(
                            format!("  now {:.1} max {:.1}", cur, vals.iter().cloned().fold(0.0f64, f64::max)),
                            load_color(if mx == 120.0 { cur / 1.2 } else { cur }),
                        ));
                    }
                    rest.push(l);
                }
            }
        }
        if viols_on {
            rest.push(sub_header(
                " cumulative time per performance point (the driver knows it, nvidia-smi hides it) ",
                w,
                PEACH,
            ));
            rest.extend(violation_bars(app, w));
        }
        let avail = h.saturating_sub(lines.len() + rest.len() + 2);
        let mut pt = proc_table(app, w, avail.max(3));
        if pt.len() > avail {
            pt.truncate(avail);
        }
        lines.extend(pt);
        lines.extend(rest);
        if host_on {
            lines.push(host_strip(app, w));
        }
    } else {
        lines.extend(gpu_panel(app, app.focus, w, false));
        let avail = h.saturating_sub(lines.len() + 1);
        lines.extend(detail(app, w, avail));
    }
    // complete jusqu'a l'avant-derniere ligne, puis la barre de touches
    let target = h.saturating_sub(1);
    while lines.len() < target {
        lines.push(vec![sp(" ".repeat(w), TEXT)]);
    }
    lines.truncate(target);
    lines.push(keybar(app, w));

    lines.into_iter().map(|l| line_str(&l, w, color)).collect()
}

pub fn line_str(l: &Line, w: usize, color: bool) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for s in l {
        if used >= w {
            break;
        }
        let t = trim_to(&s.t, w - used);
        used += width(&t);
        if color {
            out.push_str(&format!("\x1b[{}38;2;{};{};{}m", if s.b { "1;" } else { "" }, s.c.0, s.c.1, s.c.2));
            out.push_str(&t);
            out.push_str("\x1b[0m");
        } else {
            out.push_str(&t);
        }
    }
    if used < w {
        out.push_str(&" ".repeat(w - used));
    }
    out
}

/// Version texte pur pour `--once`, les logs et les rapports d'incident.
pub fn plain(app: &App, w: usize, interval: f64) -> String {
    let h = 40.max(app.gpus.len() * 18 + 20);
    let lines = render(app, w, h, false, interval);
    let mut out = String::new();
    for l in lines {
        out.push_str(l.trim_end());
        out.push('\n');
    }
    out
}

/// Une frame TUI : chaque ligne est addressée explicitement (pas de scroll,
/// pas de clignotement, un seul write par frame).
pub fn ansi_frame(app: &App, w: usize, h: usize, interval: f64, color: bool) -> String {
    let lines = render(app, w, h, color, interval);
    let mut out = String::with_capacity(lines.len() * (w + 24));
    for (i, l) in lines.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H\x1b[K", i + 1));
        out.push_str(l);
    }
    out
}
