"""Generic public-boundary scanning for MCP attribution docs and checkers."""

from __future__ import annotations

import ast
import re
import shlex
import warnings
from base64 import b64decode
from binascii import Error as BinasciiError
from collections import deque
from collections.abc import Iterable


_NEUTRAL_HOME_ACCOUNTS = frozenset(
    "example example-user me runner sandbox test user".split()
)
_NON_REPOSITORY_PRIVATE_PREFIXES = frozenset(
    "owner process provider session user".split()
)
_WORD_TOKEN_RE = re.compile(
    r"(?<![A-Za-z0-9._-])[A-Za-z0-9.]+(?:[-_][A-Za-z0-9.]+)+(?![A-Za-z0-9._-])"
)
_PATH_TOKEN_RE = re.compile(
    r"(?:[A-Za-z]:)?[\\/]?[A-Za-z0-9._~+-]+(?:[\\/][A-Za-z0-9._~+-]+)+"
)
_REGEX_SINGLETON_RE = re.compile(r"\[([A-Za-z0-9._~+/=-])\]")
_SOURCE_ESCAPE_RE = re.compile(
    r"\\(?:x(?P<hex>[0-9A-Fa-f]{2})|u(?P<short>[0-9A-Fa-f]{4})|"
    r"U(?P<long>[0-9A-Fa-f]{8})|(?P<octal>[0-7]{1,3}))"
)
_BASE64_TOKEN_RE = re.compile(
    r"(?<![A-Za-z0-9+/_-])[A-Za-z0-9+/_-]{8,}={0,2}(?![A-Za-z0-9+/_=-])"
)
_SHELL_ASSIGNMENT_RE = re.compile(r"^\s*([A-Za-z_]\w*)\s*(\+?=)\s*(.*?)\s*$")
_SHELL_VARIABLE_RE = re.compile(
    r"\$(?:\{(?P<braced>[A-Za-z_]\w*)\}|(?P<plain>[A-Za-z_]\w*))"
)
_LOCAL_ROOT_RE = re.compile(r"(?:^|[^A-Za-z0-9])/(?:h[o]me|t[m]p)(?:/|$)", re.I)
_MAX_DECODED_CANDIDATES = 4096

StringValue = str | tuple[str, ...]


def _words(value: str) -> tuple[str, ...]:
    return tuple(part for part in re.split(r"[-_.]+", value.lower()) if part)


def _concrete_boundary_class(value: str) -> str | None:
    for token in _WORD_TOKEN_RE.findall(value):
        words = _words(token)
        for index, word in enumerate(words):
            if (
                word == "private"
                and index > 0
                and words[index - 1] not in _NON_REPOSITORY_PRIVATE_PREFIXES
                and all(suffix == "git" for suffix in words[index + 1 :])
            ):
                return "private repository name"
        if any(
            words[index : index + 3] == ("multi", "repo", "workspace")
            for index in range(len(words))
        ):
            return "internal multi-repository workspace name"

    for token in _PATH_TOKEN_RE.findall(value):
        normalized = token.replace("\\", "/")
        segments = tuple(part for part in normalized.split("/") if part)
        lowered = tuple(part.lower() for part in segments)
        offset = 1 if lowered and lowered[0].endswith(":") else 0
        is_absolute = normalized.startswith(("/", "\\")) or offset == 1
        if (
            is_absolute
            and len(lowered) > offset + 1
            and lowered[offset] in {"home", "users"}
            and lowered[offset + 1] not in _NEUTRAL_HOME_ACCOUNTS
        ):
            return "maintainer home path"

        segment_words = tuple(_words(part) for part in segments)
        if any(
            ("conformance" in words or "internal" in words)
            and any(
                "proof" in later and ({"packet", "packets"} & set(later))
                for later in segment_words[index + 1 :]
            )
            for index, words in enumerate(segment_words)
        ):
            return "internal conformance proof-packet path"
    return None


def _decode_source_escapes(value: str) -> str:
    def replace(match: re.Match[str]) -> str:
        encoded = next(group for group in match.groups() if group is not None)
        base = 8 if match.group("octal") is not None else 16
        try:
            return chr(int(encoded, base))
        except (ValueError, OverflowError):
            return match.group(0)

    return _SOURCE_ESCAPE_RE.sub(replace, value)


def _negative_one(node: ast.AST | None) -> bool:
    return (
        isinstance(node, ast.UnaryOp)
        and isinstance(node.op, ast.USub)
        and isinstance(node.operand, ast.Constant)
        and node.operand.value == 1
    )


def _string_expression(
    node: ast.AST, names: dict[str, StringValue]
) -> StringValue | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.Name):
        return names.get(node.id)
    if isinstance(node, (ast.List, ast.Tuple)):
        items = tuple(_string_expression(item, names) for item in node.elts)
        if all(isinstance(item, str) for item in items):
            return tuple(item for item in items if isinstance(item, str))
        return None
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        left = _string_expression(node.left, names)
        right = _string_expression(node.right, names)
        if isinstance(left, str) and isinstance(right, str):
            return left + right
        if isinstance(left, tuple) and isinstance(right, tuple):
            return left + right
        return None
    if isinstance(node, ast.JoinedStr):
        parts: list[str] = []
        for item in node.values:
            target = item.value if isinstance(item, ast.FormattedValue) else item
            candidate = _string_expression(target, names)
            if not isinstance(candidate, str):
                return None
            parts.append(candidate)
        return "".join(parts)
    if isinstance(node, ast.Subscript) and isinstance(node.slice, ast.Slice):
        value = _string_expression(node.value, names)
        reverse = (
            node.slice.lower is None
            and node.slice.upper is None
            and _negative_one(node.slice.step)
        )
        if reverse and isinstance(value, (str, tuple)):
            return value[::-1]
    if isinstance(node, ast.Call) and not node.keywords and len(node.args) == 1:
        argument = _string_expression(node.args[0], names)
        if isinstance(node.func, ast.Name) and node.func.id == "reversed":
            if isinstance(argument, (str, tuple)):
                return tuple(reversed(argument))
        if isinstance(node.func, ast.Attribute) and node.func.attr == "join":
            separator = _string_expression(node.func.value, names)
            if isinstance(separator, str) and isinstance(argument, tuple):
                return separator.join(argument)
    return None


