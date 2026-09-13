//! The notification-area icon, its menu, and the hotkeys — the daemon's whole
//! user interface and its whole reason to exist.
//!
//! There is no window and no console: the icon is the only sign it is running,
//! and the menu is the only way to talk to it. That is also why the icon is
//! worth having at all. A resident process with nothing on screen is one you
//! cannot tell is alive, cannot quit without Task Manager, and cannot ask what
//! went wrong — which would sit badly in a tool built around never failing
//! quietly.
//!
//! Everything here runs on one thread with one message loop, because the daemon
//! has nothing else to do. The exception is running an action: switching to the
//! TV waits on CEC for up to 27 seconds, and a message loop that blocked for
//! that long would stop pumping, stop redrawing, and eventually be declared not
//! responding. So actions go to a short-lived thread and report back with a
//! posted message.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::config::Config;
use crate::display::apply;
use crate::hotkey;
use crate::icon::{self, Status};
use crate::log;

/// The tray icon reports clicks by sending this to our window, with the real
/// mouse message in lparam.
const WM_TRAY: u32 = WM_APP + 1;
/// Posted by a worker thread when its action has finished; wparam is 1 on
/// success. Posted rather than called so the worker never waits on a message
/// loop that might be sitting inside an open menu.
const WM_ACTION_DONE: u32 = WM_APP + 2;

// Menu command ids. 0 is reserved: TrackPopupMenu returns it for "clicked
// away", so no item may use it.
const ID_CONFIG: usize = 1;
const ID_LOG: usize = 2;
const ID_RELOAD: usize = 3;
const ID_AUTOSTART: usize = 4;
const ID_EXIT: usize = 5;
/// Hotkey actions get ids from here upwards, one per action, in menu order.
const ID_ACTION: usize = 100;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("monitor-switcher-tray");

/// Lives as long as the window, reachable from the window procedure through
/// GWLP_USERDATA.
struct State {
    config: PathBuf,
    actions: Vec<hotkey::Action>,
    /// Hotkeys that could not be used, and why. Shown in the menu, because a
    /// key that silently does nothing is the failure this daemon exists to
    /// avoid reintroducing.
    problems: Vec<String>,
    icons: [HICON; 3],
    status: Cell<Status>,
    /// One action at a time. A second press while the TV is waking would
    /// otherwise queue up a switch back.
    busy: Arc<AtomicBool>,
    /// The profile currently on screen, for the menu. Refreshed when Windows
    /// says the display configuration changed, which covers changes made in
    /// Settings as well as our own.
    active: RefCell<Option<String>>,
}

impl State {
    fn icon(&self) -> HICON {
        self.icons[self.status.get() as usize]
    }

    fn tooltip(&self) -> String {
        let where_ = match self.active.borrow().as_deref() {
            Some(name) => format!("on {name}"),
            None => "on an unnamed arrangement".to_string(),
        };
        match self.status.get() {
            Status::Working => format!("monitor-switcher — working, {where_}"),
            Status::Problem => format!("monitor-switcher — {}, {where_}", self.problem_summary()),
            Status::Ready => format!("monitor-switcher — {where_}"),
        }
    }

    fn problem_summary(&self) -> String {
        match self.problems.len() {
            0 => "last action failed".to_string(),
            1 => "1 hotkey unavailable".to_string(),
            n => format!("{n} hotkeys unavailable"),
        }
    }

    fn refresh_active(&self) {
        let name = Config::load(&self.config)
            .and_then(|c| apply::current_profile(&c))
            .unwrap_or(None);
        *self.active.borrow_mut() = name;
    }
}

/// Run the daemon. Returns when the user picks Exit or Windows ends the session.
pub fn run(config_path: PathBuf) -> Result<()> {
    // One daemon per user: two would both try to register the same hotkeys, and
    // the second would be the one that failed, for reasons invisible from the
    // outside.
    let _single = single_instance()?;

    let config = Config::load(&config_path)
        .with_context(|| format!("the daemon needs a config at {}", config_path.display()))?;
    let plan = hotkey::plan(&config.hotkeys);

    if plan.ready.is_empty() && plan.problems.is_empty() {
        anyhow::bail!(
            "no hotkeys are configured, so the daemon would do nothing.\n  \
             Add a \"hotkeys\" block to {}, for example:\n  \
             \"hotkeys\": {{ \"ctrl+shift+d\": \"switch\" }}",
            config_path.display()
        );
    }
    for action in &plan.ready {
        log!("{} runs \"{}\"", action.binding.describe(), action.command);
    }

    // SAFETY: everything below is one window's lifetime on one thread; each
    // call is documented at its use.
    unsafe { window_loop(config_path, plan) }
}

