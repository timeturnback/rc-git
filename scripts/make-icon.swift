// Draws the app icon and writes an .icns. Run through `swift scripts/make-icon.swift`;
// scripts/build-app.sh caches the result and only re-runs when this file is newer.
//
// Usage: make-icon.swift --output <path.icns>
//
// The mark is a commit graph on a blue squircle: a trunk of three commits with one branch
// that forks out and merges back.

import AppKit
import Foundation

let canvas = 1024.0
/// macOS draws app icons inside a squircle that leaves a margin on the full canvas.
let inset = 100.0
let side = canvas - inset * 2

let gradientTop = NSColor(srgbRed: 0.31, green: 0.55, blue: 0.98, alpha: 1)
let gradientBottom = NSColor(srgbRed: 0.09, green: 0.28, blue: 0.78, alpha: 1)

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data("error: \(message)\n".utf8))
    exit(1)
}

func option(_ name: String) -> String? {
    let arguments = CommandLine.arguments
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
        return nil
    }
    return arguments[index + 1]
}

guard let output = option("--output") else { fail("--output <path.icns> is required") }

/// A superellipse, which is what the macOS icon shape is: rounder corners than an arc.
func squircle(in rect: NSRect, exponent: Double = 5) -> NSBezierPath {
    let path = NSBezierPath()
    let halfWidth = rect.width / 2
    let halfHeight = rect.height / 2
    let center = NSPoint(x: rect.midX, y: rect.midY)
    let steps = 720
    for step in 0...steps {
        let angle = 2 * Double.pi * Double(step) / Double(steps)
        let cosine = cos(angle)
        let sine = sin(angle)
        let x = center.x + halfWidth * copysign(pow(abs(cosine), 2 / exponent), cosine)
        let y = center.y + halfHeight * copysign(pow(abs(sine), 2 / exponent), sine)
        if step == 0 {
            path.move(to: NSPoint(x: x, y: y))
        } else {
            path.line(to: NSPoint(x: x, y: y))
        }
    }
    path.close()
    return path
}

/// Trunk of three commits at `trunkX`, one branch node at `branchX` forking off the top
/// commit and merging back into the bottom one.
func drawCommitGraph(in rect: NSRect) {
    let trunkX = rect.minX + rect.width * 0.35
    let branchX = rect.minX + rect.width * 0.68
    let top = rect.maxY - rect.height * 0.17
    let middle = rect.midY
    let bottom = rect.minY + rect.height * 0.17
    let radius = rect.width * 0.098
    let stroke = rect.width * 0.062

    NSColor.white.setStroke()
    NSColor.white.setFill()

    let lines = NSBezierPath()
    lines.lineWidth = stroke
    lines.lineCapStyle = .round
    lines.lineJoinStyle = .round
    lines.move(to: NSPoint(x: trunkX, y: top))
    lines.line(to: NSPoint(x: trunkX, y: bottom))
    lines.move(to: NSPoint(x: trunkX, y: top))
    lines.line(to: NSPoint(x: branchX, y: middle))
    lines.line(to: NSPoint(x: trunkX, y: bottom))
    lines.stroke()

    for center in [
        NSPoint(x: trunkX, y: top), NSPoint(x: trunkX, y: middle),
        NSPoint(x: trunkX, y: bottom), NSPoint(x: branchX, y: middle),
    ] {
        let node = NSBezierPath(
            ovalIn: NSRect(
                x: center.x - radius, y: center.y - radius,
                width: radius * 2, height: radius * 2))
        node.fill()
    }
}

func render(size: Int) -> Data {
    guard
        let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
            bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .calibratedRGB, bytesPerRow: 0, bitsPerPixel: 0),
        let context = NSGraphicsContext(bitmapImageRep: rep)
    else {
        fail("could not allocate a \(size)×\(size) bitmap")
    }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.scaleBy(x: Double(size) / canvas, y: Double(size) / canvas)
    context.cgContext.setShouldAntialias(true)

    let bounds = NSRect(x: inset, y: inset, width: side, height: side)
    let shape = squircle(in: bounds)
    NSGradient(starting: gradientTop, ending: gradientBottom)?.draw(in: shape, angle: -90)
    drawCommitGraph(in: bounds.insetBy(dx: side * 0.16, dy: side * 0.16))

    NSGraphicsContext.restoreGraphicsState()
    guard let png = rep.representation(using: .png, properties: [:]) else {
        fail("could not encode the \(size)×\(size) image")
    }
    return png
}

let outputURL = URL(fileURLWithPath: output)
let outputDirectory = outputURL.deletingLastPathComponent()
do {
    try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)
} catch {
    fail("could not create \(outputDirectory.path): \(error)")
}

let iconset = URL(fileURLWithPath: NSTemporaryDirectory())
    .appendingPathComponent("rc-git-icon-\(getpid()).iconset")
try? FileManager.default.removeItem(at: iconset)
do {
    try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
} catch {
    fail("could not create \(iconset.path): \(error)")
}

for base in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(base)x\(base).png" : "icon_\(base)x\(base)@2x.png"
        do {
            try render(size: base * scale).write(to: iconset.appendingPathComponent(name))
        } catch {
            fail("could not write \(name): \(error)")
        }
    }
}

let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["--convert", "icns", "--output", outputURL.path, iconset.path]
do {
    try iconutil.run()
} catch {
    fail("could not run iconutil: \(error)")
}
iconutil.waitUntilExit()
try? FileManager.default.removeItem(at: iconset)
guard iconutil.terminationStatus == 0 else {
    fail("iconutil failed with status \(iconutil.terminationStatus)")
}
print("wrote \(outputURL.path)")
