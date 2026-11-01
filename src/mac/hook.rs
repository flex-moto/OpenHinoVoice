//! macOS のキーの横取り。CGEventTap で録音開始キーと、録音中の Enter / Esc を拾う
//! 動かすには「システム設定 > プライバシーとセキュリティ > アクセシビリティ」の許可が要る

use crate::config::{Hotkey, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_WIN};
use crate::mac::ffi::*;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, Ordering};
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    Hotkey,
    Enter,
    Esc,
}

pub const MIRROR_IDLE: u8 = 0;
pub const MIRROR_RECORDING: u8 = 1;
pub const MIRROR_RECOGNIZING: u8 = 2;
pub const MIRROR_FORMATTING: u8 = 3;

const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;

static SENDER: OnceLock<Sender<KeyEvent>> = OnceLock::new();
static MIRROR: AtomicU8 = AtomicU8::new(MIRROR_IDLE);
static HOTKEY_VK: AtomicU32 = AtomicU32::new(0x77);
static HOTKEY_MODS: AtomicU8 = AtomicU8::new(0);
// 押下を横取りした主キーは、離したときも横取りしてアプリに片側だけ届かないようにする
static HOTKEY_HELD: AtomicBool = AtomicBool::new(false);
// 押しっぱなしの Enter は自動リピートで何度も届く。最初の 1 回だけを扱う
static ENTER_HELD: AtomicBool = AtomicBool::new(false);
static ENTER_CAPTURED: AtomicBool = AtomicBool::new(false);
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static RUN_LOOP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

/// Returns (swallow, send_event). Also captures the matching key-up even if
/// the app state changes while the key is held.
fn capture_press(
    held: &AtomicBool,
    captured: &AtomicBool,
    is_down: bool,
    active: bool,
) -> (bool, bool) {
    if is_down {
        if held.swap(true, Ordering::SeqCst) {
            return (captured.load(Ordering::SeqCst), false);
        }
        captured.store(active, Ordering::SeqCst);
        (active, active)
    } else {
        held.store(false, Ordering::SeqCst);
        (captured.swap(false, Ordering::SeqCst), false)
    }
}

pub fn set_mirror(state: u8) {
    MIRROR.store(state, Ordering::SeqCst);
}

pub fn mirror() -> u8 {
    MIRROR.load(Ordering::SeqCst)
}

pub fn set_hotkey(hotkey: Hotkey) {
    HOTKEY_VK.store(hotkey.vk, Ordering::SeqCst);
    HOTKEY_MODS.store(hotkey.mods, Ordering::SeqCst);
}

pub fn install(sender: Sender<KeyEvent>, hotkey: Hotkey) -> Result<(), String> {
    let _ = SENDER.set(sender);
    set_hotkey(hotkey);
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("openhinovoice-keys".into())
        .spawn(move || unsafe {
            let mask = (1u64 << kCGEventKeyDown) | (1u64 << kCGEventKeyUp);
            let tap = CGEventTapCreate(
                kCGSessionEventTap,
                kCGHeadInsertEventTap,
                kCGEventTapOptionDefault,
                mask,
                tap_callback,
                std::ptr::null_mut(),
            );
            if tap.is_null() {
                let _ = tx.send(Err(PERMISSION_MESSAGE.into()));
                return;
            }
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0);
            if source.is_null() {
                let _ = tx.send(Err("キーの監視を始められません".into()));
                return;
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            TAP.store(tap, Ordering::SeqCst);
            RUN_LOOP.store(run_loop, Ordering::SeqCst);
            let _ = tx.send(Ok(()));
            CFRunLoopRun();
        })
        .map_err(|e| format!("キー監視のスレッドを作れません: {e}"))?;
    rx.recv()
        .unwrap_or_else(|_| Err("キーの監視を始められません".into()))
}

pub fn uninstall() {
    let tap = TAP.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !tap.is_null() {
        unsafe { CGEventTapEnable(tap, false) };
    }
    let run_loop = RUN_LOOP.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !run_loop.is_null() {
        unsafe { CFRunLoopStop(run_loop) };
    }
}

pub const PERMISSION_MESSAGE: &str = "キー操作を受け取るには「アクセシビリティ」の許可が必要です。\nシステム設定 > プライバシーとセキュリティ > アクセシビリティ で OpenHinoVoice をオンにしてから、もう一度起動してください。";

fn mods_from_flags(flags: CGEventFlags) -> u8 {
    let mut mods = 0;
    if flags & kCGEventFlagMaskControl != 0 {
        mods |= MOD_CTRL;
    }
    if flags & kCGEventFlagMaskAlternate != 0 {
        mods |= MOD_ALT;
    }
    if flags & kCGEventFlagMaskShift != 0 {
        mods |= MOD_SHIFT;
    }
    if flags & kCGEventFlagMaskCommand != 0 {
        mods |= MOD_WIN;
    }
    mods
}