unsafe fn window_loop(config: PathBuf, plan: hotkey::Plan) -> Result<()> {
    // ShellExecuteW can delegate to shell extensions, which are COM objects; on
    // a thread with no apartment those quietly fail to open anything.
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

    let instance = HINSTANCE(GetModuleHandleW(None)?.0);
    let class = w!("monitor-switcher-tray");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class,
        ..Default::default()
    };
    if RegisterClassW(&wc) == 0 {
        anyhow::bail!("could not register the window class");
    }

    let icons = [
        make_icon(instance, Status::Ready)?,
        make_icon(instance, Status::Working)?,
        make_icon(instance, Status::Problem)?,
    ];

    let state = Box::new(State {
        config,
        problems: plan.problems,
        actions: plan.ready,
        icons,
        status: Cell::new(Status::Ready),
        busy: Arc::new(AtomicBool::new(false)),
        active: RefCell::new(None),
    });

    // A message-only window would be simpler but cannot receive
    // WM_DISPLAYCHANGE or the taskbar-restart broadcast, both of which this
    // needs. So: a real window that is never shown.
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        w!("monitor-switcher"),
        WS_OVERLAPPED,
        0,
        0,
        0,
        0,
        None,
        None,
        Some(instance),
        Some(Box::into_raw(state) as *const c_void),
    )?;

    let Some(state) = state_of(hwnd) else {
        anyhow::bail!("the window came up without its state");
    };

    state.refresh_active();
    register_hotkeys(hwnd, state);
    for problem in &state.problems {
        log!("hotkey problem: {problem}");
    }
    if !state.problems.is_empty() {
        state.status.set(Status::Problem);
    }
    add_icon(hwnd, state);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}

/// Register every usable hotkey, and record the ones Windows refuses.
///
/// A refusal is nearly always another program already owning the combination.
/// `RegisterHotKey` says so at registration rather than by never firing, which
/// is the whole reason this is better than a shortcut's hotkey field.
unsafe fn register_hotkeys(hwnd: HWND, state: &mut State) {
    // Each hotkey's id is its index in `actions`, which is how WM_HOTKEY's
    // wparam finds the action to run. Registering first and pruning the
    // refusals afterwards would break that: drop a refused entry from the
    // middle and every later action's index shifts away from the id it was
    // registered under. So only the ones that take are pushed, and the id is
    // the index each will actually end up at.
    let planned = std::mem::take(&mut state.actions);
    for action in planned {
        let id = state.actions.len() as i32;
        let ok = RegisterHotKey(Some(hwnd), id, action.binding.modifiers, action.binding.vk);
        if ok.is_ok() {
            state.actions.push(action);
            continue;
        }
        let code = GetLastError().0;
        state.problems.push(format!(
            "{}: Windows would not register it ({}). Another program probably has it.",
            action.spec,
            crate::winerr::describe(code)
        ));
    }
}

/// Give every registered hotkey back to Windows.
unsafe fn unregister_hotkeys(hwnd: HWND, state: &State) {
    for i in 0..state.actions.len() {
        let _ = UnregisterHotKey(Some(hwnd), i as i32);
    }
}

