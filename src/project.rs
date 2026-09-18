use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::Detected;

const FILE: &str = "fluxus.yaml";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Target {
    pub name: String,
    pub fqbn: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sketch: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct File {
    #[serde(default)]
    boards: Vec<Target>,
}

// -- fluxus.yaml --

pub fn load(root: &Path) -> Result<Vec<Target>> {
    match std::fs::read_to_string(root.join(FILE)) {
        Ok(s) if s.trim().is_empty() => Ok(vec![]),
        Ok(s) => Ok(serde_norway::from_str::<File>(&s).context("parsing fluxus.yaml")?.boards),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e).context("reading fluxus.yaml"),
    }
}

pub fn save(root: &Path, boards: &[Target]) -> Result<()> {
    let yaml = serde_norway::to_string(&File { boards: boards.to_vec() })?;
    std::fs::write(root.join(FILE), yaml).context("writing fluxus.yaml")
}

// -- ports --

pub fn resolve(boards: &[Target], ports: &[Detected]) -> Vec<Option<usize>> {
    let mut taken = vec![false; ports.len()];
    let mut out = vec![None; boards.len()];
    for (i, t) in boards.iter().enumerate() {
        let hit = ports.iter().position(|d| t.port.as_deref() == Some(d.port.address.as_str()));
        if let Some(j) = hit
            && !taken[j]
        {
            taken[j] = true;
            out[i] = Some(j);
        }
    }
    // same as flash.sh, trust the reported fqbn when the port moved
    for (i, t) in boards.iter().enumerate() {
        if out[i].is_some() {
            continue;
        }
        let hit = (0..ports.len()).find(|&j| !taken[j] && ports[j].matching_boards.iter().any(|b| b.fqbn == t.fqbn));
        if let Some(j) = hit {
            taken[j] = true;
            out[i] = Some(j);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Board, Port};

    fn target(name: &str, fqbn: &str, port: Option<&str>) -> Target {
        Target { name: name.into(), fqbn: fqbn.into(), port: port.map(Into::into), sketch: None }
    }

    fn detected(addr: &str, fqbn: Option<&str>) -> Detected {
        Detected {
            port: Port { address: addr.into(), ..Default::default() },
            matching_boards: fqbn.map(|f| Board { name: f.into(), fqbn: f.into() }).into_iter().collect(),
        }
    }

    #[test]
    fn resolves_ports_like_flash_sh() {
        let boards = [
            target("giga", "arduino:mbed_giga:giga", Some("/dev/cu.usbmodem1101")),
            target("cam 1", "esp32:esp32:esp32cam", Some("/dev/cu.usbserial-110")),
            target("cam 2", "esp32:esp32:esp32cam", Some("/dev/cu.usbserial-120")),
            target("uno", "arduino:avr:uno", None),
        ];
        let ports = [
            detected("/dev/cu.usbserial-110", None),
            detected("/dev/cu.usbmodem1201", Some("arduino:mbed_giga:giga")),
            detected("/dev/cu.usbmodem1301", Some("arduino:avr:uno")),
        ];
        assert_eq!(resolve(&boards, &ports), [Some(1), Some(0), None, Some(2)]);

        let two_same = [target("a", "arduino:avr:uno", None), target("b", "arduino:avr:uno", None)];
        assert_eq!(resolve(&two_same, &ports), [Some(2), None]);
    }

    #[test]
    fn yaml_roundtrip() {
        let root = std::env::temp_dir().join(format!("fluxus-proj-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(load(&root).unwrap(), []);
        let mut giga = target("Giga R1", "arduino:mbed_giga:giga", Some("/dev/cu.usbmodem1101"));
        giga.sketch = Some("giga-r1/main".into());
        let boards = vec![giga, target("ESP32-CAM", "esp32:esp32:esp32cam", None)];
        save(&root, &boards).unwrap();
        assert_eq!(load(&root).unwrap(), boards);
        std::fs::write(root.join(FILE), "boards: [oops").unwrap();
        assert!(load(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
