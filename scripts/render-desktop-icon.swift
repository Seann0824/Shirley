// Render the desktop icon in the macOS icon style: the source artwork masked
// into Apple's continuous-corner rounded square, placed on the standard macOS
// icon grid so the Dock shows rounded corners and the same optical size as the
// system icons next to it.
//
// Usage: swift render-desktop-icon.swift input.png output.png [zoom]
//
// `zoom` (default 1) scales the artwork inside the mask, cropping the edges.
// The mask itself always stays on the grid, so the rounded square keeps its
// standard footprint while the subject fills more of it — useful when the
// source has generous margins and the icon would otherwise read as lighter and
// smaller than its neighbours.
import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

// Apple's macOS app icon grid at 1024 pt: an 824 pt body inset 100 pt on every
// side, corner radius 185.4 pt, continuous corners with 0.6 smoothing.
let canvasSize = 1024.0
let bodySize = 824.0
let cornerRadius = 185.4
let cornerSmoothing = 0.6

func toRadians(_ degrees: Double) -> Double { degrees * .pi / 180 }

/// Continuous-corner path parameters for a single corner, in a local frame
/// whose origin is the outer corner point, x running along the incoming edge
/// and y along the outgoing edge.
struct Corner {
  let a, b, c, d, p, arcSectionLength, radius: Double
}

func cornerParameters(radius: Double, smoothing: Double) -> Corner {
  let r = min(radius, bodySize / 2)
  let p = (1 + smoothing) * r
  let arcMeasure = 90 * (1 - smoothing)
  let arcSectionLength = sin(toRadians(arcMeasure / 2)) * r * sqrt(2)
  let p3ToP4Distance = r * tan(toRadians((90 - arcMeasure) / 2 / 2))
  let c = p3ToP4Distance * cos(toRadians(45 * smoothing))
  let d = c * tan(toRadians(45 * smoothing))
  let b = (p - arcSectionLength - c - d) / 3
  return Corner(a: 2 * b, b: b, c: c, d: d, p: p,
                arcSectionLength: arcSectionLength, radius: r)
}

/// Append one corner: two cubic blends either side of the circular arc section.
func appendCorner(to path: CGMutablePath, corner: Corner, transform: CGAffineTransform, arcMeasure: Double) {
  func place(_ x: Double, _ y: Double) -> CGPoint {
    CGPoint(x: x, y: y).applying(transform)
  }

  let arcStart = CGPoint(x: -corner.p + corner.a + corner.b + corner.c, y: corner.d)
  path.addCurve(to: place(arcStart.x, arcStart.y),
                control1: place(-corner.p + corner.a, 0),
                control2: place(-corner.p + corner.a + corner.b, 0))

  let center = CGPoint(x: -corner.radius, y: corner.radius)
  let startAngle = atan2(arcStart.y - center.y, arcStart.x - center.x)
  let steps = 64
  for step in 1...steps {
    let angle = startAngle + arcMeasure * Double(step) / Double(steps)
    path.addLine(to: place(center.x + corner.radius * cos(angle),
                           center.y + corner.radius * sin(angle)))
  }

  let afterArc = CGPoint(x: arcStart.x + corner.arcSectionLength,
                         y: arcStart.y + corner.arcSectionLength)
  path.addLine(to: place(afterArc.x, afterArc.y))
  path.addCurve(to: place(afterArc.x + corner.d, afterArc.y + corner.a + corner.b + corner.c),
                control1: place(afterArc.x + corner.d, afterArc.y + corner.c),
                control2: place(afterArc.x + corner.d, afterArc.y + corner.b + corner.c))
}

/// The four corners are rotations of one another, so build each in the same
/// local frame and place it with a transform (all four have determinant +1).
func squirclePath() -> CGPath {
  let corner = cornerParameters(radius: cornerRadius, smoothing: cornerSmoothing)
  let arcMeasure = toRadians(90 * (1 - cornerSmoothing))
  let w = bodySize
  let h = bodySize
  let transforms = [
    CGAffineTransform(a: 1, b: 0, c: 0, d: 1, tx: w, ty: 0),    // top right
    CGAffineTransform(a: 0, b: 1, c: -1, d: 0, tx: w, ty: h),   // bottom right
    CGAffineTransform(a: -1, b: 0, c: 0, d: -1, tx: 0, ty: h),  // bottom left
    CGAffineTransform(a: 0, b: -1, c: 1, d: 0, tx: 0, ty: 0),   // top left
  ]

  let path = CGMutablePath()
  for (index, transform) in transforms.enumerated() {
    let start = CGPoint(x: -corner.p, y: 0).applying(transform)
    if index == 0 {
      path.move(to: start)
    } else {
      path.addLine(to: start)
    }
    appendCorner(to: path, corner: corner, transform: transform, arcMeasure: arcMeasure)
  }
  path.closeSubpath()

  var placement = CGAffineTransform(translationX: (canvasSize - bodySize) / 2,
                                    y: (canvasSize - bodySize) / 2)
  guard let placed = path.copy(using: &placement) else { fatalError("Cannot place squircle path") }
  return placed
}

let arguments = CommandLine.arguments
guard arguments.count >= 3 else {
  fatalError("Usage: swift render-desktop-icon.swift input.png output.png [zoom]")
}
let zoom = arguments.count > 3 ? (Double(arguments[3]) ?? 1) : 1
guard zoom > 0 else { fatalError("zoom must be greater than 0") }

guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: arguments[1]) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
      let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
      let context = CGContext(data: nil, width: Int(canvasSize), height: Int(canvasSize),
                              bitsPerComponent: 8, bytesPerRow: 0, space: colorSpace,
                              bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
else { fatalError("Cannot read source artwork or create the drawing context") }

let inset = (canvasSize - bodySize) / 2
let body = CGRect(x: inset, y: inset, width: bodySize, height: bodySize)
// Zooming grows the drawn artwork symmetrically about the body centre; the clip
// below still uses the unzoomed squircle, so the icon's footprint never changes.
let artwork = body.insetBy(dx: body.width * (1 - zoom) / 2, dy: body.height * (1 - zoom) / 2)

context.setShouldAntialias(true)
context.interpolationQuality = .high
context.addPath(squirclePath())
context.clip()
context.draw(image, in: artwork)

guard let output = context.makeImage(),
      let destination = CGImageDestinationCreateWithURL(
        URL(fileURLWithPath: arguments[2]) as CFURL, UTType.png.identifier as CFString, 1, nil)
else { fatalError("Cannot encode RGBA PNG") }
CGImageDestinationAddImage(destination, output, nil)
guard CGImageDestinationFinalize(destination) else { fatalError("PNG export failed") }
