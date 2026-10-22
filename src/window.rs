//! A janela do pet: Win32 layered, sempre no topo, livre por todos os monitores.
//! Redesenha só na troca de quadro (`SetTimer` com a duração do quadro), então fica parada em repouso.

use crate::agents::{self, HookEvent, Sessions};
use crate::animation::{self, AgentState, Drag};
use crate::builtin;
use crate::catalog::{self, PetUpdate};
use crate::config::{Config, MAX_WIDTH, MIN_WIDTH};
use crate::install;
use crate::pets::{self, PetInfo};
use crate::size_panel;
use crate::sprite::{Cells, Quality, Sprite};
use crate::system::{self, wide};
use crate::tray::Tray;
use crate::update::{self, CheckResult, Release};
use std::cell::RefCell;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::{Duration, Instant};
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
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::NIN_BALLOONUSERCLICK;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "PetsDesktopOverlay";
const WM_APP_HOOK: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const WM_APP_QUIT: u32 = WM_APP + 4;
const WM_APP_UPDATE: u32 = WM_APP + 5;
const TIMER_FRAME: usize = 1;
const TIMER_STATE: usize = 2;
const TIMER_UPDATE: usize = 3;
const TIMER_RESIZE: usize = 4;
const TIMER_RESIZE_PREVIEW: usize = 5;
/// O slider manda dezenas de eventos por segundo; aplica o mais recente no máximo a cada ~16 ms.
const RESIZE_PREVIEW_MS: u32 = 16;
/// Depois de parar de rolar a roda: refaz os quadros na qualidade final e solta os `Cells`.
const RESIZE_SETTLE_MS: u32 = 600;
/// Deslocamento mínimo (px) para um clique virar arrasto.
const DRAG_SLOP: i32 = 3;

const ID_TOGGLE: u32 = 10;
const ID_AUTOSTART: u32 = 11;
const ID_RELOAD: u32 = 12;
const ID_INSTALL: u32 = 13;
const ID_OPEN_DIR: u32 = 14;
const ID_UPDATE: u32 = 15;
const ID_CHECK_UPDATE: u32 = 16;
const ID_AUTO_UPDATE: u32 = 17;
const ID_PET_UPDATE_ALL: u32 = 19;
const ID_PET_UPDATE_BASE: u32 = 3000;
const ID_QUIT: u32 = 99;
const ID_PET_BASE: u32 = 1000;
const ID_SIZE_PANEL: u32 = 18;

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
    /// Caixa de mensagem: modal, então só depois de soltar o App.
    Message(String, bool),
    ConfirmUpdate(String),
    /// Pergunta se aplica as atualizações de pets; o texto já vem montado.
    ConfirmPetUpdates(String),
}

/// Resultado das threads de atualização, entregue via `WM_APP_UPDATE`.
enum UpdateMsg {
    Checked {
        result: CheckResult,
        manual: bool,
    },
    Downloaded(Result<PathBuf, String>),
    PetsChecked {
        updates: Vec<PetUpdate>,
        manual: bool,
    },
    /// (label, resultado) de cada pet aplicado.
    PetsApplied(Vec<(String, String, Result<(), String>)>),
}

enum After {
    Nothing,
    Message(String, bool),
    Quit,
    /// O painel do slider é uma janela nova: abre fora do empréstimo do App.
    OpenSizePanel(u32),
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
    update: Option<Release>,
    notified_version: Option<String>,
    updating: bool,
    resize_cells: Option<Cells>,
    pending_width: Option<u32>,
    preview_armed: bool,
    pet_updates: Vec<PetUpdate>,
    notified_pets: Vec<String>,
    updating_pets: bool,
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
        wc.hIcon = crate::tray::app_icon();
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
            if size_panel::close_on_escape(&msg) {
                continue;
            }
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
            Some(Outcome::Message(text, error)) => {
                system::message_box(&text, error);
                0
            }
            Some(Outcome::ConfirmPetUpdates(question)) => {
                if system::confirm(&question) {
                    with_app(|app| app.start_pet_updates(None));
                }
                0
            }
            Some(Outcome::ConfirmUpdate(version)) => {
                let question = format!(
                    "Pets v{version} está disponível (você tem v{}).\n\nAtualizar agora?",
                    update::current_version()
                );
                if system::confirm(&question) {
                    with_app(App::start_download);
                }
                0
            }
            None if msg == WM_APP_HOOK => {
                // Não deu para entregar ao App (reentrada): só libera o evento.
                drop(Box::from_raw(lp as *mut HookEvent));
                0
            }
            None if msg == WM_APP_UPDATE => {
                drop(Box::from_raw(lp as *mut UpdateMsg));
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
            Some(After::OpenSizePanel(width)) => size_panel::open(hwnd, width),
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

fn post_update(hwnd: isize, msg: UpdateMsg) {
    let ptr = Box::into_raw(Box::new(msg));
    // SAFETY: a janela retoma a posse do Box em WM_APP_UPDATE; se o post falhar, liberamos aqui.
    if unsafe { PostMessageW(hwnd as HWND, WM_APP_UPDATE, 0, ptr as isize) } == 0 {
        drop(unsafe { Box::from_raw(ptr) });
    }
}

/// Checa o GitHub numa thread (rede bloqueante não pode rodar na thread da janela).
/// Depois do app, checa os pets nos catálogos (é mais lento: uma requisição por pet).
fn spawn_check(hwnd: HWND, manual: bool, delay: Duration, pets: Vec<PetInfo>) {
    let hwnd = hwnd as isize;
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let result = update::check();
        post_update(hwnd, UpdateMsg::Checked { result, manual });
        let updates = catalog::updates(&pets);
        post_update(hwnd, UpdateMsg::PetsChecked { updates, manual });
    });
}

