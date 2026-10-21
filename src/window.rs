//! A janela do pet: Win32 layered, sempre no topo, livre por todos os monitores.
//! Redesenha só na troca de quadro (`SetTimer` com a duração do quadro), então fica parada em repouso.

use crate::agents::{self, HookEvent, Sessions};
use crate::animation::{self, AgentState, Drag};
use crate::config::{Config, SIZE_PRESETS};
use crate::install;
use crate::pets::{self, PetInfo};
use crate::sprite::Sprite;
use crate::system::{self, wide};
use crate::tray::Tray;
use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
    MONITORINFO, MonitorFromPoint, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "PetsDesktopOverlay";
const WM_APP_HOOK: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const WM_APP_QUIT: u32 = WM_APP + 4;
// Fica em Win32_UI_Controls no windows-sys; não vale a feature inteira por uma constante.
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_FRAME: usize = 1;
const TIMER_STATE: usize = 2;
/// Deslocamento mínimo (px) para um clique virar arrasto.
const DRAG_SLOP: i32 = 3;

const ID_TOGGLE: u32 = 10;
const ID_AUTOSTART: u32 = 11;
const ID_RELOAD: u32 = 12;
const ID_INSTALL: u32 = 13;
const ID_OPEN_DIR: u32 = 14;
const ID_QUIT: u32 = 99;
const ID_PET_BASE: u32 = 1000;
const ID_SIZE_BASE: u32 = 2000;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}
static HWND_FOR_CTRL_C: AtomicIsize = AtomicIsize::new(0);

