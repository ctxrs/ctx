"""Graf's explicit, static Robot Framework adapter. JSON stdin/stdout only.

This module never runs suites, resolves variables, or loads user libraries.
The selected Python environment supplies Robot Framework; Graf does not install it.
"""
import io
import json
import re
import sys

MAX_SOURCE = 4 * 1024 * 1024
MAX_OUTPUT = 8 * 1024 * 1024
MAX_RECORDS = 20000
MAX_MATCHES = 100000


class LimitExceeded(Exception):
    pass


def response(version="", failure=None):
    return dict(schema_version=1, robot_version=version, failure=failure,
                languages=[], definitions=[], imports=[], calls=[], diagnostics=[])


def normalize(name):
    return "".join(c.lower() for c in name if not c.isspace() and c != "_")


def parse(request):
    if (set(request) != {"schema_version", "path", "source"}
            or request["schema_version"] != 1
            or not isinstance(request["source"], str)
            or not isinstance(request["path"], str)
            or not request["path"].endswith((".robot", ".resource"))):
        return response(failure="invalid_request")
    source = request["source"]
    if len(source.encode("utf-8")) > MAX_SOURCE:
        return response(failure="limit")
    try:
        from robot import __version__
        from robot.api.parsing import ModelVisitor, get_model, get_resource_model
        from robot.conf.languages import Language, Languages
        from robot.errors import DataError
        from robot.running.arguments.embedded import EmbeddedArguments
    except ImportError:
        return response(failure="missing_dependency")
    if not re.fullmatch(r"7\.5(?:\.\d+)?", __version__):
        return response(__version__, "unsupported_version")
    result = response(__version__)

    class BundledLanguages(Languages):
        def add_language(self, name):
            # The default method imports unknown language modules. Source text
            # may select bundled languages only, never Python paths or modules.
            try:
                language = Language.from_name(name)
            except ValueError:
                raise DataError("Only bundled Robot languages are supported") from None
            super().add_language(language)

    languages = BundledLanguages()
    get = get_resource_model if request["path"].endswith(".resource") else get_model
    model = get(io.StringIO(source), data_only=False, lang=languages)
    result["languages"] = [language.code for language in languages]
    rows = source.splitlines(keepends=True)
    offsets = []
    offset = 0
    for row in rows:
        offsets.append(offset)
        offset += len(row.encode("utf-8"))

    def position(line, column):
        if line <= 0 or line > len(rows) or column < 0:
            raise ValueError("invalid model location")
        original = rows[line - 1]
        bom = 1 if line == 1 and original.startswith("\ufeff") else 0
        normalized = original[bom:].replace("\r\n", "\n")
        if column > len(normalized):
            raise ValueError("invalid model column")
        extra_cr = 1 if original.endswith("\r\n") and column == len(normalized) else 0
        return offsets[line - 1] + len(original[:column + bom + extra_cr].encode("utf-8"))

    def span(node):
        return dict(start=position(node.lineno, node.col_offset),
                    end=position(node.end_lineno, node.end_col_offset))

    count = 0

    def add(kind, record):
        nonlocal count
        count += 1
        if count > MAX_RECORDS:
            raise LimitExceeded()
        for value in record.values():
            if isinstance(value, str) and len(value.encode("utf-8")) > 4096:
                raise LimitExceeded()
        result[kind].append(record)

    class Visitor(ModelVisitor):
        owner = None
        template = None
        suite_template = None
        depth = 0
        visited = 0

        def visit(self, node):
            self.depth += 1
            self.visited += 1
            if self.depth > 256 or self.visited > 100000:
                raise LimitExceeded()
            if (getattr(node, "errors", ())
                    or any(token.error for token in getattr(node, "tokens", ()))):
                add("diagnostics", dict(code="syntax", span=span(node)))
            super().visit(node)
            self.depth -= 1

        def definition(self, node, kind):
            index = len(result["definitions"])
            add("definitions", dict(id=index, kind=kind, name=node.name,
                                    span=span(node), embedded="${" in node.name))
            owner, template = self.owner, self.template
            self.owner = index
            self.template = self.suite_template if kind == "test" else None
            # A template setting applies to the whole case, including earlier rows.
            for statement in node.body:
                if type(statement).__name__ == "Template":
                    self.template = self.template_name(statement.value)
            self.generic_visit(node)
            self.owner, self.template = owner, template

        def visit_TestCase(self, node):
            self.definition(node, "test")

        def visit_Keyword(self, node):
            self.definition(node, "keyword")

        def import_(self, node, kind):
            if node.name:
                add("imports", dict(kind=kind, name=node.name,
                                    alias=getattr(node, "alias", None), span=span(node)))

        def visit_ResourceImport(self, node):
            self.import_(node, "resource")

        def visit_LibraryImport(self, node):
            self.import_(node, "library")

        def visit_VariablesImport(self, node):
            self.import_(node, "variables")

        def call(self, node, name):
            if not name:
                return
            alternatives = [name]
            prefix = languages.bdd_prefix_regexp.match(name)
            if prefix:
                alternatives.append(name[prefix.end():])
            add("calls", dict(owner=self.owner, name=name, span=span(node),
                              alternatives=alternatives, target=None, ambiguous=False))

        def visit_KeywordCall(self, node):
            self.call(node, node.keyword)

        @staticmethod
        def template_name(name):
            return name if name and name.upper() != "NONE" else None

        def visit_TestTemplate(self, node):
            self.suite_template = self.template_name(node.value)
            self.call(node, self.suite_template)

        def visit_Template(self, node):
            self.call(node, self.template_name(node.value))

        def visit_TemplateArguments(self, node):
            self.call(node, self.template)

        def visit_Fixture(self, node):
            self.call(node, self.template_name(node.name))

    visitor = Visitor()
    # Settings apply even when their section follows the test cases.
    for section in model.sections:
        for statement in getattr(section, "body", ()):
            if type(statement).__name__ == "TestTemplate":
                visitor.suite_template = visitor.template_name(statement.value)
    visitor.visit(model)
    if result["diagnostics"]:
        result["definitions"] = []
        result["imports"] = []
        result["calls"] = []
        return result

    exact = {}
    embedded = []
    for definition in result["definitions"]:
        if definition["kind"] != "keyword":
            continue
        if definition["embedded"]:
            try:
                matcher = EmbeddedArguments.from_name(definition["name"])
            except DataError:
                add("diagnostics", dict(code="embedded_syntax", span=definition["span"]))
                continue
            if matcher:
                embedded.append((definition["id"], matcher))
        else:
            exact.setdefault(normalize(definition["name"]), []).append(definition["id"])
    comparisons = 0
    namespaces = set()
    for imported in result["imports"]:
        if imported["kind"] in ("resource", "library"):
            name = imported["alias"] or imported["name"].replace("\\", "/").rsplit("/", 1)[-1]
            if name.endswith((".robot", ".resource", ".py")):
                name = name.rsplit(".", 1)[0]
            namespaces.add(normalize(name))
    local_namespace = normalize(request["path"].rsplit("/", 1)[-1].rsplit(".", 1)[0])
    for call in result["calls"]:
        # Dynamic keyword expressions and explicit external namespaces are not
        # local match candidates. Rust retains appropriate imported references.
        if any(marker in call["name"] for marker in ("${", "@{", "&{", "%{")):
            continue
        for name in call["alternatives"]:
            # Declared namespace prefixes take priority over embedded patterns.
            prefixes = [(normalize(name[:at]), name[at + 1:])
                        for at, char in enumerate(name) if char == "."]
            if any(prefix in namespaces for prefix, _ in prefixes):
                continue
            for prefix, bare in prefixes:
                if prefix == local_namespace:
                    name = bare
                    break
            matches = exact.get(normalize(name), [])
            if not matches:
                for index, matcher in embedded:
                    comparisons += 1
                    if comparisons > MAX_MATCHES:
                        raise LimitExceeded()
                    if matcher.name.fullmatch(name):
                        matches.append(index)
            if matches:
                call["ambiguous"] = len(matches) != 1
                call["target"] = matches[0] if len(matches) == 1 else None
                break
    if result["diagnostics"]:
        result["definitions"] = []
        result["imports"] = []
        result["calls"] = []
    return result


def main():
    try:
        # JSON escapes can expand the byte count of the original source sixfold.
        raw = sys.stdin.buffer.read(MAX_SOURCE * 6 + 65537)
        if len(raw) > MAX_SOURCE * 6 + 65536:
            raise LimitExceeded()
        result = parse(json.loads(raw))
    except (LimitExceeded, RecursionError):
        result = response(failure="limit")
    except Exception:
        # Never expose user expressions, local paths or third-party tracebacks.
        result = response(failure="adapter_error")
    encoded = json.dumps(result, ensure_ascii=True, separators=(",", ":")).encode("utf-8")
    if len(encoded) > MAX_OUTPUT:
        encoded = json.dumps(response(failure="limit")).encode("utf-8")
    sys.stdout.buffer.write(encoded)


if __name__ == "__main__":
    main()
