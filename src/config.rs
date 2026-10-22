use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const SIZE_PRESETS: [u32; 5] = [96, 128, 160, 192, 256];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Nome da pasta do pet dentro de `pets_dir`.
    pub pet: Option<String>,
    /// Largura do pet na tela, em pixels físicos.
    pub width: u32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub visible: bool,
    pub pets_dir: Option<PathBuf>,
    /// Procura versão nova no GitHub ao iniciar e a cada 6 h.
    pub check_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pet: None,
            width: 160,
            x: None,
            y: None,
            visible: true,
            pets_dir: None,
            check_updates: true,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("pets").join("config.toml")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        let Ok(text) = toml::to_string(self) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Why: tmp + rename evita um config.toml truncado se o app morrer no meio da escrita.
        let tmp = path.with_extension("toml.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn pets_dir(&self) -> PathBuf {
        self.pets_dir.clone().unwrap_or_else(default_pets_dir)
    }
}

pub fn default_pets_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("pets")
}
