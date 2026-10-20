#!/usr/bin/env swift
// P0 item 4: ImageIO HEIC write. Usage: swift heic.swift <in.png> <out.heic> [quality 0-1]
// UNTESTED on this Linux environment - run on your Mac.
import Foundation
import ImageIO
import UniformTypeIdentifiers

let a = CommandLine.arguments
guard a.count >= 3, let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: a[1]) as CFURL, nil),
      let img = CGImageSourceCreateImageAtIndex(src, 0, nil),
      let dst = CGImageDestinationCreateWithURL(URL(fileURLWithPath: a[2]) as CFURL, UTType.heic.identifier as CFString, 1, nil)
else { FileHandle.standardError.write(Data("usage / open failed\n".utf8)); exit(2) }
let q = a.count >= 4 ? Double(a[3]) ?? 0.8 : 0.8
CGImageDestinationAddImage(dst, img, [kCGImageDestinationLossyCompressionQuality: q] as CFDictionary)
print(CGImageDestinationFinalize(dst) ? "OK heic written (alpha: \(img.alphaInfo.rawValue))" : "NG finalize failed")
