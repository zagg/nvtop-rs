//! nvtop-rs - moniteur GPU lu directement dans le pilote NVIDIA.
//!
//! Aucun NVML, aucun nvidia-smi, aucun CUDA : des ioctl sur /dev/nvidia*, la
//! page partagee RUSD (classe 0x00de) et un peu de procfs/sysfs. La methode de
//! retro-ingenierie est documentee dans
//! https://github.com/zagg/nvidia-smi-clone-RE (docs/REVERSE_ENGINEERING.md) ;
//! ce programme en est la transcription Rust, orientee monitoring temps reel.
//!
//! Strictement lecture seule : uniquement des controles GET/QUERY, plus
//! ATTACH_IDS et la poignee de main de version. Aucun SET, aucun RESET.

mod app;
mod ffi;
mod gpu;
mod nvrm;
mod rusd;
mod sysinfo;
mod term;
mod ui;

use app::{sort_rows, App, SortKey, View, VIEWS};
use std::io::Write;
use std::time::{Duration, Instant};
use term::{Key, Term};

const VERSION: &str = "0.1.0";

struct Opts {
    interval: f64,
    gpu: Option<usize>,
    color: bool,
    mask: bool,
    cuda: bool,
    json: bool,
    once: bool,
    width: Option<usize>,
    rows: Option<usize>,
    hist: usize,
    view: View,
    help: bool,
    version: bool,
    err: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            interval: 0.5,
            gpu: None,
            color: term::is_tty(1) && std::env::var_os("NO_COLOR").is_none(),
            mask: std::env::var_os("NVTOP_MASK").is_some(),
            cuda: std::env::var_os("NVTOP_NO_CUDA").is_none(),
            json: false,
            once: false,
            width: None,
            rows: None,
            hist: 240,
            view: View::Overview,
            help: false,
            version: false,
            err: None,
        }
    }
}

fn parse() -> Opts {
    let mut o = Opts::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        let (k, val) = match a.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (a.clone(), None),
        };
        let take = |o: &mut Opts, v: &Option<String>, f: &dyn Fn(&mut Opts, f64)| {
            if let Some(v) = v {
                if let Ok(x) = v.parse::<f64>() {
                    f(o, x);
                }
            }
        };
        match k.as_str() {
            "-h" | "--help" => o.help = true,
            "-V" | "--version" => o.version = true,
            "-i" | "--interval" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                take(&mut o, &v, &|o, x| o.interval = x.clamp(0.05, 20.0));
            }
            "-n" | "--count" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                take(&mut o, &v, &|_, _| ());
            }
            "--gpu" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                if let Some(v) = v {
                    o.gpu = v.trim_start_matches("gpu").parse::<usize>().ok();
                }
            }
            "--width" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                o.width = v.and_then(|v| v.parse().ok());
            }
            "--rows" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                o.rows = v.and_then(|v| v.parse().ok());
            }
            "--hist" => {
                let v = if val.is_some() { val.clone() } else { i += 1; args.get(i).cloned() };
                if let Some(v) = v {
                    if let Ok(x) = v.parse::<usize>() {
                        o.hist = x.clamp(8, 4000);
                    }
                }
            }
            "--view" => {
                if let Some(v) = val {
                    o.view = match v.to_lowercase().as_str() {
                        "identity" | "identity" => View::Identity,
                        "memory" | "memory" | "mem" => View::Memory,
                        "power" | "power" => View::Power,
                        "pci" | "pcie" => View::Pcie,
                        "health" | "health" | "ecc" => View::Health,
                        "driver" | "driver" | "sys" => View::Driver,
                        _ => View::Overview,
                    };
                }
            }
            "--json" => {
                o.json = true;
                o.color = false;
            }
            "--once" | "-1" => {
                o.once = true;
            }
            "--no-color" | "--mono" => o.color = false,
            "--color" => o.color = true,
            "--mask" => o.mask = true,
            "--no-cuda" => o.cuda = false,
            s if s.starts_with('-') && !s[1..].is_empty() && s[1..].chars().all(|c| c.is_ascii_digit()) => {
                // `-2` = 2 Hz ? on préfére explicite : intervalle en secondes
                if let Ok(x) = s[1..].parse::<f64>() {
                    o.interval = (1.0 / x).clamp(0.05, 20.0);
                }
            }
            other => {
                o.err = Some(format!("unknown option: {other}"));
                break;
            }
        }
        i += 1;
    }
    o
}

const HELP: &str = "\
nvtop-rs — real-time GPU monitor read straight from the NVIDIA kernel driver

No NVML, no nvidia-smi, no CUDA toolkit: ioctl()s on /dev/nvidia*, the RUSD
shared telemetry page (class 0x00de) and a little procfs/sysfs.

USAGE
  nvtop-rs [OPTIONS]

