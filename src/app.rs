use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::cli::{self, Attached, Board, Detected, Port};
use crate::project::{self, Target};
use crate::sketch;

pub const BAUDS: [u32; 15] = [
    300, 1200, 2400, 4800, 9600, 19200, 38400, 57600, 74880, 115200, 230400, 250000, 500000, 1000000, 2000000,
];
const SERIAL_CAP: usize = 5000;

pub enum AppEvent {
    Ports(Result<Vec<Detected>, String>),
    AllBoards(Result<Vec<Board>, String>),
    Log(String),
    JobDone(Result<bool, String>),
    Serial(String),
    SerialDone(u64, Result<(), String>),
    Defaults(Attached),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Build,
    Monitor,
    Project,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Build, Tab::Monitor, Tab::Project];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Build => "Build",
            Tab::Monitor => "Monitor",
            Tab::Project => "Project",
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

pub struct Monitor {
    pub port: String,
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
    pub job: Option<Job>,
    queue: VecDeque<usize>,

    pub serial: Vec<String>,
    pub serial_scroll: usize,
    pub baud: u32,
    pub monitor: Option<Monitor>,
    mon_gen: u64,
    resume_monitor: bool,
    pub input: Option<String>,

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
            job: None,
            queue: VecDeque::new(),
            serial: vec![],
            serial_scroll: 0,
            baud: 9600,
            monitor: None,
            mon_gen: 0,
            resume_monitor: false,
            input: None,
            quit: false,
        };
        app.load_baud();
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
                    self.tab = Tab::Build;
                    self.load_baud();
                }
            }
            AppEvent::Log(line) => {
                let line = line
                    .replace(&format!("{}/", self.root.display()), "")
                    .replace(&format!("{}/", sketch::scratch().display()), "");
                self.log.push(line);
                if self.log_scroll > 0 {
                    self.log_scroll += 1;
                }
            }
            AppEvent::JobDone(r) => {
                let Some(job) = self.job.take() else { return };
                let secs = job.started.elapsed().as_secs_f32();
                self.log.push(match r {
                    Ok(true) => format!("✓ {} done in {secs:.1}s", job.name),
                    Ok(false) => format!("✗ {} failed ({secs:.1}s)", job.name),
                    Err(e) => format!("✗ {}: {e}", job.name),
                });
                self.next_upload();
            }
            AppEvent::Serial(chunk) => self.push_serial(&chunk),
            AppEvent::SerialDone(id, r) if id == self.mon_gen => {
                self.monitor = None;
                self.input = None;
                self.serial_note(match r {
                    Ok(()) => "── disconnected ──".into(),
                    Err(e) => format!("── disconnected: {e} ──"),
                });
            }
            AppEvent::SerialDone(..) => {}
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
                Some(Ok(all)) => all.iter().map(|b| (b.name.clone(), b.fqbn.clone())).collect(),
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
            KeyCode::Char('m') => self.toggle_monitor(),
            KeyCode::Esc if self.job.is_some() => self.cancel(),
            _ => match self.tab {
                Tab::Build => scroll(&mut self.log_scroll, self.log.len(), k.code),
                Tab::Monitor => self.monitor_key(k),
                Tab::Project => self.project_key(k),
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
            KeyCode::Enter => {
                if self.sel != self.active {
                    self.stop_monitor();
                }
                self.active = self.sel;
                self.load_baud();
            }
            KeyCode::Char('d') if self.sel < self.boards.len() => {
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

    fn monitor_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Char('i') | KeyCode::Enter if self.monitor.is_some() => self.input = Some(String::new()),
            KeyCode::Char('+') | KeyCode::Char('=') => self.step_baud(1),
            KeyCode::Char('-') => self.step_baud(-1),
            KeyCode::Char('x') => {
                self.serial.clear();
                self.serial_scroll = 0;
            }
            code => scroll(&mut self.serial_scroll, self.serial.len(), code),
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
                let line = std::mem::take(buf) + "\n";
                if let Some(m) = &self.monitor {
                    let _ = m.input.send(line);
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
        if i == self.active {
            self.load_baud();
        }
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
        let baud = (with_baud && i == self.active).then_some(self.baud);
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

    fn load_baud(&mut self) {
        let dir = self.target().and_then(|t| t.sketch.as_deref()).and_then(|s| sketch::folder(&self.root, s));
        if let Some(b) = dir.and_then(|d| sketch::load_baud(&d)) {
            self.baud = b;
        }
    }

    // -- build jobs --

    fn prepared(&mut self, i: usize) -> Option<(String, PathBuf, String)> {
        let Some(t) = self.boards.get(i) else {
            self.log.push("✗ no boards yet — press a to add one".into());
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
        self.run_job(format!("compile {name}"), vec!["compile".into(), "-b".into(), fqbn.into(), dir.into()]);
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
        self.resume_monitor |= self.monitor.is_some();
        self.stop_monitor();
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
            args.push(dir.into());
            return self.run_job(format!("upload {name}"), args);
        }
        if std::mem::take(&mut self.resume_monitor) {
            self.start_monitor();
        }
    }

    fn run_job(&mut self, name: String, args: Vec<OsString>) {
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
        if self.monitor.is_some() {
            self.stop_monitor();
        } else if !self.queue.is_empty() || self.job.as_ref().is_some_and(|j| j.name.starts_with("upload")) {
            self.resume_monitor = true;
        } else {
            self.start_monitor();
        }
    }

    fn start_monitor(&mut self) {
        let Some(port) = self.port_of(self.active).map(|d| d.port.clone()) else {
            return self.serial_note("── active board isn't connected ──".into());
        };
        self.mon_gen += 1;
        let (id, baud, tx) = (self.mon_gen, self.baud, self.tx.clone());
        let (input, rx) = mpsc::unbounded_channel();
        self.serial_note(format!("── {} @ {baud} ──", port.address));
        let address = port.address.clone();
        let handle = tokio::spawn(async move {
            let r = cli::monitor(port, baud, rx, tx.clone()).await.map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::SerialDone(id, r));
        });
        self.monitor = Some(Monitor { port: address, input, handle });
    }

    fn stop_monitor(&mut self) {
        if let Some(m) = self.monitor.take() {
            m.handle.abort();
            self.input = None;
            self.serial_note("── closed ──".into());
        }
    }

    fn step_baud(&mut self, by: isize) {
        let i = BAUDS.iter().position(|b| *b == self.baud).unwrap_or(4) as isize;
        self.baud = BAUDS[(i + by).clamp(0, BAUDS.len() as isize - 1) as usize];
        self.save_with(Some(self.active), true);
        if self.monitor.is_some() {
            self.stop_monitor();
            self.start_monitor();
        }
    }

    fn push_serial(&mut self, chunk: &str) {
        if self.serial.is_empty() {
            self.serial.push(String::new());
        }
        for c in chunk.chars() {
            match c {
                '\n' => {
                    self.serial.push(String::new());
                    if self.serial_scroll > 0 {
                        self.serial_scroll += 1;
                    }
                }
                '\r' => {}
                c => self.serial.last_mut().expect("non-empty").push(c),
            }
        }
        if self.serial.len() > SERIAL_CAP {
            self.serial.drain(..self.serial.len() - SERIAL_CAP);
        }
    }

    fn serial_note(&mut self, note: String) {
        if self.serial.last().is_some_and(|l| !l.is_empty()) {
            self.push_serial("\n");
        }
        self.push_serial(&(note + "\n"));
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
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(String::new(), std::env::temp_dir().join("fluxus-none"), tx);
        for chunk in ["tem", "p: 21\r\nhum", "id: 40\r\n", "no newline"] {
            app.push_serial(chunk);
        }
        assert_eq!(app.serial, ["temp: 21", "humid: 40", "no newline"]);
        app.serial_note("── closed ──".into());
        assert_eq!(app.serial[3..], ["── closed ──", ""]);
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
        let giga = Board { name: "Arduino Giga R1".into(), fqbn: "arduino:mbed_giga:giga".into() };
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
