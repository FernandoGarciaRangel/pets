//! Estado dos agentes: o servidor do named pipe recebe os eventos de `pets.exe hook` e o
//! agregador decide um estado único para o pet (prioridade igual à do `agentStateAnimation` do Orca).

use crate::animation::AgentState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const PIPE_NAME: &str = r"\\.\pipe\pets-agent-state";

const REVIEW_FOR: Duration = Duration::from_secs(60);
const FAILED_FOR: Duration = Duration::from_secs(5);
// Why: sem evento por muito tempo, a sessão provavelmente morreu sem `SessionEnd` (terminal fechado).
const WORKING_STALE: Duration = Duration::from_secs(10 * 60);
const WAITING_STALE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookEvent {
    pub agent: String,
    pub session: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
}

enum Action {
    Set(AgentState),
    Remove,
}

fn action(ev: &HookEvent) -> Option<Action> {
    Some(match ev.event.as_str() {
        "SessionStart" | "Interrupt" => Action::Set(AgentState::Idle),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        | "SubagentStart" => Action::Set(AgentState::Working),
        "PermissionRequest" => Action::Set(AgentState::Waiting),
        "Notification" => match ev.notification_type.as_deref() {
            Some("permission_prompt" | "elicitation_dialog") => Action::Set(AgentState::Waiting),
            _ => return None,
        },
        "Stop" => Action::Set(AgentState::Done),
        "StopFailure" => Action::Set(AgentState::Failed),
        "SessionEnd" => Action::Remove,
        _ => return None,
    })
}

fn rank(state: AgentState) -> u8 {
    match state {
        AgentState::Idle => 0,
        AgentState::Done => 1,
        AgentState::Failed => 2,
        AgentState::Working => 3,
        AgentState::Waiting => 4,
    }
}

struct Entry {
    state: AgentState,
    at: Instant,
}

impl Entry {
    fn lifetime(&self) -> Option<Duration> {
        match self.state {
            AgentState::Idle => None,
            AgentState::Working => Some(WORKING_STALE),
            AgentState::Waiting => Some(WAITING_STALE),
            AgentState::Done => Some(REVIEW_FOR),
            AgentState::Failed => Some(FAILED_FOR),
        }
    }

    fn effective(&self, now: Instant) -> Option<AgentState> {
        let lifetime = self.lifetime()?;
        (now.saturating_duration_since(self.at) < lifetime).then_some(self.state)
    }
}

#[derive(Default)]
pub struct Sessions {
    map: HashMap<(String, String), Entry>,
}

impl Sessions {
    pub fn apply(&mut self, ev: &HookEvent, now: Instant) {
        let key = (ev.agent.clone(), ev.session.clone());
        match action(ev) {
            Some(Action::Set(state)) => {
                self.map.insert(key, Entry { state, at: now });
            }
            Some(Action::Remove) => {
                self.map.remove(&key);
            }
            None => {}
        }
    }

    pub fn aggregate(&mut self, now: Instant) -> AgentState {
        self.map.retain(|_, e| e.effective(now).is_some());
        self.map
            .values()
            .filter_map(|e| e.effective(now))
            .max_by_key(|&s| rank(s))
            .unwrap_or(AgentState::Idle)
    }

    /// Quando o agregado pode mudar sozinho (fim do `review`, sessão velha).
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        self.map
            .values()
            .filter_map(|e| Some((e.at + e.lifetime()?).saturating_duration_since(now)))
            .min()
    }

    pub fn active_count(&self) -> usize {
        self.map.len()
    }
}

