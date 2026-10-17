// Persistent user configuration (Phase 3.5).
//
// A tiny dependency-free JSON store kept in `user.json` next to the executable
// (falling back to `target/user.json`). Values are written by the Settings window's
// "Save Configuration" button and auto-saved on a clean exit, then restored on the
// next launch. Absent file → sensible defaults (dark mode on, per accessibility).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct UserConfig {
    pub dark_mode: bool,
    pub ui_zoom: f32,
    pub master_vol: f32,
    pub show_controls: bool,
    pub chk_locked: bool,
    pub sw_hq_mode: bool,
    pub sw_keep_tray: bool,
    pub selected_device: usize,
    pub selected_rate: usize,
    pub selected_soundfont: usize,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            dark_mode: true,
            ui_zoom: 1.0,
            master_vol: 0.78,
            show_controls: true,
            chk_locked: false,
            sw_hq_mode: true,
            sw_keep_tray: true,
            selected_device: 0,
            selected_rate: 0,
            selected_soundfont: 0,
        }
    }
}

impl UserConfig {
    /// `user.json` next to the running executable, else `target/user.json`.
    pub fn path() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                return dir.join("user.json");
            }
        }
        PathBuf::from("target").join("user.json")
    }

    /// Load the config, falling back to defaults when the file is missing or malformed.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Serialize and write the config. Failures are surfaced to the caller (the
    /// Settings window reports them; the exit path reports via stderr).
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, self.to_json())
    }

    fn to_json(&self) -> String {
        format!(
            "{{\n  \"dark_mode\": {},\n  \"ui_zoom\": {:.4},\n  \"master_vol\": {:.4},\n  \
             \"show_controls\": {},\n  \"chk_locked\": {},\n  \"sw_hq_mode\": {},\n  \
             \"sw_keep_tray\": {},\n  \"selected_device\": {},\n  \"selected_rate\": {},\n  \
             \"selected_soundfont\": {}\n}}\n",
            self.dark_mode,
            self.ui_zoom,
            self.master_vol,
            self.show_controls,
            self.chk_locked,
            self.sw_hq_mode,
            self.sw_keep_tray,
            self.selected_device,
            self.selected_rate,
            self.selected_soundfont,
        )
    }

    /// Tolerant reader: extracts each known key by name and keeps the default on any
    /// missing / unparseable value, so a hand-edited or truncated file still loads.
    fn parse(text: &str) -> Self {
        let mut cfg = Self::default();
        if let Some(v) = json_bool(text, "dark_mode") {
            cfg.dark_mode = v;
        }
        if let Some(v) = json_f32(text, "ui_zoom") {
            cfg.ui_zoom = v.clamp(0.75, 2.0);
        }
        if let Some(v) = json_f32(text, "master_vol") {
            cfg.master_vol = v.clamp(0.0, 2.0);
        }
        if let Some(v) = json_bool(text, "show_controls") {
            cfg.show_controls = v;
        }
        if let Some(v) = json_bool(text, "chk_locked") {
            cfg.chk_locked = v;
        }
        if let Some(v) = json_bool(text, "sw_hq_mode") {
            cfg.sw_hq_mode = v;
        }
        if let Some(v) = json_bool(text, "sw_keep_tray") {
            cfg.sw_keep_tray = v;
        }
        if let Some(v) = json_usize(text, "selected_device") {
            cfg.selected_device = v;
        }
        if let Some(v) = json_usize(text, "selected_rate") {
            cfg.selected_rate = v;
        }
        if let Some(v) = json_usize(text, "selected_soundfont") {
            cfg.selected_soundfont = v;
        }
        cfg
    }
}

/// The raw text after `"key":` up to the next `,` / `}` / newline.
fn value_str<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let start = text.find(&needle)? + needle.len();
    let rest = &text[start..];
    let colon = rest.find(':')? + 1;
    let rest = rest[colon..].trim_start();
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c == '\n' || c == '\r')
        .unwrap_or(rest.len());
    Some(rest[..end].trim())
}

fn json_bool(text: &str, key: &str) -> Option<bool> {
    match value_str(text, key)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn json_f32(text: &str, key: &str) -> Option<f32> {
    value_str(text, key)?.parse().ok()
}

fn json_usize(text: &str, key: &str) -> Option<usize> {
    value_str(text, key)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let cfg = UserConfig {
            dark_mode: false,
            ui_zoom: 1.25,
            master_vol: 0.42,
            show_controls: false,
            chk_locked: true,
            sw_hq_mode: false,
            sw_keep_tray: false,
            selected_device: 2,
            selected_rate: 1,
            selected_soundfont: 3,
        };
        let back = UserConfig::parse(&cfg.to_json());
        assert_eq!(back, cfg);
    }

    #[test]
    fn missing_values_keep_defaults() {
        let back = UserConfig::parse("{\n  \"dark_mode\": false\n}");
        assert!(!back.dark_mode);
        assert_eq!(back.master_vol, UserConfig::default().master_vol);
    }
}
