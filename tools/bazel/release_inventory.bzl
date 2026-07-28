"""Configured dependency and license inventories for target-exact releases."""

load("@rules_rust//rust:defs.bzl", "rust_common")

ConfiguredDependencyInventoryInfo = provider(
    fields = {
        "features": "Configured crate feature records in the release target closure.",
        "labels": "Configured labels in the release target closure.",
        "license_files": "Cargo manifests and notice/license files in the closure.",
    },
)

def _dependency_inventory_aspect_impl(target, ctx):
    transitive_features = []
    transitive_labels = []
    transitive_license_files = []
    if ctx.rule:
        for attr_name in ("deps", "proc_macro_deps"):
            dependencies = getattr(ctx.rule.attr, attr_name, [])
            if type(dependencies) != "list":
                continue
            for dependency in dependencies:
                if ConfiguredDependencyInventoryInfo in dependency:
                    transitive_features.append(
                        dependency[ConfiguredDependencyInventoryInfo].features,
                    )
                    transitive_labels.append(
                        dependency[ConfiguredDependencyInventoryInfo].labels,
                    )
                    transitive_license_files.append(
                        dependency[ConfiguredDependencyInventoryInfo].license_files,
                    )

    features = []
    crate_features = getattr(ctx.rule.attr, "crate_features", []) if ctx.rule else []
    if type(crate_features) == "list":
        features = [
            "{}\t{}".format(target.label, feature)
            for feature in crate_features
        ]

    license_files = []
    if rust_common.crate_info in target:
        crate_info = target[rust_common.crate_info]
        for source in depset(
            transitive = [crate_info.srcs, crate_info.compile_data],
        ).to_list():
            short_path = source.short_path
            if not short_path.startswith("../"):
                continue
            relative = short_path[3:]
            parts = relative.split("/")
            if len(parts) != 2:
                continue
            basename = parts[-1].lower()
            if (
                basename == "cargo.toml" or
                basename == "authors" or
                basename == "unlicense" or
                basename.startswith("license") or
                basename.startswith("licence") or
                basename.startswith("copying") or
                basename.startswith("notice")
            ):
                license_files.append(source)

    return [
        ConfiguredDependencyInventoryInfo(
            features = depset(features, transitive = transitive_features),
            labels = depset([str(target.label)], transitive = transitive_labels),
            license_files = depset(
                license_files,
                transitive = transitive_license_files,
            ),
        ),
    ]

_dependency_inventory_aspect = aspect(
    implementation = _dependency_inventory_aspect_impl,
    attr_aspects = [
        "deps",
        "proc_macro_deps",
    ],
)

def _configured_dependency_inventory_impl(ctx):
    labels = ctx.attr.target[ConfiguredDependencyInventoryInfo].labels.to_list()
    output = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        output = output,
        content = "\n".join(sorted(labels)) + "\n",
    )
    return [DefaultInfo(files = depset([output]))]

configured_dependency_inventory = rule(
    implementation = _configured_dependency_inventory_impl,
    attrs = {
        "target": attr.label(
            aspects = [_dependency_inventory_aspect],
            mandatory = True,
        ),
    },
)

def _configured_license_materials_impl(ctx):
    inventory = ctx.attr.target[ConfiguredDependencyInventoryInfo]
    external_files = inventory.license_files.to_list()
    lines = [
        "external\t{}".format(source.short_path[3:])
        for source in external_files
    ]
    lines.extend([
        "feature\t{}".format(feature)
        for feature in inventory.features.to_list()
    ])
    lines.extend([
        "main\t{}".format(source.short_path)
        for source in ctx.files.workspace_materials
    ])
    output = ctx.actions.declare_file(ctx.label.name + ".txt")
    ctx.actions.write(
        output = output,
        content = "\n".join(sorted(depset(lines).to_list())) + "\n",
    )
    material_files = depset(
        direct = ctx.files.workspace_materials,
        transitive = [inventory.license_files],
    )
    return [DefaultInfo(
        files = depset([output]),
        runfiles = ctx.runfiles(
            files = [output],
            transitive_files = material_files,
        ),
    )]

configured_license_materials = rule(
    implementation = _configured_license_materials_impl,
    attrs = {
        "target": attr.label(
            aspects = [_dependency_inventory_aspect],
            mandatory = True,
        ),
        "workspace_materials": attr.label_list(
            allow_files = True,
            mandatory = True,
        ),
    },
)