/// Re-read the config and rebuild the hotkeys from it, without restarting.
///
/// A config that no longer parses, or one whose hotkeys are all unusable,
/// leaves the running set alone. Losing working hotkeys to a half-finished edit
/// would be worse than ignoring the edit, and the log says which happened. A
/// config that deliberately has no hotkeys left is a different thing and is
/// honoured — that is someone turning them off, not a mistake.
unsafe fn reload(hwnd: HWND, state: &mut State) {
    let config = match Config::load(&state.config) {
        Ok(c) => c,
        Err(e) => {
            log!("reload failed, keeping the hotkeys already registered: {e:#}");
            return;
        }
    };

    let plan = hotkey::plan(&config.hotkeys);
    if plan.ready.is_empty() && !plan.problems.is_empty() {
        log!("reload found no usable hotkey, keeping the ones already registered:");
        for problem in &plan.problems {
            log!("  {problem}");
        }
        return;
    }

    unregister_hotkeys(hwnd, state);
    state.actions = plan.ready;
    state.problems = plan.problems;
    register_hotkeys(hwnd, state);

    for problem in &state.problems {
        log!("hotkey problem: {problem}");
    }
    for action in &state.actions {
        log!("{} runs \"{}\"", action.binding.describe(), action.command);
    }
    if state.actions.is_empty() {
        log!("no hotkeys are configured now; the daemon is idle until you add one");
    }

    state.refresh_active();
    state.status.set(if state.problems.is_empty() {
        Status::Ready
    } else {
        Status::Problem
    });
    // Unconditionally, because the tooltip changes even when the status does
    // not — a different count of problems is still something to show.
    update_icon(hwnd, state);
    log!("config reloaded");
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            LRESULT(0)
        }
        WM_HOTKEY => {
            if let Some(state) = state_of(hwnd) {
                dispatch(hwnd, state, wparam.0);
            }
            LRESULT(0)
        }
        WM_TRAY => {
            let click = lparam.0 as u32;
            if let Some(state) = state_of(hwnd) {
                match click {
                    WM_RBUTTONUP | WM_LBUTTONUP => show_menu(hwnd, state),
                    // The obvious gesture on a switcher: do the first thing.
                    WM_LBUTTONDBLCLK => dispatch(hwnd, state, 0),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_ACTION_DONE => {
            if let Some(state) = state_of(hwnd) {
                let status = if wparam.0 == 1 {
                    Status::Ready
                } else {
                    Status::Problem
                };
                state.refresh_active();
                set_status(hwnd, state, status);
            }
            LRESULT(0)
        }
        // Someone changed the display setup — us, or Settings, or a game.
        WM_DISPLAYCHANGE => {
            if let Some(state) = state_of(hwnd) {
                state.refresh_active();
                update_icon(hwnd, state);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            stop(hwnd);
            LRESULT(0)
        }
        WM_QUERYENDSESSION => {
            stop(hwnd);
            LRESULT(1)
        }
        WM_DESTROY => {
            if let Some(state) = state_of(hwnd) {
                let data = icon_data(hwnd, state);
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                unregister_hotkeys(hwnd, state);
                // Retake the box so the state is dropped rather than leaked.
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
                if !ptr.is_null() {
                    drop(Box::from_raw(ptr));
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ if msg == taskbar_created() => {
            // Explorer restarted and took every tray icon with it.
            if let Some(state) = state_of(hwnd) {
                add_icon(hwnd, state);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[allow(clippy::mut_from_ref)]
unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if ptr.is_null() {
        None
    } else {
        Some(&mut *ptr)
    }
}

unsafe fn stop(hwnd: HWND) {
    let _ = DestroyWindow(hwnd);
}

/// Run an action on its own thread.
///
/// The message loop must keep turning while a switch takes its seconds, so this
/// returns immediately and the worker posts `WM_ACTION_DONE` when it is through.
unsafe fn dispatch(hwnd: HWND, state: &State, index: usize) {
    let Some(action) = state.actions.get(index) else {
        return;
    };
    if state.busy.swap(true, Ordering::SeqCst) {
        log!(
            "\"{}\" ignored: the previous action is still running",
            action.command
        );
        return;
    }

    let words = action.words.clone();
    let label = action.command.clone();
    let config = state.config.clone();
    let busy = state.busy.clone();
    // HWND is not Send; the integer is.
    let target = hwnd.0 as isize;

    set_status(hwnd, state, Status::Working);
    std::thread::spawn(move || {
        log!("running \"{label}\"");
        let ok = match crate::app::run_words(&words, &config) {
            Ok(()) => true,
            Err(e) => {
                log!("\"{label}\" failed: {e:#}");
                for cause in e.chain().skip(1) {
                    log!("  caused by: {cause}");
                }
                false
            }
        };
        busy.store(false, Ordering::SeqCst);
        let _ = PostMessageW(
            Some(HWND(target as *mut c_void)),
            WM_ACTION_DONE,
            WPARAM(ok as usize),
            LPARAM(0),
        );
    });
}

unsafe fn set_status(hwnd: HWND, state: &State, status: Status) {
    // A daemon with a hotkey that never registered stays in Problem: that is a
    // standing condition, not the outcome of the last action.
    let status = if status == Status::Ready && !state.problems.is_empty() {
        Status::Problem
    } else {
        status
    };
    if state.status.get() == status {
        return;
    }
    state.status.set(status);
    update_icon(hwnd, state);
}

unsafe fn add_icon(hwnd: HWND, state: &State) {
    let data = icon_data(hwnd, state);
    let _ = Shell_NotifyIconW(NIM_ADD, &data);
}

unsafe fn update_icon(hwnd: HWND, state: &State) {
    let data = icon_data(hwnd, state);
    let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
}

fn icon_data(hwnd: HWND, state: &State) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: state.icon(),
        ..Default::default()
    };
    let tip: Vec<u16> = state.tooltip().encode_utf16().take(127).collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

unsafe fn show_menu(hwnd: HWND, state: &mut State) {
    let Ok(menu) = CreatePopupMenu() else {
        return;
    };

    let header = match state.active.borrow().as_deref() {
        Some(name) => format!("Active: {name}"),
        None => "Active: an unnamed arrangement".to_string(),
    };
    let _ = AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, wide(&header));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

    for (i, action) in state.actions.iter().enumerate() {
        let label = format!("{}\t{}", action.command, action.binding.describe());
        let _ = AppendMenuW(menu, MF_STRING, ID_ACTION + i, wide(&label));
    }

    if !state.problems.is_empty() {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        for problem in &state.problems {
            let _ = AppendMenuW(menu, MF_STRING | MF_DISABLED, 0, wide(problem));
        }
    }

    let autostart = autostart_enabled();
    let checked = if autostart { MF_CHECKED } else { MF_UNCHECKED };
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_CONFIG, w!("Open config file"));
    let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open log file"));
    let _ = AppendMenuW(menu, MF_STRING, ID_RELOAD, w!("Reload config"));
    let _ = AppendMenuW(
        menu,
        MF_STRING | checked,
        ID_AUTOSTART,
        w!("Start automatically at sign-in"),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));

    let mut at = POINT::default();
    let _ = GetCursorPos(&mut at);
    // Mandatory: without it the menu stays up after a click elsewhere, because
    // a tray menu's owner is never the active window.
    let _ = SetForegroundWindow(hwnd);
    let picked = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
        at.x,
        at.y,
        Some(0),
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);

    match picked.0 as usize {
        ID_CONFIG => open(&state.config),
        ID_LOG => match log::path() {
            Some(p) => open(&p),
            None => log!("no log file is configured"),
        },
        ID_RELOAD => reload(hwnd, state),
        ID_AUTOSTART => set_autostart(!autostart, &state.config),
        ID_EXIT => {
            log!("exit requested from the tray");
            stop(hwnd);
        }
        picked if picked >= ID_ACTION => dispatch(hwnd, state, picked - ID_ACTION),
        _ => {}
    }
    // The companion to SetForegroundWindow: without a message to process
    // afterwards the window can miss the next click on the icon.
    let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
}

/// Explorer broadcasts this when it restarts, so tray icons can put themselves
/// back. Registered once and cached.
fn taskbar_created() -> u32 {
    use std::sync::OnceLock;
    static MSG: OnceLock<u32> = OnceLock::new();
    // SAFETY: registering a window message by name; the name is a literal.
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

fn open(path: &Path) {
    let file = wide_z(&path.as_os_str().to_string_lossy());
    // SAFETY: both strings outlive the call.
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn autostart_enabled() -> bool {
    // SAFETY: a read of one registry value into a local buffer.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_READ, &mut key).is_err() {
            return false;
        }
        let present = RegQueryValueExW(key, RUN_VALUE, None, None, None, None).is_ok();
        let _ = RegCloseKey(key);
        present
    }
}

fn set_autostart(on: bool, config: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        log!("cannot find my own path, so autostart cannot be changed");
        return;
    };
    // SAFETY: one registry write or delete; every buffer outlives the call.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_WRITE, &mut key).is_err() {
            log!("cannot open the Run key, so autostart cannot be changed");
            return;
        }
        if on {
            // The config path goes in explicitly: the daemon started at sign-in
            // must read the same file as the one that set this up.
            let command = format!("\"{}\" --config \"{}\"", exe.display(), config.display());
            let value = wide_z(&command);
            let bytes = std::slice::from_raw_parts(
                value.as_ptr() as *const u8,
                std::mem::size_of_val(&value[..]),
            );
            let _ = RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes));
            log!("autostart on: {command}");
        } else {
            let _ = RegDeleteValueW(key, RUN_VALUE);
            log!("autostart off");
        }
        let _ = RegCloseKey(key);
    }
}

