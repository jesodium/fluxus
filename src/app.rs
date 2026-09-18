use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;

use crate::cli::{self, Attached, Board, Detected, Port};
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
    Boards,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Build, Tab::Monitor, Tab::Boards];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Build => "Build",
            Tab::Monitor => "Monitor",
            Tab::Boards => "Boards",
        }
    }
}

pub struct Picker {
    pub query: String,
    pub sel: usize,
}

pub struct Job {
    pub name: &'static str,
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
    pub sketch: Result<PathBuf, String>,
    pub tab: Tab,

    pub ports: Vec<Detected>,
    pub ports_err: Option<String>,
    pub scanned: bool,
    pub sel: usize,
    pub port: Option<Port>,
    pub board: Option<Board>,
    pub all_boards: Option<Result<Vec<Board>, String>>,
    pub picker: Option<Picker>,

    pub log: Vec<String>,
    pub log_scroll: usize,
    pub job: Option<Job>,

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
    pub fn new(cli_version: String, sketch: Result<PathBuf, String>, tx: UnboundedSender<AppEvent>) -> Self {
        let baud = sketch.as_ref().ok().and_then(|d| sketch::load_baud(d)).unwrap_or(9600);
        Self {
            tx,
            cli_version,
            sketch,
            tab: Tab::Boards,
            ports: vec![],
            ports_err: None,
            scanned: false,
            sel: 0,
            port: None,
            board: None,
            all_boards: None,
            picker: None,
            log: vec![],
            log_scroll: 0,
            job: None,
            serial: vec![],
            serial_scroll: 0,
            baud,
            monitor: None,
            mon_gen: 0,
            resume_monitor: false,
            input: None,
            quit: false,
        }
    }

    // -- events --

    pub fn event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Ports(Ok(mut p)) => {
                p.sort_by(|a, b| a.port.address.cmp(&b.port.address));
                self.ports = p;
                self.ports_err = None;
                self.scanned = true;
                self.sel = self.sel.min(self.ports.len().saturating_sub(1));
            }
            AppEvent::Ports(Err(e)) => self.ports_err = Some(e),
            AppEvent::AllBoards(r) => {
                self.all_boards = Some(r);
                if let Some(b) = &self.board {
                    let name = self.board_name(&b.fqbn);
                    self.board.as_mut().expect("checked").name = name;
                }
            }
            AppEvent::Defaults(d) => {
                if self.board.is_none() && !d.fqbn.is_empty() {
                    self.board = Some(Board { name: self.board_name(&d.fqbn), fqbn: d.fqbn });
                    if self.tab == Tab::Boards {
                        self.tab = Tab::Build;
                    }
                }
                if self.port.is_none() {
                    self.port = d.port;
                }
            }
            AppEvent::Log(mut line) => {
                if let Ok(dir) = &self.sketch {
                    line = line.replace(&format!("{}/", dir.display()), "");
                }
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
                self.after_job();
            }
            AppEvent::Serial(chunk) => self.push_serial(&chunk),
            AppEvent::SerialDone(g, r) if g == self.mon_gen => {
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

    pub fn port_connected(&self) -> bool {
        self.port.as_ref().is_some_and(|p| self.ports.iter().any(|d| d.port.address == p.address))
    }

    fn board_name(&self, fqbn: &str) -> String {
        let Some(Ok(all)) = &self.all_boards else { return fqbn.into() };
        all.iter().find(|b| b.fqbn == fqbn).map_or(fqbn.into(), |b| b.name.clone())
    }

    pub fn filtered(&self) -> Vec<&Board> {
        let Some(Ok(all)) = &self.all_boards else { return vec![] };
        let q = self.picker.as_ref().map_or(String::new(), |p| p.query.to_lowercase());
        all.iter()
            .filter(|b| b.name.to_lowercase().contains(&q) || b.fqbn.contains(&q))
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
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('1') => self.tab = Tab::Build,
            KeyCode::Char('2') => self.tab = Tab::Monitor,
            KeyCode::Char('3') => self.tab = Tab::Boards,
            KeyCode::Tab => self.cycle_tab(1),
            KeyCode::BackTab => self.cycle_tab(Tab::ALL.len() - 1),
            KeyCode::Char('b') => self.open_picker(),
            KeyCode::Char('c') => self.compile(),
            KeyCode::Char('u') => self.upload(),
            KeyCode::Char('m') => self.toggle_monitor(),
            KeyCode::Esc if self.job.is_some() => self.cancel(),
            _ => match self.tab {
                Tab::Build => scroll(&mut self.log_scroll, self.log.len(), k.code),
                Tab::Monitor => self.monitor_key(k),
                Tab::Boards => self.boards_key(k),
            },
        }
    }

