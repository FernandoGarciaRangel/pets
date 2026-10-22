//! Painel flutuante com o slider de tamanho, aberto pelo menu "Tamanho…". O menu do Windows não
//! aceita controles dentro dele, então o slider vive numa janelinha própria perto do pet, que
//! fecha sozinha ao perder o foco (como os flyouts do Windows).

use crate::config::{DEFAULT_WIDTH, MAX_WIDTH, MIN_WIDTH};
use crate::system::wide;
use std::cell::Cell;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontIndirectW, DeleteObject, GetMonitorInfoW, GetSysColorBrush, HFONT,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, SetBkMode, TRANSPARENT,
};
use windows_sys::Win32::UI::Controls::{
    ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TBM_SETLINESIZE, TBM_SETPAGESIZE,
    TBM_SETPOS, TBM_SETRANGEMAX, TBM_SETRANGEMIN, TBS_HORZ, TBS_NOTICKS, TRACKBAR_CLASSW,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "PetsSizePanel";
const ID_TRACKBAR: usize = 1;
const ID_DEFAULT: usize = 2;
// Faltam no windows-sys (ou vivem em features que não valem a pena por uma constante).
const TBM_GETPOS: u32 = WM_USER;
const SS_CENTERIMAGE: u32 = 0x0200;

thread_local! {
    static PANEL: Cell<HWND> = const { Cell::new(null_mut()) };
    static TRACKBAR: Cell<HWND> = const { Cell::new(null_mut()) };
    static LABEL: Cell<HWND> = const { Cell::new(null_mut()) };
    static FONT: Cell<HFONT> = const { Cell::new(null_mut()) };
}

fn label_text(width: u32) -> String {
    format!("Tamanho do pet: {width} px")
}

fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: texto terminado em NUL, copiado pelo controle.
    unsafe { SetWindowTextW(hwnd, text.as_ptr()) };
}

/// Abre o painel perto do pet (acima dele, ou abaixo se não couber). Se já estiver aberto, só traz para a frente.
pub fn open(pet: HWND, width: u32) {
    let existing = PANEL.get();
    if !existing.is_null() {
        // SAFETY: janela nossa.
        unsafe { SetForegroundWindow(existing) };
        return;
    }
    // SAFETY: criação de janela e controles na thread da UI; recursos liberados em WM_DESTROY.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        InitCommonControlsEx(&icc);

        let class = wide(CLASS);
        let hinstance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(null());
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(panel_proc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
        wc.hbrBackground = GetSysColorBrush(COLOR_WINDOW);
        wc.lpszClassName = class.as_ptr();
        // Falha na segunda abertura (classe já registrada), o que é esperado.
        RegisterClassExW(&wc);

        let dpi = GetDpiForWindow(pet).max(96);
        let px = |v: i32| v * dpi as i32 / 96;
        let (w, h) = (px(300), px(84));

        let mut pet_rect: RECT = std::mem::zeroed();
        GetWindowRect(pet, &mut pet_rect);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(MonitorFromWindow(pet, MONITOR_DEFAULTTONEAREST), &mut info);
        let work = info.rcWork;
        let x = ((pet_rect.left + pet_rect.right) / 2 - w / 2).clamp(work.left, work.right - w);
        let above = pet_rect.top - h - px(8);
        let y = if above >= work.top {
            above
        } else {
            (pet_rect.bottom + px(8)).min(work.bottom - h)
        };

        let title = wide("Tamanho do pet");
        let panel = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_BORDER,
            x,
            y,
            w,
            h,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if panel.is_null() {
            return;
        }

        let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
        metrics.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let font = if SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS,
            metrics.cbSize,
            (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
            0,
            dpi,
        ) != 0
        {
            CreateFontIndirectW(&metrics.lfMessageFont)
        } else {
            null_mut()
        };

        let static_class = wide("STATIC");
        let label_w = wide(&label_text(width));
        let label = CreateWindowExW(
            0,
            static_class.as_ptr(),
            label_w.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_CENTERIMAGE,
            px(14),
            px(10),
            px(190),
            px(24),
            panel,
            null_mut(),
            hinstance,
            null(),
        );
        let button_class = wide("BUTTON");
        let button_text = wide("Padrão");
        let button = CreateWindowExW(
            0,
            button_class.as_ptr(),
            button_text.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            w - px(14) - px(76),
            px(9),
            px(76),
            px(26),
            panel,
            ID_DEFAULT as HMENU,
            hinstance,
            null(),
        );
        let trackbar = CreateWindowExW(
            0,
            TRACKBAR_CLASSW,
            null(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | TBS_HORZ | TBS_NOTICKS,
            px(8),
            px(42),
            w - px(16),
            px(32),
            panel,
            ID_TRACKBAR as HMENU,
            hinstance,
            null(),
        );
        for control in [label, button] {
            if !font.is_null() {
                SendMessageW(control, WM_SETFONT, font as WPARAM, 1);
            }
        }
        SendMessageW(trackbar, TBM_SETRANGEMIN, 0, MIN_WIDTH as LPARAM);
        SendMessageW(trackbar, TBM_SETRANGEMAX, 1, MAX_WIDTH as LPARAM);
        SendMessageW(trackbar, TBM_SETLINESIZE, 0, 4);
        SendMessageW(trackbar, TBM_SETPAGESIZE, 0, 32);
        SendMessageW(trackbar, TBM_SETPOS, 1, width as LPARAM);

        PANEL.set(panel);
        TRACKBAR.set(trackbar);
        LABEL.set(label);
        FONT.set(font);
        ShowWindow(panel, SW_SHOW);
        SetForegroundWindow(panel);
        windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus(trackbar);
    }
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: mensagens da nossa janela; os handles guardados pertencem a ela.
    unsafe {
        match msg {
            // O slider avisa o pai a cada movimento (arrastar, setas, clique na trilha).
            WM_HSCROLL => {
                let width = SendMessageW(TRACKBAR.get(), TBM_GETPOS, 0, 0) as u32;
                set_text(LABEL.get(), &label_text(width));
                crate::window::preview_width(width);
                0
            }
            WM_COMMAND if (wp & 0xffff) == ID_DEFAULT => {
                SendMessageW(TRACKBAR.get(), TBM_SETPOS, 1, DEFAULT_WIDTH as LPARAM);
                set_text(LABEL.get(), &label_text(DEFAULT_WIDTH));
                crate::window::preview_width(DEFAULT_WIDTH);
                0
            }
            WM_CTLCOLORSTATIC => {
                SetBkMode(wp as _, TRANSPARENT as _);
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
            // Clicou fora: fecha, como um flyout.
            WM_ACTIVATE if (wp & 0xffff) == WA_INACTIVE as usize => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                PANEL.set(null_mut());
                TRACKBAR.set(null_mut());
                LABEL.set(null_mut());
                let font = FONT.replace(null_mut());
                if !font.is_null() {
                    DeleteObject(font);
                }
                crate::window::commit_width();
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Esc fecha o painel. Chamado no loop de mensagens: o foco fica no slider, que não repassa a tecla.
pub fn close_on_escape(msg: &MSG) -> bool {
    const VK_ESCAPE: usize = 0x1B;
    let panel = PANEL.get();
    if panel.is_null() || msg.message != WM_KEYDOWN || msg.wParam != VK_ESCAPE {
        return false;
    }
    // SAFETY: janela nossa.
    unsafe { DestroyWindow(panel) };
    true
}
