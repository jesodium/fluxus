use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::cli::{self, Attached, Board, Detected, Port};
use crate::project::{self, Target};
use crate::settings::{self, Settings};
use crate::sketch;

pub const BAUDS: [u32; 15] = [
    300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 74880, 115200, 230400, 250000, 500000, 1000000, 2000000,
];
const SERIAL_CAP: usize = 5000;

pub enum AppEvent {
    Ports(Result<Vec<Detected>, String>),
    AllBoards(Result<Vec<Board>, String>),
    Log(String),
    Progress(String),
    JobDone(Result<bool, String>),
    Serial(u64, String),
    SerialDone(u64, Result<(), String>),
    Defaults(Attached),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Build,
    Monitor,
    Project,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Build, Tab::Monitor, Tab::Project, Tab::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Build => "Build",
            Tab::Monitor => "Monitor",
            Tab::Project => "Project",
            Tab::Settings => "Settings",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    AddBoard,
    Board,
    Sketch,
    Port,
}

pub struct Picker {
    pub kind: Pick,
    pub target: usize,
    pub query: String,
    pub sel: usize,
}

pub struct Job {
    pub name: String,
    started: Instant,
    handle: JoinHandle<()>,
}

pub struct Pane {
    pub lines: Vec<String>,
    pub scroll: usize,
    pub baud: u32,
    pub link: Option<Link>,
}

pub struct Link {
    pub port: String,
    id: u64,
    input: UnboundedSender<String>,
    handle: JoinHandle<()>,
}

pub struct App {
    tx: UnboundedSender<AppEvent>,
    pub cli_version: String,
    pub root: PathBuf,
    root_is_sketch: bool,
    pub project_err: Option<String>,
    pub tab: Tab,

    pub boards: Vec<Target>,
    pub sketches: Vec<String>,
    sketch_fqbn: HashMap<String, String>,
    pending_port: Option<String>,
    pub active: usize,
    pub sel: usize,
    pub ports: Vec<Detected>,
    pub ports_err: Option<String>,
    pub scanned: bool,
    pub all_boards: Option<Result<Vec<Board>, String>>,
    pub picker: Option<Picker>,

    pub log: Vec<String>,
    pub log_scroll: usize,
    redrawing: bool,
    pub job: Option<Job>,
    queue: VecDeque<usize>,

    pub panes: Vec<Pane>,
    mon_gen: u64,
    resume: Vec<usize>,
    pub input: Option<String>,

    pub settings: Settings,
    pub set_sel: usize,

    pub quit: bool,
}

impl App {
    pub fn new(cli_version: String, root: PathBuf, tx: UnboundedSender<AppEvent>) -> Self {
        let (boards, project_err) = match project::load(&root) {
            Ok(b) => (b, None),
            Err(e) => (vec![], Some(format!("{e:#}"))),
        };
        let sketches = sketch::scan(&root);
        let sketch_fqbn = sketches
            .iter()
            .filter_map(|s| Some((s.clone(), sketch::yaml_value(&sketch::folder(&root, s)?, "default_fqbn")?)))
            .collect();
        let mut app = Self {
            tx,
            cli_version,
            root_is_sketch: sketches == ["."],
            root,
            project_err,
            tab: if boards.is_empty() { Tab::Project } else { Tab::Build },
            boards,
            sketches,
            sketch_fqbn,
            pending_port: None,
            active: 0,
            sel: 0,
            ports: vec![],
            ports_err: None,
            scanned: false,
            all_boards: None,
            picker: None,
            log: vec![],
            log_scroll: 0,
            redrawing: false,
            job: None,
            queue: VecDeque::new(),
            panes: vec![],
            mon_gen: 0,
            resume: vec![],
            input: None,
            settings: settings::load(),
            set_sel: 0,
            quit: false,
        };
        for i in 0..app.boards.len() {
            app.new_pane(i);
        }
        app
    }

    pub fn wants_defaults(&self) -> bool {
        self.root_is_sketch && self.boards.is_empty()
    }

    // -- events --

