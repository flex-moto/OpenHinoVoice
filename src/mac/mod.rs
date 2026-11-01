//! macOS 版でだけ使う部品。hook、paste、overlay、startup は main.rs から同じ名前で差し替える

pub mod ffi;
pub mod secret;

use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 設定や履歴を置くフォルダ(~/Library/Application Support/OpenHinoVoice)
pub fn app_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library")
        .join("Application Support")
        .join("OpenHinoVoice")
}

/// プロセスをまたいだ排他。Windows の名前付きミューテックスの代わりに、ファイルのロックを使う
/// プロセスが落ちると OS がロックを外すので、取り残されることはない
pub struct FileLock(#[allow(dead_code)] File);

impl FileLock {
    fn open(name: &str) -> std::io::Result<File> {
        let dir = app_dir();
        std::fs::create_dir_all(&dir)?;
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!(".{safe}.lock")))
    }

    /// 取れるまで最大 timeout 待つ
    pub fn acquire(name: &str, timeout: Duration) -> std::io::Result<FileLock> {
        let file = Self::open(name)?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(FileLock(file)),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "排他ロックを取得できません",
                    ))
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }

    /// すでにほかのプロセスが持っていれば None
    pub fn try_acquire(name: &str) -> Option<FileLock> {
        let file = Self::open(name).ok()?;
        file.try_lock().ok()?;
        Some(FileLock(file))
    }
}

/// 設定の "Ctrl+Alt+V" を、Mac のキーの名前で "control + option + V" と書く
pub fn hotkey_label(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|part| match part.trim().to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" => "control".to_string(),
            "ALT" | "OPTION" | "OPT" => "option".to_string(),
            "WIN" | "CMD" | "COMMAND" => "command".to_string(),
            "SHIFT" => "shift".to_string(),
            _ => part.trim().to_ascii_uppercase(),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// 既定のブラウザで開く
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}

/// 入力先アプリの表示名。AI 整形の参考に使う
pub fn process_name(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let app =
        objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32)?;
    let name = app.localizedName()?.to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_lock_excludes_a_second_holder_until_released() {
        let name = format!("test-{}", std::process::id());
        let first = FileLock::try_acquire(&name).expect("first lock");
        assert!(FileLock::try_acquire(&name).is_none());
        assert!(FileLock::acquire(&name, Duration::from_millis(50)).is_err());
        drop(first);
        assert!(FileLock::try_acquire(&name).is_some());
        let _ = std::fs::remove_file(app_dir().join(format!(".{name}.lock")));
    }

    #[test]
    fn hotkey_label_uses_mac_key_names() {
        assert_eq!(hotkey_label("Ctrl+Alt+V"), "control + option + V");
        assert_eq!(hotkey_label("cmd+shift+j"), "command + shift + J");
        assert_eq!(hotkey_label("F8"), "F8");
    }

    #[test]
    fn own_process_has_a_name() {
        // テストの実行ファイルは GUI アプリではないが、名前は取れる
        let _ = process_name(std::process::id());
        assert!(process_name(0).is_none());
    }
}
