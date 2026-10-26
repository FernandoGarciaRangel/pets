use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const MIN_WIDTH: u32 = 48;
pub const MAX_WIDTH: u32 = 512;
pub const DEFAULT_WIDTH: u32 = 160;

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
    /// Pets embutidos já instalados uma vez (não voltam se o usuário apagar), com a impressão
    /// digital da versão gravada (`builtin::sync`); vazia nos instalados antes da v0.5.2.
    #[serde(deserialize_with = "builtins_list_or_map")]
    pub installed_builtins: BTreeMap<String, String>,
    /// O atalho da área de trabalho já foi criado uma vez; se o usuário apagar, não volta.
    pub desktop_shortcut_created: bool,
    /// O que o pet percebe sozinho (`activity.rs`); cada um liga e desliga no menu.
    pub detect_meeting: bool,
    pub detect_away: bool,
    pub detect_downloads: bool,
    pub detect_apps: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            pet: None,
            width: DEFAULT_WIDTH,
            x: None,
            y: None,
            visible: true,
            pets_dir: None,
            check_updates: true,
            installed_builtins: BTreeMap::new(),
            desktop_shortcut_created: false,
            detect_meeting: true,
            detect_away: true,
            detect_downloads: true,
            detect_apps: true,
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
            .and_then(|text| toml::from_str::<Config>(&text).ok())
            .map(|mut cfg| {
                cfg.width = cfg.width.clamp(MIN_WIDTH, MAX_WIDTH);
                cfg
            })
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

/// Até a v0.5.1 era só a lista de ids.
fn builtins_list_or_map<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Builtins {
        Ids(Vec<String>),
        Fingerprints(BTreeMap<String, String>),
    }
    Ok(match Builtins::deserialize(d)? {
        Builtins::Ids(ids) => ids.into_iter().map(|id| (id, String::new())).collect(),
        Builtins::Fingerprints(map) => map,
    })
}

pub fn default_pets_dir() -> PathBuf {
    codex_home().join("pets")
}

/// Pasta de config do Codex: `CODEX_HOME`, como o próprio Codex, ou `~/.codex`.
pub fn codex_home() -> PathBuf {
    agent_home("CODEX_HOME", ".codex")
}

/// Pasta de config do Claude Code: `CLAUDE_CONFIG_DIR`, como o próprio Claude, ou `~/.claude`.
pub fn claude_home() -> PathBuf {
    agent_home("CLAUDE_CONFIG_DIR", ".claude")
}

fn agent_home(var: &str, default: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_old_builtin_list_and_round_trips_the_map() {
        let old: Config = toml::from_str(r#"installed_builtins = ["draco"]"#).unwrap();
        assert_eq!(old.installed_builtins["draco"], "");
        let mut cfg = Config::default();
        cfg.installed_builtins.insert("draco".into(), "abc".into());
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.installed_builtins, cfg.installed_builtins);
        assert!(back.visible);
    }
}
