// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "AudioTapHelper",
    platforms: [
        // Core Audio Process Taps (CATapDescription) require 14.2+; we
        // target 14.4+ in practice since earlier 14.x point releases have
        // flaky TCC permission categorization for this API.
        .macOS("14.4")
    ],
    targets: [
        .executableTarget(
            name: "AudioTapHelper",
            path: "Sources/AudioTapHelper"
        )
    ]
)
