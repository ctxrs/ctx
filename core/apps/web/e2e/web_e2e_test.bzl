load("@rules_shell//shell:sh_test.bzl", "sh_test")

def web_e2e_test(name, config, runtime_profile, suite = "", specs = None, timeout = "long", linux_browser = "", web_dist = ""):
    resolved_specs = specs or []
    args = [
        "--config",
        config,
        "--runtime-profile",
        runtime_profile,
        "--ctx-http-bin",
        "$(location //core/crates/ctx-http:ctx)",
    ]
    playwright_browser_args = select({
        "@platforms//os:linux": [
            "--playwright-runtime-manifest",
            "$(location @playwright_browser_runtime_ubuntu24_04_x64//:runtime_manifest.json)",
        ],
        "@platforms//os:macos": [
            "--playwright-runtime-manifest",
            "$(location @playwright_browser_runtime_mac15_arm64//:runtime_manifest.json)",
        ],
        "//conditions:default": [],
    })
    data = [
        "//core/apps/web:e2e_runtime_data",
        "//core/apps/web:node_modules",
        "//core/apps/web:node_modules/autoprefixer",
        "//core/apps/web:node_modules/postcss",
        "//core/apps/web:node_modules/tailwindcss",
        "//core/crates/ctx-http:ctx",
        "@rules_nodejs//nodejs:current_node_runtime",
    ]
    playwright_browser_data = select({
        "@platforms//os:linux": [
            "@playwright_browser_runtime_ubuntu24_04_x64//:runtime_manifest.json",
            "@playwright_browser_runtime_ubuntu24_04_x64//:runtime_trees",
        ],
        "@platforms//os:macos": [
            "@playwright_browser_runtime_mac15_arm64//:runtime_manifest.json",
            "@playwright_browser_runtime_mac15_arm64//:runtime_trees",
        ],
        "//conditions:default": [],
    })
    playwright_browser_env = {}
    if linux_browser:
        playwright_browser_env = select({
            "@platforms//os:linux": {"CTX_E2E_BROWSER": linux_browser},
            "//conditions:default": {},
        })
    if runtime_profile == "agent-full":
        args.extend([
            "--ctx-mcp-bin",
            "$(location //core/crates/ctx-mcp:ctx-mcp)",
        ])
        data.append("//core/crates/ctx-mcp:ctx-mcp")
    if runtime_profile == "web-artifact":
        if not web_dist:
            fail("web-artifact E2E tests must declare web_dist")
        args.extend([
            "--web-dist",
            "$(location %s)" % web_dist,
        ])
        data.append(web_dist)
    if suite:
        args.extend(["--suite", suite])
    for spec in resolved_specs:
        args.extend(["--spec", spec])
    sh_test(
        name = name,
        srcs = ["//core/apps/web:scripts/run-e2e-bazel-runtime.sh"],
        args = args + playwright_browser_args,
        data = data + playwright_browser_data,
        env = playwright_browser_env,
        tags = [
            "local",
            "no-remote",
        ],
        timeout = timeout,
    )
