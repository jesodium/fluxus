use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use crate::app::AppEvent;

const BIN: &str = "arduino-cli";
const MIN_MAJOR: u64 = 1;

// -- types --

#[derive(Deserialize, Clone, Debug)]
pub struct Board {
    pub name: String,
    #[serde(default)]
    pub fqbn: String,
    #[serde(skip)]
    pub core: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Port {
    pub address: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub protocol_label: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Detected {
    pub port: Port,
    #[serde(default)]
    pub matching_boards: Vec<Board>,
}

// -- one-shot json --

pub async fn check_version() -> Result<String> {
    #[derive(Deserialize)]
    struct V {
        #[serde(rename = "VersionString")]
        version: String,
    }
    let out = match Command::new(BIN).args(["version", "--json"]).output().await {
        Err(e) if e.kind() == ErrorKind::NotFound => bail!(
            "arduino-cli not found on PATH.\n\
             Install it: https://arduino.github.io/arduino-cli/latest/installation/"
        ),
        r => r.context("running arduino-cli version")?,
    };
    let v: V = serde_json::from_slice(&out.stdout).context("parsing arduino-cli version")?;
    if let Some(major) = v.version.split('.').next().and_then(|m| m.parse::<u64>().ok())
        && major < MIN_MAJOR
    {
        bail!("arduino-cli {} is too old; fluxus needs >= {MIN_MAJOR}.0", v.version);
    }
    Ok(v.version)
}

async fn json<T: DeserializeOwned>(args: &[&str]) -> Result<T> {
    let out = Command::new(BIN).args(args).arg("--json").output().await?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(if out.stderr.is_empty() { &out.stdout } else { &out.stderr });
        bail!("arduino-cli {}: {}", args.join(" "), err.trim());
    }
    serde_json::from_slice(&out.stdout).with_context(|| format!("parsing arduino-cli {}", args.join(" ")))
}

#[derive(Deserialize)]
struct BoardList {
    #[serde(default)]
    detected_ports: Vec<Detected>,
}

pub async fn board_list() -> Result<Vec<Detected>> {
    Ok(json::<BoardList>(&["board", "list"]).await?.detected_ports)
}

// search, not listall: it also lists boards whose core isn't installed yet (no fqbn)
pub async fn board_search() -> Result<Vec<Board>> {
    Ok(json::<Search>(&["board", "search"]).await?.boards())
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    boards: Vec<Found>,
}

#[derive(Deserialize)]
struct Found {
    #[serde(flatten)]
    board: Board,
    platform: Platform,
}

#[derive(Deserialize)]
struct Platform {
    metadata: Meta,
}

#[derive(Deserialize)]
struct Meta {
    id: String,
}

impl Search {
    fn boards(self) -> Vec<Board> {
        self.boards.into_iter().map(|f| Board { core: f.platform.metadata.id, ..f.board }).collect()
    }
}

// -- sketch.yaml --

#[derive(Deserialize)]
pub struct Attached {
    #[serde(default)]
    pub fqbn: String,
    pub port: Option<Port>,
}

pub async fn attached(sketch: &Path) -> Result<Attached> {
    json(&["board", "attach", &sketch.to_string_lossy()]).await
}

pub async fn attach(sketch: &Path, fqbn: Option<&str>, port: Option<&Port>) -> Result<()> {
    let mut args = vec!["board".to_string(), "attach".into()];
    if let Some(f) = fqbn {
        args.extend(["-b".into(), f.into()]);
    }
    if let Some(p) = port {
        args.extend(["-p".into(), p.address.clone()]);
        if !p.protocol.is_empty() {
            args.extend(["-l".into(), p.protocol.clone()]);
        }
    }
    args.push(sketch.to_string_lossy().into_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    json::<serde_json::Value>(&args).await.map(drop)
}

// -- streaming --

fn spawn<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> Result<(Child, Group)> {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .arg("--no-color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().context("spawning arduino-cli")?;
    let group = Group(child.id());
    Ok((child, group))
}

// fix, avrdude outlives it otherwise
struct Group(Option<u32>);

impl Drop for Group {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGTERM) };
        }
    }
}

