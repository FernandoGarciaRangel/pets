//! Ícone na bandeja do Windows (`Shell_NotifyIconW`), sempre com a logo do app.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{HICON, IDI_APPLICATION, LoadIconW};

pub struct Tray {
    hwnd: HWND,
    message: u32,
    icon: HICON,
    tip: String,
}

impl Tray {
    pub fn new(hwnd: HWND, message: u32) -> Self {
        Tray {
            hwnd,
            message,
            icon: app_icon(),
            tip: "Pets".into(),
        }
    }

    fn data(&self) -> NOTIFYICONDATAW {
        // SAFETY: struct POD; zero é um estado válido.
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = self.message;
        nid.hIcon = self.icon;
        for (dst, src) in nid.szTip.iter_mut().zip(self.tip.encode_utf16().take(127)) {
            *dst = src;
        }
        nid
    }

    /// Também serve para recriar o ícone depois que o Explorer reinicia (`TaskbarCreated`).
    pub fn add(&self) {
        let nid = self.data();
        // SAFETY: nid válido durante a chamada.
        unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    }

    pub fn remove(&self) {
        let nid = self.data();
        // SAFETY: nid válido durante a chamada.
        unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
    }

    pub fn set_tip(&mut self, tip: &str) {
        self.tip = tip.to_string();
        let nid = self.data();
        // SAFETY: nid válido durante a chamada.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    /// Notificação (balão/toast). Clicar nela chega como `NIN_BALLOONUSERCLICK` na mensagem da bandeja.
    pub fn notify(&self, title: &str, text: &str) {
        let mut nid = self.data();
        nid.uFlags |= NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        for (dst, src) in nid
            .szInfoTitle
            .iter_mut()
            .zip(title.encode_utf16().take(63))
        {
            *dst = src;
        }
        for (dst, src) in nid.szInfo.iter_mut().zip(text.encode_utf16().take(255)) {
            *dst = src;
        }
        // SAFETY: nid válido durante a chamada.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }
}

/// A logo embutida no exe (recurso 1, de `pets.rc`); o ícone genérico do Windows se faltar.
/// Ícones de recurso são compartilhados: nunca são destruídos.
pub fn app_icon() -> HICON {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    // SAFETY: MAKEINTRESOURCE(1) no módulo do próprio exe.
    unsafe {
        let icon = LoadIconW(
            GetModuleHandleW(std::ptr::null()),
            std::ptr::without_provenance(1),
        );
        if icon.is_null() {
            LoadIconW(null_mut(), IDI_APPLICATION)
        } else {
            icon
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
    }
}
