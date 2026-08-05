"""Configured Rust action inventory for the crate source/dependency gate."""

load("@rules_rust//rust:defs.bzl", "rust_common")
load(":release_inventory.bzl", "PUBLIC_RELEASE_ROUTES")

RustCrateGateInfo = provider(
    fields = {
        "lines": "Canonical configured crate/source/direct-dependency records.",
        "sources": "Checked-in Rust source files represented by those actions.",
    },
)

def _main_workspace_label(label):
    value = str(label)
    return value.startswith("//") or value.startswith("@@//")

def _rust_crate_gate_aspect_impl(target, ctx):
    transitive_lines = []
    transitive_sources = []
    if ctx.rule:
        for attr_name in ("deps", "proc_macro_deps"):
            dependencies = getattr(ctx.rule.attr, attr_name, [])
            if type(dependencies) != "list":
                continue
            for dependency in dependencies:
                if RustCrateGateInfo in dependency:
                    transitive_lines.append(dependency[RustCrateGateInfo].lines)
                    transitive_sources.append(dependency[RustCrateGateInfo].sources)

    lines = []
    sources = []
    if rust_common.crate_info in target and _main_workspace_label(target.label):
        crate = target[rust_common.crate_info]
        lines.append("crate\t{}\t{}\t{}\t{}".format(
            target.label,
            crate.name,
            crate.type,
            crate.root.short_path,
        ))
        for source in crate.srcs.to_list():
            # Generated OUT_DIR and other generated action inputs are deliberately
            # absent. A checked-in generated .rs file is still a source File.
            if source.is_source and source.extension == "rs" and not source.short_path.startswith("../"):
                lines.append("source\t{}\t{}".format(target.label, source.short_path))
                sources.append(source)
        for dependency_set in (crate.deps, crate.proc_macro_deps):
            for dependency in dependency_set.to_list():
                dependency_crate = dependency.crate_info
                if dependency_crate and _main_workspace_label(dependency_crate.owner):
                    lines.append("dep\t{}\t{}".format(target.label, dependency_crate.owner))

    return [RustCrateGateInfo(
        lines = depset(lines, transitive = transitive_lines),
        sources = depset(sources, transitive = transitive_sources),
    )]

_rust_crate_gate_aspect = aspect(
    implementation = _rust_crate_gate_aspect_impl,
    attr_aspects = ["deps", "proc_macro_deps"],
)

def _configured_rust_crate_gate_impl(ctx):
    route = PUBLIC_RELEASE_ROUTES[ctx.attr.target_id]
    lines = ["platform\t{}\t{}\t{}".format(ctx.attr.target_id, route[1], route[0])]
    sources = []
    for root in ctx.attr.roots:
        info = root[RustCrateGateInfo]
        lines.extend(info.lines.to_list())
        sources.append(info.sources)
    output = ctx.actions.declare_file(ctx.label.name + ".tsv")
    ctx.actions.write(output, "\n".join(sorted(depset(lines).to_list())) + "\n")
    return [DefaultInfo(
        files = depset([output]),
        runfiles = ctx.runfiles(
            files = [output],
            transitive_files = depset(transitive = sources),
        ),
    )]

_configured_rust_crate_gate = rule(
    implementation = _configured_rust_crate_gate_impl,
    attrs = {
        "roots": attr.label_list(
            aspects = [_rust_crate_gate_aspect],
            mandatory = True,
        ),
        "target_id": attr.string(mandatory = True, values = sorted(PUBLIC_RELEASE_ROUTES.keys())),
    },
)

def rust_crate_gate(name, roots):
    """Joins the configured production graph to every supported release target.

    Public Rust toolchains are native-only, so a Linux sandbox cannot analyze a
    macOS/FreeBSD/Windows rustc action. The internal workspace source/dependency
    graph is intentionally platform-invariant; the target-inventory checker
    rejects select() in those production attrs. Cargo cfg/feature reachability
    is evaluated separately for each route by the gate itself.
    """
    labels = []
    for target_id in sorted(PUBLIC_RELEASE_ROUTES.keys()):
        target_name = "{}_{}".format(name, target_id.replace("-", "_"))
        _configured_rust_crate_gate(
            name = target_name,
            roots = roots,
            target_id = target_id,
        )
        labels.append(":" + target_name)
    native.filegroup(
        name = name,
        srcs = labels,
        visibility = ["//visibility:public"],
    )
    return labels