/// Mac のキーコードを、設定で使う Windows の仮想キーコードに読み替える。US と JIS で文字キーの位置は同じ
pub fn vk_from_keycode(keycode: u16) -> Option<u32> {
    let letter = |c: u8| Some(c as u32);
    match keycode {
        0 => letter(b'A'),
        1 => letter(b'S'),
        2 => letter(b'D'),
        3 => letter(b'F'),
        4 => letter(b'H'),
        5 => letter(b'G'),
        6 => letter(b'Z'),
        7 => letter(b'X'),
        8 => letter(b'C'),
        9 => letter(b'V'),
        11 => letter(b'B'),
        12 => letter(b'Q'),
        13 => letter(b'W'),
        14 => letter(b'E'),
        15 => letter(b'R'),
        16 => letter(b'Y'),
        17 => letter(b'T'),
        18 => letter(b'1'),
        19 => letter(b'2'),
        20 => letter(b'3'),
        21 => letter(b'4'),
        22 => letter(b'6'),
        23 => letter(b'5'),
        25 => letter(b'9'),
        26 => letter(b'7'),
        28 => letter(b'8'),
        29 => letter(b'0'),
        31 => letter(b'O'),
        32 => letter(b'U'),
        34 => letter(b'I'),
        35 => letter(b'P'),
        37 => letter(b'L'),
        38 => letter(b'J'),
        40 => letter(b'K'),
        45 => letter(b'N'),
        46 => letter(b'M'),
        49 => Some(0x20),
        36 | 76 => Some(VK_RETURN),
        53 => Some(VK_ESCAPE),
        _ => function_key(keycode).map(|n| 0x6F + n),
    }
}

fn function_key(keycode: u16) -> Option<u32> {
    const KEYS: [u16; 20] = [
        122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90,
    ];
    KEYS.iter()
        .position(|&k| k == keycode)
        .map(|i| i as u32 + 1)
}

fn send(event: KeyEvent) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.send(event);
    }
}

/// true なら、そのキーを他のアプリに渡さない
fn handle(vk: u32, is_down: bool, mods: u8) -> bool {
    let state = mirror();
    if vk == HOTKEY_VK.load(Ordering::SeqCst) {
        if is_down {
            if HOTKEY_HELD.load(Ordering::SeqCst) {
                // 押しっぱなしの自動リピートは、横取りしたまま何もしない
                return true;
            }
            let hit = mods == HOTKEY_MODS.load(Ordering::SeqCst);
            if hit {
                HOTKEY_HELD.store(true, Ordering::SeqCst);
                send(KeyEvent::Hotkey);
            }
            return hit;
        }
        return HOTKEY_HELD.swap(false, Ordering::SeqCst);
    }
    if vk == VK_RETURN {
        let (swallow, fire) =
            capture_press(&ENTER_HELD, &ENTER_CAPTURED, is_down, state != MIRROR_IDLE);
        if fire {
            send(KeyEvent::Enter);
        }
        return swallow;
    }
    if vk == VK_ESCAPE && state != MIRROR_IDLE {
        if is_down {
            send(KeyEvent::Esc);
        }
        return true;
    }
    false
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event_type == kCGEventTapDisabledByTimeout || event_type == kCGEventTapDisabledByUserInput {
        // 応答が遅れたなどで OS に止められたら、すぐに付け直す
        let tap = TAP.load(Ordering::SeqCst);
        if !tap.is_null() {
            unsafe { CGEventTapEnable(tap, true) };
        }
        return event;
    }
    if event_type != kCGEventKeyDown && event_type != kCGEventKeyUp {
        return event;
    }
    let (keycode, flags) = unsafe {
        (
            CGEventGetIntegerValueField(event, kCGKeyboardEventKeycode) as u16,
            CGEventGetFlags(event),
        )
    };
    let Some(vk) = vk_from_keycode(keycode) else {
        return event;
    };
    if handle(vk, event_type == kCGEventKeyDown, mods_from_flags(flags)) {
        std::ptr::null_mut()
    } else {
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_repeat_never_issues_second_event_after_state_transition() {
        let held = AtomicBool::new(false);
        let captured = AtomicBool::new(false);
        assert_eq!(capture_press(&held, &captured, true, true), (true, true));
        assert_eq!(capture_press(&held, &captured, true, true), (true, false));
        assert_eq!(capture_press(&held, &captured, true, false), (true, false));
        assert_eq!(capture_press(&held, &captured, false, false), (true, false));
        assert_eq!(capture_press(&held, &captured, true, true), (true, true));
    }

    #[test]
    fn mac_keycodes_map_to_hotkey_codes() {
        assert_eq!(vk_from_keycode(100), Some(0x77)); // F8
        assert_eq!(vk_from_keycode(122), Some(0x70)); // F1
        assert_eq!(vk_from_keycode(9), Some(b'V' as u32));
        assert_eq!(vk_from_keycode(38), Some(b'J' as u32));
        assert_eq!(vk_from_keycode(29), Some(b'0' as u32));
        assert_eq!(vk_from_keycode(36), Some(0x0D));
        assert_eq!(vk_from_keycode(76), Some(0x0D));
        assert_eq!(vk_from_keycode(53), Some(0x1B));
        assert_eq!(vk_from_keycode(49), Some(0x20));
        assert_eq!(vk_from_keycode(57), None); // Caps Lock
    }

    #[test]
    fn modifier_flags_follow_windows_bits() {
        assert_eq!(
            mods_from_flags(kCGEventFlagMaskControl | kCGEventFlagMaskAlternate),
            MOD_CTRL | MOD_ALT
        );
        assert_eq!(mods_from_flags(kCGEventFlagMaskCommand), MOD_WIN);
        assert_eq!(mods_from_flags(0x0001_0000), 0); // Caps Lock は数えない
    }
}
