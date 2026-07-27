"""Declared files for the pinned Linux mold linker."""

def _mold_toolchain_files_impl(ctx):
    output = ctx.actions.declare_directory(ctx.label.name)
    ctx.actions.run_shell(
        inputs = [ctx.file.mold],
        outputs = [output],
        command = """
set -eu
mkdir -p "{output}"
cp "{mold}" "{output}/ld.mold"
chmod 0755 "{output}/ld.mold"
""".format(
            mold = ctx.file.mold.path,
            output = output.path,
        ),
        mnemonic = "CtxMoldToolchain",
        progress_message = "Materializing pinned mold linker %{label}",
    )
    return [DefaultInfo(files = depset([output]))]

mold_toolchain_files = rule(
    implementation = _mold_toolchain_files_impl,
    attrs = {
        "mold": attr.label(
            allow_single_file = True,
            mandatory = True,
        ),
    },
)
