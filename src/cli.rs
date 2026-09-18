use std::ffi::OsStr;
use std::io::ErrorKind;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
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
}

#[derive(Deserialize, Clone, Debug)]
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

pub async fn board_listall() -> Result<Vec<Board>> {
    #[derive(Deserialize)]
    struct R {
        #[serde(default)]
        boards: Vec<Board>,
    }
    Ok(json::<R>(&["board", "listall"]).await?.boards)
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
    let out = lines(child.stdout.take().expect("piped"), tx.clone(), AppEvent::Log);
    let err = lines(child.stderr.take().expect("piped"), tx.clone(), AppEvent::Log);
    let _ = tokio::join!(out, err);
    let status = child.wait().await?;
    group.0 = None;
    Ok(status.success())
}

fn lines(
    r: impl AsyncRead + Unpin + Send + 'static,
    tx: UnboundedSender<AppEvent>,
    wrap: fn(String) -> AppEvent,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut r = BufReader::new(r);
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).await.is_ok_and(|n| n > 0) {
            let _ = tx.send(wrap(String::from_utf8_lossy(&buf).trim_end().to_string()));
            buf.clear();
        }
    })
}

// -- serial monitor --

pub async fn monitor(
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
    lines(child.stderr.take().expect("piped"), tx.clone(), |l| AppEvent::Serial(l + "\n"));

    // for Serial.print without newline
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            n = out.read(&mut buf) => match n? {
                0 => break,
                n => { let _ = tx.send(AppEvent::Serial(String::from_utf8_lossy(&buf[..n]).into_owned())); }
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
}
