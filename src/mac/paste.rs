//! Paste the final text at once with Cmd+V, instead of simulating typing through the IME.
//! Retain every item on the pasteboard and restore it after the paste.

use crate::mac::ffi::*;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSWorkspace};
use objc2_foundation::{NSArray, NSData, NSString};
use std::thread;
use std::time::Duration;

/// Windows ではクリップボードの持ち主のウィンドウを渡す。macOS では要らないので形だけ合わせる
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Owner;

const KEY_V: CGKeyCode = 9;
// クリップボード履歴アプリに「一時的な内容なので記録しない」と伝える型
// https://nspasteboard.org
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasteTarget {
    process_id: u32,
    window: usize,
    focused: usize,
}

impl PasteTarget {
    pub fn process_id(&self) -> u32 {
        self.process_id
    }
}

fn ax_attribute(element: AXUIElementRef, name: &str) -> Option<Owned> {
    let attr = cfstr(name);
    let mut value: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(element, attr.0, &mut value) };
    if err == kAXErrorSuccess {
        Owned::new(value)
    } else {
        None
    }
}

fn active_target() -> Result<PasteTarget, String> {
    let system =
        Owned::new(unsafe { AXUIElementCreateSystemWide() }).ok_or("入力先を調べられません")?;
    let app = ax_attribute(system.0, "AXFocusedApplication");
    let mut pid = 0i32;
    if let Some(app) = &app {
        unsafe { AXUIElementGetPid(app.0, &mut pid) };
    }
    if pid <= 0 {
        // アクセシビリティから取れないアプリでは、前面のアプリだけで見分ける
        pid = NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|a| a.processIdentifier())
            .unwrap_or(0);
    }
    if pid <= 0 {
        return Err("入力先のアプリを取得できません".into());
    }
    let hash = |o: Option<Owned>| o.map_or(0, |o| unsafe { CFHash(o.0) });
    let window = hash(
        app.as_ref()
            .and_then(|a| ax_attribute(a.0, "AXFocusedWindow")),
    );
    let focused = hash(ax_attribute(system.0, "AXFocusedUIElement"));
    Ok(PasteTarget {
        process_id: pid as u32,
        window,
        focused,
    })
}

/// Record the original text destination before showing the recording overlay.
pub fn capture_target() -> Result<PasteTarget, String> {
    active_target()
}

fn unchanged_target(expected: PasteTarget, current: PasteTarget) -> bool {
    expected == current
}

fn check_target(expected: PasteTarget) -> Result<(), String> {
    let current = active_target()?;
    if !unchanged_target(expected, current) {
        return Err(
            "録音開始時と入力先またはフォーカスが変わったため、貼り付けを中止しました".into(),
        );
    }
    Ok(())
}

fn no_modifiers_pressed() -> bool {
    // Modifiers could turn Cmd+V into another shortcut, particularly if the user is still
    // holding the start hotkey. Never paste while Control/Option/Shift/Command are down.
    let flags = unsafe { CGEventSourceFlagsState(kCGEventSourceStateHIDSystemState) };
    flags
        & (kCGEventFlagMaskShift
            | kCGEventFlagMaskControl
            | kCGEventFlagMaskAlternate
            | kCGEventFlagMaskCommand)
        == 0
}

fn set_text(pasteboard: &NSPasteboard, text: &str, transient: bool) -> Result<(), String> {
    pasteboard.clearContents();
    if !pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
        return Err("クリップボードへテキストを書き込めません".into());
    }
    if transient {
        let _ =
            pasteboard.setData_forType(Some(&NSData::new()), &NSString::from_str(TRANSIENT_TYPE));
    }
    Ok(())
}

/// Called ONLY after the user explicitly selects the recovery action.
pub fn copy_text_to_clipboard(text: &str, _owner: Owner) -> Result<(), String> {
    set_text(&NSPasteboard::generalPasteboard(), text, false)
}

/// 貼り付け前のクリップボードの中身。項目ごとに、すべての型のデータを取っておく
fn snapshot(pasteboard: &NSPasteboard) -> Vec<Vec<(Retained<NSString>, Retained<NSData>)>> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| item.dataForType(&ty).map(|data| (ty.clone(), data)))
                .collect()
        })
        .collect()
}

