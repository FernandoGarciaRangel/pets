//! Ícone na bandeja do Windows (`Shell_NotifyIconW`), com o ícone feito do primeiro quadro do pet.

use crate::sprite::ICON_SIZE;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO, IDI_APPLICATION, LoadIconW,
};

pub struct Tray {
    hwnd: HWND,
    message: u32,
    icon: HICON,
    owns_icon: bool,
    tip: String,
}

impl Tray {
    pub fn new(hwnd: HWND, message: u32) -> Self {
        // SAFETY: ícone compartilhado do sistema; nunca é destruído.
        let icon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
        Tray {
            hwnd,
            message,
            icon,
            owns_icon: false,
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

    pub fn update(&mut self, icon_bgra: Option<&[u8]>, tip: &str) {
        if let Some(bgra) = icon_bgra {
            // SAFETY: bgra tem ICON_SIZE² pixels (garantido por Sprite::build).
            let icon = unsafe { icon_from_bgra(bgra) };
            if !icon.is_null() {
                self.release_icon();
                self.icon = icon;
                self.owns_icon = true;
            }
        }
        self.tip = tip.to_string();
        let nid = self.data();
        // SAFETY: nid válido durante a chamada.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }

    fn release_icon(&mut self) {
        if self.owns_icon {
            // SAFETY: ícone criado por nós em icon_from_bgra.
            unsafe { DestroyIcon(self.icon) };
            self.owns_icon = false;
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        self.release_icon();
    }
}

unsafe fn icon_from_bgra(bgra: &[u8]) -> HICON {
    let size = ICON_SIZE as i32;
    unsafe {
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size;
        bmi.bmiHeader.biHeight = -size;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        let mut bits = null_mut();
        let color = CreateDIBSection(null_mut(), &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return null_mut();
        }
        let len = bgra.len().min((size * size * 4) as usize);
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), len);
        // Com bitmap de cor 32 bpp o alpha manda; a máscara só precisa existir.
        let mask_bits = vec![0u8; (size * size / 8) as usize];
        let mask = CreateBitmap(size, size, 1, 1, mask_bits.as_ptr().cast());
        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}
