load("//tools/bazel:playwright_browser_runtime_lock.generated.bzl", "PLAYWRIGHT_BROWSER_RUNTIME_LOCK")

def _manifest_json(entries_by_platform):
    lines = [
        "{",
        "  \"platforms\": {",
    ]
    platform_names = sorted(entries_by_platform.keys())
    for platform_index, host_platform in enumerate(platform_names):
        platform_suffix = "," if platform_index + 1 < len(platform_names) else ""
        lines.append("    \"%s\": {" % host_platform)
        browser_entries = entries_by_platform[host_platform]
        browser_names = sorted(browser_entries.keys())
        for browser_index, browser_name in enumerate(browser_names):
            browser_suffix = "," if browser_index + 1 < len(browser_names) else ""
            entry = browser_entries[browser_name]
            lines.append("      \"%s\": {" % browser_name)
            lines.append(
                "        \"path\": \"runtime_trees/%s/%s\","
                % (host_platform, entry["directory_name"]),
            )
            lines.append("        \"directory\": \"%s\"" % entry["directory_name"])
            lines.append("      }%s" % browser_suffix)
        lines.append("    }%s" % platform_suffix)
    lines.extend([
        "  }",
        "}",
        "",
    ])
    return "\n".join(lines)

def _repository_impl(repository_ctx):
    platforms = repository_ctx.attr.platforms
    if not platforms:
        fail("playwright_browser_runtime_repository requires at least one locked platform")

    entries_by_platform = {}
    for host_platform in platforms:
        entries = PLAYWRIGHT_BROWSER_RUNTIME_LOCK.get(host_platform)
        if entries == None:
            fail(
                "missing locked Playwright browser runtime for platform %s; regenerate tools/bazel/playwright_browser_runtime_lock.generated.bzl" % host_platform,
            )
        entries_by_platform[host_platform] = entries
        for _browser_name, entry in entries.items():
            repository_ctx.download_and_extract(
                output = "runtime_trees/%s/%s" % (host_platform, entry["directory_name"]),
                sha256 = entry["sha256"],
                url = entry["url"],
            )

    repository_ctx.file(
        "runtime_manifest.json",
        _manifest_json(entries_by_platform),
    )
    repository_ctx.file(
        "BUILD.bazel",
        """package(default_visibility = ["//visibility:public"])

exports_files(["runtime_manifest.json"])

filegroup(
    name = "runtime_trees",
    srcs = glob(["runtime_trees/**/*"]),
)
""",
    )

playwright_browser_runtime_repository = repository_rule(
    attrs = {
        "platforms": attr.string_list(),
    },
    implementation = _repository_impl,
)
