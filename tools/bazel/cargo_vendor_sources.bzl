"""Expose crate_universe source closures as hermetic runfiles."""

load("@rules_rust//rust:defs.bzl", "rust_common")


_CargoVendorSourcesInfo = provider(fields = {
    "sources": "depset[File]: Rust source and compile data dependency closure.",
})


def _cargo_vendor_sources_aspect_impl(target, ctx):
    source_sets = []
    if rust_common.crate_info in target:
        crate_info = target[rust_common.crate_info]
        source_sets.extend([crate_info.srcs, crate_info.compile_data])

    dependencies = getattr(ctx.rule.attr, "deps", []) + getattr(ctx.rule.attr, "proc_macro_deps", [])
    crate = getattr(ctx.rule.attr, "crate", None)
    if crate:
        dependencies.append(crate)
    for dependency in dependencies:
        if _CargoVendorSourcesInfo in dependency:
            source_sets.append(dependency[_CargoVendorSourcesInfo].sources)

    return [_CargoVendorSourcesInfo(sources = depset(transitive = source_sets))]


# DepInfo.transitive_crates omits proc-macro descendants. Let Bazel traverse
# those edges too, without a capped worklist that can silently drop sources.
_cargo_vendor_sources_aspect = aspect(
    implementation = _cargo_vendor_sources_aspect_impl,
    attr_aspects = ["deps", "proc_macro_deps", "crate"],
)


def _cargo_vendor_sources_impl(ctx):
    targets = list(ctx.attr.crates) + list(ctx.attr.platform_crates)
    sources = depset(transitive = [target[_CargoVendorSourcesInfo].sources for target in targets])
    cargo_manifests = []
    for source in sources.to_list():
        short_path = source.short_path
        if short_path.startswith("../") and short_path.endswith("/Cargo.toml"):
            runfile_path = short_path[3:]
            if runfile_path.count("/") == 1:
                cargo_manifests.append(runfile_path)

    manifest = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        output = manifest,
        content = "\n".join(sorted(depset(cargo_manifests).to_list())) + "\n",
    )

    return [DefaultInfo(
        files = depset([manifest]),
        runfiles = ctx.runfiles(files = [manifest], transitive_files = sources),
    )]


cargo_vendor_sources = rule(
    implementation = _cargo_vendor_sources_impl,
    attrs = {
        "crates": attr.label_list(
            aspects = [_cargo_vendor_sources_aspect],
            mandatory = True,
            providers = [[rust_common.crate_info, rust_common.dep_info]],
        ),
        "platform_crates": attr.label_list(
            aspects = [_cargo_vendor_sources_aspect],
            providers = [[rust_common.crate_info, rust_common.dep_info]],
        ),
    },
)


def _rust_toolchain_file_impl(ctx):
    toolchain = ctx.toolchains[str(Label("@rules_rust//rust:toolchain_type"))]
    if ctx.attr.tool == "cargo":
        executable = toolchain.cargo
        runfiles = ctx.runfiles(
            files = [toolchain.cargo, toolchain.rustc],
            transitive_files = toolchain.rustc_lib,
        )
    elif ctx.attr.tool == "rustc":
        executable = toolchain.rustc
        runfiles = ctx.runfiles(
            files = [toolchain.rustc],
            transitive_files = toolchain.rustc_lib,
        )
    else:
        executable = toolchain.rust_objcopy
        if executable == None:
            fail("configured Rust toolchain does not declare rust-objcopy")
        runfiles = ctx.runfiles(
            files = [executable, toolchain.rustc],
            transitive_files = toolchain.rustc_lib,
        )

    return [DefaultInfo(files = depset([executable]), runfiles = runfiles)]


rust_toolchain_file = rule(
    implementation = _rust_toolchain_file_impl,
    attrs = {
        "tool": attr.string(
            mandatory = True,
            values = [
                "cargo",
                "rust-objcopy",
                "rustc",
            ],
        ),
    },
    toolchains = [str(Label("@rules_rust//rust:toolchain_type"))],
)
