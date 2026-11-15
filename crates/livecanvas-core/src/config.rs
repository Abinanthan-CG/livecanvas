//! User configuration, persisted as TOML in `%LOCALAPPDATA%\LiveCanvas\config.toml`.

use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// Which part of the desktop the wallpaper covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorTarget {
    /// Only the primary monitor.
    Primary,
    /// One page stretched across the whole virtual desktop.
    Span,
    /// A specific monitor, zero-based, in enumeration order.
    Index(usize),
}

/// All user-tunable settings. Unknown keys are ignored and missing keys take
/// their default, so old config files keep working across upgrades.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// URL, HTML file, or folder containing `index.html`. Empty = built-in demo.
    pub source: String,
    /// `"primary"`, `"span"`, or a zero-based monitor index such as `"1"`.
    pub monitor: String,
    /// Forward mouse input from the desktop to the page.
    pub interactive: bool,
    /// Give the page keyboard focus when it is clicked.
    pub keyboard: bool,
    /// Also forward right-clicks (the desktop context menu still opens).
    pub forward_right_click: bool,
    /// Mute all audio produced by the page.
    pub muted: bool,
    /// Suspend rendering while a full-screen window covers the wallpaper.
    pub pause_when_covered: bool,
    /// Page zoom factor, clamped to `0.25..=5.0`.
    pub zoom: f64,
    /// Allow opening DevTools from the tray menu.
    pub devtools: bool,
    /// Optional user-agent override; empty keeps the WebView2 default.
    pub user_agent: String,
    /// Register Ctrl+Alt+I as a global "toggle interactivity" hotkey.
    pub hotkey: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source: String::new(),
            monitor: "primary".into(),
            interactive: true,
            keyboard: true,
            forward_right_click: false,
            muted: false,
            pause_when_covered: true,
            zoom: 1.0,
            devtools: true,
            user_agent: String::new(),
            hotkey: true,
        }
    }
}

impl Config {
    /// Loads the config, falling back to defaults if the file does not exist.
    /// A malformed file is an error: we never silently discard user settings.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Parses TOML text and normalises out-of-range values.
    pub fn parse(text: &str) -> io::Result<Self> {
        let mut cfg: Self = toml::from_str(text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        cfg.normalise();
        Ok(cfg)
    }

    /// Writes the config atomically (temp file + rename) so a crash mid-write
    /// can never leave a truncated file behind.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, path)
    }

    /// Clamps numeric values into their supported range.
    pub fn normalise(&mut self) {
        if !self.zoom.is_finite() {
            self.zoom = 1.0;
        }
        self.zoom = self.zoom.clamp(0.25, 5.0);
    }

    /// Interprets [`Config::monitor`]; anything unrecognised means `Primary`.
    pub fn monitor_target(&self) -> MonitorTarget {
        match self.monitor.trim().to_ascii_lowercase().as_str() {
            "span" | "all" => MonitorTarget::Span,
            other => other.parse().map_or(MonitorTarget::Primary, MonitorTarget::Index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_take_defaults() {
        let cfg = Config::parse("source = \"https://example.com\"\nmuted = true\n").unwrap();
        assert_eq!(cfg.source, "https://example.com");
        assert!(cfg.muted);
        assert!(cfg.interactive, "unspecified keys must keep their default");
    }

    #[test]
    fn malformed_file_is_an_error() {
        assert!(Config::parse("zoom = \"big\"").is_err());
    }

    #[test]
    fn zoom_is_clamped() {
        assert_eq!(Config::parse("zoom = 99.0").unwrap().zoom, 5.0);
        assert_eq!(Config::parse("zoom = 0.0").unwrap().zoom, 0.25);
    }

    #[test]
    fn monitor_target_parsing() {
        let mut cfg = Config::default();
        assert_eq!(cfg.monitor_target(), MonitorTarget::Primary);
        cfg.monitor = "Span".into();
        assert_eq!(cfg.monitor_target(), MonitorTarget::Span);
        cfg.monitor = "2".into();
        assert_eq!(cfg.monitor_target(), MonitorTarget::Index(2));
        cfg.monitor = "nonsense".into();
        assert_eq!(cfg.monitor_target(), MonitorTarget::Primary);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = std::env::temp_dir().join(format!("livecanvas-cfg-{}.toml", std::process::id()));
        let cfg = Config { source: "C:\\walls\\clock".into(), zoom: 1.5, ..Config::default() };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::env::temp_dir().join("livecanvas-definitely-missing.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
    }
}
