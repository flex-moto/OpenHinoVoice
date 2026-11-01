//! macOS の録音中の表示と、メニューバーのアイコン。絵は Windows と同じ overlay_draw で描き、
//! 枠なしの透明なウィンドウに貼る。クリックは下のアプリへ素通しし、フォーカスも奪わない

use crate::mac::ffi::*;
use crate::overlay_draw::{
    compose, ease_out, level_from_rms, palette_for, rms, Canvas, Palette, TextMask, FADE_IN,
    FADE_OUT, FRAME, HINOMARU,
};
pub use crate::overlay_draw::{Mode, Tone};
use crate::paste::Owner;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor,
    NSEventMask, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString, NSUserDefaults,
};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
static RECOVERY_COUNT: AtomicUsize = AtomicUsize::new(0);
static RECOVERY_REQUESTED: AtomicBool = AtomicBool::new(false);

const MENU_SETTINGS: &str = "settings";
const MENU_RECOVER: &str = "recover";
const MENU_EXIT: &str = "exit";
// メニューバーより前に出す。全画面のアプリの上にも出る
const OVERLAY_LEVEL: isize = 101;

struct Ui {
    window: Retained<NSWindow>,
    _tray: TrayIcon,
    recover: MenuItem,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

pub fn set_recovery_count(count: usize) {
    RECOVERY_COUNT.store(count, Ordering::SeqCst);
    UI.with(|ui| {
        if let Some(ui) = ui.borrow().as_ref() {
            ui.recover.set_text(recover_label(count));
            ui.recover.set_enabled(count > 0);
        }
    });
}

pub fn take_recovery_request() -> bool {
    RECOVERY_REQUESTED.swap(false, Ordering::SeqCst)
}

fn recover_label(count: usize) -> String {
    if count > 0 {
        format!("未入力テキストをコピー（残り{count}件）")
    } else {
        "未入力テキストはありません".into()
    }
}

/// 表示を置く画面の、使える範囲(ポイント、左下が原点)
#[derive(Clone, Copy)]
struct Area {
    x: f64,
    top: f64,
    width: f64,
}

struct Overlay {
    mode: Option<Mode>,
    shown_at: Instant,
    hiding_since: Option<Instant>,
    visible: bool,
    level: f32,
    area: Area,
    scale: f32,
    theme: String,
    palette: &'static Palette,
    last_frame: Instant,
}

fn store() -> &'static Mutex<Overlay> {
    static S: OnceLock<Mutex<Overlay>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Overlay {
            mode: None,
            shown_at: Instant::now(),
            hiding_since: None,
            visible: false,
            level: 0.0,
            area: Area {
                x: 0.0,
                top: 0.0,
                width: 0.0,
            },
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

pub fn hwnd() -> Owner {
    Owner
}

/// 常駐アプリには見える出力先がないので、致命的なエラーはダイアログで知らせる
pub fn show_error(message: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("{message}");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("OpenHinoVoice"));
    alert.setInformativeText(&NSString::from_str(message));
    alert.runModal();
}

pub fn create() -> Result<Owner, String> {
    let mtm = MainThreadMarker::new().ok_or("メインスレッドで起動してください")?;
    let app = NSApplication::sharedApplication(mtm);
    // Dock には出さず、メニューバーのアイコンだけで常駐する
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setIgnoresMouseEvents(true);
    window.setLevel(OVERLAY_LEVEL);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    if let Some(view) = window.contentView() {
        view.setWantsLayer(true);
    }

    let recover = MenuItem::with_id(MENU_RECOVER, recover_label(0), false, None);
    let menu = Menu::new();
    menu.append_items(&[
        &MenuItem::with_id(MENU_SETTINGS, "設定…", true, None),
        &recover,
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(MENU_EXIT, "OpenHinoVoice を終了", true, None),
    ])
    .map_err(|e| format!("メニューを作れません: {e}"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(tray_icon_image()?)
        .with_tooltip("OpenHinoVoice")
        .build()
        .map_err(|e| format!("メニューバーにアイコンを置けません: {e}"))?;

    UI.with(|ui| {
        *ui.borrow_mut() = Some(Ui {
            window,
            _tray: tray,
            recover,
        })
    });
    Ok(Owner)
}

/// 白い縁の中に赤い丸。メニューバーが明るくても暗くても見える
fn tray_icon_image() -> Result<Icon, String> {
    const SIZE: u32 = 36;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = SIZE as f32 / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let ring = (13.5 - d).clamp(0.0, 1.0);
            let dot = (10.5 - d).clamp(0.0, 1.0);
            let (r, g, b) = (
                255.0 * (1.0 - dot) + 188.0 * dot,
                255.0 * (1.0 - dot),
                255.0 * (1.0 - dot) + 45.0 * dot,
            );
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[r as u8, g as u8, b as u8, (ring * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).map_err(|e| format!("アイコンを作れません: {e}"))
}

pub fn remove_tray() {
    UI.with(|ui| ui.borrow_mut().take());
}

/// メインループから毎回呼ぶ。たまった画面の出来事と、メニューの選択を処理する
pub fn pump() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    loop {
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                NSDefaultRunLoopMode,
                true,
            )
        };
        match event {
            Some(event) => app.sendEvent(&event),
            None => break,
        }
    }
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        match event.id.as_ref() {
            MENU_SETTINGS => {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).arg("--settings").spawn();
                }
            }
            MENU_RECOVER if RECOVERY_COUNT.load(Ordering::SeqCst) > 0 => {
                RECOVERY_REQUESTED.store(true, Ordering::SeqCst);
            }
            MENU_EXIT => EXIT_REQUESTED.store(true, Ordering::SeqCst),
            _ => {}
        }
    }
}

