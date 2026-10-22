import AppKit
import ImageIO
import PDFKit
import UniformTypeIdentifiers
import Vision
import CoreImage
import FlectCore

/// Results of work that only macOS frameworks can do (HEIC, PDFKit, Vision).
struct NativeReport {
    var lines: [(LogLine.Kind, String)] = []
    var ocrText: String?
    /// Files this work created (exact paths, for the results window and Undo).
    var outputs: [String] = []
    var problems = false
    mutating func add(_ kind: LogLine.Kind, _ text: String) {
        lines.append((kind, text))
        if kind != .info { problems = true }
    }
}

enum NativeLimits {
    static let maxInputBytes: Int64 = 1 << 30
    static let maxPixels = 200_000_000
    static let maxPdfPages = 2000
    /// "One file per page" refuses a longer document rather than writing hundreds of files.
    static let maxSplitFiles = 500
    static let maxRenderPixels = 100_000_000
}

enum NativeWorker {
    static func fileSize(_ url: URL) -> Int64 {
        ((try? FileManager.default.attributesOfItem(atPath: url.path))?[.size] as? NSNumber)?.int64Value ?? 0
    }

    static func isRegularFile(_ url: URL) -> Bool {
        var st = stat()
        return lstat(url.path, &st) == 0 && (st.st_mode & S_IFMT) == S_IFREG
    }