OPTIONS
  -i, --interval=SEC   sampling period, 0.05 to 20 s            (default 0.5)
      --gpu=N          show only GPU N
      --view=NAME      initial view: overview identity memory power pcie
                       health driver
      --once           plain-text snapshot, then exit (good for tickets/logs)
      --json           JSON snapshot (exporters, dashboards, jq)
      --width=N        force the output width
      --hist=N         history depth used by the graphs (default 240)
      --no-color       no 24-bit ANSI (NO_COLOR is honoured)
      --mask           mask serial, UUID, users and command lines
      --no-cuda        do not load libcuda just for the UMD version
  -h, --help           this text
  -V, --version        version

KEYS
  q/Ctrl-C quit · Space pause · +/- speed · Tab/1..8 GPU · h l or arrows views
  i m p x e d direct views · s sort · / filter · mouse wheel scroll · z mask · ? help

EXAMPLES
  nvtop-rs                          the monitor
  nvtop-rs -i 0.1 --gpu 0           10 Hz on GPU 0
  nvtop-rs --once  > gpu.txt        a trace for an incident report
  nvtop-rs --json | jq '.gpus[0].power_w'     dashboards without nvidia-smi

PROVENANCE AND RIGHTS
  Reverse engineering: github.com/zagg/nvidia-smi-clone-RE
  MIT licensed · read-only · not affiliated with NVIDIA
";

fn main() {
    let o = parse();
    if o.help {
        print!("{HELP}");
        return;
    }
    if o.version {
        println!("nvtop-rs {VERSION} — reads the NVIDIA driver directly (ioctl /dev/nvidia*, RUSD 0x00de)");
        return;
    }
    if let Some(e) = &o.err {
        eprintln!("nvtop-rs: {e}\n(see --help)");
        std::process::exit(2);
    }

    let built = App::new(o.mask, o.cuda, o.hist);
    let mut app = match built {
        Ok(a) => a,
        Err(e) => {
            eprintln!("nvtop-rs: {e}");
            eprintln!(
                "  verification : ls -l /dev/nvidiactl /dev/nvidia* · lsmod | grep nvidia\n  \
                 the module must be loaded and the account needs nvidia-smi privileges."
            );
            std::process::exit(1);
        }
    };
    if let Some(g) = o.gpu {
        if g < app.gpus.len() {
            app.focus = g;
            if app.gpus.len() > 1 {
                // on ne garde que le GPU demande : panels + processus dedies
                app.gpus.rotate_left(g);
                app.gpus.truncate(1);
                app.focus = 0;
            }
        } else {
            eprintln!("nvtop-rs: GPU {g} does not exist ({} visible)", app.gpus.len());
            std::process::exit(1);
        }
    }
    app.view = o.view;

    if o.json {
        app.sample();
        println!("{}", app::json(&app));
        return;
    }

    let is_tty = term::is_tty(1) && term::is_tty(0);
    if o.once || !is_tty {
        app.sample();
        let (rows, cols) = term::winsize(1);
        let w = o.width.unwrap_or(cols as usize).clamp(72, 240);
        let h = o.rows.unwrap_or(64usize).max(16);
        let text = ui::plain(&app, w, 1.0 / o.interval);
        let _ = rows;
        let _ = h;
        let stdout = std::io::stdout();
        let mut lk = stdout.lock();
        let _ = lk.write_all(text.as_bytes());
        let _ = lk.flush();
        return;
    }

    run_tui(&mut app, &o);
}

fn run_tui(app: &mut App, o: &Opts) {
    let mut term = Term::new(o.color);
    term::set_nonblocking_stdin();
    let _raw = term::RawGuard::enter(0);
    term.enter();
    let mut interval = Duration::from_secs_f64(o.interval);
    let mut help = false;
    let mut filtering = false;
    let mut next = Instant::now();
    let trace = std::env::var_os("NVTOP_TRACE").is_some();
    let mut loops = 0usize;

    loop {
        loops += 1;
        let (rows, cols) = term::winsize(1);
        let w = o.width.unwrap_or(cols as usize).max(60);
        let h = o.rows.unwrap_or(rows as usize).max(12);

        let now = Instant::now();
        if now >= next {
            if !app.paused && !filtering {
                app.sample();
            }
            next = now + interval;
        }

        let frame = if help {
            let hl = ui::help(w, h);
            let mut s = String::new();
            for (i, l) in hl.iter().enumerate() {
                s.push_str(&format!("\x1b[{};1H\x1b[K", i + (h / 2).saturating_sub(hl.len() / 2)));
                s.push_str(&ui::line_str(l, w, o.color));
            }
            s
        } else {
            ui::ansi_frame(app, w, h, interval.as_secs_f64(), o.color)
        };
        if trace && loops % 5 == 1 {
            eprintln!("trace boucle={loops} w={w} h={h} filtering={filtering} paused={} frames={}", app.paused, app.frames_done);
        }
        term.w(&frame);
        term.flush_frame();

        // sommeil jusqu'au prochain echantillon, en ecoutant le clavier
        let mut slept = Duration::ZERO;
        while slept < interval {
            std::thread::sleep(Duration::from_millis(15));
            slept += Duration::from_millis(15);
            let keys = term.poll_keys();
            if trace && !keys.is_empty() {
                eprintln!("trace boucle={loops} slept={:?} keys={:?}", slept, keys);
            }
            for k in keys {
                match handle(app, k, &mut interval, &mut help, &mut filtering) {
                    Action::Quit => {
                        term.leave();
                        return;
                    }
                    Action::Redraw => {
                        next = Instant::now();
                        slept = Duration::ZERO;
                        break;
                    }
                    Action::Nothing => {}
                }
            }
        }
    }
}