/// 表示内容を切り替える。非表示からの表示では、いま使っている画面の上端に出す
pub fn show(mode: Mode) {
    let mut o = lock();
    if !o.visible || o.hiding_since.is_some() {
        let (area, scale) = anchor_screen();
        o.area = area;
        o.scale = scale;
        o.palette = palette_for(&o.theme, uses_light_theme);
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
    let fade = match o.hiding_since {
        Some(since) => {
            let p = since.elapsed().as_secs_f32() / FADE_OUT.as_secs_f32();
            if p >= 1.0 {
                o.visible = false;
                o.hiding_since = None;
                o.mode = None;
                drop(o);
                UI.with(|ui| {
                    if let Some(ui) = ui.borrow().as_ref() {
                        ui.window.orderOut(None);
                    }
                });
                return;
            }
            1.0 - ease_out(p)
        }
        None => ease_out((o.shown_at.elapsed().as_secs_f32() / FADE_IN.as_secs_f32()).min(1.0)),
    };
    let Some(mode) = o.mode.clone() else { return };
    let (area, scale, level, palette) = (o.area, o.scale, o.level, o.palette);
    drop(o);
    present(&mode, palette, area, scale, level, fade);
}

fn anchor_screen() -> (Area, f32) {
    let screen = MainThreadMarker::new().and_then(NSScreen::mainScreen);
    match screen {
        Some(screen) => {
            let f = screen.visibleFrame();
            (
                Area {
                    x: f.origin.x,
                    top: f.origin.y + f.size.height,
                    width: f.size.width,
                },
                screen.backingScaleFactor() as f32,
            )
        }
        None => (
            Area {
                x: 0.0,
                top: 800.0,
                width: 1280.0,
            },
            1.0,
        ),
    }
}

fn uses_light_theme() -> bool {
    let style = NSUserDefaults::standardUserDefaults()
        .stringForKey(&NSString::from_str("AppleInterfaceStyle"));
    !style.is_some_and(|s| s.to_string().eq_ignore_ascii_case("dark"))
}

fn present(mode: &Mode, pal: &Palette, area: Area, s: f32, level: f32, fade: f32) {
    let c = compose(mode, pal, s, level, render_text);
    // 画面上端の中央。下中央はチャットの入力欄など文字を入れる場所に重なりやすいので避ける。出るときは上から少し降りてくる
    let scale = s as f64;
    let (w, h) = (c.w as f64 / scale, c.h as f64 / scale);
    let drop = 8.0 * (1.0 - fade as f64);
    let x = (area.x + (area.width - w) / 2.0).round();
    let y = area.top - 4.0 + drop - h;
    UI.with(|ui| {
        let ui = ui.borrow();
        let Some(ui) = ui.as_ref() else { return };
        let Some(image) = canvas_image(&c) else {
            return;
        };
        ui.window
            .setFrame_display(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)), true);
        if let Some(view) = ui.window.contentView() {
            let layer: *mut AnyObject = unsafe { msg_send![&*view, layer] };
            if !layer.is_null() {
                unsafe {
                    let _: () = msg_send![layer, setContents: image.0 as *mut AnyObject];
                    let _: () = msg_send![layer, setContentsScale: scale];
                }
            }
        }
        ui.window.setAlphaValue(fade as f64);
        ui.window.orderFrontRegardless();
    });
}

