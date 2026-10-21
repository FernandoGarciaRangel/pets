//! `pets.exe hook <claude|codex>`: lê o payload do hook no stdin e repassa o essencial ao app.
//! Nunca bloqueia o agente: sem app rodando, sai com 0 em silêncio e não escreve nada no stdout.

use crate::agents::{HookEvent, PIPE_NAME};
use std::io::{Read, Write};

const ERROR_PIPE_BUSY: i32 = 231;

pub fn run(agent: &str) {
    let mut input = Vec::new();
    // Why: ler o stdin inteiro, senão o agente pode receber EPIPE ao escrever payloads grandes.
    let _ = std::io::stdin().read_to_end(&mut input);
    let Some(ev) = parse(agent, &input) else {
        return;
    };
    let Ok(mut line) = serde_json::to_string(&ev) else {
        return;
    };
    line.push('\n');
    send(line.as_bytes());
}

pub fn parse(agent: &str, input: &[u8]) -> Option<HookEvent> {
    let payload: serde_json::Value = serde_json::from_slice(input).ok()?;
    let field = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| payload.get(*n)?.as_str().map(str::to_string))
    };
    Some(HookEvent {
        agent: agent.to_string(),
        session: field(&["session_id", "sessionId"]).unwrap_or_else(|| "default".into()),
        event: field(&["hook_event_name", "hookEventName"])?,
        notification_type: field(&["notification_type", "notificationType"]),
    })
}

fn send(bytes: &[u8]) {
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    for _ in 0..3 {
        match std::fs::OpenOptions::new().write(true).open(PIPE_NAME) {
            Ok(mut pipe) => {
                let _ = pipe.write_all(bytes);
                return;
            }
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                let name = crate::system::wide(PIPE_NAME);
                // SAFETY: nome terminado em NUL.
                unsafe { WaitNamedPipeW(name.as_ptr(), 200) };
            }
            // App não está rodando.
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_and_codex_payloads() {
        let ev = parse(
            "claude",
            br#"{"session_id":"s1","hook_event_name":"Notification","notification_type":"permission_prompt","message":"x"}"#,
        )
        .unwrap();
        assert_eq!(ev.session, "s1");
        assert_eq!(ev.event, "Notification");
        assert_eq!(ev.notification_type.as_deref(), Some("permission_prompt"));

        let ev = parse("codex", br#"{"hook_event_name":"Stop"}"#).unwrap();
        assert_eq!(
            (ev.agent.as_str(), ev.session.as_str()),
            ("codex", "default")
        );

        assert!(parse("codex", b"not json").is_none());
        assert!(parse("codex", br#"{"session_id":"s"}"#).is_none());
    }
}
