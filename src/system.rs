//! Pequenos utilitários Win32: strings UTF-16, console, início com o Windows, abrir pasta, aviso.

use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW,
    RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Pets";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// No build release o exe é GUI; para os comandos de CLI, reaproveita o console de quem chamou.
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: sem pré-condições; falha (já tem console) é ignorada.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

pub fn autostart_enabled() -> bool {
    run_value_exists(RUN_VALUE)
}

fn run_value_exists(name: &str) -> bool {
    let (key, value) = (wide(RUN_KEY), wide(name));
    let mut size = 0u32;
    // SAFETY: strings terminadas em NUL; só consulta a existência do valor.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
    }
}

pub fn set_autostart(enabled: bool) {
    let exe = std::env::current_exe().ok().filter(|_| enabled);
    set_autostart_exe(exe.as_deref());
}

/// `None` desliga o início com o Windows.
pub fn set_autostart_exe(exe: Option<&Path>) {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: chave aberta e fechada aqui; dados UTF-16 com NUL, tamanho em bytes.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_SET_VALUE, &mut hkey)
            != ERROR_SUCCESS
        {
            return;
        }
        if let Some(exe) = exe {
            let data = wide(&format!("\"{}\"", exe.display()));
            RegSetValueExW(
                hkey,
                value.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            );
        } else {
            RegDeleteValueW(hkey, value.as_ptr());
        }
        RegCloseKey(hkey);
    }
}

pub fn open_folder(path: &Path) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let _ = std::fs::create_dir_all(path);
    let (verb, target) = (wide("open"), wide(&path.display().to_string()));
    // SAFETY: strings terminadas em NUL.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

/// Onde criar um atalho.
pub enum ShortcutDir<'a> {
    /// Pasta especial do Windows (`"Desktop"`, `"Programs"` = Menu Iniciar), resolvida pelo
    /// próprio Windows: a área de trabalho pode estar redirecionada para o OneDrive.
    Special(&'static str),
    #[cfg_attr(not(test), allow(dead_code))]
    Path(&'a Path),
}

/// Cria `Pets.lnk` apontando para `exe`, com o ícone do próprio exe. Usa o WScript.Shell por
/// um PowerShell sem janela: o windows-sys não traz as interfaces COM do IShellLink.
pub fn create_shortcut(dir: ShortcutDir, exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // Aspas simples do PowerShell: a única coisa a escapar é a própria aspa, dobrando.
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let exe_q = quote(&exe.display().to_string());
    let folder = match dir {
        ShortcutDir::Special(name) => format!("[Environment]::GetFolderPath({})", quote(name)),
        ShortcutDir::Path(path) => quote(&path.display().to_string()),
    };
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $l=(New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ({folder}) 'Pets.lnk')); \
         $l.TargetPath={exe_q}; $l.WorkingDirectory=(Split-Path {exe_q}); \
         $l.IconLocation={exe_q}+',0'; $l.Description='Pets do Codex em qualquer tela'; $l.Save()"
    );
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("não consegui rodar o PowerShell: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn message_box(text: &str, error: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW,
    };
    let (text, title) = (wide(text), wide("Pets"));
    let icon = if error {
        MB_ICONERROR
    } else {
        MB_ICONINFORMATION
    };
    // SAFETY: strings terminadas em NUL; modal sem dono.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | icon,
        )
    };
}

/// Pergunta Sim/Não; devolve true no Sim.
pub fn confirm(text: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
    };
    let (text, title) = (wide(text), wide("Pets"));
    // SAFETY: strings terminadas em NUL; modal sem dono.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

/// Vigia `dir` numa thread bloqueada no kernel (`ReadDirectoryChangesW`, sem polling) e chama
/// `on_change` a cada lote com (ação `FILE_ACTION_*`, caminho relativo). `None` = o buffer
/// estourou e o kernel descartou a lista, então não dá para saber o que mudou.
pub fn watch_dir(
    dir: PathBuf,
    subtree: bool,
    filter: u32,
    mut on_change: impl FnMut(Option<Vec<(u32, String)>>) + Send + 'static,
) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_NOTIFY_INFORMATION,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
    };

    std::thread::spawn(move || {
        let path = wide(&dir.to_string_lossy());
        loop {
            // SAFETY: caminho terminado em NUL; BACKUP_SEMANTICS é o que permite abrir uma pasta.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                // A pasta ainda não existe (ou sumiu): tenta de novo mais tarde.
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            // u32 para o alinhamento de FILE_NOTIFY_INFORMATION.
            let mut buf = vec![0u32; 16 * 1024];
            loop {
                let mut len = 0u32;
                // SAFETY: buffer nosso com o tamanho informado; chamada síncrona (sem OVERLAPPED).
                let ok = unsafe {
                    ReadDirectoryChangesW(
                        handle,
                        buf.as_mut_ptr().cast(),
                        (buf.len() * 4) as u32,
                        subtree as i32,
                        filter,
                        &mut len,
                        std::ptr::null_mut(),
                        None,
                    )
                };
                if ok == 0 {
                    break;
                }
                // len 0: o buffer estourou e o kernel descartou a lista.
                if len == 0 {
                    on_change(None);
                    continue;
                }
                let mut entries = Vec::new();
                let base = buf.as_ptr().cast::<u8>();
                let mut offset = 0usize;
                while (offset as u32) < len {
                    // SAFETY: o kernel preencheu `len` bytes com entradas encadeadas por
                    // NextEntryOffset. O nome passa do `[u16; 1]` da struct, então ele é lido por
                    // ponteiro cru, não por referência à struct.
                    let entry = unsafe { base.add(offset).cast::<FILE_NOTIFY_INFORMATION>() };
                    let (next, action, name) = unsafe {
                        let name = std::slice::from_raw_parts(
                            std::ptr::addr_of!((*entry).FileName).cast::<u16>(),
                            (*entry).FileNameLength as usize / 2,
                        );
                        ((*entry).NextEntryOffset, (*entry).Action, name)
                    };
                    entries.push((action, String::from_utf16_lossy(name)));
                    if next == 0 {
                        break;
                    }
                    offset += next as usize;
                }
                on_change(Some(entries));
            }
            // SAFETY: handle aberto acima.
            unsafe { CloseHandle(handle) };
            // Why: só chega aqui se a vigia falhou (pasta apagada, ou numa unidade de rede/WSL
            // que não avisa mudanças); reabrir na hora giraria a CPU em 100%.
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_run_value_is_not_enabled() {
        assert!(!super::run_value_exists("PetsValorQueNaoExiste"));
    }

    #[test]
    fn creates_a_shortcut_even_with_an_apostrophe_in_the_path() {
        let dir = std::env::temp_dir().join(format!("pets-lnk-d'oeste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        super::create_shortcut(super::ShortcutDir::Path(&dir), &exe).unwrap();
        let lnk = dir.join("Pets.lnk");
        assert!(lnk.is_file());
        // Lê de volta o destino do atalho.
        let script = format!(
            "(New-Object -ComObject WScript.Shell).CreateShortcut('{}').TargetPath",
            lnk.display().to_string().replace('\'', "''")
        );
        let out = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap();
        let target = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            target.to_lowercase(),
            exe.display().to_string().to_lowercase()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
