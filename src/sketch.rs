use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const PORT_CONFIG: &str = "default_port_config:";
const SKIP: [&str; 4] = ["node_modules", "target", "build", "dist"];

// -- discovery --

fn is_sketch(dir: &Path) -> bool {
    dir.file_name().is_some_and(|n| dir.join(format!("{}.ino", n.to_string_lossy())).is_file())
}

pub fn scan(root: &Path) -> Vec<String> {
    let mut out = vec![];
    walk(root, root, 0, &mut out);
    out.sort_by_key(|s| s.to_lowercase());
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    if is_sketch(dir) {
        let rel = dir.strip_prefix(root).unwrap_or(dir).to_string_lossy().into_owned();
        out.push(if rel.is_empty() { ".".into() } else { rel });
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            walk(root, &path, depth + 1, out);
        } else if name.ends_with(".ino") {
            out.push(path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned());
        }
    }
}

pub fn folder(root: &Path, rel: &str) -> Option<PathBuf> {
    (!rel.ends_with(".ino")).then(|| root.join(rel))
}

pub fn scratch() -> PathBuf {
    let tmp = std::env::temp_dir();
    tmp.canonicalize().unwrap_or(tmp).join("fluxus")
}

pub fn build_dir(root: &Path, rel: &str) -> Result<PathBuf> {
    if let Some(dir) = folder(root, rel) {
        if !is_sketch(&dir) {
            bail!("{rel} is no longer a sketch");
        }
        return Ok(dir);
    }
    // stray .ino, arduino-cli wants it in a folder of the same name
    let src = root.join(rel);
    let stem = src.file_stem().context("bad sketch path")?.to_string_lossy().into_owned();
    let dir = scratch().join(&stem);
    std::fs::create_dir_all(&dir).context("making scratch sketch folder")?;
    std::fs::copy(&src, dir.join(format!("{stem}.ino"))).with_context(|| format!("copying {rel}"))?;
    Ok(dir)
}

// -- baud in sketch.yaml --

pub fn load_baud(dir: &Path) -> Option<u32> {
    get_baud(&std::fs::read_to_string(dir.join("sketch.yaml")).ok()?)
}

pub fn save_baud(dir: &Path, baud: u32) -> Result<()> {
    let path = dir.join("sketch.yaml");
    let yaml = match std::fs::read_to_string(&path) {
        Ok(y) => y,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("reading sketch.yaml"),
    };
    if get_baud(&yaml) == Some(baud) {
        return Ok(());
    }
    std::fs::write(&path, set_baud(&yaml, baud)?).context("writing sketch.yaml")
}

pub fn yaml_value(dir: &Path, key: &str) -> Option<String> {
    let yaml = std::fs::read_to_string(dir.join("sketch.yaml")).ok()?;
    let v = yaml.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))?;
    let v = v.split('#').next()?.trim().trim_matches(['"', '\'']);
    (!v.is_empty()).then(|| v.to_string())
}

fn top_level(line: &str) -> bool {
    !line.starts_with([' ', '\t']) && !line.trim().is_empty() && !line.starts_with('#')
}

fn get_baud(yaml: &str) -> Option<u32> {
    let mut in_block = false;
    for l in yaml.lines() {
        if top_level(l) {
            in_block = l.starts_with(PORT_CONFIG);
        } else if in_block && let Some(v) = l.trim().strip_prefix("baudrate:") {
            return v.split('#').next()?.trim().trim_matches(['"', '\'']).parse().ok();
        }
    }
    None
}

fn set_baud(yaml: &str, baud: u32) -> Result<String> {
    let mut lines: Vec<String> = yaml.lines().map(String::from).collect();
    match lines.iter().position(|l| l.starts_with(PORT_CONFIG)) {
        None => lines.extend([PORT_CONFIG.into(), format!("  baudrate: {baud}")]),
        Some(h) => {
            let rest = lines[h][PORT_CONFIG.len()..].split('#').next().unwrap_or("");
            if !rest.trim().is_empty() {
                bail!("default_port_config in sketch.yaml is inline; set the baud there by hand");
            }
            let end = lines[h + 1..].iter().position(|l| top_level(l)).map_or(lines.len(), |i| h + 1 + i);
            match (h + 1..end).find(|&i| lines[i].trim_start().starts_with("baudrate:")) {
                Some(i) => {
                    let indent = lines[i].len() - lines[i].trim_start().len();
                    lines[i] = format!("{}baudrate: {baud}", &lines[i][..indent]);
                }
                None => lines.insert(h + 1, format!("  baudrate: {baud}")),
            }
        }
    }
    Ok(lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_repo_like_blackout() {
        let root = std::env::temp_dir().join(format!("fluxus-scan-{}", std::process::id()));
        for (dir, ino) in [
            ("giga-r1/main", "main.ino"),
            ("giga-r1/main", "extra_tab.ino"),
            ("giga-r1/i2c_scan", "i2c_scan.ino"),
            ("scratchpad", "motor_test.ino"),
            ("server/node_modules/x", "x.ino"),
            (".git/y", "y.ino"),
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join(ino), "").unwrap();
        }
        std::fs::create_dir_all(root.join("OUTDATED/old")).unwrap();
        std::fs::write(root.join("OUTDATED/old/old.ino"), "").unwrap();
        assert_eq!(scan(&root), ["giga-r1/i2c_scan", "giga-r1/main", "OUTDATED/old", "scratchpad/motor_test.ino"]);
        std::fs::write(root.join("giga-r1/main/sketch.yaml"), "default_fqbn: a:b:c\ndefault_port: /dev/cu.usbmodem1101 # giga\n").unwrap();
        assert_eq!(yaml_value(&root.join("giga-r1/main"), "default_port").as_deref(), Some("/dev/cu.usbmodem1101"));
        assert_eq!(scan(&root.join("giga-r1/main")), ["."]);

        let built = build_dir(&root, "scratchpad/motor_test.ino").unwrap();
        assert!(built.join("motor_test.ino").is_file());
        assert_eq!(build_dir(&root, "giga-r1/main").unwrap(), root.join("giga-r1/main"));
        assert!(build_dir(&root, "scratchpad").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn baud_roundtrip_keeps_the_rest() {
        let yaml = "# mine\nprofiles:\n  uno:\n    fqbn: arduino:avr:uno\n    port_config:\n      baudrate: 300\n\
                    default_port_config:\n    parity: none\n    baudrate: \"57600\" # fast\ndefault_fqbn: arduino:avr:nano\n";
        assert_eq!(get_baud(yaml), Some(57600));
        let out = set_baud(yaml, 115200).unwrap();
        assert_eq!(get_baud(&out), Some(115200));
        assert_eq!(out, yaml.replace("    baudrate: \"57600\" # fast", "    baudrate: 115200"));

        let added = set_baud("default_fqbn: a:b:c\ndefault_port_config:\n  parity: none\n", 9600).unwrap();
        assert_eq!(added, "default_fqbn: a:b:c\ndefault_port_config:\n  baudrate: 9600\n  parity: none\n");
        assert_eq!(set_baud("", 9600).unwrap(), "default_port_config:\n  baudrate: 9600\n");
        assert_eq!(get_baud("profiles:\n  x:\n    port_config:\n      baudrate: 300\n"), None);
        assert!(set_baud("default_port_config: {baudrate: 9600}\n", 300).is_err());
    }
}
