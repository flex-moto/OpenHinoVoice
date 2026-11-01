use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::overlay_draw::{
    compose, ease_out, level_from_rms, palette_for, rms, Canvas, Palette, TextMask, FADE_IN,
    FADE_OUT, FRAME, HINOMARU,
};
pub use crate::overlay_draw::{Mode, Tone};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    GetMonitorInfoW, MonitorFromWindow, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, DT_CALCRECT,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, HFONT, HGDIOBJ, MONITORINFO,
    MONITOR_DEFAULTTOPRIMARY, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    GetCursorPos, GetForegroundWindow, LoadIconW, MessageBoxW, PostMessageW, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, ShowWindow, TrackPopupMenu, UpdateLayeredWindow,
    HWND_TOPMOST, IDI_APPLICATION, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MF_STRING,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, TPM_RETURNCMD, ULW_ALPHA,
    WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_RBUTTONUP, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const CMD_EXIT: usize = 1001;
pub const CMD_SETTINGS: usize = 1002;
pub const CMD_RECOVER_TEXT: usize = 1003;

pub static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static RECOVERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static RECOVERY_REQUESTED: AtomicBool = AtomicBool::new(false);
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();

pub fn set_recovery_count(count: usize) {
    RECOVERY_COUNT.store(count, Ordering::SeqCst);
}

pub fn take_recovery_request() -> bool {
    RECOVERY_REQUESTED.swap(false, Ordering::SeqCst)
}

struct Overlay {
    hwnd: isize,
    mode: Option<Mode>,
    shown_at: Instant,
    hiding_since: Option<Instant>,
    visible: bool,
    level: f32,
    work: RECT,
    scale: f32,
    theme: String,
    palette: &'static Palette,
    last_frame: Instant,
}

