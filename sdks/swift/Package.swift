// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "CtxAgentHistory",
    platforms: [
        .macOS(.v12)
    ],
    products: [
        .library(
            name: "CtxAgentHistory",
            targets: ["CtxAgentHistory"]
        ),
        .executable(
            name: "LocalAgentHistorySmoke",
            targets: ["LocalAgentHistorySmoke"]
        )
    ],
    targets: [
        .target(
            name: "CtxAgentHistory"
        ),
        .executableTarget(
            name: "LocalAgentHistorySmoke",
            dependencies: ["CtxAgentHistory"],
            path: "Examples/LocalAgentHistorySmoke",
            exclude: ["README.md"]
        ),
        .testTarget(
            name: "CtxAgentHistoryTests",
            dependencies: ["CtxAgentHistory"]
        )
    ]
)
