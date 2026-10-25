//! O que o pet percebe do computador sozinho, além dos agentes: reunião (algum app usando o
//! microfone), app em primeiro plano, download concluído e você longe do teclado. Tudo chega por
//! eventos do Windows; só a ausência usa um timer, e ele dispara uma vez a cada 5 min.

use crate::system::wide;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME,
    RRF_RT_REG_QWORD, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegNotifyChangeKeyValue, RegOpenKeyExW,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetWindowThreadProcessId, PostMessageW,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
};

/// Sem mexer no mouse nem no teclado por este tempo, o pet entende que você saiu.
pub const AWAY_AFTER: Duration = Duration::from_secs(5 * 60);
/// Enquanto você está fora, de quanto em quanto tempo ver se voltou (para acenar logo).
pub const AWAY_POLL: Duration = Duration::from_secs(1);

/// Onde o Windows anota quem usa o microfone: uma subchave por app, e os apps não empacotados
/// dentro de `NonPackaged`. Em uso = `LastUsedTimeStop` zerado.
const MIC_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";
const SHELL_FOLDERS_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders";
/// FOLDERID_Downloads; a pasta pode ter sido movida para outro disco.
const DOWNLOADS_VALUE: &str = "{374DE290-123F-4565-9164-39C4925E467B}";

/// Apps em que o pet "programa junto" (nome do exe, minúsculo).
const CODING_APPS: &[&str] = &[
    "code.exe",
    "code - insiders.exe",
    "cursor.exe",
    "windsurf.exe",
    "zed.exe",
    "devenv.exe",
    "idea64.exe",
    "pycharm64.exe",
    "webstorm64.exe",
    "rider64.exe",
    "clion64.exe",
    "goland64.exe",
    "rustrover64.exe",
    "phpstorm64.exe",
    "studio64.exe",
    "sublime_text.exe",
    "neovide.exe",
    // Agentes de código com janela própria.
    "orca.exe",
    "claude.exe",
    "windowsterminal.exe",
    "wezterm-gui.exe",
    "alacritty.exe",
    "mintty.exe",
    "conhost.exe",
    "openconsole.exe",
    "powershell.exe",
    "pwsh.exe",
    "cmd.exe",
];

/// Arquivos que o navegador ainda está baixando; o download termina quando eles são renomeados
/// para o nome final.
const PARTIAL_DOWNLOADS: &[&str] = &[".crdownload", ".part", ".partial", ".download", ".tmp"];

pub fn is_coding_app(exe: &str) -> bool {
    CODING_APPS.contains(&exe.to_lowercase().as_str())
}

pub fn is_finished_download(name: &str) -> bool {
    let name = name.to_lowercase();
    !name.contains('\\') && !PARTIAL_DOWNLOADS.iter().any(|ext| name.ends_with(ext))
}

fn mic_in_use(start: Option<u64>, stop: Option<u64>) -> bool {
    start.is_some_and(|s| s != 0) && stop == Some(0)
}

/// Há quanto tempo ninguém mexe no mouse ou no teclado.
pub fn idle_for() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: struct com cbSize preenchido.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // SAFETY: sem argumentos. Os dois contadores dão a volta juntos a cada 49 dias.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dwTime) as u64)
}

/// Nome do exe da janela em primeiro plano, em minúsculas.
pub fn foreground_exe() -> Option<String> {
    // SAFETY: consultas ao processo da janela ativa; o handle é fechado antes de sair.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(str::to_lowercase)
    }
}

static FOREGROUND_HWND: AtomicIsize = AtomicIsize::new(0);
static FOREGROUND_MSG: AtomicU32 = AtomicU32::new(0);