fn store() -> &'static Mutex<Overlay> {
    static S: OnceLock<Mutex<Overlay>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Overlay {
            hwnd: 0,
            mode: None,
            shown_at: Instant::now(),
            hiding_since: None,
            visible: false,
            level: 0.0,
            work: RECT::default(),
            scale: 1.0,
            theme: "hinomaru".into(),
            palette: &HINOMARU,
            last_frame: Instant::now(),
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, Overlay> {
    store().lock().unwrap_or_else(|p| p.into_inner())
}

pub fn hwnd() -> HWND {
    HWND(lock().hwnd as *mut std::ffi::c_void)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// GUI-subsystem binaries have no visible stderr; surface fatal errors to the user.
pub fn show_error(message: &str) {
    let text = wide(message);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            windows::core::w!("OpenHinoVoice"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

pub fn create() -> Result<HWND, String> {
    unsafe {
        let taskbar_created = *TASKBAR_CREATED
            .get_or_init(|| RegisterWindowMessageW(windows::core::w!("TaskbarCreated")));
        if taskbar_created == 0 {
            return Err(format!(
                "TaskbarCreated メッセージを登録できません: {}",
                std::io::Error::last_os_error()
            ));
        }
        let hinstance = GetModuleHandleW(None)
            .map_err(|e| format!("モジュールハンドルを取得できません: {e}"))?;
        let class_name = wide("OpenHinoVoiceOverlay");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassW(&wc);
        // 下のアプリへのクリックを素通しし、フォーカスも奪わない
        let window = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(wide("OpenHinoVoice").as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            HINSTANCE(hinstance.0),
            None,
        )
        .map_err(|e| format!("オーバーレイを作れません: {e}"))?;
        if let Err(e) = add_tray(window) {
            let _ = DestroyWindow(window);
            return Err(e);
        }
        lock().hwnd = window.0 as isize;
        Ok(window)
    }
}

unsafe fn add_tray(window: HWND) -> Result<(), String> {
    let icon = LoadIconW(None, IDI_APPLICATION)
        .map_err(|e| format!("トレイ用アイコンを読み込めません: {e}"))?;
    let mut tip = [0u16; 128];
    let label: Vec<u16> = "OpenHinoVoice".encode_utf16().collect();
    tip[..label.len()].copy_from_slice(&label);
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        szTip: tip,
        ..Default::default()
    };
    if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
        return Err(format!(
            "通知領域にトレイアイコンを登録できません: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

pub fn remove_tray() {
    unsafe {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd(),
            uID: 1,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// 表示内容を切り替える。非表示からの表示では、いま使っているアプリのある画面の上端に出す
pub fn show(mode: Mode) {
    let mut o = lock();
    if !o.visible || o.hiding_since.is_some() {
        let (work, scale) = unsafe { anchor_monitor() };
        o.work = work;
        o.scale = scale;
        o.palette = palette_for(&o.theme, windows_uses_light_theme);
        o.shown_at = Instant::now();
        o.level = 0.0;
    }
    o.mode = Some(mode);
    o.hiding_since = None;
    o.visible = true;
    o.last_frame = Instant::now() - FRAME;
}

/// 設定の theme を渡す。次に表示を出すときから効く
pub fn set_theme(theme: &str) {
    lock().theme = theme.to_string();
}

pub fn hide() {
    let mut o = lock();
    if o.visible && o.hiding_since.is_none() {
        o.hiding_since = Some(Instant::now());
    }
}

/// フェードアウト中も更新が必要なので、完全に非表示になってからアイドル待機する。
pub fn is_visible() -> bool {
    lock().visible
}

/// 録音中のマイクの音を渡す。表示の棒の高さに使う
pub fn push_level(samples: &[f32]) {
    if samples.is_empty() {
        return;
    }
    let target = level_from_rms(rms(samples));
    let mut o = lock();
    // 立ち上がりは速く、下がりはゆっくり
    let k = if target > o.level { 0.6 } else { 0.18 };
    o.level += (target - o.level) * k;
}

/// メインループから毎回呼ぶ。アニメーション中だけ描き直す
pub fn tick() {
    let mut o = lock();
    if !o.visible || o.last_frame.elapsed() < FRAME {
        return;
    }
    o.last_frame = Instant::now();
    let window = HWND(o.hwnd as *mut std::ffi::c_void);
    if window.0.is_null() {
        return;
    }
    let fade = match o.hiding_since {
        Some(since) => {
            let p = since.elapsed().as_secs_f32() / FADE_OUT.as_secs_f32();
            if p >= 1.0 {
                o.visible = false;
                o.hiding_since = None;
                o.mode = None;
                unsafe {
                    let _ = ShowWindow(window, SW_HIDE);
                }
                return;
            }
            1.0 - ease_out(p)
        }
        None => ease_out((o.shown_at.elapsed().as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0)),
    };
    let Some(mode) = o.mode.clone() else { return };
    let (work, scale, level, palette) = (o.work, o.scale, o.level, o.palette);
    drop(o);
    unsafe { present(window, &mode, palette, work, scale, level, fade) };
}

unsafe fn anchor_monitor() -> (RECT, f32) {
    let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let _ = GetMonitorInfoW(monitor, &mut info);
    let (mut dx, mut dy) = (96u32, 96u32);
    let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
    (info.rcWork, dx as f32 / 96.0)
}

fn windows_uses_light_theme() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut std::ffi::c_void),
            Some(&mut size),
        )
    };
    err.is_err() || value != 0
}

/// GDI で白黒の文字を描き、その濃さを塗りの割合として取り出す。GDI は 32bit 画像のアルファを壊すため別に描く
unsafe fn render_text(text: &str, px_height: i32, bold: bool, max_w: i32) -> TextMask {
    let dc = CreateCompatibleDC(None);
    let font: HFONT = CreateFontW(
        -px_height,
        0,
        0,
        0,
        if bold { 600 } else { 400 },
        0,
        0,
        0,
        1, // DEFAULT_CHARSET
        0,
        0,
        4, // ANTIALIASED_QUALITY(白黒の濃淡で描き、色にじみを出さない)
        0,
        PCWSTR(wide("Yu Gothic UI").as_ptr()),
    );
    let old_font = SelectObject(dc, HGDIOBJ(font.0));
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let flags = DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: max_w,
        bottom: px_height * 2,
    };
    DrawTextW(dc, &mut buf, &mut rc, flags | DT_CALCRECT);
    let w = (rc.right - rc.left).clamp(1, max_w);
    let h = (rc.bottom - rc.top).max(1);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut cov = vec![0u8; (w * h) as usize];
    if let Ok(bmp) = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
        let old_bmp = SelectObject(dc, HGDIOBJ(bmp.0));
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00FFFFFF));
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let mut buf: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(dc, &mut buf, &mut rc, flags);
        let pixels = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
        for i in 0..(w * h) as usize {
            cov[i] = pixels[i * 4 + 1];
        }
        SelectObject(dc, old_bmp);
        let _ = DeleteObject(bmp);
    }
    SelectObject(dc, old_font);
    let _ = DeleteObject(font);
    let _ = DeleteDC(dc);
    TextMask { w, h, cov }
}

