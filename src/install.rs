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
    use crate::config::{claude_home, codex_home};
    vec![
        Target {
            agent: "claude",
            path: claude_home().join("settings.json"),
            events: CLAUDE_EVENTS,
        },
        Target {
            agent: "codex",
            path: codex_home().join("hooks.json"),
            events: CODEX_EVENTS,
        },
    ]
}

pub fn install() -> Result<String, String> {
    install_hooks_for(&hook_exe()?, false)
}

/// Para onde os hooks apontam: a cópia instalada, se existir; senão este exe.
/// Why: reinstalar os hooks rodando um build de dev desviava os agentes para `target/`.
fn hook_exe() -> Result<PathBuf, String> {
    let installed = installed_exe();
    if installed.is_file() {
        return Ok(installed);
    }
    std::env::current_exe().map_err(|e| e.to_string())
}

/// Caminho fixo do app, para hooks e autostart não dependerem da pasta de build.
pub fn installed_exe() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    base.join("Programs").join("Pets").join("pets.exe")
}

/// `pets.exe install`: copia este exe para `installed_exe()`, aponta hooks e autostart para lá
/// e reinicia o pet a partir da cópia instalada. Numa atualização (`from_update`) só refaz os
/// hooks dos agentes que já os tinham, para não desfazer um `uninstall-hooks`.
pub fn install_app(from_update: bool) -> Result<String, String> {
    let src = std::env::current_exe().map_err(|e| e.to_string())?;
    let dest = installed_exe();
    if same_file(&src, &dest) {
        return Err(
            "já está rodando da pasta instalada; rode `install` a partir do build novo.".into(),
        );
    }
    let was_running = crate::window::quit_running_instance();

    // Why: daqui em diante o pet antigo já foi fechado; qualquer falha ainda deixa um rodando.
    if let Err(e) = replace_exe(&src, &dest) {
        if was_running {
            let _ = spawn_detached(&dest, &[]);
        }
        return Err(e);
    }
    let mut lines = vec![format!("copiado para {}", dest.display())];
    let hooks = install_hooks_for(&dest, from_update);
    if crate::system::autostart_enabled() {
        crate::system::set_autostart_exe(Some(&dest));
        lines.push("iniciar com o Windows agora aponta para a cópia instalada".into());
    }
    lines.push(create_shortcuts(&dest));
    spawn_detached(&dest, &[])?;
    lines.push(if was_running {
        "pet reiniciado".into()
    } else {
        "pet iniciado".into()
    });
    match hooks {
        Ok(line) => lines.push(line),
        Err(e) => return Err(format!("o pet foi instalado, mas os hooks não: {e}")),
    }
    Ok(lines.join("\n"))
}