/// Posta `message` na janela toda vez que outra janela vem para a frente. O callback roda na
/// thread da janela, mas só posta: quem trata lê `foreground_exe` fora de qualquer empréstimo.
pub fn watch_foreground(hwnd: isize, message: u32) {
    unsafe extern "system" fn on_event(
        _hook: HWINEVENTHOOK,
        _event: u32,
        _hwnd: HWND,
        _object: i32,
        _child: i32,
        _thread: u32,
        _time: u32,
    ) {
        let hwnd = FOREGROUND_HWND.load(Ordering::SeqCst);
        let message = FOREGROUND_MSG.load(Ordering::SeqCst);
        // SAFETY: PostMessageW para a nossa janela.
        unsafe { PostMessageW(hwnd as HWND, message, 0, 0) };
    }
    FOREGROUND_HWND.store(hwnd, Ordering::SeqCst);
    FOREGROUND_MSG.store(message, Ordering::SeqCst);
    // SAFETY: OUTOFCONTEXT entrega o evento pela fila desta thread; o gancho vive até o app fechar.
    unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(on_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
}

/// Vigia numa thread (bloqueada em `RegNotifyChangeKeyValue`, sem polling) se algum app está
/// usando o microfone, e posta `message` com wParam 1/0 quando isso muda (e uma vez ao começar).
pub fn spawn_mic_watch(hwnd: isize, message: u32) {
    std::thread::spawn(move || {
        let path = wide(MIC_KEY);
        let mut last = None;
        loop {
            let mut key: HKEY = std::ptr::null_mut();
            // SAFETY: caminho terminado em NUL; a chave é fechada no fim da volta.
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) }
                != ERROR_SUCCESS
            {
                // Nenhum app usou o microfone ainda: a chave aparece no primeiro uso.
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
            loop {
                let in_use = any_app_using(key);
                if last != Some(in_use) {
                    last = Some(in_use);
                    // SAFETY: PostMessageW para a nossa janela.
                    unsafe { PostMessageW(hwnd as HWND, message, in_use as usize, 0) };
                }
                // SAFETY: chave aberta acima; síncrono, sem evento: volta quando algo muda.
                let ok = unsafe {
                    RegNotifyChangeKeyValue(
                        key,
                        1,
                        REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET,
                        std::ptr::null_mut(),
                        0,
                    )
                };
                if ok != ERROR_SUCCESS {
                    break;
                }
            }
            // SAFETY: chave aberta acima.
            unsafe { RegCloseKey(key) };
            // Why: como no vigia de pastas, reabrir na hora depois de uma falha giraria a CPU.
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

fn any_app_using(key: HKEY) -> bool {
    subkeys(key).iter().any(|name| {
        if String::from_utf16_lossy(&name[..name.len() - 1]) != "NonPackaged" {
            return app_using(key, name);
        }
        let mut inner: HKEY = std::ptr::null_mut();
        // SAFETY: nome terminado em NUL vindo de RegEnumKeyExW; fechada logo abaixo.
        if unsafe { RegOpenKeyExW(key, name.as_ptr(), 0, KEY_READ, &mut inner) } != ERROR_SUCCESS {
            return false;
        }
        let using = subkeys(inner).iter().any(|app| app_using(inner, app));
        // SAFETY: chave aberta acima.
        unsafe { RegCloseKey(inner) };
        using
    })
}

/// Nomes das subchaves, terminados em NUL.
fn subkeys(key: HKEY) -> Vec<Vec<u16>> {
    let mut names = Vec::new();
    for index in 0.. {
        // Nomes de chave têm no máximo 255 caracteres.
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: buffer nosso com o tamanho informado.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                buf.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(buf[..=len as usize].to_vec());
    }
    names
}

fn app_using(key: HKEY, app: &[u16]) -> bool {
    let read = |value: &str| {
        let value = wide(value);
        let mut data = 0u64;
        let mut size = size_of::<u64>() as u32;
        // SAFETY: strings terminadas em NUL; o QWORD cabe em `data`.
        let status = unsafe {
            RegGetValueW(
                key,
                app.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_QWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u64).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(data)
    };
    mic_in_use(read("LastUsedTimeStart"), read("LastUsedTimeStop"))
}

/// Vigia a pasta de Downloads e posta `message` quando um download termina.
pub fn spawn_download_watch(hwnd: isize, message: u32) {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ACTION_RENAMED_NEW_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    };
    crate::system::watch_dir(
        downloads_dir(),
        false,
        FILE_NOTIFY_CHANGE_FILE_NAME,
        move |entries| {
            // Chrome, Edge e Firefox baixam para um nome temporário e renomeiam no fim.
            let finished = entries.into_iter().flatten().any(|(action, name)| {
                action == FILE_ACTION_RENAMED_NEW_NAME && is_finished_download(&name)
            });
            if finished {
                // SAFETY: PostMessageW para a nossa janela.
                unsafe { PostMessageW(hwnd as HWND, message, 0, 0) };
            }
        },
    );
}

fn downloads_dir() -> PathBuf {
    let (key, value) = (wide(SHELL_FOLDERS_KEY), wide(DOWNLOADS_VALUE));
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: strings terminadas em NUL; buffer nosso com o tamanho informado. Sem RRF_NOEXPAND,
    // o `%USERPROFILE%` do REG_EXPAND_SZ já vem expandido.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status == ERROR_SUCCESS {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        return PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    }
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Downloads")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_apps_ignore_case() {
        assert!(is_coding_app("Code.exe"));
        assert!(is_coding_app("WindowsTerminal.exe"));
        assert!(is_coding_app("Orca.exe"));
        assert!(is_coding_app("claude.exe"));
        assert!(!is_coding_app("chrome.exe"));
        assert!(!is_coding_app("ms-teams.exe"));
    }

    #[test]
    fn partial_downloads_are_not_finished() {
        assert!(is_finished_download("relatorio.pdf"));
        assert!(is_finished_download("Setup.EXE"));
        assert!(!is_finished_download("Unconfirmed 123.crdownload"));
        assert!(!is_finished_download("relatorio.pdf.part"));
        assert!(!is_finished_download("sub\\arquivo.zip"));
    }

    #[test]
    fn mic_in_use_needs_start_without_stop() {
        assert!(mic_in_use(Some(134_000), Some(0)));
        assert!(!mic_in_use(Some(134_000), Some(135_000)));
        assert!(!mic_in_use(None, None));
        assert!(!mic_in_use(Some(0), Some(0)));
    }
}