def _statement_candidates(
    statements: list[ast.stmt], names: dict[str, StringValue]
) -> Iterable[str]:
    for statement in statements:
        value: StringValue | None = None
        targets: tuple[ast.AST, ...] = ()
        if isinstance(statement, ast.Assign):
            value = _string_expression(statement.value, names)
            targets = tuple(statement.targets)
        elif isinstance(statement, ast.AnnAssign) and statement.value is not None:
            value = _string_expression(statement.value, names)
            targets = (statement.target,)
        elif isinstance(statement, ast.AugAssign) and isinstance(statement.op, ast.Add):
            prior = _string_expression(statement.target, names)
            added = _string_expression(statement.value, names)
            if isinstance(prior, str) and isinstance(added, str):
                value = prior + added
                targets = (statement.target,)
        elif isinstance(statement, (ast.Expr, ast.Return)):
            expression = getattr(statement, "value", None)
            if expression is not None:
                value = _string_expression(expression, names)

        if value is not None:
            for target in targets:
                if isinstance(target, ast.Name):
                    names[target.id] = value
            if isinstance(value, str):
                yield value

        nested = []
        for attribute in ("body", "orelse", "finalbody"):
            child = getattr(statement, attribute, None)
            if isinstance(child, list):
                nested.append(child)
        handlers = getattr(statement, "handlers", ())
        nested.extend(handler.body for handler in handlers)
        for child in nested:
            yield from _statement_candidates(child, dict(names))


def _parse_python(value: str) -> ast.Module:
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", SyntaxWarning)
        return ast.parse(value)


def _python_literal_candidates(value: str) -> Iterable[str]:
    try:
        tree = _parse_python(value)
    except (SyntaxError, ValueError):
        pass
    else:
        yield from _statement_candidates(tree.body, {})

    line_names: dict[str, StringValue] = {}
    for line in value.splitlines():
        try:
            tree = _parse_python(line.strip())
        except (SyntaxError, ValueError):
            continue
        yield from _statement_candidates(tree.body, line_names)


def _expand_shell_variables(value: str, assignments: dict[str, str]) -> str:
    def replace(match: re.Match[str]) -> str:
        name = match.group("braced") or match.group("plain")
        return assignments.get(name, match.group(0))

    for _ in range(len(assignments) + 1):
        expanded = _SHELL_VARIABLE_RE.sub(replace, value)
        if expanded == value:
            return value
        value = expanded
    return value


def _shell_assignment_candidates(value: str) -> Iterable[str]:
    assignments: dict[str, str] = {}
    for line in value.splitlines():
        match = _SHELL_ASSIGNMENT_RE.fullmatch(line)
        if match is None:
            continue
        try:
            parts = shlex.split(match.group(3), comments=True, posix=True)
        except ValueError:
            continue
        if len(parts) != 1:
            continue
        name, operator = match.group(1), match.group(2)
        fragment = _expand_shell_variables(parts[0], assignments)
        prior = assignments.get(name, "") if operator == "+=" else ""
        assignments[name] = prior + fragment
        yield assignments[name]


def _base64_candidates(value: str) -> Iterable[str]:
    for match in _BASE64_TOKEN_RE.finditer(value):
        token = match.group(0)
        if len(token) % 4 == 1:
            continue
        padded = token + "=" * (-len(token) % 4)
        try:
            decoded = b64decode(padded, altchars=b"-_", validate=True).decode("utf-8")
        except (BinasciiError, UnicodeDecodeError, ValueError):
            continue
        if decoded and all(
            character.isprintable() or character.isspace() for character in decoded
        ):
            yield decoded


def decoded_text_candidates(value: str) -> Iterable[str]:
    pending = deque([value])
    seen: set[str] = set()
    while pending:
        candidate = pending.popleft()
        if candidate in seen:
            continue
        seen.add(candidate)
        yield candidate
        if len(seen) > _MAX_DECODED_CANDIDATES:
            return
        decoded = (
            _REGEX_SINGLETON_RE.sub(r"\1", candidate),
            _decode_source_escapes(candidate),
            *_python_literal_candidates(candidate),
            *_shell_assignment_candidates(candidate),
            *_base64_candidates(candidate),
        )
        pending.extend(item for item in decoded if item != candidate and item not in seen)


def _boundary_violation(value: str, local_roots: bool) -> str | None:
    candidates = 0
    for candidate in decoded_text_candidates(value):
        candidates += 1
        violation = _concrete_boundary_class(candidate)
        if violation is not None:
            return violation
        if local_roots and _LOCAL_ROOT_RE.search(candidate):
            return "private or transient absolute path"
    if candidates > _MAX_DECODED_CANDIDATES:
        return "public boundary decoding exceeded its fail-closed candidate limit"
    return None


def public_boundary_violation(value: str) -> str | None:
    return _boundary_violation(value, False)


def contract_path_violation(value: str) -> str | None:
    return _boundary_violation(value, True)
