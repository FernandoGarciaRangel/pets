//! Pequenos utilitários Win32: strings UTF-16, console, início com o Windows, abrir pasta, aviso.

use std::path::Path;
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

#[cfg(test)]
mod tests {
    #[test]
    fn missing_run_value_is_not_enabled() {
        assert!(!super::run_value_exists("PetsValorQueNaoExiste"));
    }
}