fn restore(pasteboard: &NSPasteboard, saved: &[Vec<(Retained<NSString>, Retained<NSData>)>]) {
    pasteboard.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn objc2_app_kit::NSPasteboardWriting>>> = saved
        .iter()
        .map(|entries| {
            let item = NSPasteboardItem::new();
            for (ty, data) in entries {
                item.setData_forType(data, ty);
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    let array = NSArray::from_retained_slice(&items);
    if !pasteboard.writeObjects(&array) {
        eprintln!("元のクリップボードを復元できませんでした");
    }
}

fn post_paste_shortcut() -> Result<(), String> {
    unsafe {
        let source = CGEventSourceCreate(kCGEventSourceStateCombinedSessionState);
        let down = CGEventCreateKeyboardEvent(source, KEY_V, true);
        let up = CGEventCreateKeyboardEvent(source, KEY_V, false);
        let result = if down.is_null() || up.is_null() {
            Err("貼り付けのキー操作を作れません".to_string())
        } else {
            // 押されている修飾キーの状態に関係なく、Command だけを付ける
            CGEventSetFlags(down, kCGEventFlagMaskCommand);
            CGEventSetFlags(up, kCGEventFlagMaskCommand);
            CGEventPost(kCGHIDEventTap, down);
            CGEventPost(kCGHIDEventTap, up);
            Ok(())
        };
        for ptr in [down, up, source] {
            if !ptr.is_null() {
                CFRelease(ptr);
            }
        }
        result
    }
}

/// A small, one-time pasteboard transaction. Cmd+V is a shortcut, not character injection.
/// Preserve all types of all items, and never overwrite a newer external copy.
pub fn paste_text(text: &str, target: PasteTarget, _owner: Owner) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if text.contains('\0') {
        return Err("入力文字列に NUL が含まれています".into());
    }
    if !accessibility_trusted(false) {
        return Err("アクセシビリティの許可がないため貼り付けできません".into());
    }
    check_target(target)?;
    if !no_modifiers_pressed() {
        return Err("修飾キーが押されているため、文字入力を中止しました".into());
    }

    let pasteboard = NSPasteboard::generalPasteboard();
    let before = pasteboard.changeCount();
    let previous = snapshot(&pasteboard);
    // Bail out rather than racing with another copy operation.
    check_target(target)?;
    if !no_modifiers_pressed() {
        return Err("修飾キーが押されているため、貼り付けを中止しました".into());
    }
    if before != pasteboard.changeCount() {
        return Err("クリップボードが他のアプリで更新されました".into());
    }
    let set_result = set_text(&pasteboard, text, true);
    let ours = pasteboard.changeCount();
    let result = set_result.and_then(|_| {
        check_target(target)?;
        if !no_modifiers_pressed() {
            return Err("修飾キーが押されているため、貼り付けを中止しました".into());
        }
        if ours != pasteboard.changeCount() {
            return Err("貼り付け前にクリップボードが別のアプリで変更されました".into());
        }
        post_paste_shortcut()?;
        // The destination processes the shortcut asynchronously. Don't restore
        // the pasteboard until it has had an opportunity to read the text.
        thread::sleep(Duration::from_millis(250));
        Ok(())
    });
    // External copy during paste takes precedence; never overwrite it.
    if ours != before && ours == pasteboard.changeCount() {
        restore(&pasteboard, &previous);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> PasteTarget {
        PasteTarget {
            process_id: 103,
            window: 100,
            focused: 101,
        }
    }

    #[test]
    fn focus_and_window_changes_are_rejected() {
        let t = target();
        assert_eq!(t.process_id(), 103);
        assert!(unchanged_target(t, t));
        for other in [
            PasteTarget { window: 200, ..t },
            PasteTarget { focused: 200, ..t },
            PasteTarget {
                process_id: 200,
                ..t
            },
        ] {
            assert!(!unchanged_target(t, other));
        }
    }
}