unsafe fn present(
    window: HWND,
    mode: &Mode,
    pal: &Palette,
    work: RECT,
    s: f32,
    level: f32,
    fade: f32,
) {
    let px = |v: f32| (v * s).round();
    let c = compose(mode, pal, s, level, |text, px_height, bold, max_w| {
        render_text(text, px_height, bold, max_w)
    });
    let w = c.w;

    // 画面上端の中央。下中央はチャットの入力欄など文字を入れる場所に重なりやすいので避ける。出るときは上から少し降りてくる
    let drop = px(8.0) * (1.0 - fade);
    let wx = work.left + ((work.right - work.left) - w) / 2;
    let wy = work.top + px(4.0) as i32 - drop as i32;
    blit(window, &c, wx, wy, (fade * 255.0) as u8);
}

unsafe fn blit(window: HWND, c: &Canvas, x: i32, y: i32, alpha: u8) {
    let dc = CreateCompatibleDC(None);
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: c.w,
            biHeight: -c.h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    if let Ok(bmp) = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, (c.w * c.h * 4) as usize);
        for (i, p) in c.px.iter().enumerate() {
            for k in 0..4 {
                out[i * 4 + k] = (p[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        let old = SelectObject(dc, HGDIOBJ(bmp.0));
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let dst = POINT { x, y };
        let size = SIZE { cx: c.w, cy: c.h };
        let src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(
            window,
            None,
            Some(&dst),
            Some(&size),
            dc,
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
            window,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        );
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        SelectObject(dc, old);
        let _ = DeleteObject(bmp);
    }
    let _ = DeleteDC(dc);
}

unsafe fn show_exit_menu(window: HWND) {
    let menu = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    let settings = wide("設定");
    let _ = AppendMenuW(menu, MF_STRING, CMD_SETTINGS, PCWSTR(settings.as_ptr()));
    let count = RECOVERY_COUNT.load(Ordering::SeqCst);
    if count > 0 {
        let recover = wide(&format!("未入力テキストをコピー（残り{count}件）"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_RECOVER_TEXT, PCWSTR(recover.as_ptr()));
    }
    let label = wide("終了");
    let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, PCWSTR(label.as_ptr()));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    // メニューの外をクリックしたときに閉じるため、一時的に前面にする
    let _ = SetForegroundWindow(window);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD, pt.x, pt.y, 0, window, None);
    let _ = DestroyMenu(menu);
    if cmd.0 as usize == CMD_SETTINGS {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--settings").spawn();
        }
    }
    if cmd.0 as usize == CMD_RECOVER_TEXT && count > 0 {
        RECOVERY_REQUESTED.store(true, Ordering::SeqCst);
    }
    if cmd.0 as usize == CMD_EXIT {
        EXIT_REQUESTED.store(true, Ordering::SeqCst);
        let _ = PostMessageW(
            window,
            windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn wnd_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if TASKBAR_CREATED.get().copied() == Some(msg) {
        // Explorer recreates its notification area on restart; icons must be re-added.
        if let Err(e) = add_tray(window) {
            show_error(&format!("Explorer 再起動後のトレイ復元に失敗しました: {e}"));
        }
        return LRESULT(0);
    }
    match msg {
        x if x == WM_TRAY => {
            let event = (lparam.0 as u32) & 0xFFFF;
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                show_exit_menu(window);
            }
            LRESULT(0)
        }
        x if x == WM_COMMAND => {
            if wparam.0 == CMD_EXIT {
                EXIT_REQUESTED.store(true, Ordering::SeqCst);
            } else if wparam.0 == CMD_RECOVER_TEXT && RECOVERY_COUNT.load(Ordering::SeqCst) > 0 {
                RECOVERY_REQUESTED.store(true, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        x if x == WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(window, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskbar_created_message_registration_is_stable() {
        let first = unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) };
        let second = unsafe { RegisterWindowMessageW(windows::core::w!("TaskbarCreated")) };
        assert!((0xC000..=0xFFFF).contains(&first));
        assert_eq!(first, second);
    }
}
