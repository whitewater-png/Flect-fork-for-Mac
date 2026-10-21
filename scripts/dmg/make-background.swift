// Draws the background of the Flect-for-Mac disk image: an arrow from the app to Applications and the
// steps for macOS's "cannot verify the developer" warning, in Japanese and English.
//   swift scripts/dmg/make-background.swift <output.png>
// W, H, the icon centres and the arrow must match scripts/dmg/settings.py. The PNG is written at 2x pixels
// with a 144 dpi tag, so Finder shows it as 720x540 points on Retina screens.
import AppKit

let W: CGFloat = 720, H: CGFloat = 540
let appCenter = CGPoint(x: 190, y: 185), applicationsCenter = CGPoint(x: 530, y: 185)   // y from the top

guard CommandLine.arguments.count >= 2 else {
    FileHandle.standardError.write(Data("usage: make-background.swift <output.png>\n".utf8))
    exit(2)
}
let out = CommandLine.arguments[1]
let scale = 2
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W) * scale, pixelsHigh: Int(H) * scale, bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: W, height: H)   // points; with 2x pixels this writes 144 dpi
let gc = NSGraphicsContext(bitmapImageRep: rep)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = gc

// Work with a y axis that grows downwards (like the Finder window).
let ctx = gc.cgContext
ctx.translateBy(x: 0, y: H)
ctx.scaleBy(x: 1, y: -1)

func fill(_ rect: CGRect, _ color: NSColor) { color.setFill(); rect.fill() }
fill(CGRect(x: 0, y: 0, width: W, height: H), .white)
fill(CGRect(x: 0, y: 0, width: W, height: 108), NSColor(srgbRed: 0.90, green: 0.97, blue: 0.93, alpha: 1))

func text(_ s: String, in rect: CGRect, size: CGFloat, bold: Bool = false, color: NSColor = NSColor(white: 0.12, alpha: 1), align: NSTextAlignment = .left) {
    let para = NSMutableParagraphStyle()
    para.alignment = align
    para.lineSpacing = 2
    let font = NSFont(name: bold ? "HiraginoSans-W6" : "HiraginoSans-W3", size: size) ?? NSFont.systemFont(ofSize: size, weight: bold ? .bold : .regular)
    let attr = NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: color, .paragraphStyle: para])
    // The context is flipped, so draw through a flipped NSGraphicsContext-aware path.
    NSGraphicsContext.saveGraphicsState()
    let flipped = NSGraphicsContext(cgContext: ctx, flipped: true)
    NSGraphicsContext.current = flipped
    attr.draw(with: rect, options: [.usesLineFragmentOrigin])
    NSGraphicsContext.restoreGraphicsState()
}

let green = NSColor(srgbRed: 0.16, green: 0.55, blue: 0.38, alpha: 1)
text("Flect-for-Mac", in: CGRect(x: 0, y: 26, width: W, height: 36), size: 27, bold: true, color: green, align: .center)
text("アプリを「Applications」へドラッグ  /  Drag the app onto “Applications”", in: CGRect(x: 0, y: 70, width: W, height: 24), size: 14, align: .center)

// Arrow between the two icons.
let y = appCenter.y
let x0 = appCenter.x + 70, x1 = applicationsCenter.x - 70
green.setStroke()
let line = NSBezierPath()
line.lineWidth = 6
line.lineCapStyle = .round
line.move(to: CGPoint(x: x0, y: y)); line.line(to: CGPoint(x: x1 - 12, y: y))
line.stroke()
let head = NSBezierPath()
head.lineWidth = 6; head.lineCapStyle = .round; head.lineJoinStyle = .round
head.move(to: CGPoint(x: x1 - 24, y: y - 16)); head.line(to: CGPoint(x: x1, y: y)); head.line(to: CGPoint(x: x1 - 24, y: y + 16))
head.stroke()

// Divider and the warning steps.
fill(CGRect(x: 36, y: 288, width: W - 72, height: 1), NSColor(white: 0.85, alpha: 1))
text("最初に警告が出て開けないとき", in: CGRect(x: 36, y: 300, width: 320, height: 20), size: 14, bold: true, color: NSColor(srgbRed: 0.75, green: 0.35, blue: 0.05, alpha: 1))
text("If macOS blocks the app", in: CGRect(x: 384, y: 300, width: 310, height: 20), size: 14, bold: true, color: NSColor(srgbRed: 0.75, green: 0.35, blue: 0.05, alpha: 1))

let ja = """
Apple の署名・公証を受けていないため、最初の 1 回だけ許可が必要です。
・macOS 15 以降: 一度開こうとして閉じる → システム設定 → プライバシーとセキュリティ →「このまま開く」
・macOS 14: アプリを右クリック →「開く」→「開く」
"""
let en = """
The app is not notarized by Apple, so allow it once.
• macOS 15+: try to open it and close the message → System Settings → Privacy & Security → “Open Anyway”
• macOS 14: right-click the app → Open → Open
"""
text(ja, in: CGRect(x: 36, y: 326, width: 320, height: 110), size: 12)
text(en, in: CGRect(x: 384, y: 326, width: 300, height: 110), size: 12)
text("同じ案内は下のテキストにもあります。 / The same steps are in the text file below.", in: CGRect(x: 0, y: 428, width: W, height: 16), size: 11, color: NSColor(white: 0.4, alpha: 1), align: .center)

NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
try png.write(to: URL(fileURLWithPath: out))
print("wrote \(out) (\(Int(W) * scale)x\(Int(H) * scale) px)")