    private static func source(_ url: URL, loc: Loc, report: inout NativeReport) -> CGImageSource? {
        guard isRegularFile(url) else { report.add(.error, loc.f("msg.notAFile", [url.lastPathComponent])); return nil }
        guard fileSize(url) <= NativeLimits.maxInputBytes else { report.add(.error, loc.f("msg.fileTooLarge", [url.lastPathComponent])); return nil }
        let opts: [CFString: Any] = [kCGImageSourceShouldCache: false]
        guard let src = CGImageSourceCreateWithURL(url as CFURL, opts as CFDictionary), CGImageSourceGetCount(src) > 0 else {
            report.add(.error, loc.f("msg.imageDecodeFailed", [url.lastPathComponent])); return nil
        }
        if let props = CGImageSourceCopyPropertiesAtIndex(src, 0, nil) as? [CFString: Any],
           let w = (props[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue, let h = (props[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue,
           w * h > NativeLimits.maxPixels {
            report.add(.error, loc.f("msg.imageTooLarge", [url.lastPathComponent])); return nil
        }
        return src
    }

    // MARK: HEIC

    static func heic(url: URL, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let src = source(url, loc: loc, report: &r) else { return r }
        let out = NSMutableData()
        guard let dest = CGImageDestinationCreateWithData(out, UTType.heic.identifier as CFString, 1, nil) else {
            r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); return r
        }
        let opts: [CFString: Any] = [kCGImageDestinationLossyCompressionQuality: 0.9]
        CGImageDestinationAddImageFromSource(dest, src, 0, opts as CFDictionary)
        guard CGImageDestinationFinalize(dest) else { r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); return r }
        do { let o = try OutputPath.writeNew(out as Data, source: url, ext: "heic", folder: folder); r.outputs.append(o.path); r.add(.info, o.lastPathComponent) }
        catch { r.add(.error, loc.f(OutputPath.errorKey(error), [url.lastPathComponent])) }
        return r
    }

    // MARK: background removal (Vision)

    static func removeBackground(url: URL, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let src = source(url, loc: loc, report: &r), let image = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return r }
        let request = VNGenerateForegroundInstanceMaskRequest()
        let handler = VNImageRequestHandler(cgImage: image, options: [:])
        do {
            try handler.perform([request])
            guard let observation = request.results?.first, !observation.allInstances.isEmpty else {
                r.add(.error, loc.f("msg.noSubject", [url.lastPathComponent])); return r
            }
            let buffer = try observation.generateMaskedImage(ofInstances: observation.allInstances, from: handler, croppedToInstancesExtent: false)
            let ci = CIImage(cvPixelBuffer: buffer)
            guard let cg = CIContext().createCGImage(ci, from: ci.extent) else {
                r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); return r
            }
            let data = NSMutableData()
            guard let dest = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else {
                r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); return r
            }
            CGImageDestinationAddImage(dest, cg, nil)
            guard CGImageDestinationFinalize(dest) else { r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); return r }
            let o = try OutputPath.writeNew(data as Data, source: url, suffix: "_nobg", ext: "png", folder: folder)
            r.outputs.append(o.path)
            r.add(.info, o.lastPathComponent)
        } catch {
            r.add(.error, loc.f("msg.imageDecodeFailed", [url.lastPathComponent]))
        }
        return r
    }

    // MARK: PDF

    static func openPdf(_ url: URL, loc: Loc, report: inout NativeReport) -> PDFDocument? {
        guard isRegularFile(url), fileSize(url) <= NativeLimits.maxInputBytes else { report.add(.error, loc.f("msg.fileTooLarge", [url.lastPathComponent])); return nil }
        guard let doc = PDFDocument(url: url) else { report.add(.error, loc.f("msg.pdfInvalid", [url.lastPathComponent])); return nil }
        if doc.isLocked { report.add(.error, loc.f("msg.pdfEncrypted", [url.lastPathComponent])); return nil }
        if doc.pageCount == 0 { report.add(.error, loc.f("msg.noPages", [url.lastPathComponent])); return nil }
        if doc.pageCount > NativeLimits.maxPdfPages { report.add(.error, loc.f("msg.fileTooLarge", [url.lastPathComponent])); return nil }
        return doc
    }

    static func pdfRender(url: URL, jpg: Bool, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
        let scale = 150.0 / 72.0
        for i in 0..<doc.pageCount {
            guard let page = doc.page(at: i) else { continue }
            let box = page.bounds(for: .mediaBox)
            let w = Int((box.width * scale).rounded(.up)), h = Int((box.height * scale).rounded(.up))
            guard w > 0, h > 0, w * h <= NativeLimits.maxRenderPixels else { r.add(.error, loc.f("msg.imageTooLarge", [url.lastPathComponent])); continue }
            guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: 0,
                                      space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { continue }
            ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
            ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
            ctx.scaleBy(x: scale, y: scale)
            ctx.translateBy(x: -box.origin.x, y: -box.origin.y)
            page.draw(with: .mediaBox, to: ctx)
            guard let image = ctx.makeImage() else { continue }
            let data = NSMutableData()
            let type = jpg ? UTType.jpeg : UTType.png
            guard let dest = CGImageDestinationCreateWithData(data, type.identifier as CFString, 1, nil) else { continue }
            CGImageDestinationAddImage(dest, image, jpg ? [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary : nil)
            guard CGImageDestinationFinalize(dest) else { r.add(.error, loc.f("msg.imageEncodeFailed", [url.lastPathComponent])); continue }
            do { r.outputs.append(try OutputPath.writeNew(data as Data, source: url, suffix: "_p\(i + 1)", ext: jpg ? "jpg" : "png", folder: folder).path) }
            catch { r.add(.error, loc.f(OutputPath.errorKey(error), [url.lastPathComponent])) }
        }
        return r
    }

    static func pdfText(url: URL, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
        let text = (doc.string ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { r.add(.warn, loc.f("msg.noTextFound", [url.lastPathComponent])); return r }
        do { r.outputs.append(try OutputPath.writeNew(Data(text.utf8), source: url, ext: "txt", folder: folder).path) }
        catch { r.add(.error, loc.f(OutputPath.errorKey(error), [url.lastPathComponent])) }
        return r
    }

    /// Merge in the given order; the result goes next to the first file.
    static func pdfMerge(urls: [URL], folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard urls.count >= 2 else { r.add(.error, loc.t("msg.pdfNeedTwo")); return r }
        let merged = PDFDocument()
        for url in urls {
            guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
            guard merged.pageCount + doc.pageCount <= NativeLimits.maxPdfPages else {
                r.add(.error, loc.f("msg.fileTooLarge", [url.lastPathComponent])); return r
            }
            for i in 0..<doc.pageCount {
                if let page = doc.page(at: i) { merged.insert(page, at: merged.pageCount) }
            }
        }
        guard let data = merged.dataRepresentation() else { r.add(.error, loc.t("msg.ioError")); return r }
        do { let o = try OutputPath.writeNew(data, source: urls[0], suffix: "_merged", ext: "pdf", folder: folder); r.outputs.append(o.path); r.add(.info, o.lastPathComponent) }
        catch { r.add(.error, loc.t(OutputPath.errorKey(error))) }
        return r
    }

    // MARK: PDF split / rotate / delete pages
    // Each of these writes new files next to the original (or in the PDF output folder); the original is never changed.

    /// A copy that belongs to no document, so it can go into a new one and the source stays intact.
    private static func freePage(_ page: PDFPage) -> PDFPage { (page.copy() as? PDFPage) ?? page }

    private static func writePdf(_ doc: PDFDocument, source: URL, suffix: String, folder: String, loc: Loc, report r: inout NativeReport) {
        guard let data = doc.dataRepresentation() else { r.add(.error, loc.t("msg.ioError")); return }
        do { let o = try OutputPath.writeNew(data, source: source, suffix: suffix, ext: "pdf", folder: folder); r.outputs.append(o.path); r.add(.info, o.lastPathComponent) }
        catch { r.add(.error, loc.t(OutputPath.errorKey(error))) }
    }

    /// A new document with the pages at `indexes` (zero-based, in the order given).
    private static func subset(of doc: PDFDocument, _ indexes: [Int], turn degrees: Int = 0) -> PDFDocument {
        let out = PDFDocument()
        for i in indexes {
            guard let page = doc.page(at: i) else { continue }
            let copy = freePage(page)
            if degrees != 0 { copy.rotation = ((copy.rotation + degrees) % 360 + 360) % 360 }
            out.insert(copy, at: out.pageCount)
        }
        return out
    }

    static func pdfSplitEach(url: URL, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
        guard doc.pageCount <= NativeLimits.maxSplitFiles else {
            r.add(.error, loc.f("msg.pdfTooManyFiles", [url.lastPathComponent, String(NativeLimits.maxSplitFiles)])); return r
        }
        let width = String(doc.pageCount).count
        for i in 0..<doc.pageCount {
            let number = String(i + 1)
            let padded = String(repeating: "0", count: max(0, width - number.count)) + number
            writePdf(subset(of: doc, [i]), source: url, suffix: "_p" + padded, folder: folder, loc: loc, report: &r)
        }
        return r
    }

    static func pdfRotate(url: URL, degrees: Int, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard [90, 180, 270].contains(degrees), let doc = openPdf(url, loc: loc, report: &r) else { return r }
        writePdf(subset(of: doc, Array(0..<doc.pageCount), turn: degrees), source: url, suffix: "_rot\(degrees)", folder: folder, loc: loc, report: &r)
        return r
    }

    static func pdfExtract(url: URL, spec: String, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
        switch PageSpec.pages(spec, pageCount: doc.pageCount) {
        case .failure:
            r.add(.error, loc.f("msg.pdfBadPages", [url.lastPathComponent, String(doc.pageCount)]))
        case .success(let indexes):
            writePdf(subset(of: doc, indexes), source: url, suffix: "_pages", folder: folder, loc: loc, report: &r)
        }
        return r
    }

    static func pdfDelete(url: URL, spec: String, folder: String = "", loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let doc = openPdf(url, loc: loc, report: &r) else { return r }
        switch PageSpec.pages(spec, pageCount: doc.pageCount) {
        case .failure:
            r.add(.error, loc.f("msg.pdfBadPages", [url.lastPathComponent, String(doc.pageCount)]))
        case .success(let removed):
            let gone = Set(removed)
            let keep = (0..<doc.pageCount).filter { !gone.contains($0) }
            if keep.isEmpty { r.add(.error, loc.f("msg.pdfNoPagesLeft", [url.lastPathComponent])); return r }
            writePdf(subset(of: doc, keep), source: url, suffix: "_removed", folder: folder, loc: loc, report: &r)
        }
        return r
    }

    // MARK: OCR

    static func languages(setting: String) -> [String] {
        let s = setting.trimmingCharacters(in: .whitespaces)
        if s.isEmpty { return ["ja-JP", "en-US", "zh-Hans"] }
        return s.split(separator: ",").map { String($0).trimmingCharacters(in: .whitespaces) }
    }

    static func ocr(url: URL, languageSetting: String, loc: Loc) -> NativeReport {
        var r = NativeReport()
        guard let src = source(url, loc: loc, report: &r), let image = CGImageSourceCreateImageAtIndex(src, 0, nil) else { return r }
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = true
        let supported = (try? request.supportedRecognitionLanguages()) ?? []
        let wanted = languages(setting: languageSetting)
        let usable = wanted.filter { w in supported.contains { $0.lowercased().hasPrefix(w.lowercased().prefix(2)) } }
        if usable.isEmpty { r.add(.error, loc.f("msg.ocrNoLanguage", [wanted.joined(separator: ", ")])); return r }
        request.recognitionLanguages = usable
        do { try VNImageRequestHandler(cgImage: image, options: [:]).perform([request]) }
        catch { r.add(.error, loc.f("msg.processing", [url.lastPathComponent])); return r }
        let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
        let text = lines.joined(separator: "\n")
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { r.add(.warn, loc.f("msg.noTextFound", [url.lastPathComponent])) }
        else { r.ocrText = text }
        return r
    }
}

extension AppModel {
    func runNative(_ task: NativeTask) {
        if case .pdfPreview = task {
            if let first = paths.first { screen = .pdfPreview(URL(fileURLWithPath: first)) } else { screen = .message(loc.t("app.noTask")) }
            return
        }
        resetProgress()
        screen = .running
        tracker.begin(task: Self.nativeName(task), inputs: paths)
        let urls = paths.map { URL(fileURLWithPath: $0) }
        let loc = self.loc
        let pdfFolder = settings.outputFolder("Pdf"), imageFolder = settings.outputFolder("Image"), ocrFolder = settings.outputFolder("Ocr")
        if case .pdfMerge = task {
            status = loc.t("app.running"); total = 1
            Task {
                let report = await Task.detached(priority: .userInitiated) { NativeWorker.pdfMerge(urls: urls, folder: pdfFolder, loc: loc) }.value
                for (k, t) in report.lines { log.append(LogLine(kind: k, text: t)) }
                tracker.add(paths: report.outputs)
                done = 1
                finishRun(outcome: report.problems ? .failed : .succeeded, summary: nil, sawProblem: report.problems)
            }
            return
        }
        let ocrLang = settings.string("Ocr", "Language")
        let ocrOutput = settings.string("Ocr", "Output", default: "TextFile")
        Task {
            var problems = false
            var combined: [String] = []
            var fallbackPaths: [String] = []
            for (i, url) in urls.enumerated() {
                status = loc.f("msg.processing", [url.lastPathComponent])
                total = urls.count; done = i
                let report: NativeReport = await Task.detached(priority: .userInitiated) {
                    switch task {
                    case .heicConvert: return NativeWorker.heic(url: url, folder: imageFolder, loc: loc)
                    case .pdfRender(let f): return NativeWorker.pdfRender(url: url, jpg: f == "JPG", folder: pdfFolder, loc: loc)
                    case .pdfText: return NativeWorker.pdfText(url: url, folder: pdfFolder, loc: loc)
                    case .pdfSplitEach: return NativeWorker.pdfSplitEach(url: url, folder: pdfFolder, loc: loc)
                    case .pdfExtract(let spec): return NativeWorker.pdfExtract(url: url, spec: spec, folder: pdfFolder, loc: loc)
                    case .pdfRotate(let degrees): return NativeWorker.pdfRotate(url: url, degrees: degrees, folder: pdfFolder, loc: loc)
                    case .pdfDelete(let spec): return NativeWorker.pdfDelete(url: url, spec: spec, folder: pdfFolder, loc: loc)
                    case .ocr: return NativeWorker.ocr(url: url, languageSetting: ocrLang, loc: loc)
                    case .removeBackground: return NativeWorker.removeBackground(url: url, folder: imageFolder, loc: loc)
                    case .pdfPreview, .pdfMerge: return NativeReport()
                    }
                }.value
                tracker.add(paths: report.outputs)
                if case .removeBackground(let fallback) = task, fallback, report.problems {
                    // The AI step failed for this file: the engine's colour key takes over for it.
                    fallbackPaths.append(url.path)
                    continue
                }
                for (k, t) in report.lines { log.append(LogLine(kind: k, text: t)) }
                problems = problems || report.problems
                if let text = report.ocrText {
                    switch ocrOutput {
                    case "Clipboard":
                        NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string)
                        log.append(LogLine(kind: .info, text: loc.t("msg.ocrCopied")))
                    case "ShowOnly": combined.append(text)
                    default:
                        do { tracker.add(paths: [try OutputPath.writeNew(Data(("\u{FEFF}" + text).utf8), source: url, ext: "txt", folder: ocrFolder).path]) }
                        catch { log.append(LogLine(kind: .error, text: loc.f(OutputPath.errorKey(error), [url.lastPathComponent]))); problems = true }
                    }
                }
            }
            done = urls.count
            if !fallbackPaths.isEmpty && !problems {
                tracker.carryOver = true // keep what the AI step already created for the other files
                runEngine("image.chromakey", paths: fallbackPaths)
                tracker.carryOver = false // begin() has used it by now; never let it leak into an unrelated run
                return
            }
            if !combined.isEmpty { screen = .tree(combined.joined(separator: "\n\n")); return }
            finishRun(outcome: problems ? .failed : .succeeded, summary: nil, sawProblem: problems)
        }
    }
}
