//! Windows notification-area controls; the watcher never initializes a rendering context.
use crate::updater_background::{CHECK_NOW, EXIT};
use anyhow::{Result, ensure};
use std::{ptr::null_mut, sync::atomic::Ordering};
use windows_sys::Win32::{
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::*, WindowsAndMessaging::*},
};
const CALLBACK: u32 = WM_APP + 19;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn icon(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = CALLBACK;
    data.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    let tip = wide("Jan's Modpack Updater — right-click for controls");
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}
fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}
unsafe extern "system" fn window(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if msg == RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) {
            Shell_NotifyIconW(NIM_ADD, &icon(hwnd));
            return 0;
        }
        match msg {
            CALLBACK => {
                if l as u32 == WM_LBUTTONDBLCLK {
                    open_settings();
                }
                if l as u32 == WM_RBUTTONUP {
                    let menu = CreatePopupMenu();
                    AppendMenuW(menu, MF_STRING, 1, wide("Open updater / status").as_ptr());
                    AppendMenuW(menu, MF_STRING, 2, wide("Check now").as_ptr());
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                    AppendMenuW(
                        menu,
                        MF_STRING,
                        3,
                        wide("Exit after current update").as_ptr(),
                    );
                    let mut point: POINT = std::mem::zeroed();
                    GetCursorPos(&mut point);
                    SetForegroundWindow(hwnd);
                    let selected = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_NONOTIFY,
                        point.x,
                        point.y,
                        0,
                        hwnd,
                        std::ptr::null(),
                    );
                    DestroyMenu(menu);
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    match selected {
                        1 => open_settings(),
                        2 => CHECK_NOW.store(true, Ordering::Release),
                        3 => {
                            EXIT.store(true, Ordering::Release);
                        }
                        _ => {}
                    }
                }
                0
            }
            WM_DESTROY => {
                Shell_NotifyIconW(NIM_DELETE, &icon(hwnd));
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}
pub fn run() -> Result<()> {
    unsafe {
        let class = wide("JansModpackUpdaterTray");
        let instance = GetModuleHandleW(std::ptr::null());
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(window);
        wc.hInstance = instance;
        wc.lpszClassName = class.as_ptr();
        ensure!(
            RegisterClassW(&wc) != 0,
            "Could not register the background window"
        );
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("Jan's Modpack Updater").as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            std::ptr::null(),
        );
        ensure!(!hwnd.is_null(), "Could not create the tray window");
        ensure!(
            Shell_NotifyIconW(NIM_ADD, &icon(hwnd)) != 0,
            "Could not add the tray icon"
        );
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}