/// O slider do painel mudou: redimensiona ao vivo (filtro rápido).
pub fn preview_width(width: u32) {
    with_app(|app| app.queue_resize(width));
}

/// O painel fechou: refaz em qualidade final e salva.
pub fn commit_width() {
    with_app(App::finish_resize);
}

fn checked(on: bool) -> u32 {
    if on { MF_CHECKED } else { MF_UNCHECKED }
}

impl App {
    fn new(hwnd: HWND) -> App {
        let mut cfg = Config::load();
        let fresh = builtin::install_missing(&cfg.pets_dir(), &cfg.installed_builtins);
        if !fresh.is_empty() {
            cfg.installed_builtins.extend(fresh);
            cfg.save();
        }
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
            update: None,
            notified_version: None,
            updating: false,
            resize_cells: None,
            pending_width: None,
            preview_armed: false,
            pet_updates: Vec::new(),
            notified_pets: Vec::new(),
            updating_pets: false,
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

        // Sobra de uma atualização anterior.
        let _ = std::fs::remove_dir_all(update::staging_dir());
        if self.cfg.check_updates {
            spawn_check(self.hwnd, false, Duration::from_secs(10), self.pets.clone());
        }
        // SAFETY: timer da nossa janela.
        unsafe {
            SetTimer(
                self.hwnd,
                TIMER_UPDATE,
                update::CHECK_EVERY.as_millis() as u32,
                None,
            )
        };
    }

    /// Aplica numa thread as atualizações de pets: todas (`None`) ou a de índice `only`.
    fn start_pet_updates(&mut self, only: Option<usize>) {
        if self.updating_pets {
            return;
        }
        let chosen: Vec<(PetUpdate, PathBuf)> = self
            .pet_updates
            .iter()
            .enumerate()
            .filter(|(i, _)| only.is_none_or(|o| o == *i))
            .filter_map(|(_, u)| {
                let dir = self.pets.iter().find(|p| p.id == u.id)?.dir.clone();
                Some((u.clone(), dir))
            })
            .collect();
        if chosen.is_empty() {
            return;
        }
        self.updating_pets = true;
        let hwnd = self.hwnd as isize;
        std::thread::spawn(move || {
            let results = chosen
                .iter()
                .map(|(u, dir)| (u.id.clone(), u.label.clone(), catalog::apply(dir, u)))
                .collect();
            post_update(hwnd, UpdateMsg::PetsApplied(results));
        });
    }

    fn pet_updates_question(&self) -> String {
        let names: Vec<String> = self
            .pet_updates
            .iter()
            .map(|u| format!("• {} ({})", u.label, u.remote.source.label()))
            .collect();
        format!(
            "Há versão nova destes pets:\n\n{}\n\nAtualizar agora? Os arquivos atuais ficam em backup.",
            names.join("\n")
        )
    }

    fn start_download(&mut self) {
        let Some(release) = self.update.clone() else {
            return;
        };
        if self.updating {
            return;
        }
        self.updating = true;
        self.refresh_tip();
        let hwnd = self.hwnd as isize;
        std::thread::spawn(move || {
            post_update(hwnd, UpdateMsg::Downloaded(update::download(&release)));
        });
    }

