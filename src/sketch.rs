use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const PORT_CONFIG: &str = "default_port_config:";

// -- discovery --

pub fn find(path: &Path) -> Result<PathBuf> {
    let p = path.canonicalize().with_context(|| format!("{} not found", path.display()))?;
    let dir = if p.is_file() { p.parent().context("sketch has no parent folder")?.to_path_buf() } else { p };
    let name = dir.file_name().context("sketch folder has no name")?.to_string_lossy();
    if !dir.join(format!("{name}.ino")).is_file() {
        bail!("no sketch: {} has no {name}.ino", dir.display());
    }
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
    fn finds_sketch_by_folder_or_ino() {
        let root = std::env::temp_dir().join(format!("fluxus-test-{}", std::process::id()));
        let dir = root.join("Blink");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Blink.ino"), "").unwrap();
        std::fs::write(dir.join("Other.ino"), "").unwrap();
        let want = dir.canonicalize().unwrap();

        assert_eq!(find(&dir).unwrap(), want);
        assert_eq!(find(&dir.join("Blink.ino")).unwrap(), want);
        assert_eq!(find(&dir.join("Other.ino")).unwrap(), want);
        assert!(find(&root).is_err());
        assert!(find(&root.join("missing")).is_err());
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
