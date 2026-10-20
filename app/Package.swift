// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "FlectForMac",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "FlectCore", targets: ["FlectCore"]),
        .executable(name: "Flect-for-Mac", targets: ["FlectApp"]),
    ],
    targets: [
        // Everything that can be tested without a window: engine client, protocol, planning, settings.
        .target(
            name: "FlectCore",
            resources: [.copy("Resources")]
        ),
        .executableTarget(
            name: "FlectApp",
            dependencies: ["FlectCore"]
        ),
        .testTarget(
            name: "FlectCoreTests",
            dependencies: ["FlectCore"]
        ),
    ]
)