    fn on_update(&mut self, msg: UpdateMsg) -> Outcome {
        match msg {
            UpdateMsg::Checked { result, manual } => match result {
                CheckResult::Available(release) => {
                    let version = release.version.clone();
                    self.update = Some(release);
                    self.refresh_tip();
                    if manual {
                        return Outcome::ConfirmUpdate(version);
                    }
                    // Avisa uma vez por versão; o item do menu continua lá.
                    if self.notified_version.as_deref() != Some(version.as_str()) {
                        self.tray.notify(
                            &format!("Pets v{version} disponível"),
                            "Clique aqui ou use o menu do pet para atualizar.",
                        );
                        self.notified_version = Some(version);
                    }
                }
                CheckResult::UpToDate if manual => {
                    return Outcome::Message(
                        format!(
                            "Você já está na versão mais recente (v{}).",
                            update::current_version()
                        ),
                        false,
                    );
                }
                CheckResult::Failed(e) if manual => {
                    return Outcome::Message(
                        format!("Não consegui procurar atualizações: {e}"),
                        true,
                    );
                }
                CheckResult::Failed(e) => eprintln!("[update] {e}"),
                CheckResult::UpToDate => {}
            },
            UpdateMsg::PetsChecked { updates, manual } => {
                self.pet_updates = updates;
                if self.pet_updates.is_empty() {
                    return Outcome::Handled(0);
                }
                if manual {
                    return Outcome::ConfirmPetUpdates(self.pet_updates_question());
                }
                // Avisa só quando aparece pet novo na lista.
                let ids: Vec<String> = self.pet_updates.iter().map(|u| u.id.clone()).collect();
                if ids.iter().any(|id| !self.notified_pets.contains(id)) {
                    let first = &self.pet_updates[0].label;
                    let text = match self.pet_updates.len() {
                        1 => format!("{first} tem versão nova. Clique aqui para atualizar."),
                        n => format!(
                            "{first} e mais {} têm versão nova. Clique aqui para atualizar.",
                            n - 1
                        ),
                    };
                    self.tray.notify("Atualização de pets", &text);
                    self.notified_pets = ids;
                }
            }
            UpdateMsg::PetsApplied(results) => {
                self.updating_pets = false;
                let mut failures = Vec::new();
                let mut reload = false;
                for (id, label, result) in results {
                    match result {
                        Ok(()) => {
                            self.pet_updates.retain(|u| u.id != id);
                            reload |= self.cfg.pet.as_deref() == Some(id.as_str());
                        }
                        Err(e) => failures.push(format!("• {label}: {e}")),
                    }
                }
                // Nome e descrição podem ter mudado.
                self.pets = pets::discover(&self.cfg.pets_dir());
                if reload {
                    self.load_pet();
                }
                if !failures.is_empty() {
                    return Outcome::Message(
                        format!("Alguns pets não atualizaram:\n\n{}", failures.join("\n")),
                        true,
                    );
                }
            }
            UpdateMsg::Downloaded(Ok(path)) => {
                // O exe baixado roda `install`: fecha este pet, troca o exe instalado e sobe de novo.
                if let Err(e) = install::spawn_detached(&path, &["install", "--from-update"]) {
                    self.updating = false;
                    self.refresh_tip();
                    return Outcome::Message(e, true);
                }
            }
            UpdateMsg::Downloaded(Err(e)) => {
                self.updating = false;
                self.refresh_tip();
                return Outcome::Message(format!("Não consegui baixar a atualização: {e}"), true);
            }
        }
        Outcome::Handled(0)
    }

