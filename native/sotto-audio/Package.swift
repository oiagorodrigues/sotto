// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "sotto-audio",
    platforms: [.macOS("26.0")],
    products: [
        .executable(name: "sotto-audio", targets: ["sotto-audio"]),
        .library(name: "SottoProtocol", targets: ["SottoProtocol"]),
        .library(name: "SottoAudioCore", targets: ["SottoAudioCore"]),
    ],
    targets: [
        .target(name: "SottoProtocol"),
        .target(name: "SottoAudioCore", dependencies: ["SottoProtocol"]),
        .executableTarget(
            name: "sotto-audio",
            dependencies: ["SottoProtocol", "SottoAudioCore"],
            linkerSettings: [
                .unsafeFlags([
                    "-Xlinker", "-sectcreate", "-Xlinker", "__TEXT",
                    "-Xlinker", "__info_plist", "-Xlinker", "Support/Info.plist",
                ]),
            ]
        ),
        .testTarget(name: "SottoProtocolTests", dependencies: ["SottoProtocol"]),
        .testTarget(name: "SottoAudioCoreTests", dependencies: ["SottoAudioCore", "SottoProtocol"]),
        .testTarget(name: "IntegrationTests", dependencies: ["SottoProtocol"]),
    ]
)