enum Action {
    Quit,
    Redraw,
    Nothing,
}

fn handle(app: &mut App, k: Key, interval: &mut Duration, help: &mut bool, filtering: &mut bool) -> Action {
    // mode filtre : tout ce qui est tape va dans app.filter
    if *filtering {
        match k {
            Key::Enter | Key::Esc => *filtering = false,
            Key::Del => {
                app.filter.pop();
            }
            Key::Char(c) => app.filter.push(c),
            _ => {}
        }
        return Action::Redraw;
    }
    match k {
        Key::Char('q') | Key::Char('Q') | Key::Ctrl('c') | Key::Ctrl('d') => return Action::Quit,
        Key::Char('?') | Key::Char('h') if !matches!(k, Key::Char('h')) => *help = !*help,
        Key::Char('?') => *help = !*help,
        Key::Char('h') => {
            app.view = match VIEWS_idx(app.view) {
                0 => View::Driver,
                n => VIEWS[n - 1],
            };
            app.scroll = 0;
        }
        Key::Char('l') => {
            app.view = app.view.next();
            app.scroll = 0;
        }
        Key::Right => {
            app.view = app.view.next();
            app.scroll = 0;
        }
        Key::Left => {
            app.view = match VIEWS_idx(app.view) {
                0 => View::Driver,
                n => VIEWS[n - 1],
            };
            app.scroll = 0;
        }
        Key::Esc => {
            *help = false;
            if !app.filter.is_empty() {
                app.filter.clear();
            }
        }
        Key::Char('0') => {
            app.view = View::Overview;
            app.scroll = 0;
        }
        Key::Char(c @ '1'..='8') => {
            let n = (c as usize) - ('1' as usize);
            if n < app.gpus.len() {
                app.focus = n;
                app.scroll = 0;
            }
        }
        Key::Char('\t') => {
            if app.gpus.len() > 1 {
                app.focus = (app.focus + 1) % app.gpus.len();
                app.scroll = 0;
            }
        }
        Key::Char('i') => app.view = View::Identity,
        Key::Char('m') => app.view = View::Memory,
        Key::Char('p') => app.view = View::Power,
        Key::Char('x') => app.view = View::Pcie,
        Key::Char('e') => app.view = View::Health,
        Key::Char('d') => app.view = View::Driver,
        Key::Char('s') => {
            app.sort = match app.sort {
                SortKey::Mem => SortKey::Pid,
                SortKey::Pid => SortKey::Cpu,
                SortKey::Cpu => SortKey::Name,
                SortKey::Name => SortKey::Mem,
            };
            let (k, d) = (app.sort, app.sort_desc);
            if let Some(f) = app.gpus.get_mut(app.focus).and_then(|g| g.frame.as_mut()) {
                sort_rows(f.procs.as_mut_slice(), k, d);
            }
        }
        Key::Char('S') => {
            app.sort_desc = !app.sort_desc;
            let (k, d) = (app.sort, app.sort_desc);
            if let Some(f) = app.gpus.get_mut(app.focus).and_then(|g| g.frame.as_mut()) {
                sort_rows(f.procs.as_mut_slice(), k, d);
            }
        }
        Key::Char('/') => {
            app.filter.clear();
            *filtering = true;
        }
        Key::Char(' ') => app.paused = !app.paused,
        Key::Char('+') | Key::Char('=') => {
            *interval = Duration::from_secs_f64((interval.as_secs_f64() / 2.0).clamp(0.05, 20.0));
            return Action::Redraw;
        }
        Key::Char('-') | Key::Char('_') => {
            *interval = Duration::from_secs_f64((interval.as_secs_f64() * 2.0).clamp(0.05, 20.0));
            return Action::Redraw;
        }
        Key::Char('c') => {
            crate::ui::toggle_color();
        }
        Key::Char('z') => app.mask = !app.mask,
        Key::Down | Key::Char('j') => app.scroll = app.scroll.saturating_add(1),
        Key::Up | Key::Char('k') => app.scroll = app.scroll.saturating_sub(1),
        Key::WheelDown => app.scroll = app.scroll.saturating_add(3),
        Key::WheelUp => app.scroll = app.scroll.saturating_sub(3),
        Key::PgDn => app.scroll = app.scroll.saturating_add(10),
        Key::PgUp => app.scroll = app.scroll.saturating_sub(10),
        Key::Home => app.scroll = 0,
        _ => {}
    }
    Action::Nothing
}

fn VIEWS_idx(v: View) -> usize {
    VIEWS.iter().position(|x| *x == v).unwrap_or(0)
}