    fn refresh_tip(&mut self) {
        let tip = self.tip();
        self.tray.update(None, &tip);
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
            .or_else(|| self.pets.iter().find(|p| p.id == builtin::DEFAULT_PET))
            .or(self.pets.first())
            .cloned();
        self.sprite = None;
        self.dib = None;
        self.resize_cells = None;
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

    /// Prévia do slider: só a animação atual, com filtro rápido; tudo em qualidade final depois
    /// de `RESIZE_SETTLE_MS` parado.
    fn queue_resize(&mut self, target: u32) {
        self.pending_width = Some(target);
        // Why: não rearma um timer pendente; rearmar a cada evento adiaria a prévia enquanto o
        // usuário arrasta sem parar.
        if !self.preview_armed {
            self.preview_armed = true;
            // SAFETY: timer da nossa janela.
            unsafe { SetTimer(self.hwnd, TIMER_RESIZE_PREVIEW, RESIZE_PREVIEW_MS, None) };
        }
    }

    fn apply_pending_resize(&mut self) {
        // SAFETY: timer da nossa janela.
        unsafe { KillTimer(self.hwnd, TIMER_RESIZE_PREVIEW) };
        self.preview_armed = false;
        if let Some(width) = self.pending_width.take() {
            self.resize_to(width);
        }
    }

    fn resize_to(&mut self, target: u32) {
        let target = target.clamp(MIN_WIDTH, MAX_WIDTH);
        if target == self.cfg.width || self.sprite.is_none() {
            return;
        }
        if self.resize_cells.is_none() {
            let dir = self
                .cfg
                .pet
                .as_ref()
                .and_then(|id| self.pets.iter().find(|p| &p.id == id))
                .map(|p| p.dir.clone());
            self.resize_cells = dir.and_then(|d| Cells::load(&d).ok());
        }
        let Some(cells) = &self.resize_cells else {
            return;
        };
        self.cfg.width = target;
        let sprite = cells.scale_only(target, &self.anim, Quality::Fast);
        self.install_resized(sprite);
        // SAFETY: timer da nossa janela; SetTimer com o mesmo id reinicia a contagem.
        unsafe { SetTimer(self.hwnd, TIMER_RESIZE, RESIZE_SETTLE_MS, None) };
    }

    /// Parou de arrastar: todas as animações em qualidade final. Os `Cells` ficam enquanto o
    /// painel está aberto, para o próximo movimento não decodificar a spritesheet de novo.
    fn settle_resize(&mut self) {
        // SAFETY: timer da nossa janela.
        unsafe { KillTimer(self.hwnd, TIMER_RESIZE) };
        if let Some(cells) = &self.resize_cells {
            let sprite = cells.scale(self.cfg.width, Quality::Final);
            self.install_resized(sprite);
        }
        self.cfg.save();
    }

    /// O painel fechou: aplica o último valor, acerta a qualidade e solta a memória extra.
    fn finish_resize(&mut self) {
        self.apply_pending_resize();
        self.settle_resize();
        self.resize_cells = None;
    }

    /// Troca os quadros mantendo animação, quadro atual e os pés do pet no lugar.
    fn install_resized(&mut self, sprite: Sprite) {
        let old_size = self.size();
        self.dib = Dib::new(sprite.width, sprite.height);
        self.sprite = Some(sprite);
        if let (Some((ow, oh)), Some((nw, nh))) = (old_size, self.size()) {
            self.pos.x += (ow - nw) / 2;
            self.pos.y += oh - nh;
            self.move_to(self.pos);
        }
        if self.frame >= self.frames_len() {
            self.frame = 0;
        }
        self.render();
    }

    fn tip(&self) -> String {
        match &self.error {
            Some(e) => format!("Pets — {e}"),
            None => {
                let mut tip = format!("{} — {}", self.current_label(), self.state.label());
                if self.updating {
                    tip.push_str(" · atualizando…");
                } else if let Some(release) = &self.update {
                    tip.push_str(&format!(" · v{} disponível", release.version));
                }
                tip
            }
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
                TIMER_UPDATE if self.cfg.check_updates => {
                    spawn_check(self.hwnd, false, Duration::ZERO, self.pets.clone())
                }
                TIMER_UPDATE => {}
                TIMER_RESIZE => self.settle_resize(),
                TIMER_RESIZE_PREVIEW => self.apply_pending_resize(),
                _ => return Outcome::Default,
            },
            WM_APP_HOOK => {
                // SAFETY: o servidor do pipe criou este Box e passou a posse pela mensagem.
                let ev = unsafe { Box::from_raw(lp as *mut HookEvent) };
                self.sessions.apply(&ev, Instant::now());
                self.refresh_state();
            }
            WM_APP_UPDATE => {
                // SAFETY: post_update criou este Box e passou a posse pela mensagem.
                let msg = unsafe { Box::from_raw(lp as *mut UpdateMsg) };
                return self.on_update(*msg);
            }
            WM_APP_TRAY => match (lp as u32) & 0xffff {
                WM_LBUTTONUP => self.set_visible(!self.cfg.visible),
                WM_RBUTTONUP | WM_CONTEXTMENU => return Outcome::Menu,
                NIN_BALLOONUSERCLICK => {
                    if let Some(release) = &self.update
                        && !self.updating
                    {
                        return Outcome::ConfirmUpdate(release.version.clone());
                    }
                    if !self.pet_updates.is_empty() && !self.updating_pets {
                        return Outcome::ConfirmPetUpdates(self.pet_updates_question());
                    }
                }
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
            if self.updating {
                append(menu, MF_STRING | MF_GRAYED, 0, "Baixando atualização…");
            } else if let Some(release) = &self.update {
                append(
                    menu,
                    MF_STRING,
                    ID_UPDATE,
                    &format!("Atualizar para v{}", release.version),
                );
            }
            if self.updating_pets {
                append(menu, MF_STRING | MF_GRAYED, 0, "Atualizando pets…");
            } else if !self.pet_updates.is_empty() {
                let pets_menu = CreatePopupMenu();
                for (i, u) in self.pet_updates.iter().enumerate() {
                    append(
                        pets_menu,
                        MF_STRING,
                        ID_PET_UPDATE_BASE + i as u32,
                        &format!("{} ({})", u.label, u.remote.source.label()),
                    );
                }
                append(pets_menu, MF_SEPARATOR, 0, "");
                append(pets_menu, MF_STRING, ID_PET_UPDATE_ALL, "Atualizar todos");
                append_submenu(
                    menu,
                    pets_menu,
                    &format!("Atualizar pets ({})", self.pet_updates.len()),
                );
            }
            append(menu, MF_SEPARATOR, 0, "");

            let pets_menu = CreatePopupMenu();
            if self.pets.is_empty() {
                append(pets_menu, MF_STRING | MF_GRAYED, 0, "Nenhum pet encontrado");
            }
            // Os que vêm com o app primeiro, na ordem de `BUILTIN`; depois os do usuário.
            let builtin_rank = |id: &str| builtin::BUILTIN.iter().position(|b| b.id == id);
            let mut ours: Vec<(usize, &PetInfo)> = self
                .pets
                .iter()
                .enumerate()
                .filter(|(_, p)| builtin_rank(&p.id).is_some())
                .collect();
            ours.sort_by_key(|(_, p)| builtin_rank(&p.id));
            let theirs: Vec<(usize, &PetInfo)> = self
                .pets
                .iter()
                .enumerate()
                .filter(|(_, p)| builtin_rank(&p.id).is_none())
                .collect();
            let add_pet = |(i, pet): &(usize, &PetInfo)| {
                let on = self.cfg.pet.as_deref() == Some(pet.id.as_str());
                append(
                    pets_menu,
                    MF_STRING | checked(on),
                    ID_PET_BASE + *i as u32,
                    &pet.label,
                );
            };
            if !ours.is_empty() {
                append(pets_menu, MF_STRING | MF_GRAYED, 0, "Do Pets");
                ours.iter().for_each(add_pet);
            }
            if !theirs.is_empty() {
                if !ours.is_empty() {
                    append(pets_menu, MF_SEPARATOR, 0, "");
                    append(pets_menu, MF_STRING | MF_GRAYED, 0, "Seus pets");
                }
                theirs.iter().for_each(add_pet);
            }
            append_submenu(menu, pets_menu, "Pet");

            append(
                menu,
                MF_STRING,
                ID_SIZE_PANEL,
                &format!("Tamanho… ({} px)", self.cfg.width),
            );

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
            // Só vira ação quando falta algum: o install e as atualizações já instalam os hooks.
            let hooks = install::status();
            let mark = |ok: bool| if ok { "✓" } else { "✗" };
            let marks = format!(
                "Claude {} · Codex {}",
                mark(hooks.claude),
                mark(hooks.codex)
            );
            if hooks.claude && hooks.codex {
                append(menu, MF_STRING | MF_GRAYED, 0, &format!("Hooks: {marks}"));
            } else {
                append(
                    menu,
                    MF_STRING,
                    ID_INSTALL,
                    &format!("Reinstalar hooks ({marks})"),
                );
            }
            append(menu, MF_SEPARATOR, 0, "");
            append(
                menu,
                MF_STRING,
                ID_CHECK_UPDATE,
                &format!("Procurar atualizações (v{})", update::current_version()),
            );
            append(
                menu,
                MF_STRING | checked(self.cfg.check_updates),
                ID_AUTO_UPDATE,
                "Procurar atualizações automaticamente",
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
            ID_UPDATE => self.start_download(),
            ID_PET_UPDATE_ALL => self.start_pet_updates(None),
            id if (ID_PET_UPDATE_BASE..ID_PET_UPDATE_BASE + self.pet_updates.len() as u32)
                .contains(&id) =>
            {
                self.start_pet_updates(Some((id - ID_PET_UPDATE_BASE) as usize))
            }
            ID_CHECK_UPDATE => spawn_check(self.hwnd, true, Duration::ZERO, self.pets.clone()),
            ID_AUTO_UPDATE => {
                self.cfg.check_updates = !self.cfg.check_updates;
                self.cfg.save();
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
            ID_SIZE_PANEL => return After::OpenSizePanel(self.cfg.width),
            _ => {}
        }
        if let Some(error) = self.error.clone() {
            return After::Message(error, true);
        }
        After::Nothing
    }
}