pub async fn stream<S: AsRef<OsStr>>(
    args: impl IntoIterator<Item = S>,
    tx: &UnboundedSender<AppEvent>,
) -> Result<bool> {
    let (mut child, mut group) = spawn(args)?;
    drop(child.stdin.take());
    let log = |l, redraw| if redraw { AppEvent::Progress(l) } else { AppEvent::Log(l) };
    let out = lines(child.stdout.take().expect("piped"), tx.clone(), log);
    let err = lines(child.stderr.take().expect("piped"), tx.clone(), log);
    let _ = tokio::join!(out, err);
    let status = child.wait().await?;
    group.0 = None;
    Ok(status.success())
}

fn lines(
    r: impl AsyncRead + Unpin + Send + 'static,
    tx: UnboundedSender<AppEvent>,
    wrap: impl Fn(String, bool) -> AppEvent + Send + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        while let Ok(Some(redraw)) = chunk(&mut r, &mut buf).await {
            let _ = tx.send(wrap(String::from_utf8_lossy(&buf).trim_end().to_string(), redraw));
            buf.clear();
        }
    })
}

// stops on \r as well as \n: avrdude redraws its progress bar in place
async fn chunk<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    loop {
        let avail = r.fill_buf().await?;
        if avail.is_empty() {
            return Ok((!buf.is_empty()).then_some(false));
        }
        match avail.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            Some(i) => {
                let redraw = avail[i] == b'\r';
                buf.extend_from_slice(&avail[..i]);
                r.consume(i + 1);
                return Ok(Some(redraw));
            }
            None => {
                let n = avail.len();
                buf.extend_from_slice(avail);
                r.consume(n);
            }
        }
    }
}

// -- serial monitor --

pub async fn monitor(
    id: u64,
    port: Port,
    baud: u32,
    mut input: UnboundedReceiver<String>,
    tx: UnboundedSender<AppEvent>,
) -> Result<()> {
    let mut args = vec!["monitor".to_string(), "-p".into(), port.address, "--quiet".into()];
    args.extend(["--config".into(), format!("baudrate={baud}")]);
    if !port.protocol.is_empty() {
        args.extend(["-l".into(), port.protocol]);
    }
    let (mut child, mut group) = spawn(args)?;
    let mut stdin = child.stdin.take().expect("piped");
    let mut out = child.stdout.take().expect("piped");
    lines(child.stderr.take().expect("piped"), tx.clone(), move |l, _| AppEvent::Serial(id, l + "\n"));

    // for Serial.print without newline
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = out.read(&mut buf) => match n? {
                0 => break,
                n => { let _ = tx.send(AppEvent::Serial(id, String::from_utf8_lossy(&buf[..n]).into_owned())); }
            },
            Some(s) = input.recv() => stdin.write_all(s.as_bytes()).await?,
        }
    }
    let status = child.wait().await?;
    group.0 = None;
    if !status.success() {
        bail!("monitor exited ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_board_list() {
        let raw = r#"{"detected_ports":[
            {"matching_boards":[{"name":"Arduino Uno","fqbn":"arduino:avr:uno"}],
             "port":{"address":"/dev/cu.usbmodem1","label":"x","protocol":"serial","protocol_label":"Serial Port (USB)"}},
            {"port":{"address":"/dev/cu.wchusbserial1","protocol":"serial"}},
            {"matching_boards":[{"name":"Uninstalled Core Board"}],"port":{"address":"COM3","protocol":"serial"}}
        ]}"#;
        let d = serde_json::from_str::<BoardList>(raw).unwrap().detected_ports;
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].matching_boards[0].fqbn, "arduino:avr:uno");
        assert!(d[1].matching_boards.is_empty());
        assert_eq!(d[2].matching_boards[0].fqbn, "");
        assert!(serde_json::from_str::<BoardList>("{}").unwrap().detected_ports.is_empty());
    }

    #[test]
    fn parses_board_search() {
        let raw = r#"{"boards":[
            {"name":"Arduino Uno","fqbn":"arduino:avr:uno","platform":{"metadata":{"id":"arduino:avr"},"release":{"installed":true}}},
            {"name":"Arduino Due","platform":{"metadata":{"id":"arduino:sam"},"release":{}}}
        ]}"#;
        let b = serde_json::from_str::<Search>(raw).unwrap().boards();
        assert_eq!((b[0].fqbn.as_str(), b[0].core.as_str()), ("arduino:avr:uno", "arduino:avr"));
        assert_eq!((b[1].fqbn.as_str(), b[1].core.as_str()), ("", "arduino:sam"));
    }
}
