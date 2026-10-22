//! `install-hooks`/`uninstall-hooks`: põe ou tira o `pets.exe hook` nos hooks do Claude Code e do
//! Codex, sem mexer nas entradas de outros apps (Orca, Codenotch). Backup `.bak-pets` antes de gravar.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// (evento, usa matcher de ferramenta)
const CLAUDE_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("PostToolUseFailure", true),
    ("PermissionRequest", true),
    ("Notification", false),
    ("Stop", false),
    ("StopFailure", false),
    ("SubagentStart", false),
    ("SessionEnd", false),
];

const CODEX_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("PermissionRequest", true),
    ("Stop", false),
    ("SubagentStart", false),
    ("Interrupt", false),
];

struct Target {
    agent: &'static str,
    path: PathBuf,
    events: &'static [(&'static str, bool)],
}

fn targets() -> Vec<Target> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default();
    vec![
        Target {
            agent: "claude",
            path: home.join(".claude").join("settings.json"),
            events: CLAUDE_EVENTS,
        },
        Target {
            agent: "codex",
            path: home.join(".codex").join("hooks.json"),
            events: CODEX_EVENTS,
        },
    ]
}

pub fn install() -> Result<String, String> {
    install_hooks_for(&std::env::current_exe().map_err(|e| e.to_string())?)
}

/// Caminho fixo do app, para hooks e autostart não dependerem da pasta de build.
pub fn installed_exe() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    base.join("Programs").join("Pets").join("pets.exe")
}

/// `pets.exe install`: copia este exe para `installed_exe()`, aponta hooks e autostart para lá
/// e reinicia o pet a partir da cópia instalada.
pub fn install_app() -> Result<String, String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dest = installed_exe();
    if same_file(&src, &dest) {
        return Err(
            "já está rodando da pasta instalada; rode `install` a partir do build novo.".into(),
        );
    }
    let was_running = crate::window::quit_running_instance();

    let dir = dest.parent().ok_or("caminho de instalação inválido")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let old = dest.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    // Why: o Windows deixa renomear um exe em uso (um hook rodando agora), mas não sobrescrever.
    if dest.exists() {
        std::fs::rename(&dest, &old)
            .map_err(|e| format!("não consegui tirar o exe antigo do lugar: {e}"))?;
    }
    if let Err(e) = std::fs::copy(&src, &dest) {
        let _ = std::fs::rename(&old, &dest);
        return Err(format!("não consegui copiar para {}: {e}", dest.display()));
    }
    let _ = std::fs::remove_file(&old);

    let mut lines = vec![format!("copiado para {}", dest.display())];
    lines.push(install_hooks_for(&dest)?);
    if crate::system::autostart_enabled() {
        crate::system::set_autostart_exe(Some(&dest));
        lines.push("iniciar com o Windows agora aponta para a cópia instalada".into());
    }
    spawn_detached(&dest, &[])?;
    lines.push(if was_running {
        "pet reiniciado".into()
    } else {
        "pet iniciado".into()
    });
    Ok(lines.join("\n"))
}

