import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let size = 1024
let cs = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(
    data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
    space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
)!

func color(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: cs, components: [r, g, b, a])!
}

let outer = CGPath(
    roundedRect: CGRect(x: 34, y: 34, width: 956, height: 956),
    cornerWidth: 220, cornerHeight: 220, transform: nil
)
ctx.addPath(outer)
ctx.clip()

let gradient = CGGradient(
    colorsSpace: cs,
    colors: [
        color(0.10, 0.27, 0.72),
        color(0.035, 0.065, 0.17)
    ] as CFArray,
    locations: [0, 1]
)!
ctx.drawLinearGradient(gradient, start: CGPoint(x: 180, y: 900), end: CGPoint(x: 840, y: 80), options: [])

// Soft glow behind the pages.
let glow = CGGradient(
    colorsSpace: cs,
    colors: [color(0.30, 0.62, 1.0, 0.38), color(0.30, 0.62, 1.0, 0.0)] as CFArray,
    locations: [0, 1]
)!
ctx.drawRadialGradient(
    glow,
    startCenter: CGPoint(x: 512, y: 560), startRadius: 10,
    endCenter: CGPoint(x: 512, y: 560), endRadius: 460,
    options: []
)

// Back sheets (stacked, long-screenshot motif).
func rounded(_ rect: CGRect, _ radius: CGFloat, _ fill: CGColor) {
    let path = CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
    ctx.addPath(path)
    ctx.setFillColor(fill)
    ctx.fillPath()
}

rounded(CGRect(x: 250, y: 190, width: 540, height: 690), 44, color(0.12, 0.28, 0.58, 0.85))
rounded(CGRect(x: 218, y: 238, width: 548, height: 700), 46, color(0.20, 0.43, 0.85, 0.92))
rounded(CGRect(x: 188, y: 286, width: 566, height: 700), 48, color(0.96, 0.98, 1.0))

// Page content lines.
ctx.setFillColor(color(0.12, 0.31, 0.68, 1.0))
for (index, y) in stride(from: 832, through: 500, by: -56).enumerated() {
    let width: CGFloat = index % 3 == 1 ? 250 : 420
    let rect = CGRect(x: 244, y: CGFloat(y), width: width, height: 24)
    let path = CGPath(roundedRect: rect, cornerWidth: 12, cornerHeight: 12, transform: nil)
    ctx.addPath(path)
    ctx.fillPath()
}
ctx.setFillColor(color(0.77, 0.83, 0.93, 1.0))
for y in stride(from: 452, through: 348, by: -44) {
    let rect = CGRect(x: 244, y: CGFloat(y), width: 400, height: 16)
    let path = CGPath(roundedRect: rect, cornerWidth: 8, cornerHeight: 8, transform: nil)
    ctx.addPath(path)
    ctx.fillPath()
}

// Down arrow badge.
ctx.setFillColor(color(0.17, 0.48, 1.0, 1.0))
let badge = CGPath(ellipseIn: CGRect(x: 592, y: 170, width: 260, height: 260), transform: nil)
ctx.addPath(badge)
ctx.fillPath()

ctx.setStrokeColor(color(1, 1, 1, 1))
ctx.setLineWidth(30)
ctx.setLineCap(.round)
ctx.move(to: CGPoint(x: 722, y: 344))
ctx.addLine(to: CGPoint(x: 722, y: 222))
ctx.strokePath()
ctx.move(to: CGPoint(x: 668, y: 272))
ctx.addLine(to: CGPoint(x: 722, y: 218))
ctx.addLine(to: CGPoint(x: 776, y: 272))
ctx.strokePath()

let image = ctx.makeImage()!
let url = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "assets/AppIcon.png")
let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, image, nil)
CGImageDestinationFinalize(dest)
print("icon written to \(url.path)")
