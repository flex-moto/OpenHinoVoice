//! ログイン時の自動起動。~/Library/LaunchAgents に、今の実行ファイルを起動する設定を置く

use std::path::PathBuf;

const LABEL: &str = "io.github.openhinovoice";

fn plist_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{LABEL}.plist")),
    )
}

/// Windows の旧名からの移行に合わせた口。macOS には移すものがない
pub fn migrate_legacy() {}

pub fn is_enabled() -> bool {
    plist_path().is_some_and(|p| p.is_file())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(exe)
    )
}

pub fn set_enabled(on: bool) -> Result<(), String> {
    let path = plist_path().ok_or("ホームフォルダが分かりません")?;
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("自動起動を解除できません: {e}")),
        };
    }
    // 有効時は常に現在の実行ファイルのパスを書き直す。アプリを移動したあとの古い設定を直す
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("自動起動を設定できません: {e}"))?;
    }
    std::fs::write(&path, plist(&exe.to_string_lossy()))
        .map_err(|e| format!("自動起動を設定できません: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_agent_points_at_the_escaped_executable() {
        let text = plist("/Applications/A&B <x>.app/Contents/MacOS/openhinovoice");
        assert!(text.contains(
            "<string>/Applications/A&amp;B &lt;x&gt;.app/Contents/MacOS/openhinovoice</string>"
        ));
        assert!(text.contains("<key>RunAtLoad</key>"));
        assert!(text.contains(LABEL));
    }
}
