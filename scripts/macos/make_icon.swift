// macOS のアプリのアイコン(AppIcon.icns)を作る。使い方: swift scripts/macos/make_icon.swift 出力先.icns
// 白い角丸の板に日の丸の赤い丸。録音中の表示と同じ 5 本の音量の棒を白で抜く

import AppKit

let red = NSColor(srgbRed: 188 / 255, green: 0, blue: 45 / 255, alpha: 1)
let paper = NSColor(srgbRed: 252 / 255, green: 252 / 255, blue: 250 / 255, alpha: 1)

func draw(size: Int) -> Data {
    let s = CGFloat(size)
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let k = s / 1024

    // macOS のアイコンの決まりに合わせ、1024 の中に 824 の板を置き、少し影を付ける
    let plate = NSRect(x: 100 * k, y: 100 * k, width: 824 * k, height: 824 * k)
    let shape = NSBezierPath(roundedRect: plate, xRadius: 185 * k, yRadius: 185 * k)
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.28)
    shadow.shadowBlurRadius = 24 * k
    shadow.shadowOffset = NSSize(width: 0, height: -10 * k)
    NSGraphicsContext.saveGraphicsState()
    shadow.set()
    paper.setFill()
    shape.fill()
    NSGraphicsContext.restoreGraphicsState()
    NSColor.black.withAlphaComponent(0.08).setStroke()
    shape.lineWidth = 2 * k
    shape.stroke()

    // 日の丸。国旗と同じく、直径は板の高さの 5 分の 3
    let r = 824 * k * 0.3
    let c = NSPoint(x: 512 * k, y: 512 * k)
    red.setFill()
    NSBezierPath(ovalIn: NSRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)).fill()

    // 音量の棒
    let heights: [CGFloat] = [0.42, 0.66, 0.9, 0.66, 0.42]
    let barW = 34 * k
    let gap = 26 * k
    let total = barW * 5 + gap * 4
    NSColor.white.setFill()
    for (i, h) in heights.enumerated() {
        let bh = r * 1.15 * h
        let x = c.x - total / 2 + CGFloat(i) * (barW + gap)
        NSBezierPath(
            roundedRect: NSRect(x: x, y: c.y - bh / 2, width: barW, height: bh),
            xRadius: barW / 2, yRadius: barW / 2
        ).fill()
    }
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let out = URL(fileURLWithPath: CommandLine.arguments[1])
let iconset = FileManager.default.temporaryDirectory
    .appendingPathComponent("AppIcon-\(ProcessInfo.processInfo.processIdentifier).iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    try draw(size: base).write(to: iconset.appendingPathComponent("icon_\(base)x\(base).png"))
    try draw(size: base * 2).write(to: iconset.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", out.path]
try iconutil.run()
iconutil.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
if iconutil.terminationStatus != 0 { exit(iconutil.terminationStatus) }
print(out.path)
