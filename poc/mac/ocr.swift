// P0 item 3: Apple Vision OCR. Usage: swift ocr.swift <image> [lang,lang]   (e.g. ja-JP,zh-Hans,en-US)
// UNTESTED on this Linux environment - run on your Mac.
import Foundation
import Vision
import AppKit

let args = CommandLine.arguments
guard args.count >= 2, let image = NSImage(contentsOfFile: args[1]),
      let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    FileHandle.standardError.write(Data("usage: swift ocr.swift <image> [langs]\n".utf8)); exit(2)
}
let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true
if args.count >= 3 { request.recognitionLanguages = args[2].split(separator: ",").map(String.init) }
print("supported:", (try? request.supportedRecognitionLanguages()) ?? [])
let t = Date()
try VNImageRequestHandler(cgImage: cg).perform([request])
for o in request.results ?? [] { if let c = o.topCandidates(1).first { print(String(format: "%.2f", c.confidence), c.string) } }
print(String(format: "elapsed %.0f ms", Date().timeIntervalSince(t) * 1000))
