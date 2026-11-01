#!/bin/sh
# macOS 用の OpenHinoVoice.app を作る。使い方: scripts/macos/bundle.sh [出力先フォルダ]
# 出力先を省くと target/release に置く。署名は自分の Mac で動かすためのアドホック署名
# アドホック署名は作り直すたびに変わり、アクセシビリティの許可が外れる。
# 証明書があれば CODESIGN_IDENTITY="名前" を付けて実行すると、作り直しても許可が残る。
# 指定がなく、キーチェーンに「OpenHinoVoice Local Signing」があれば、それを使う
set -eu

cd "$(dirname "$0")/../.."
out=${1:-target/release}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)

cargo build --release --locked

app="$out/OpenHinoVoice.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/openhinovoice "$app/Contents/MacOS/openhinovoice"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>OpenHinoVoice</string>
    <key>CFBundleDisplayName</key>
    <string>OpenHinoVoice</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.openhinovoice</string>
    <key>CFBundleExecutable</key>
    <string>openhinovoice</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <!-- Dock に出さず、メニューバーのアイコンだけで常駐する -->
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>話した声を文字にするためにマイクを使います。音声はこの Mac の中だけで処理します。</string>
</dict>
</plist>
PLIST

identity=${CODESIGN_IDENTITY:-}
if [ -z "$identity" ] && security find-identity -p codesigning | grep -q '"OpenHinoVoice Local Signing"'; then
    identity="OpenHinoVoice Local Signing"
fi
codesign --force --sign "${identity:--}" "$app"
echo "$app"
