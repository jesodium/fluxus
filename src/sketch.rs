use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub fn find(path: &Path) -> Result<PathBuf> {
    let p = path.canonicalize().with_context(|| format!("{} not found", path.display()))?;
    let dir = if p.is_file() { p.parent().context("sketch has no parent folder")?.to_path_buf() } else { p };
    let name = dir.file_name().context("sketch folder has no name")?.to_string_lossy();
    if !dir.join(format!("{name}.ino")).is_file() {
        bail!("no sketch: {} has no {name}.ino", dir.display());
    }
    Ok(dir)
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
}
