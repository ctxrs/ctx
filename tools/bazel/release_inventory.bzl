"""Configured dependency inventories for target-exact release SBOMs."""

ConfiguredDependencyInventoryInfo = provider(
    fields = {"labels": "Configured labels in the release target closure."},
)

def _dependency_inventory_aspect_impl(target, ctx):
    transitive = []
    if ctx.rule:
        for attr_name in ("deps", "proc_macro_deps"):
            dependencies = getattr(ctx.rule.attr, attr_name, [])
            if type(dependencies) != "list":
                continue
            for dependency in dependencies:
                if ConfiguredDependencyInventoryInfo in dependency:
                    transitive.append(
                        dependency[ConfiguredDependencyInventoryInfo].labels,
                    )
    return [
        ConfiguredDependencyInventoryInfo(
            labels = depset([str(target.label)], transitive = transitive),
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