/// Bitmap 32 bpp selecionado num DC de memória: a origem do `UpdateLayeredWindow`.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Dib {
    fn new(width: u32, height: u32) -> Option<Dib> {
        let (width, height) = (width as i32, height as i32);
        // SAFETY: recursos GDI criados aqui e liberados no Drop.
        unsafe {
            let dc = CreateCompatibleDC(null_mut());
            let mut bmi: BITMAPINFO = std::mem::zeroed();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = width;
            bmi.bmiHeader.biHeight = -height;
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut bits = null_mut();
            let bitmap = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            Some(Dib {
                dc,
                bitmap,
                old,
                bits: bits.cast(),
                width,
                height,
            })
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: devolve o objeto original ao DC antes de apagar o bitmap.
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

struct DragState {
    cursor0: POINT,
    pos0: POINT,
    baseline_x: i32,
    dir: Drag,
    moved: bool,
}

enum Outcome {
    Handled(LRESULT),
    Default,
    Menu,
}

enum After {
    Nothing,
    Message(String, bool),
    Quit,
}

struct App {
    hwnd: HWND,
    cfg: Config,
    pets: Vec<PetInfo>,
    sprite: Option<Sprite>,
    error: Option<String>,
    dib: Option<Dib>,
    tray: Tray,
    taskbar_created: u32,
    sessions: Sessions,
    state: AgentState,
    anim: String,
    frame: usize,
    pos: POINT,
    hovering: bool,
    drag: Option<DragState>,
}

pub fn run() {
    // SAFETY: sequência padrão de criação de janela Win32 na thread principal.
    unsafe {
        let class = wide(CLASS);
        let mutex_name = wide("Local\\PetsDesktopSingleton");
        let _mutex = CreateMutexW(null(), 0, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // Já tem um pet rodando: só pede para ele aparecer.
            let other = FindWindowW(class.as_ptr(), null());
            if !other.is_null() {
                PostMessageW(other, WM_APP_SHOW, 0, 0);
            }
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let hinstance = GetModuleHandleW(null());
        let mut wc: WNDCLASSEXW = std::mem::zeroed();
        wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.hCursor = LoadCursorW(null_mut(), IDC_HAND);
        wc.lpszClassName = class.as_ptr();
        RegisterClassExW(&wc);

        let title = wide("Pets");
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            system::message_box("Não consegui criar a janela do pet.", true);
            return;
        }

        let app = App::new(hwnd);
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(App::start);
        agents::spawn_pipe_server(hwnd as isize, WM_APP_HOOK);
        install_ctrl_c(hwnd);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // Drop do App tira o ícone da bandeja e libera o GDI.
        APP.with(|cell| cell.borrow_mut().take());
    }
}

/// Pede ao pet que estiver rodando para fechar (salvando o config) e espera até 5 s.
/// Devolve se havia um rodando.
pub fn quit_running_instance() -> bool {
    let class = wide(CLASS);
    // SAFETY: FindWindowW/PostMessageW com strings terminadas em NUL.
    unsafe {
        let hwnd = FindWindowW(class.as_ptr(), null());
        if hwnd.is_null() {
            return false;
        }
        PostMessageW(hwnd, WM_APP_QUIT, 0, 0);
        for _ in 0..50 {
            if FindWindowW(class.as_ptr(), null()).is_null() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    true
}

/// No debug (console), Ctrl+C fecha limpo em vez de deixar um ícone fantasma na bandeja.
fn install_ctrl_c(hwnd: HWND) {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_ctrl: u32) -> i32 {
        let hwnd = HWND_FOR_CTRL_C.load(Ordering::SeqCst);
        // SAFETY: PostMessageW é seguro a partir de qualquer thread.
        unsafe { PostMessageW(hwnd as HWND, WM_APP_QUIT, 0, 0) };
        1
    }
    HWND_FOR_CTRL_C.store(hwnd as isize, Ordering::SeqCst);
    // SAFETY: handler com a assinatura de PHANDLER_ROUTINE.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

/// Why: TrackPopupMenu, MessageBox e várias chamadas Win32 reentram o wndproc. Com `try_borrow_mut`
/// a reentrada cai no DefWindowProc em vez de dar panic.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .and_then(|mut slot| slot.as_mut().map(f))
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let outcome = with_app(|app| app.handle(msg, wp, lp));
    // SAFETY: chamadas Win32 com o hwnd recebido do sistema.
    unsafe {
        match outcome {
            Some(Outcome::Handled(result)) => result,
            Some(Outcome::Menu) => {
                show_menu(hwnd);
                0
            }
            None if msg == WM_APP_HOOK => {
                // Não deu para entregar ao App (reentrada): só libera o evento.
                drop(Box::from_raw(lp as *mut HookEvent));
                0
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Some(menu) = with_app(|app| app.build_menu()) else {
        return;
    };
    // SAFETY: menu criado em build_menu e destruído aqui.
    unsafe {
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Why: sem foreground, o menu da bandeja não fecha ao clicar fora (comportamento conhecido do Shell).
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        if cmd <= 0 {
            return;
        }
        match with_app(|app| app.on_command(cmd as u32)) {
            Some(After::Message(text, error)) => system::message_box(&text, error),
            Some(After::Quit) => PostQuitMessage(0),
            _ => {}
        }
    }
}

fn cursor() -> POINT {
    let mut pt = POINT { x: 0, y: 0 };
    // SAFETY: ponteiro válido.
    unsafe { GetCursorPos(&mut pt) };
    pt
}

fn monitor_work_area(
    pt: POINT,
    fallback_primary: bool,
) -> Option<windows_sys::Win32::Foundation::RECT> {
    let flag = if fallback_primary {
        MONITOR_DEFAULTTOPRIMARY
    } else {
        MONITOR_DEFAULTTONULL
    };
    // SAFETY: MONITORINFO com cbSize preenchido.
    unsafe {
        let monitor = MonitorFromPoint(pt, flag);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcWork)
    }
}

fn append(menu: HMENU, flags: u32, id: u32, text: &str) {
    let text = wide(text);
    // SAFETY: texto terminado em NUL, copiado pelo sistema.
    unsafe { AppendMenuW(menu, flags, id as usize, text.as_ptr()) };
}

fn append_submenu(menu: HMENU, submenu: HMENU, text: &str) {
    let text = wide(text);
    // SAFETY: o submenu passa a pertencer ao menu pai e é destruído junto.
    unsafe { AppendMenuW(menu, MF_POPUP, submenu as usize, text.as_ptr()) };
}

fn checked(on: bool) -> u32 {
    if on { MF_CHECKED } else { MF_UNCHECKED }
}

impl App {
    fn new(hwnd: HWND) -> App {
        let cfg = Config::load();
        let pets = pets::discover(&cfg.pets_dir());
        let taskbar = wide("TaskbarCreated");
        App {
            hwnd,
            pets,
            sprite: None,
            error: None,
            dib: None,
            tray: Tray::new(hwnd, WM_APP_TRAY),
            // SAFETY: string terminada em NUL.
            taskbar_created: unsafe { RegisterWindowMessageW(taskbar.as_ptr()) },
            sessions: Sessions::default(),
            state: AgentState::Idle,
            anim: String::new(),
            frame: 0,
            pos: POINT {
                x: cfg.x.unwrap_or(0),
                y: cfg.y.unwrap_or(0),
            },
            hovering: false,
            drag: None,
            cfg,
        }
    }

    fn start(&mut self) {
        self.tray.add();
        self.load_pet();
        let saved_ok = self.cfg.x.is_some() && self.cfg.y.is_some() && self.center_on_a_monitor();
        if !saved_ok {
            self.pos = self.default_pos();
        }
        self.move_to(self.pos);
        self.apply_visibility();
    }

    fn size(&self) -> Option<(i32, i32)> {
        self.sprite
            .as_ref()
            .map(|s| (s.width as i32, s.height as i32))
    }

    fn center_on_a_monitor(&self) -> bool {
        let (w, h) = self.size().unwrap_or((0, 0));
        monitor_work_area(
            POINT {
                x: self.pos.x + w / 2,
                y: self.pos.y + h / 2,
            },
            false,
        )
        .is_some()
    }

    /// Canto inferior direito da área de trabalho do monitor principal.
    fn default_pos(&self) -> POINT {
        let (w, h) = self.size().unwrap_or((0, 0));
        match monitor_work_area(POINT { x: 0, y: 0 }, true) {
            Some(r) => POINT {
                x: r.right - w - 24,
                y: r.bottom - h,
            },
            None => POINT { x: 100, y: 100 },
        }
    }

    fn current_label(&self) -> String {
        let id = self.cfg.pet.as_deref();
        self.pets
            .iter()
            .find(|p| Some(p.id.as_str()) == id)
            .map(|p| p.label.clone())
            .unwrap_or_else(|| "Pets".into())
    }

    fn load_pet(&mut self) {
        let old_size = self.size();
        let chosen = self
            .cfg
            .pet
            .as_ref()
            .and_then(|id| self.pets.iter().find(|p| &p.id == id))
            .or(self.pets.first())
            .cloned();
        self.sprite = None;
        self.dib = None;
        self.error = None;
        self.anim.clear();
        self.frame = 0;
        match chosen {
            None => self.error = Some(format!("Nenhum pet em {}", self.cfg.pets_dir().display())),
            Some(pet) => match Sprite::build(&pet.dir, self.cfg.width) {
                Ok(sprite) => {
                    self.cfg.pet = Some(pet.id.clone());
                    self.dib = Dib::new(sprite.width, sprite.height);
                    self.sprite = Some(sprite);
                }
                Err(e) => self.error = Some(format!("{}: {e}", pet.label)),
            },
        }
        // Mantém os pés do pet no mesmo lugar ao trocar de tamanho ou de pet.
        if let (Some((ow, oh)), Some((nw, nh))) = (old_size, self.size()) {
            self.pos.x += (ow - nw) / 2;
            self.pos.y += oh - nh;
            self.move_to(self.pos);
        }
        self.refresh_anim();
        let icon = self.sprite.as_ref().map(|s| s.icon.clone());
        let tip = self.tip();
        self.tray.update(icon.as_deref(), &tip);
        self.apply_visibility();
    }

    fn tip(&self) -> String {
        match &self.error {
            Some(e) => format!("Pets — {e}"),
            None => format!("{} — {}", self.current_label(), self.state.label()),
        }
    }

    fn apply_visibility(&mut self) {
        let show = self.cfg.visible && self.sprite.is_some();
        // SAFETY: hwnd é a nossa janela.
        unsafe {
            if show {
                self.render();
                ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            } else {
                ShowWindow(self.hwnd, SW_HIDE);
            }
        }
        self.schedule_frame();
    }

    fn set_visible(&mut self, visible: bool) {
        self.cfg.visible = visible;
        self.cfg.save();
        self.apply_visibility();
    }

    fn move_to(&mut self, pos: POINT) {
        self.pos = pos;
        // SAFETY: hwnd é a nossa janela.
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                pos.x,
                pos.y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    fn frames_len(&self) -> usize {
        self.sprite
            .as_ref()
            .map_or(0, |s| s.frames(&self.anim).len())
    }

    fn render(&mut self) {
        let (Some(sprite), Some(dib)) = (&self.sprite, &self.dib) else {
            return;
        };
        let Some(frame) = sprite.frames(&self.anim).get(self.frame) else {
            return;
        };
        let len = frame.bgra.len().min((dib.width * dib.height * 4) as usize);
        // SAFETY: dib.bits tem width*height*4 bytes; o quadro tem o mesmo tamanho.
        unsafe {
            std::ptr::copy_nonoverlapping(frame.bgra.as_ptr(), dib.bits, len);
            GdiFlush();
            let size = SIZE {
                cx: dib.width,
                cy: dib.height,
            };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            UpdateLayeredWindow(
                self.hwnd,
                null_mut(),
                &self.pos,
                &size,
                dib.dc,
                &src,
                0,
                &blend,
                ULW_ALPHA,
            );
        }
    }

    fn schedule_frame(&self) {
        let duration = self
            .sprite
            .as_ref()
            .map(|s| s.frames(&self.anim))
            .filter(|frames| frames.len() > 1 && self.cfg.visible)
            .and_then(|frames| frames.get(self.frame).map(|f| f.duration_ms));
        // SAFETY: timers da nossa janela; SetTimer com o mesmo id substitui o anterior.
        unsafe {
            match duration {
                Some(ms) => SetTimer(self.hwnd, TIMER_FRAME, ms.max(10), None),
                None => KillTimer(self.hwnd, TIMER_FRAME) as usize,
            };
        }
    }

    fn next_frame(&mut self) {
        let len = self.frames_len();
        if len == 0 {
            return;
        }
        self.frame = (self.frame + 1) % len;
        self.render();
        self.schedule_frame();
    }

    fn refresh_anim(&mut self) {
        let Some(sprite) = &self.sprite else { return };
        let (dragging, dir) = self
            .drag
            .as_ref()
            .map_or((false, Drag::None), |d| (true, d.dir));
        let wanted = animation::select(self.state, dragging, dir, self.hovering);
        let name = if sprite.has(wanted) {
            wanted.to_string()
        } else {
            sprite.default.clone()
        };
        if name != self.anim {
            self.anim = name;
            self.frame = 0;
            self.render();
            self.schedule_frame();
        }
    }

    fn refresh_state(&mut self) {
        let now = Instant::now();
        let state = self.sessions.aggregate(now);
        if state != self.state {
            self.state = state;
            self.refresh_anim();
            let tip = self.tip();
            self.tray.update(None, &tip);
        }
        // SAFETY: timer da nossa janela.
        unsafe {
            match self.sessions.next_deadline(now) {
                Some(wait) => SetTimer(
                    self.hwnd,
                    TIMER_STATE,
                    (wait.as_millis() as u32).saturating_add(50),
                    None,
                ),
                None => KillTimer(self.hwnd, TIMER_STATE) as usize,
            };
        }
    }

    fn on_mouse_move(&mut self) {
        if !self.hovering {
            self.hovering = true;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: struct preenchida para a nossa janela.
            unsafe { TrackMouseEvent(&mut tme) };
        }
        let mut new_pos = None;
        if let Some(d) = &mut self.drag {
            let c = cursor();
            let (dx, dy) = (c.x - d.cursor0.x, c.y - d.cursor0.y);
            if !d.moved && (dx.abs() > DRAG_SLOP || dy.abs() > DRAG_SLOP) {
                d.moved = true;
            }
            if d.moved {
                let (dir, accepted) = animation::next_drag(d.dir, c.x - d.baseline_x);
                d.dir = dir;
                if accepted {
                    d.baseline_x = c.x;
                }
                new_pos = Some(POINT {
                    x: d.pos0.x + dx,
                    y: d.pos0.y + dy,
                });
            }
        }
        if let Some(pos) = new_pos {
            self.move_to(pos);
        }
        self.refresh_anim();
    }

    fn end_drag(&mut self) {
        if let Some(d) = self.drag.take() {
            if d.moved {
                self.cfg.x = Some(self.pos.x);
                self.cfg.y = Some(self.pos.y);
                self.cfg.save();
            }
            self.refresh_anim();
        }
    }

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Outcome {
        match msg {
            WM_TIMER => match wp {
                TIMER_FRAME => self.next_frame(),
                TIMER_STATE => self.refresh_state(),
                _ => return Outcome::Default,
            },
            WM_APP_HOOK => {
                // SAFETY: o servidor do pipe criou este Box e passou a posse pela mensagem.
                let ev = unsafe { Box::from_raw(lp as *mut HookEvent) };
                self.sessions.apply(&ev, Instant::now());
                self.refresh_state();
            }
            WM_APP_TRAY => match (lp as u32) & 0xffff {
                WM_LBUTTONUP => self.set_visible(!self.cfg.visible),
                WM_RBUTTONUP | WM_CONTEXTMENU => return Outcome::Menu,
                _ => {}
            },
            WM_APP_SHOW => self.set_visible(true),
            WM_APP_QUIT => {
                self.cfg.save();
                self.tray.remove();
                // SAFETY: encerra o loop de mensagens desta thread.
                unsafe { PostQuitMessage(0) };
            }
            WM_MOUSEACTIVATE => return Outcome::Handled(MA_NOACTIVATE as LRESULT),
            WM_LBUTTONDOWN => {
                let c = cursor();
                self.drag = Some(DragState {
                    cursor0: c,
                    pos0: self.pos,
                    baseline_x: c.x,
                    dir: Drag::None,
                    moved: false,
                });
                // SAFETY: captura o mouse para seguir o arrasto mesmo fora da janela.
                unsafe { SetCapture(self.hwnd) };
                self.refresh_anim();
            }
            WM_MOUSEMOVE => self.on_mouse_move(),
            WM_LBUTTONUP => {
                self.end_drag();
                // SAFETY: solta a captura; o WM_CAPTURECHANGED reentrante cai no DefWindowProc.
                unsafe { ReleaseCapture() };
            }
            WM_CAPTURECHANGED => self.end_drag(),
            WM_MOUSELEAVE => {
                self.hovering = false;
                self.refresh_anim();
            }
            WM_RBUTTONUP => return Outcome::Menu,
            WM_DISPLAYCHANGE => {
                if !self.center_on_a_monitor() {
                    let pos = self.default_pos();
                    self.move_to(pos);
                }
            }
            WM_ENDSESSION => self.cfg.save(),
            WM_DESTROY => {
                // SAFETY: encerra o loop de mensagens desta thread.
                unsafe { PostQuitMessage(0) };
            }
            m if m == self.taskbar_created && m != 0 => {
                self.tray.add();
                let tip = self.tip();
                self.tray.update(None, &tip);
            }
            _ => return Outcome::Default,
        }
        Outcome::Handled(0)
    }

    fn build_menu(&self) -> HMENU {
        // SAFETY: menus criados aqui; o pai é destruído por show_menu e leva os submenus junto.
        unsafe {
            let menu = CreatePopupMenu();
            let mut header = self.tip();
            let sessions = self.sessions.active_count();
            if sessions > 0 {
                header.push_str(&format!(
                    " ({sessions} sess{})",
                    if sessions == 1 { "ão" } else { "ões" }
                ));
            }
            append(menu, MF_STRING | MF_GRAYED, 0, &header);
            append(menu, MF_SEPARATOR, 0, "");

            let pets_menu = CreatePopupMenu();
            if self.pets.is_empty() {
                append(pets_menu, MF_STRING | MF_GRAYED, 0, "Nenhum pet encontrado");
            }
            for (i, pet) in self.pets.iter().enumerate() {
                let on = self.cfg.pet.as_deref() == Some(pet.id.as_str());
                append(
                    pets_menu,
                    MF_STRING | checked(on),
                    ID_PET_BASE + i as u32,
                    &pet.label,
                );
            }
            append_submenu(menu, pets_menu, "Pet");

            let size_menu = CreatePopupMenu();
            for (i, px) in SIZE_PRESETS.iter().enumerate() {
                append(
                    size_menu,
                    MF_STRING | checked(*px == self.cfg.width),
                    ID_SIZE_BASE + i as u32,
                    &format!("{px} px"),
                );
            }
            append_submenu(menu, size_menu, "Tamanho");

            append(
                menu,
                MF_STRING,
                ID_TOGGLE,
                if self.cfg.visible {
                    "Esconder pet"
                } else {
                    "Mostrar pet"
                },
            );
            append(
                menu,
                MF_STRING | checked(system::autostart_enabled()),
                ID_AUTOSTART,
                "Iniciar com o Windows",
            );
            append(menu, MF_SEPARATOR, 0, "");
            append(menu, MF_STRING, ID_OPEN_DIR, "Abrir pasta dos pets");
            append(menu, MF_STRING, ID_RELOAD, "Recarregar pets");
            append(
                menu,
                MF_STRING,
                ID_INSTALL,
                "Instalar hooks (Claude Code + Codex)",
            );
            append(menu, MF_SEPARATOR, 0, "");
            append(menu, MF_STRING, ID_QUIT, "Sair");
            menu
        }
    }

    fn on_command(&mut self, id: u32) -> After {
        match id {
            ID_TOGGLE => self.set_visible(!self.cfg.visible),
            ID_AUTOSTART => system::set_autostart(!system::autostart_enabled()),
            ID_OPEN_DIR => system::open_folder(&self.cfg.pets_dir()),
            ID_RELOAD => {
                self.pets = pets::discover(&self.cfg.pets_dir());
                self.load_pet();
            }
            ID_INSTALL => {
                return match install::install() {
                    Ok(text) => After::Message(text, false),
                    Err(text) => After::Message(text, true),
                };
            }
            ID_QUIT => {
                self.cfg.save();
                self.tray.remove();
                return After::Quit;
            }
            id if (ID_PET_BASE..ID_PET_BASE + self.pets.len() as u32).contains(&id) => {
                self.cfg.pet = Some(self.pets[(id - ID_PET_BASE) as usize].id.clone());
                self.load_pet();
                self.cfg.save();
            }
            id if (ID_SIZE_BASE..ID_SIZE_BASE + SIZE_PRESETS.len() as u32).contains(&id) => {
                self.cfg.width = SIZE_PRESETS[(id - ID_SIZE_BASE) as usize];
                self.load_pet();
                self.cfg.save();
            }
            _ => {}
        }
        if let Some(error) = self.error.clone() {
            return After::Message(error, true);
        }
        After::Nothing
    }
}