/// A named mutex that lives as long as the process. Held, never touched again.
fn single_instance() -> Result<SingleInstance> {
    // SAFETY: creating a named mutex; the name is a literal.
    let handle = unsafe { CreateMutexW(None, true, w!("monitor-switcher-tray-single")) }
        .context("could not take the single-instance lock")?;
    // SAFETY: reading the calling thread's last-error value.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        anyhow::bail!(
            "monitor-switcher-tray is already running.\n  \
             Its icon is in the notification area; use Exit there before starting another."
        );
    }
    Ok(SingleInstance(handle))
}

pub struct SingleInstance(windows::Win32::Foundation::HANDLE);

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: a handle from CreateMutexW, closed exactly once.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

unsafe fn make_icon(instance: HINSTANCE, status: Status) -> Result<HICON> {
    let pixels = icon::draw(status);
    // An all-zero AND mask means "take the colour bitmap as it is", which for a
    // 32-bit bitmap means its alpha channel decides what shows.
    let mask = [0u8; icon::SIZE * icon::SIZE / 8];
    CreateIcon(
        Some(instance),
        icon::SIZE as i32,
        icon::SIZE as i32,
        1,
        32,
        mask.as_ptr(),
        pixels.as_ptr(),
    )
    .context("drawing the tray icon")
}

fn wide(s: &str) -> PCWSTR {
    // Leaked deliberately: menu labels are built per menu and the strings must
    // outlive AppendMenuW's use of them. A handful of short strings per
    // right-click is not a leak worth a lifetime.
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    PCWSTR(Box::leak(v.into_boxed_slice()).as_ptr())
}

fn wide_z(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}