    fn cycle_tab(&mut self, by: usize) {
        let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(i + by) % Tab::ALL.len()];
    }

    fn boards_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1).min(self.ports.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Enter => self.choose_port(),
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

    fn picker_key(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => {
                let sel = self.picker.as_ref().map_or(0, |p| p.sel);
                if let Some(b) = self.filtered().get(sel).map(|b| (*b).clone()) {
                    self.board = Some(b);
                    self.picker = None;
                    self.save();
                }
            }
            code => {
                let n = self.filtered().len();
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

    // -- boards --

    fn open_picker(&mut self) {
        self.picker = Some(Picker { query: String::new(), sel: 0 });
    }

    fn choose_port(&mut self) {
        let Some(d) = self.ports.get(self.sel) else { return };
        self.port = Some(d.port.clone());
        match d.matching_boards.iter().find(|b| !b.fqbn.is_empty()) {
            Some(b) => self.board = Some(b.clone()),
            None if self.board.is_none() => self.open_picker(),
            None => {}
        }
        self.save();
    }

    fn save(&self) {
        let Ok(dir) = self.sketch.clone() else { return };
        let fqbn = self.board.as_ref().map(|b| b.fqbn.clone());
        let (port, baud, tx) = (self.port.clone(), self.baud, self.tx.clone());
        tokio::spawn(async move {
            static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
            let _held = LOCK.lock().await;
            let r = async {
                if fqbn.is_some() || port.is_some() {
                    cli::attach(&dir, fqbn.as_deref(), port.as_ref()).await?;
                }
                sketch::save_baud(&dir, baud)
            };
            if let Err(e) = r.await {
                let _ = tx.send(AppEvent::Log(format!("✗ couldn't save sketch.yaml: {e:#}")));
            }
        });
    }

    // -- build jobs --

    fn compile(&mut self) {
        let Some((sketch, fqbn)) = self.ready_to_build() else { return };
        self.run_job("compile", vec!["compile".into(), "-b".into(), fqbn.into(), sketch.into()]);
    }

    fn upload(&mut self) {
        let Some((sketch, fqbn)) = self.ready_to_build() else { return };
        let Some(port) = self.port.clone() else {
            return self.log.push("✗ no port selected — pick one in Boards (3)".into());
        };
        // dont touch, port stays busy otherwise
        self.resume_monitor = self.monitor.is_some();
        self.stop_monitor();
        let mut args: Vec<OsString> = vec!["compile".into(), "-u".into(), "-b".into(), fqbn.into()];
        args.extend(["-p".into(), port.address.into()]);
        if !port.protocol.is_empty() {
            args.extend(["-l".into(), port.protocol.into()]);
        }
        args.push(sketch.into());
        self.run_job("upload", args);
    }

    fn ready_to_build(&mut self) -> Option<(PathBuf, String)> {
        self.tab = Tab::Build;
        if self.job.is_some() {
            return None;
        }
        let sketch = match &self.sketch {
            Ok(s) => s.clone(),
            Err(e) => {
                self.log.push(format!("✗ {e}"));
                return None;
            }
        };
        let Some(board) = &self.board else {
            self.log.push("✗ no board selected — press b".into());
            return None;
        };
        Some((sketch, board.fqbn.clone()))
    }

    fn run_job(&mut self, name: &'static str, args: Vec<OsString>) {
        self.log.clear();
        self.log_scroll = 0;
        let shown: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        self.log.push(format!("$ arduino-cli {}", shown.join(" ")));

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
            self.after_job();
        }
    }

    fn after_job(&mut self) {
        if std::mem::take(&mut self.resume_monitor) {
            self.start_monitor();
        }
    }

    // -- serial monitor --

    fn toggle_monitor(&mut self) {
        self.tab = Tab::Monitor;
        if self.monitor.is_some() {
            self.stop_monitor();
        } else if self.job.as_ref().is_some_and(|j| j.name == "upload") {
            self.resume_monitor = true;
        } else {
            self.start_monitor();
        }
    }

    fn start_monitor(&mut self) {
        let Some(port) = self.port.clone() else {
            return self.serial_note("── no port selected — pick one in Boards (3) ──".into());
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
        self.save();
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
        let mut app = App::new(String::new(), Err(String::new()), tx);
        for chunk in ["tem", "p: 21\r\nhum", "id: 40\r\n", "no newline"] {
            app.push_serial(chunk);
        }
        assert_eq!(app.serial, ["temp: 21", "humid: 40", "no newline"]);
        app.serial_note("── closed ──".into());
        assert_eq!(app.serial[3..], ["── closed ──", ""]);
    }
}