    pub fn event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Ports(Ok(mut p)) => {
                p.sort_by(|a, b| a.port.address.cmp(&b.port.address));
                self.ports = p;
                self.ports_err = None;
                self.scanned = true;
                self.resume_ready();
            }
            AppEvent::Ports(Err(e)) => self.ports_err = Some(e),
            AppEvent::AllBoards(r) => {
                self.all_boards = Some(r);
                for i in 0..self.boards.len() {
                    if self.boards[i].name == self.boards[i].fqbn {
                        self.boards[i].name = self.board_name(&self.boards[i].fqbn);
                    }
                }
            }
            AppEvent::Defaults(d) => {
                if self.wants_defaults() && !d.fqbn.is_empty() {
                    self.boards.push(Target {
                        name: self.board_name(&d.fqbn),
                        fqbn: d.fqbn,
                        port: d.port.map(|p| p.address),
                        sketch: Some(".".into()),
                    });
                    self.new_pane(self.boards.len() - 1);
                    self.tab = Tab::Build;
                }
            }
            AppEvent::Log(line) => {
                let line = line
                    .replace(&format!("{}/", self.root.display()), "")
                    .replace(&format!("{}/", sketch::scratch().display()), "");
                // the last redraw of a progress bar is the one worth keeping
                if self.redrawing && !line.is_empty() {
                    self.log.pop();
                }
                self.redrawing = false;
                self.log.push(line);
                if self.log_scroll > 0 {
                    self.log_scroll += 1;
                }
            }
            AppEvent::Progress(line) => {
                if std::mem::replace(&mut self.redrawing, true) {
                    self.log.pop();
                } else if self.log_scroll > 0 {
                    self.log_scroll += 1;
                }
                self.log.push(line);
            }
            AppEvent::JobDone(r) => {
                let Some(job) = self.job.take() else { return };
                self.redrawing = false;
                let secs = job.started.elapsed().as_secs_f32();
                let ok = r.as_ref().ok().copied();
                self.log.push(match r {
                    Ok(true) => format!("✓ {} done in {secs:.1}s", job.name),
                    Ok(false) => format!("✗ {} failed ({secs:.1}s)", job.name),
                    Err(e) => format!("✗ {}: {e}", job.name),
                });
                if job.name.starts_with("install") && ok == Some(true) {
                    self.log.push("press a to pick your board".into());
                    self.fetch_boards();
                } else if ok == Some(false) && self.log.iter().any(|l| l.contains("platform not installed")) {
                    self.log.push("press I to install the missing core".into());
                }
                self.next_upload();
            }
            AppEvent::Serial(id, chunk) => {
                if let Some(i) = self.pane_of(id) {
                    self.panes[i].push(&chunk);
                }
            }
            AppEvent::SerialDone(id, r) => {
                let Some(i) = self.pane_of(id) else { return };
                self.panes[i].link = None;
                if i == self.active {
                    self.input = None;
                }
                self.panes[i].note(match r {
                    Ok(()) => "── disconnected ──".into(),
                    Err(e) => format!("── disconnected: {e} ──"),
                });
            }
        }
    }

    // -- lookups --

    pub fn target(&self) -> Option<&Target> {
        self.boards.get(self.active)
    }

    pub fn links(&self) -> Vec<Option<usize>> {
        project::resolve(&self.boards, &self.ports)
    }

    pub fn port_of(&self, i: usize) -> Option<&Detected> {
        self.links().get(i).copied().flatten().map(|j| &self.ports[j])
    }

    pub fn free_ports(&self) -> Vec<usize> {
        let links = self.links();
        (0..self.ports.len()).filter(|j| !links.contains(&Some(*j))).collect()
    }

    pub fn no_boards(&self) -> String {
        // skip bluetooth/debug leftovers, they're never boards
        let boards = self
            .ports
            .iter()
            .filter(|d| d.port.protocol_label.contains("USB") || d.matching_boards.iter().any(|b| !b.fqbn.is_empty()))
            .count();
        match boards {
            0 => "no boards in project — plug one in, or press a to add one".into(),
            1 => "detected 1 board — press 3, then enter to add it".into(),
            n => format!("detected {n} boards — press 3, then enter to add them"),
        }
    }

    pub fn on_port_row(&self) -> bool {
        self.tab == Tab::Project && self.sel >= self.boards.len()
    }

    fn board_name(&self, fqbn: &str) -> String {
        let Some(Ok(all)) = &self.all_boards else { return fqbn.into() };
        all.iter().find(|b| b.fqbn == fqbn).map_or(fqbn.into(), |b| b.name.clone())
    }

    pub fn picks(&self) -> Vec<(String, String)> {
        let Some(p) = &self.picker else { return vec![] };
        let items: Vec<(String, String)> = match p.kind {
            Pick::AddBoard | Pick::Board => match &self.all_boards {
                Some(Ok(all)) => {
                    let mut v: Vec<&Board> = all.iter().collect();
                    v.sort_by_key(|b| b.fqbn.is_empty());
                    v.into_iter()
                        .map(|b| match b.fqbn.as_str() {
                            "" => (b.name.clone(), format!("install {}", b.core)),
                            f => (b.name.clone(), f.into()),
                        })
                        .collect()
                }
                _ => vec![],
            },
            Pick::Sketch => {
                let fqbn = self.boards.get(p.target).map(|t| t.fqbn.as_str());
                let mut v: Vec<(String, String)> = self
                    .sketches
                    .iter()
                    .map(|s| {
                        let hit = fqbn.is_some() && self.sketch_fqbn.get(s).map(String::as_str) == fqbn;
                        (s.clone(), if hit { "made for this board".into() } else { String::new() })
                    })
                    .collect();
                v.sort_by_key(|(_, d)| d.is_empty());
                v
            }
            Pick::Port => self
                .ports
                .iter()
                .map(|d| {
                    let names: Vec<_> = d.matching_boards.iter().map(|b| b.name.as_str()).collect();
                    (d.port.address.clone(), names.join(", "))
                })
                .collect(),
        };
        let q = p.query.to_lowercase();
        items
            .into_iter()
            .filter(|(a, b)| a.to_lowercase().contains(&q) || b.to_lowercase().contains(&q))
            .collect()
    }

    // -- keys --

    pub fn key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.picker.is_some() {
            return self.picker_key(k);
        }
        if self.input.is_some() {
            return self.input_key(k);
        }
        let focus = if self.tab == Tab::Project { self.sel } else { self.active };
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('1') => self.tab = Tab::Build,
            KeyCode::Char('2') => self.tab = Tab::Monitor,
            KeyCode::Char('3') => self.tab = Tab::Project,
            KeyCode::Char('4') => self.tab = Tab::Settings,
            KeyCode::Tab => self.cycle_tab(1),
            KeyCode::BackTab => self.cycle_tab(Tab::ALL.len() - 1),
            KeyCode::Char('a') => self.open(Pick::AddBoard, 0),
            KeyCode::Char('b') if focus >= self.boards.len() => self.open(Pick::AddBoard, 0),
            KeyCode::Char('b') => self.open(Pick::Board, focus),
            KeyCode::Char('s') if focus < self.boards.len() => self.open(Pick::Sketch, focus),
            KeyCode::Char('p') if focus < self.boards.len() => self.open(Pick::Port, focus),
            KeyCode::Char('c') => self.compile(),
            KeyCode::Char('u') => self.upload(),
            KeyCode::Char('F') => self.flash_all(),
            KeyCode::Char('I') => {
                if let Some(core) = self.target().map(|t| t.fqbn.splitn(3, ':').take(2).collect::<Vec<_>>().join(":")) {
                    self.install_core(core);
                }
            }
            KeyCode::Char('m') => self.toggle_monitor(),
            KeyCode::Esc if self.job.is_some() => self.cancel(),
            _ => match self.tab {
                Tab::Build => scroll(&mut self.log_scroll, self.log.len(), k.code),
                Tab::Monitor => self.monitor_key(k),
                Tab::Project => self.project_key(k),
                Tab::Settings => self.settings_key(k),
            },
        }
    }

    fn cycle_tab(&mut self, by: usize) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + by) % Tab::ALL.len()];
    }

    fn project_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let rows = self.boards.len() + self.free_ports().len();
                self.sel = (self.sel + 1).min(rows.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Enter if self.sel >= self.boards.len() => {
                if let Some(&j) = self.free_ports().get(self.sel - self.boards.len()) {
                    self.add_from_port(j);
                }
            }
            KeyCode::Enter => self.focus(self.sel),
            KeyCode::Char('d') if self.sel < self.boards.len() => {
                self.stop_monitor(self.sel);
                self.panes.remove(self.sel);
                let sel = self.sel;
                self.resume.retain(|&r| r != sel);
                self.resume.iter_mut().filter(|r| **r > sel).for_each(|r| *r -= 1);
                self.boards.remove(self.sel);
                if self.active > self.sel || self.active >= self.boards.len() {
                    self.active = self.active.saturating_sub(1);
                }
                self.sel = self.sel.min(self.boards.len().saturating_sub(1));
                self.save(None);
            }
            _ => {}
        }
    }

    fn settings_key(&mut self, k: KeyEvent) {
        let by = match k.code {
            KeyCode::Down | KeyCode::Char('j') => return self.set_sel = (self.set_sel + 1).min(settings::ROWS - 1),
            KeyCode::Up | KeyCode::Char('k') => return self.set_sel = self.set_sel.saturating_sub(1),
            KeyCode::Left | KeyCode::Char('h') => -1,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter | KeyCode::Char(' ') => 1,
            _ => return,
        };
        self.settings.step(self.set_sel, by);
        if let Err(e) = settings::save(&self.settings) {
            self.log.push(format!("✗ couldn't save settings: {e:#}"));
        }
    }

    fn monitor_key(&mut self, k: KeyEvent) {
        let Some(p) = self.panes.get_mut(self.active) else { return };
        match k.code {
            KeyCode::Char('i') | KeyCode::Enter if p.link.is_some() => self.input = Some(String::new()),
            KeyCode::Char('+') | KeyCode::Char('=') => self.step_baud(1),
            KeyCode::Char('-') => self.step_baud(-1),
            KeyCode::Left | KeyCode::Char('h') => self.cycle_pane(-1),
            KeyCode::Right | KeyCode::Char('l') => self.cycle_pane(1),
            KeyCode::Char('x') => {
                p.lines.clear();
                p.scroll = 0;
            }
            code => scroll(&mut p.scroll, p.lines.len(), code),
        }
    }

    fn input_key(&mut self, k: KeyEvent) {
        let Some(buf) = self.input.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            KeyCode::Enter => {
                let line = std::mem::take(buf);
                if let Some(p) = self.panes.get_mut(self.active)
                    && let Some(l) = &p.link
                {
                    let _ = l.input.send(line.clone() + self.settings.eol());
                    p.note(format!("→ {line}"));
                }
            }
            _ => {}
        }
    }

    // -- pickers --

    fn open(&mut self, kind: Pick, target: usize) {
        self.picker = Some(Picker { kind, target, query: String::new(), sel: 0 });
    }

    fn picker_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => {
                self.picker = None;
                self.pending_port = None;
            }
            KeyCode::Enter => {
                let Some(p) = &self.picker else { return };
                let Some((label, detail)) = self.picks().into_iter().nth(p.sel) else { return };
                let (kind, i) = (p.kind, p.target);
                self.picker = None;
                self.apply(kind, i, label, detail);
            }
            code => {
                let n = self.picks().len();
                let Some(p) = self.picker.as_mut() else { return };
                match code {
                    KeyCode::Down => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Up => p.sel = p.sel.saturating_sub(1),
                    KeyCode::Backspace => {
                        p.query.pop();
                        p.sel = 0;
                    }
                    KeyCode::Char(c) => {
                        p.query.push(c);
                        p.sel = 0;
                    }
                    _ => {}
                }
            }
        }
    }

    fn apply(&mut self, kind: Pick, i: usize, label: String, detail: String) {
        if let Some(core) = detail.strip_prefix("install ") {
            self.pending_port = None;
            return self.install_core(core.into());
        }
        match kind {
            Pick::AddBoard => {
                let port = self.pending_port.take();
                self.add_board(label, detail, port);
            }
            Pick::Board => {
                if let Some(t) = self.boards.get_mut(i) {
                    t.name = label;
                    t.fqbn = detail;
                    self.save(Some(i));
                }
            }
            Pick::Sketch => self.set_sketch(i, label),
            Pick::Port => {
                if let Some(t) = self.boards.get_mut(i) {
                    t.port = Some(label);
                    self.save(Some(i));
                }
            }
        }
    }

    fn add_from_port(&mut self, j: usize) {
        let d = &self.ports[j];
        let port = Some(d.port.address.clone());
        match d.matching_boards.iter().find(|b| !b.fqbn.is_empty()) {
            Some(b) => self.add_board(b.name.clone(), b.fqbn.clone(), port),
            None => {
                self.pending_port = port;
                self.open(Pick::AddBoard, 0);
            }
        }
    }

    fn add_board(&mut self, name: String, fqbn: String, port: Option<String>) {
        let made_for: Vec<String> =
            self.sketches.iter().filter(|s| self.sketch_fqbn.get(*s) == Some(&fqbn)).cloned().collect();
        self.boards.push(Target { name, fqbn, port, sketch: None });
        let i = self.boards.len() - 1;
        self.new_pane(i);
        self.sel = i;
        self.tab = Tab::Project;
        if i == 0 {
            self.active = 0;
        }
        match made_for.as_slice() {
            [only] => self.set_sketch(i, only.clone()),
            _ => {
                self.save(None);
                self.open(Pick::Sketch, i);
            }
        }
    }

    fn set_sketch(&mut self, i: usize, rel: String) {
        let dir = sketch::folder(&self.root, &rel);
        let Some(t) = self.boards.get_mut(i) else { return };
        if t.port.is_none() {
            t.port = dir.and_then(|d| sketch::yaml_value(&d, "default_port"));
        }
        t.sketch = Some(rel);
        self.load_baud(i);
        self.save(Some(i));
    }

    // -- saving --

    fn save(&mut self, mirror: Option<usize>) {
        self.save_with(mirror, false);
    }

    fn save_with(&mut self, mirror: Option<usize>, with_baud: bool) {
        if self.project_err.is_some() {
            return self.log.push("✗ fluxus.yaml didn't parse, not overwriting it — fix it by hand".into());
        }
        let single = self.root_is_sketch && self.boards.len() <= 1;
        if !single && let Err(e) = project::save(&self.root, &self.boards) {
            self.log.push(format!("✗ couldn't save fluxus.yaml: {e:#}"));
        }
        let Some(i) = mirror else { return };
        let Some(t) = self.boards.get(i).cloned() else { return };
        let Some(rel) = t.sketch.clone() else { return };
        let Some(dir) = sketch::folder(&self.root, &rel) else { return };
        self.sketch_fqbn.insert(rel, t.fqbn.clone());
        let port = self.port_of(i).map(|d| d.port.clone()).or(t.port.map(|address| Port { address, ..Default::default() }));
        let baud = with_baud.then(|| self.panes[i].baud);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _held = LOCK.lock().await;
            let r = async {
                cli::attach(&dir, Some(&t.fqbn), port.as_ref()).await?;
                match baud {
                    Some(b) => sketch::save_baud(&dir, b),
                    None => Ok(()),
                }
            };
            if let Err(e) = r.await {
                let _ = tx.send(AppEvent::Log(format!("✗ couldn't save sketch.yaml: {e:#}")));
            }
        });
    }

    fn load_baud(&mut self, i: usize) {
        let dir = self.boards.get(i).and_then(|t| t.sketch.as_deref()).and_then(|s| sketch::folder(&self.root, s));
        if let (Some(b), Some(p)) = (dir.and_then(|d| sketch::load_baud(&d)), self.panes.get_mut(i)) {
            p.baud = b;
        }
    }

    // -- build jobs --

    fn prepared(&mut self, i: usize) -> Option<(String, PathBuf, String)> {
        let Some(t) = self.boards.get(i) else {
            self.log.push(format!("✗ {}", self.no_boards()));
            return None;
        };
        let Some(rel) = t.sketch.clone() else {
            self.log.push(format!("✗ {} has no sketch — press s", t.name));
            return None;
        };
        let (name, fqbn) = (t.name.clone(), t.fqbn.clone());
        match sketch::build_dir(&self.root, &rel) {
            Ok(dir) => Some((name, dir, fqbn)),
            Err(e) => {
                self.log.push(format!("✗ {name}: {e:#}"));
                None
            }
        }
    }

    fn compile(&mut self) {
        self.tab = Tab::Build;
        if self.job.is_some() {
            return;
        }
        self.log.clear();
        self.log_scroll = 0;
        let Some((name, dir, fqbn)) = self.prepared(self.active) else { return };
        let mut args: Vec<OsString> = vec!["compile".into(), "-b".into(), fqbn.into(), dir.into()];
        if self.settings.verbose {
            args.push("-v".into());
        }
        self.run_job(format!("compile {name}"), args);
    }

    fn install_core(&mut self, core: String) {
        self.tab = Tab::Build;
        if self.job.is_some() {
            return;
        }
        self.log.clear();
        self.log_scroll = 0;
        self.run_job(format!("install {core}"), vec!["core".into(), "install".into(), core.into()]);
    }

    pub fn fetch_boards(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = cli::board_search().await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::AllBoards(r));
        });
    }

    fn upload(&mut self) {
        self.start_uploads(vec![self.active]);
    }

    fn flash_all(&mut self) {
        let links = self.links();
        let with_sketch: Vec<usize> = (0..self.boards.len()).filter(|&i| self.boards[i].sketch.is_some()).collect();
        if !with_sketch.iter().any(|&i| links[i].is_some()) {
            self.tab = Tab::Build;
            return self.log.push("✗ flash all: no connected boards with a sketch".into());
        }
        self.start_uploads(with_sketch);
    }

    fn start_uploads(&mut self, which: Vec<usize>) {
        self.tab = Tab::Build;
        if self.job.is_some() {
            return;
        }
        self.log.clear();
        self.log_scroll = 0;
        // dont touch, port stays busy otherwise
        // IMPORTANT NOTE: pauses every pane, not just the ones being flashed
        self.resume.extend((0..self.panes.len()).filter(|&i| self.panes[i].link.is_some()));
        self.stop_monitors();
        self.queue = which.into();
        self.next_upload();
    }

    fn next_upload(&mut self) {
        while let Some(i) = self.queue.pop_front() {
            let Some((name, dir, fqbn)) = self.prepared(i) else { continue };
            let Some(port) = self.port_of(i).map(|d| d.port.clone()) else {
                self.log.push(format!("✗ {name} isn't connected — plug it in or press p"));
                continue;
            };
            let mut args: Vec<OsString> = vec!["compile".into(), "-u".into(), "-b".into(), fqbn.into()];
            args.extend(["-p".into(), port.address.into()]);
            if !port.protocol.is_empty() {
                args.extend(["-l".into(), port.protocol.into()]);
            }
            if self.settings.verbose {
                args.push("-v".into());
            }
            args.push(dir.into());
            return self.run_job(format!("upload {name}"), args);
        }
    }

    // a flashed board reboots, so wait for its port to come back before reopening
    fn resume_ready(&mut self) {
        if self.job.is_some() || !self.queue.is_empty() {
            return;
        }
        for i in std::mem::take(&mut self.resume) {
            match self.port_of(i).is_some() {
                true => self.start_monitor(i),
                false => self.resume.push(i),
            }
        }
    }

    fn run_job(&mut self, name: String, args: Vec<OsString>) {
        self.redrawing = false;
        let shown: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        self.event(AppEvent::Log(format!("$ arduino-cli {}", shown.join(" "))));
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            let r = cli::stream(args, &tx).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::JobDone(r));
        });
        self.job = Some(Job { name, started: Instant::now(), handle });
    }

    fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.handle.abort();
            self.log.push(format!("✗ {} cancelled", job.name));
            self.queue.clear();
            self.next_upload();
        }
    }

    // -- serial monitor --

    fn toggle_monitor(&mut self) {
        self.tab = Tab::Monitor;
        if self.panes.iter().any(|p| p.link.is_some()) || !self.resume.is_empty() {
            self.resume.clear();
            return self.stop_monitors();
        }
        let connected: Vec<usize> = (0..self.boards.len()).filter(|&i| self.port_of(i).is_some()).collect();
        if connected.is_empty() {
            if let Some(p) = self.panes.get_mut(self.active) {
                p.note("── no connected boards ──".into());
            }
        } else if !self.queue.is_empty() || self.job.as_ref().is_some_and(|j| j.name.starts_with("upload")) {
            self.resume = connected;
        } else {
            connected.into_iter().for_each(|i| self.start_monitor(i));
        }
    }

    fn start_monitor(&mut self, i: usize) {
        if self.panes.get(i).is_none_or(|p| p.link.is_some()) {
            return;
        }
        let Some(port) = self.port_of(i).map(|d| d.port.clone()) else {
            return self.panes[i].note("── not connected ──".into());
        };
        self.mon_gen += 1;
        let (id, baud, tx) = (self.mon_gen, self.panes[i].baud, self.tx.clone());
        let (input, rx) = mpsc::unbounded_channel();
        self.panes[i].note(format!("── {} @ {baud} ──", port.address));
        let address = port.address.clone();
        let handle = tokio::spawn(async move {
            let r = cli::monitor(id, port, baud, rx, tx.clone()).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::SerialDone(id, r));
        });
        self.panes[i].link = Some(Link { port: address, id, input, handle });
    }

    fn stop_monitor(&mut self, i: usize) {
        let Some(l) = self.panes.get_mut(i).and_then(|p| p.link.take()) else { return };
        l.handle.abort();
        if i == self.active {
            self.input = None;
        }
        self.panes[i].note("── closed ──".into());
    }

    fn stop_monitors(&mut self) {
        (0..self.panes.len()).for_each(|i| self.stop_monitor(i));
    }

    fn new_pane(&mut self, i: usize) {
        self.panes.insert(i, Pane { lines: vec![], scroll: 0, baud: self.settings.baud, link: None });
        self.load_baud(i);
    }

    fn pane_of(&self, id: u64) -> Option<usize> {
        self.panes.iter().position(|p| p.link.as_ref().is_some_and(|l| l.id == id))
    }

    pub fn shown_panes(&self) -> Vec<usize> {
        (0..self.panes.len()).filter(|&i| i == self.active || self.panes[i].link.is_some()).collect()
    }

    fn focus(&mut self, i: usize) {
        if i != self.active {
            self.input = None;
        }
        self.active = i;
    }

    fn cycle_pane(&mut self, by: isize) {
        let mut shown = self.shown_panes();
        if shown.len() <= 1 {
            shown = (0..self.panes.len()).collect();
        }
        let Some(at) = shown.iter().position(|&i| i == self.active) else { return };
        let n = shown.len() as isize;
        self.focus(shown[((at as isize + by).rem_euclid(n)) as usize]);
    }

    fn step_baud(&mut self, by: isize) {
        let i = self.active;
        let Some(p) = self.panes.get_mut(i) else { return };
        let at = BAUDS.iter().position(|b| *b == p.baud).unwrap_or(4) as isize;
        p.baud = BAUDS[(at + by).clamp(0, BAUDS.len() as isize - 1) as usize];
        let open = p.link.is_some();
        self.save_with(Some(i), true);
        if open {
            self.stop_monitor(i);
            self.start_monitor(i);
        }
    }
}