fn replace_exe(src: &Path, dest: &Path) -> Result<(), String> {
    let dir = dest.parent().ok_or("caminho de instalação inválido")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let old = dest.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    // Why: o Windows deixa renomear um exe em uso (um hook rodando agora), mas não sobrescrever.
    if dest.exists() {
        std::fs::rename(dest, &old)
            .map_err(|e| format!("não consegui tirar o exe antigo do lugar: {e}"))?;
    }
    if let Err(e) = std::fs::copy(src, dest) {
        let _ = std::fs::rename(&old, dest);
        return Err(format!("não consegui copiar para {}: {e}", dest.display()));
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// Menu Iniciar a cada instalação (mantém o caminho certo); área de trabalho só uma vez, para
/// não recriar a cada atualização um atalho que o usuário apagou. Falha aqui não impede o install.
fn create_shortcuts(exe: &Path) -> String {
    use crate::system::{ShortcutDir, create_shortcut};
    let mut cfg = crate::config::Config::load();
    let mut done = Vec::new();
    let mut errors = Vec::new();
    match create_shortcut(ShortcutDir::Special("Programs"), exe) {
        Ok(()) => done.push("Menu Iniciar"),
        Err(e) => errors.push(format!("Menu Iniciar: {e}")),
    }
    if !cfg.desktop_shortcut_created {
        match create_shortcut(ShortcutDir::Special("Desktop"), exe) {
            Ok(()) => {
                done.push("área de trabalho");
                cfg.desktop_shortcut_created = true;
                cfg.save();
            }
            Err(e) => errors.push(format!("área de trabalho: {e}")),
        }
    }
    let mut line = if done.is_empty() {
        "atalhos não criados".to_string()
    } else {
        format!("atalho em: {}", done.join(" e "))
    };
    if !errors.is_empty() {
        line.push_str(&format!(" (aviso: {})", errors.join("; ")));
    }
    line
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

/// `only_installed`: pula o agente que não tem hooks do pets (o usuário tirou ou nunca quis).
fn install_hooks_for(exe: &Path, only_installed: bool) -> Result<String, String> {
    let mut lines = Vec::new();
    for t in targets() {
        if only_installed && !contains_ours(&std::fs::read_to_string(&t.path).unwrap_or_default()) {
            continue;
        }
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
    // Sem nada nosso para tirar, não regrava (o merge reformata o arquivo inteiro).
    if cmd.is_none() && !contains_ours(&text) {
        return Ok(false);
    }
    let merged = merge(&text, events, cmd).map_err(|e| format!("{}: {e}", path.display()))?;
    if merged == text {
        return Ok(false);
    }
    if path.exists() {
        let mut backup = path.as_os_str().to_owned();
        backup.push(".bak-pets");
        // Why: o backup é do arquivo de antes do pets; regravar a cada install perderia o original.
        if !Path::new(&backup).exists() {
            std::fs::copy(path, &backup)
                .map_err(|e| format!("backup de {}: {e}", path.display()))?;
        }
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
    group["hooks"]
        .as_array()
        .is_some_and(|hooks| hooks.iter().any(is_our_hook))
}

/// O formato exato de `hook_command`, com qualquer pasta: `<...>/pets.exe hook <agente>`, com
/// ou sem aspas. `codex-pets.exe hook` ou um script que só cita pets.exe não são nossos.
/// O exe pode ter o nome que o navegador dá a um download repetido (`pets (1).exe`).
fn is_our_hook(hook: &Value) -> bool {
    let Some(cmd) = hook["command"].as_str() else {
        return false;
    };
    let cmd = cmd.trim();
    let (exe, rest) = match cmd.strip_prefix('"') {
        Some(quoted) => match quoted.split_once('"') {
            Some(parts) => parts,
            None => return false,
        },
        None => match cmd.to_ascii_lowercase().find(" hook ") {
            Some(i) => cmd.split_at(i),
            None => return false,
        },
    };
    let file = exe.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut args = rest.split_whitespace();
    is_pets_exe(file)
        && args.next().is_some_and(|a| a.eq_ignore_ascii_case("hook"))
        && args.next().is_some()
        && args.next().is_none()
}

/// `pets.exe`, ou `pets (N).exe` de quem baixou mais de uma vez e rodou direto do Downloads.
fn is_pets_exe(file: &str) -> bool {
    let file = file.to_ascii_lowercase();
    let Some(stem) = file.strip_suffix(".exe") else {
        return false;
    };
    let Some(copy) = stem.strip_prefix("pets") else {
        return false;
    };
    let copy = copy.trim_start();
    copy.is_empty()
        || copy
            .strip_prefix('(')
            .and_then(|n| n.strip_suffix(')'))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Algum hook do pets no arquivo, apontando para qualquer exe.
fn contains_ours(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|root| {
        root["hooks"].as_object().is_some_and(|hooks| {
            hooks
                .values()
                .filter_map(Value::as_array)
                .any(|groups| groups.iter().any(is_ours))
        })
    })
}

/// Hooks instalados e apontando para `hook_exe()`, por agente.
pub struct HooksStatus {
    pub claude: bool,
    pub codex: bool,
}

pub fn status() -> HooksStatus {
    let Ok(exe) = hook_exe() else {
        return HooksStatus {
            claude: false,
            codex: false,
        };
    };
    let ok = |agent: &str| {
        targets()
            .into_iter()
            .find(|t| t.agent == agent)
            .is_some_and(|t| {
                let text = std::fs::read_to_string(&t.path).unwrap_or_default();
                has_hooks(&text, t.events, &hook_command(&exe, agent))
            })
    };
    HooksStatus {
        claude: ok("claude"),
        codex: ok("codex"),
    }
}

/// Todo evento tem um hook com exatamente `cmd`. Hook apontando para outro exe (pasta antiga,
/// build de dev) conta como faltando: aquele exe pode sumir e os eventos param de chegar.
pub fn has_hooks(text: &str, events: &[(&str, bool)], cmd: &str) -> bool {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    events.iter().all(|(event, _)| {
        root["hooks"][event].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                g["hooks"]
                    .as_array()
                    .is_some_and(|hooks| hooks.iter().any(|h| h["command"] == cmd))
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
        // Tira só o nosso hook de dentro de cada grupo: um grupo pode ter também o de outro app.
        arr.retain_mut(|g| {
            let Some(hooks) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let had = hooks.len();
            hooks.retain(|h| !is_our_hook(h));
            !(hooks.is_empty() && had > 0)
        });
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
    fn recognizes_only_our_exact_hook_command() {
        let ours = |c: &str| is_our_hook(&json!({ "command": c }));
        assert!(ours(CMD));
        assert!(ours(r#""C:/Program Files/Pets/pets.exe" hook codex"#));
        assert!(ours(
            r"C:\Users\x\AppData\Local\Programs\Pets\PETS.EXE hook claude"
        ));
        assert!(ours(r#""D:/Downloads/pets (1).exe" hook claude"#));
        assert!(ours("D:/Downloads/pets (12).exe hook codex"));
        assert!(!ours("C:/x/codex-pets.exe hook claude"));
        assert!(!ours("C:/x/pets (a).exe hook claude"));
        assert!(!ours("C:/x/pets ().exe hook claude"));
        assert!(!ours("C:/x/petshop.exe hook claude"));
        assert!(!ours("C:/x/pets.exe hook"));
        assert!(!ours("C:/x/pets.exe hook claude --extra"));
        assert!(!ours("node run.js pets.exe hook claude"));
    }

    #[test]
    fn removes_our_hook_but_keeps_a_shared_group() {
        let shared = format!(
            r#"{{"hooks":{{"Stop":[{{"hooks":[
                {{"type":"command","command":"{CMD}"}},
                {{"type":"command","command":"orca-agent-hook.exe claude"}}]}}]}}}}"#
        );
        let out: Value =
            serde_json::from_str(&merge(&shared, CLAUDE_EVENTS, None).unwrap()).unwrap();
        let hooks = out["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0]["command"], "orca-agent-hook.exe claude");
    }

    #[test]
    fn update_only_touches_agents_that_had_our_hooks() {
        assert!(!contains_ours(ORCA));
        assert!(!contains_ours(""));
        assert!(!contains_ours("{ quebrado"));
        // Aponta para outro exe e ainda conta: a atualização deve corrigir o caminho.
        assert!(contains_ours(
            &merge(ORCA, CLAUDE_EVENTS, Some(CMD)).unwrap()
        ));
    }

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
    fn status_requires_every_event_pointing_to_this_exe() {
        assert!(!has_hooks(ORCA, CLAUDE_EVENTS, CMD));
        let installed = merge(ORCA, CLAUDE_EVENTS, Some(CMD)).unwrap();
        assert!(has_hooks(&installed, CLAUDE_EVENTS, CMD));
        assert!(!has_hooks(
            &installed,
            CLAUDE_EVENTS,
            "C:/outro/pets.exe hook claude"
        ));
        let partial = merge(ORCA, &CLAUDE_EVENTS[..3], Some(CMD)).unwrap();
        assert!(!has_hooks(&partial, CLAUDE_EVENTS, CMD));
        assert!(!has_hooks("", CLAUDE_EVENTS, CMD));
    }

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(
            hook_command(Path::new(r"C:\Program Files\pets.exe"), "codex"),
            "\"C:/Program Files/pets.exe\" hook codex"
        );
    }
}
