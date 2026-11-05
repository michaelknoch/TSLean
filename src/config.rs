use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tsclientlib::Identity;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceMode {
    Activation,
    PushToTalk,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl From<Appearance> for eframe::egui::ThemePreference {
    fn from(a: Appearance) -> Self {
        match a {
            Appearance::System => Self::System,
            Appearance::Light => Self::Light,
            Appearance::Dark => Self::Dark,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub address: String,
    pub nickname: String,
    pub server_password: String,
    pub identity: Option<Identity>,
    pub voice_mode: VoiceMode,
    pub vad_threshold_db: f32,
    pub ptt_hotkey: String,
    pub input_muted: bool,
    pub output_muted: bool,
    pub appearance: Appearance,
    /// Playback volume per user, keyed by their unique identity.
    pub volumes: HashMap<String, f32>,
    pub recent: Vec<Recent>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub address: String,
    pub nickname: String,
    pub password: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            address: String::new(),
            nickname: std::env::var("USER").unwrap_or_default(),
            server_password: String::new(),
            identity: None,
            voice_mode: VoiceMode::Activation,
            vad_threshold_db: -45.0,
            ptt_hotkey: "Alt+Space".into(),
            input_muted: false,
            output_muted: false,
            appearance: Appearance::System,
            volumes: HashMap::new(),
            recent: Vec::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("TSLean").join("config.json"))
}

impl Config {
    pub fn load() -> Self {
        path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }
}
