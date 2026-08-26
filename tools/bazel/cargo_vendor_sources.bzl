"""Expose crate_universe source closures as hermetic runfiles."""

load("@rules_rust//rust:defs.bzl", "rust_common")


def _cargo_vendor_sources_impl(ctx):
    source_sets = []
    crate_infos = []

    targets = list(ctx.attr.crates) + list(ctx.attr.platform_crates)
    for target in targets:
        crate_infos.append(target[rust_common.crate_info])
        crate_infos.extend(target[rust_common.dep_info].transitive_crates.to_list())

    seen = {}
    for _unused in range(100000):
        if not crate_infos:
            break
        crate_info = crate_infos.pop()
        owner = str(crate_info.owner)
        if owner in seen:
            continue
        seen[owner] = True
        source_sets.extend([crate_info.srcs, crate_info.compile_data])
        for dependency in crate_info.deps.to_list() + crate_info.proc_macro_deps.to_list():
            if dependency.crate_info:
                crate_infos.append(dependency.crate_info)
            if dependency.dep_info:
                crate_infos.extend(dependency.dep_info.transitive_crates.to_list())

    sources = depset(transitive = source_sets)
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
            mandatory = True,
            providers = [[rust_common.crate_info, rust_common.dep_info]],
        ),
        "platform_crates": attr.label_list(
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