/// Why: se o pet herdar o stdout redirecionado de quem chamou o install, o chamador fica
/// esperando o pipe fechar, e o pet nunca fecha. Tira a herança dos std handles antes do spawn.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: só muda a flag de herança dos handles deste processo.
        unsafe { SetHandleInformation(GetStdHandle(id), HANDLE_FLAG_INHERIT, 0) };
    }
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(drop)
        .map_err(|e| format!("não consegui iniciar o pet: {e}"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn install_hooks_for(exe: &Path) -> Result<String, String> {
    let mut lines = Vec::new();
    for t in targets() {
        let cmd = hook_command(exe, t.agent);
        let changed = update_file(&t.path, t.events, Some(&cmd))?;
        lines.push(format!(
            "{} {}",
            if changed {
                "instalado em"
            } else {
                "já estava em"
            },
            t.path.display()
        ));
    }
    if exe.components().any(|c| c.as_os_str() == "debug") {
        lines.push("aviso: os hooks apontam para o build debug; prefira `pets.exe install` a partir do release.".into());
    }
    lines.push("O Codex pode pedir para confiar nos hooks novos na próxima sessão.".into());
    Ok(lines.join("\n"))
}

pub fn uninstall() -> Result<String, String> {
    let mut lines = Vec::new();
    for t in targets() {
        if update_file(&t.path, t.events, None)? {
            lines.push(format!("removido de {}", t.path.display()));
        }
    }
    if lines.is_empty() {
        lines.push("nenhum hook do pets encontrado.".into());
    }
    Ok(lines.join("\n"))
}

fn hook_command(exe: &Path, agent: &str) -> String {
    let exe = exe.display().to_string().replace('\\', "/");
    if exe.contains(' ') {
        format!("\"{exe}\" hook {agent}")
    } else {
        format!("{exe} hook {agent}")
    }
}

fn update_file(path: &Path, events: &[(&str, bool)], cmd: Option<&str>) -> Result<bool, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if cmd.is_none() {
                return Ok(false);
            }
            String::new()
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let merged = merge(&text, events, cmd).map_err(|e| format!("{}: {e}", path.display()))?;
    if merged == text {
        return Ok(false);
    }
    if path.exists() {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak-pets");
        std::fs::copy(path, &backup).map_err(|e| format!("backup de {}: {e}", path.display()))?;
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp-pets");
    std::fs::write(&tmp, &merged).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(true)
}

fn is_ours(group: &Value) -> bool {
    group["hooks"].as_array().is_some_and(|hooks| {
        hooks.iter().any(|h| {
            h["command"].as_str().is_some_and(|c| {
                let c = c.to_ascii_lowercase();
                c.contains("pets.exe") && c.contains(" hook ")
            })
        })
    })
}

/// Tira as entradas antigas do pets e, com `cmd`, põe as novas no fim de cada evento.
pub fn merge(text: &str, events: &[(&str, bool)], cmd: Option<&str>) -> Result<String, String> {
    let mut root: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("JSON inválido: {e}"))?
    };
    let obj = root.as_object_mut().ok_or("a raiz não é um objeto")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" não é um objeto")?;

    hooks.retain(|_, groups| {
        let Some(arr) = groups.as_array_mut() else {
            return true;
        };
        let before = arr.len();
        arr.retain(|g| !is_ours(g));
        // Só some com o evento se fomos nós que o esvaziamos.
        !(arr.is_empty() && before > 0)
    });

    if let Some(cmd) = cmd {
        for &(event, tool) in events {
            let mut group = Map::new();
            if tool {
                group.insert("matcher".into(), json!("*"));
            }
            group.insert(
                "hooks".into(),
                json!([{ "type": "command", "command": cmd, "timeout": 5 }]),
            );
            let list = hooks.entry(event).or_insert_with(|| json!([]));
            list.as_array_mut()
                .ok_or_else(|| format!("hooks.{event} não é uma lista"))?
                .push(Value::Object(group));
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }

    let mut out = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORCA: &str = r#"{
  "model": "opus",
  "hooks": {
    "Stop": [
      { "hooks": [ { "type": "command", "command": "C:/Users/x/.orca/agent-hooks/orca-agent-hook.exe claude" } ] }
    ]
  }
}"#;
    const CMD: &str = "C:/repos/Pets-Prj/target/release/pets.exe hook claude";

    #[test]
    fn install_keeps_other_hooks_and_is_idempotent() {
        let once = merge(ORCA, CLAUDE_EVENTS, Some(CMD)).unwrap();
        let v: Value = serde_json::from_str(&once).unwrap();
        assert_eq!(v["model"], "opus");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert!(
            v["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("orca")
        );
        assert_eq!(v["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert!(v["hooks"]["Stop"][1].get("matcher").is_none());
        assert_eq!(v["hooks"].as_object().unwrap().len(), CLAUDE_EVENTS.len());
        assert_eq!(merge(&once, CLAUDE_EVENTS, Some(CMD)).unwrap(), once);
        // A ordem das chaves do usuário é preservada.
        assert!(once.find("\"model\"").unwrap() < once.find("\"hooks\"").unwrap());
    }

    #[test]
    fn uninstall_restores_original_shape() {
        let installed = merge(ORCA, CLAUDE_EVENTS, Some(CMD)).unwrap();
        let removed = merge(&installed, CLAUDE_EVENTS, None).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&removed).unwrap(),
            serde_json::from_str::<Value>(ORCA).unwrap()
        );
    }

    #[test]
    fn creates_hooks_in_empty_file() {
        let v: Value =
            serde_json::from_str(&merge("", CODEX_EVENTS, Some("pets.exe hook codex")).unwrap())
                .unwrap();
        assert_eq!(
            v["hooks"]["Interrupt"][0]["hooks"][0]["command"],
            "pets.exe hook codex"
        );
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(
            hook_command(Path::new(r"C:\Program Files\pets.exe"), "codex"),
            "\"C:/Program Files/pets.exe\" hook codex"
        );
    }
}