/// 乗算済みの b, g, r, a を、そのまま並べた画像にする
fn canvas_image(c: &Canvas) -> Option<Owned> {
    let mut bytes = Vec::with_capacity(c.px.len() * 4);
    for p in &c.px {
        for v in p {
            bytes.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
    }
    unsafe {
        let data = Owned::new(CFDataCreate(
            kCFAllocatorDefault,
            bytes.as_ptr(),
            bytes.len() as CFIndex,
        ))?;
        let provider = CGDataProviderCreateWithCFData(data.0);
        if provider.is_null() {
            return None;
        }
        let space = CGColorSpaceCreateDeviceRGB();
        let image = CGImageCreate(
            c.w as usize,
            c.h as usize,
            8,
            32,
            c.w as usize * 4,
            space,
            kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little,
            provider,
            std::ptr::null(),
            false,
            0,
        );
        CGColorSpaceRelease(space);
        CGDataProviderRelease(provider);
        Owned::new(image)
    }
}

/// Core Text で白黒の文字を描き、その濃さを塗りの割合として取り出す
fn render_text(text: &str, px_height: i32, bold: bool, max_w: i32) -> TextMask {
    let empty = TextMask {
        w: 1,
        h: 1,
        cov: vec![0],
    };
    let font_name = cfstr(if bold {
        "HiraginoSans-W6"
    } else {
        "HiraginoSans-W3"
    });
    unsafe {
        let Some(font) = Owned::new(CTFontCreateWithName(
            font_name.0,
            px_height as CGFloat,
            std::ptr::null(),
        )) else {
            return empty;
        };
        let line_for = |s: &str| -> Option<Owned> {
            let string = cfstr(s);
            let keys = [
                kCTFontAttributeName,
                kCTForegroundColorFromContextAttributeName,
            ];
            let values = [font.0, kCFBooleanTrue];
            let attrs = Owned::new(CFDictionaryCreate(
                kCFAllocatorDefault,
                keys.as_ptr(),
                values.as_ptr(),
                2,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            ))?;
            let attributed = Owned::new(CFAttributedStringCreate(
                kCFAllocatorDefault,
                string.0,
                attrs.0,
            ))?;
            Owned::new(CTLineCreateWithAttributedString(attributed.0))
        };
        let Some(mut line) = line_for(text) else {
            return empty;
        };
        let (mut ascent, mut descent, mut leading) = (0.0, 0.0, 0.0);
        let mut width = CTLineGetTypographicBounds(line.0, &mut ascent, &mut descent, &mut leading);
        if width > max_w as f64 {
            if let Some(token) = line_for("…") {
                if let Some(cut) = Owned::new(CTLineCreateTruncatedLine(
                    line.0,
                    max_w as f64,
                    kCTLineTruncationEnd,
                    token.0,
                )) {
                    line = cut;
                    width =
                        CTLineGetTypographicBounds(line.0, &mut ascent, &mut descent, &mut leading);
                }
            }
        }
        let w = (width.ceil() as i32).clamp(1, max_w);
        let h = ((ascent + descent).ceil() as i32 + 2).max(1);
        let mut cov = vec![0u8; (w * h) as usize];
        let space = CGColorSpaceCreateDeviceGray();
        let ctx = CGBitmapContextCreate(
            cov.as_mut_ptr().cast(),
            w as usize,
            h as usize,
            8,
            w as usize,
            space,
            kCGImageAlphaNone,
        );
        CGColorSpaceRelease(space);
        if ctx.is_null() {
            return empty;
        }
        CGContextSetShouldAntialias(ctx, true);
        CGContextSetShouldSmoothFonts(ctx, false);
        CGContextSetGrayFillColor(ctx, 1.0, 1.0);
        CGContextSetTextPosition(ctx, 0.0, descent + 1.0);
        CTLineDraw(line.0, ctx);
        CGContextRelease(ctx);
        TextMask { w, h, cov }
    }
}