/// Thread que aceita conexões no pipe e entrega cada evento à janela via `PostMessageW`.
pub fn spawn_pipe_server(hwnd: isize, message: u32) {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OVERLAPPED, PIPE_ACCESS_INBOUND, ReadFile,
    };
    use windows_sys::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

    /// O hook escreve uma linha e fecha na hora; um cliente que segura a conexão além disso
    /// travaria todos os eventos seguintes.
    const READ_DEADLINE: Duration = Duration::from_millis(250);
    const MAX_EVENT_BYTES: usize = 64 * 1024;

    /// Lê até o cliente fechar, `MAX_EVENT_BYTES` ou `READ_DEADLINE`, o que vier antes.
    ///
    /// SAFETY: `pipe` é uma instância conectada aberta com FILE_FLAG_OVERLAPPED e `event` é um
    /// evento de reset manual; os dois continuam abertos durante a chamada.
    unsafe fn read_event(pipe: HANDLE, event: HANDLE) -> Vec<u8> {
        let deadline = Instant::now() + READ_DEADLINE;
        let mut buf = vec![0u8; MAX_EVENT_BYTES];
        let mut len = 0usize;
        while len < buf.len() {
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = event;
            let mut n = 0u32;
            let started = unsafe {
                ReadFile(
                    pipe,
                    buf[len..].as_mut_ptr(),
                    (buf.len() - len) as u32,
                    std::ptr::null_mut(),
                    &mut ov,
                )
            };
            if started == 0 {
                if unsafe { GetLastError() } != ERROR_IO_PENDING {
                    break; // ERROR_BROKEN_PIPE: o cliente terminou de escrever e fechou.
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if unsafe { WaitForSingleObject(event, left.as_millis() as u32) } != WAIT_OBJECT_0 {
                    unsafe {
                        CancelIo(pipe);
                        // Espera o cancelamento: `ov` e `buf` não podem sair de cena antes.
                        GetOverlappedResult(pipe, &ov, &mut n, 1);
                    }
                    break;
                }
            }
            if unsafe { GetOverlappedResult(pipe, &ov, &mut n, 1) } == 0 || n == 0 {
                break;
            }
            len += n as usize;
        }
        buf.truncate(len);
        buf
    }

    std::thread::spawn(move || {
        let name = crate::system::wide(PIPE_NAME);
        let create = || loop {
            // SAFETY: nome terminado em NUL; segurança padrão (só o usuário atual e admins escrevem).
            let handle = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    0,
                    MAX_EVENT_BYTES as u32,
                    0,
                    std::ptr::null(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return handle;
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        // SAFETY: evento de reset manual, sem nome; vive enquanto a thread vive.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            return;
        }
        let mut next = create();
        loop {
            let handle = next;
            // SAFETY: handle aberto com FILE_FLAG_OVERLAPPED, então ConnectNamedPipe exige um
            // OVERLAPPED; esperar a conexão não tem prazo (é o app ocioso esperando hooks).
            let connected = unsafe {
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = event;
                let mut n = 0u32;
                ConnectNamedPipe(handle, &mut ov) != 0
                    || match GetLastError() {
                        // NO_DATA: o cliente já escreveu e fechou; o que ele mandou ainda está lá.
                        ERROR_PIPE_CONNECTED | ERROR_NO_DATA => true,
                        ERROR_IO_PENDING => GetOverlappedResult(handle, &ov, &mut n, 1) != 0,
                        _ => false,
                    }
            };
            // Why: a próxima instância nasce antes de ler esta. Sem isso, entre um evento e outro
            // o pipe não existia, e o hook seguinte achava que o app estava fechado e desistia.
            next = create();
            // SAFETY: instância conectada acima, aberta com FILE_FLAG_OVERLAPPED; fechada logo depois.
            let buf = if connected {
                unsafe { read_event(handle, event) }
            } else {
                Vec::new()
            };
            unsafe { CloseHandle(handle) };

            for line in String::from_utf8_lossy(&buf).lines() {
                let Ok(ev) = serde_json::from_str::<HookEvent>(line) else {
                    continue;
                };
                if cfg!(debug_assertions) {
                    eprintln!("[hook] {} {} {}", ev.agent, ev.event, ev.session);
                }
                let ptr = Box::into_raw(Box::new(ev));
                // SAFETY: a janela retoma a posse do Box em WM_APP_HOOK; se o post falhar, liberamos aqui.
                if unsafe { PostMessageW(hwnd as _, message, 0, ptr as isize) } == 0 {
                    drop(unsafe { Box::from_raw(ptr) });
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(agent: &str, session: &str, event: &str) -> HookEvent {
        HookEvent {
            agent: agent.into(),
            session: session.into(),
            event: event.into(),
            notification_type: None,
        }
    }

    #[test]
    fn single_session_lifecycle() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(&ev("codex", "a", "SessionStart"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Idle);
        s.apply(&ev("codex", "a", "PreToolUse"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Working);
        s.apply(&ev("codex", "a", "PermissionRequest"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Waiting);
        s.apply(&ev("codex", "a", "PostToolUse"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Working);
        s.apply(&ev("codex", "a", "Stop"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Done);
        assert_eq!(s.next_deadline(t0), Some(REVIEW_FOR));
        assert_eq!(s.aggregate(t0 + REVIEW_FOR), AgentState::Idle);
        assert_eq!(s.active_count(), 0);
    }

    #[test]
    fn waiting_wins_over_working_across_sessions() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(&ev("claude", "a", "PreToolUse"), t0);
        s.apply(&ev("codex", "b", "PermissionRequest"), t0);
        s.apply(&ev("claude", "c", "Stop"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Waiting);
        s.apply(&ev("codex", "b", "SessionEnd"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Working);
    }

    #[test]
    fn same_session_id_on_different_agents_is_separate() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(&ev("claude", "x", "PreToolUse"), t0);
        s.apply(&ev("codex", "x", "Stop"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Working);
    }

    #[test]
    fn claude_notifications_only_wait_on_prompts() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        let mut n = ev("claude", "a", "Notification");
        n.notification_type = Some("idle_prompt".into());
        s.apply(&n, t0);
        assert_eq!(s.aggregate(t0), AgentState::Idle);
        n.notification_type = Some("permission_prompt".into());
        s.apply(&n, t0);
        assert_eq!(s.aggregate(t0), AgentState::Waiting);
    }

    #[test]
    fn failure_shows_briefly_and_stale_work_expires() {
        let t0 = Instant::now();
        let mut s = Sessions::default();
        s.apply(&ev("claude", "a", "StopFailure"), t0);
        assert_eq!(s.aggregate(t0), AgentState::Failed);
        assert_eq!(s.aggregate(t0 + FAILED_FOR), AgentState::Idle);
        s.apply(&ev("claude", "b", "PreToolUse"), t0);
        assert_eq!(s.aggregate(t0 + WORKING_STALE), AgentState::Idle);
    }
}
