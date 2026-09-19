use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::app::BAUDS;

pub const THEMES: [&str; 4] = ["fluxus", "light", "gruvbox", "mono"];
const EOLS: [(&str, &str); 4] = [("lf", "\n"), ("crlf", "\r\n"), ("cr", "\r"), ("none", "")];
pub const ROWS: usize = 4;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub baud: u32,
    pub line_ending: String,
    pub verbose: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: THEMES[0].into(), baud: 9600, line_ending: "lf".into(), verbose: false }
    }
}

impl Settings {
    pub fn rows(&self) -> [(&'static str, String); ROWS] {
        [
            ("theme", self.theme.clone()),
            ("default baud", self.baud.to_string()),
            ("send line ending", self.line_ending.clone()),
            ("verbose compile", (if self.verbose { "on" } else { "off" }).into()),
        ]
    }

    pub fn step(&mut self, row: usize, by: isize) {
        match row {
            0 => self.theme = step_in(&THEMES, self.theme.as_str(), by).into(),
            1 => self.baud = step_in(&BAUDS, self.baud, by),
            2 => {
                let names = EOLS.map(|(n, _)| n);
                self.line_ending = step_in(&names, self.line_ending.as_str(), by).into();
            }
            _ => self.verbose = !self.verbose,
        }
    }

    pub fn eol(&self) -> &'static str {
        EOLS.iter().find(|(n, _)| *n == self.line_ending).map_or("\n", |(_, e)| e)
    }

    pub fn theme_index(&self) -> usize {
        THEMES.iter().position(|t| *t == self.theme).unwrap_or(0)
    }
}

fn step_in<T: PartialEq + Copy>(list: &[T], cur: T, by: isize) -> T {
    let at = list.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    list[(at + by).rem_euclid(list.len() as isize) as usize]
}

// -- ~/.config/fluxus/settings.yaml --

pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("fluxus").join("settings.yaml"))
}

pub fn load() -> Settings {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_norway::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<()> {
    let p = path().context("no HOME to save settings in")?;
    std::fs::create_dir_all(p.parent().expect("has parent"))?;
    std::fs::write(&p, serde_norway::to_string(s)?).with_context(|| format!("writing {}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_wrap_both_ways() {
        let mut s = Settings::default();
        s.step(0, -1);
        assert_eq!(s.theme, "mono");
        s.step(0, 1);
        assert_eq!(s.theme_index(), 0);
        s.step(2, 1);
        assert_eq!(s.eol(), "\r\n");
        s.baud = 2000000;
        s.step(1, 1);
        assert_eq!(s.baud, 300);
        let partial: Settings = serde_norway::from_str("verbose: true\n").unwrap();
        assert!(partial.verbose && partial.baud == 9600);
    }
}