impl Pane {
    fn push(&mut self, chunk: &str) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        for c in chunk.chars() {
            match c {
                '\n' => {
                    self.lines.push(String::new());
                    if self.scroll > 0 {
                        self.scroll += 1;
                    }
                }
                // junk from a board that is still rebooting would scramble the pane
                c if c.is_control() => {}
                c => self.lines.last_mut().expect("non-empty").push(c),
            }
        }
        if self.lines.len() > SERIAL_CAP {
            self.lines.drain(..self.lines.len() - SERIAL_CAP);
        }
    }

    fn note(&mut self, note: String) {
        if self.lines.last().is_some_and(|l| !l.is_empty()) {
            self.push("\n");
        }
        self.push(&(note + "\n"));
    }
}

// -- misc --

fn scroll(pos: &mut usize, max: usize, code: KeyCode) {
    *pos = match code {
        KeyCode::Up | KeyCode::Char('k') => (*pos + 1).min(max),
        KeyCode::Down | KeyCode::Char('j') => pos.saturating_sub(1),
        KeyCode::PageUp => (*pos + 20).min(max),
        KeyCode::PageDown => pos.saturating_sub(20),
        KeyCode::Char('g') => max,
        KeyCode::Char('G') => 0,
        _ => *pos,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_chunks_join_into_lines() {
        let mut p = Pane { lines: vec![], scroll: 0, baud: 9600, link: None };
        for chunk in ["tem", "p: 21\r\nhum", "id: 40\r\n", "no newline"] {
            p.push(chunk);
        }
        assert_eq!(p.lines, ["temp: 21", "humid: 40", "no newline"]);
        p.note("── closed ──".into());
        assert_eq!(p.lines[3..], ["── closed ──", ""]);
    }

    #[tokio::test]
    async fn progress_redraws_in_place_and_resume_waits_for_the_port() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(String::new(), std::env::temp_dir().join("fluxus-none"), tx);
        app.event(AppEvent::Log("$ arduino-cli compile -u".into()));
        for pct in [10, 55, 100] {
            app.event(AppEvent::Progress(format!("Writing | ## | {pct}%")));
        }
        app.event(AppEvent::Log(String::new()));
        app.event(AppEvent::Log("Done".into()));
        assert_eq!(app.log, ["$ arduino-cli compile -u", "Writing | ## | 100%", "", "Done"]);

        app.boards.push(Target { name: "uno".into(), fqbn: "x:y:z".into(), port: Some("/dev/uno".into()), sketch: None });
        app.new_pane(0);
        app.resume = vec![0];
        app.event(AppEvent::Ports(Ok(vec![])));
        assert!(app.panes[0].link.is_none(), "port still gone");
        let back = Detected { port: Port { address: "/dev/uno".into(), ..Default::default() }, matching_boards: vec![] };
        app.event(AppEvent::Ports(Ok(vec![back])));
        assert!(app.panes[0].link.is_some(), "port back");
        assert!(app.resume.is_empty());
    }

    #[tokio::test]
    async fn each_board_gets_its_own_pane() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(String::new(), std::env::temp_dir().join("fluxus-none"), tx);
        let port = |a: &str| Port { address: a.into(), ..Default::default() };
        app.event(AppEvent::Ports(Ok(vec![
            Detected { port: port("/dev/a"), matching_boards: vec![] },
            Detected { port: port("/dev/b"), matching_boards: vec![] },
        ])));
        for a in ["/dev/a", "/dev/b", "/dev/gone"] {
            app.boards.push(Target { name: a.into(), fqbn: "x:y:z".into(), port: Some(a.into()), sketch: None });
            app.new_pane(app.boards.len() - 1);
        }
        app.toggle_monitor();
        let ids: Vec<Option<u64>> = app.panes.iter().map(|p| p.link.as_ref().map(|l| l.id)).collect();
        assert_eq!(ids, [Some(1), Some(2), None]);
        assert_eq!(app.shown_panes(), [0, 1]);

        app.event(AppEvent::Serial(2, "rx on b\n".into()));
        app.event(AppEvent::Serial(99, "stale\n".into()));
        assert!(app.panes[1].lines.contains(&"rx on b".to_string()));
        assert!(!app.panes.iter().any(|p| p.lines.contains(&"stale".to_string())));

        app.cycle_pane(1);
        assert_eq!(app.active, 1);
        app.cycle_pane(1);
        assert_eq!(app.active, 0);
        app.event(AppEvent::SerialDone(1, Ok(())));
        assert!(app.panes[0].link.is_none() && app.panes[1].link.is_some());

        app.toggle_monitor();
        assert!(app.panes.iter().all(|p| p.link.is_none()));
        app.cycle_pane(1);
        assert_eq!(app.active, 1);
        app.cycle_pane(1);
        assert_eq!(app.active, 2);
    }

    #[tokio::test]
    async fn detected_port_adds_board_and_guesses_sketch() {
        let root = std::env::temp_dir().join(format!("fluxus-add-{}", std::process::id()));
        for (dir, fqbn) in [("giga-r1/main", "arduino:mbed_giga:giga"), ("cam/main", "esp32:esp32:esp32cam"), ("cam/test", "esp32:esp32:esp32cam")] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            let name = dir.rsplit('/').next().unwrap();
            std::fs::write(root.join(dir).join(format!("{name}.ino")), "").unwrap();
            std::fs::write(root.join(dir).join("sketch.yaml"), format!("default_fqbn: {fqbn}\n")).unwrap();
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(String::new(), root.clone(), tx);
        let port = |a: &str| Port { address: a.into(), ..Default::default() };
        let giga = Board { name: "Arduino Giga R1".into(), fqbn: "arduino:mbed_giga:giga".into(), core: String::new() };
        app.event(AppEvent::Ports(Ok(vec![
            Detected { port: port("/dev/cu.usbmodem1101"), matching_boards: vec![giga] },
            Detected { port: port("/dev/cu.usbserial-110"), matching_boards: vec![] },
        ])));
        assert_eq!(app.free_ports(), [0, 1]);

        app.add_from_port(0);
        assert_eq!(app.boards[0].sketch.as_deref(), Some("giga-r1/main"));
        assert_eq!(app.boards[0].port.as_deref(), Some("/dev/cu.usbmodem1101"));
        assert!(app.picker.is_none());
        assert_eq!(app.free_ports(), [1]);

        app.add_from_port(1);
        assert!(app.picker.as_ref().is_some_and(|p| p.kind == Pick::AddBoard));
        app.apply(Pick::AddBoard, 0, "AI Thinker ESP32-CAM".into(), "esp32:esp32:esp32cam".into());
        assert_eq!(app.boards[1].port.as_deref(), Some("/dev/cu.usbserial-110"));
        assert!(app.picker.as_ref().is_some_and(|p| p.kind == Pick::Sketch));
        let picks: Vec<_> = app.picks().into_iter().map(|(s, _)| s).collect();
        assert_eq!(picks, ["cam/main", "cam/test", "giga-r1/main"]);
        std::fs::remove_dir_all(root).unwrap();
    }
}
