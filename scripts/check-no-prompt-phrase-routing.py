#!/usr/bin/env python3
"""Reject hard-coded natural-language matching used as user-intent routing.

This is deliberately narrower than a generic ban on string parsing. Explicit command
grammars, protocol fields, search queries, security classifiers, and presentation-only
formatting remain valid. The gate targets production code that treats a user prompt or
guidance string as a semantic enum by comparing it with fixed natural-language phrases.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


SOURCE_SUFFIXES = {".rs", ".ts", ".tsx", ".py"}
PROMPT_VALUE = re.compile(
    r"\b(?:prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b",
    re.IGNORECASE,
)
DIRECT_PHRASE_MATCH = re.compile(
    r"\b(?P<value>prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b"
    r"(?P<chain>(?:\s*\.\s*(?:trim|trim_start|trim_end|to_lowercase|to_ascii_lowercase|toLowerCase)\s*\(\s*\))*)"
    r"\s*\.\s*(?P<operator>contains|starts_with|ends_with|strip_prefix|strip_suffix|find|"
    r"eq_ignore_ascii_case|includes|startsWith|endsWith)\s*"
    r"\(\s*(?P<literal>\"(?:[^\"\\]|\\.)*\")",
    re.IGNORECASE,
)
DIRECT_EQUALITY_MATCH = re.compile(
    r"(?:\b(?P<left>prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b\s*(?:==|!=|===|!==)\s*"
    r"(?P<right_literal>\"(?:[^\"\\]|\\.)*\")|"
    r"(?P<left_literal>\"(?:[^\"\\]|\\.)*\")\s*(?:==|!=|===|!==)\s*\b"
    r"(?P<right>prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b)",
    re.IGNORECASE,
)
DIRECT_REGEX_MATCH = re.compile(
    r"(?:\b(?:prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b\s*\.\s*match\s*\(\s*"
    r"/(?P<method_regex>(?:\\.|[^/])*)/[a-z]*\s*\)|"
    r"/(?P<test_regex>(?:\\.|[^/])*)/[a-z]*\s*\.\s*test\s*\(\s*\b"
    r"(?:prompt|guidance|user_input|user_message|message_text|request_text|"
    r"userInput|userMessage|messageText|requestText)\b\s*\))",
    re.IGNORECASE,
)
SEMANTIC_HELPER = re.compile(
    r"\b(?:fn|function)\s+(?P<name>[a-zA-Z0-9_]*(?:intent|routing|route_decision|route_choice|"
    r"semantic|resume|continu|task_handoff|task_planning|plan_request|approval_intent|"
    r"memory_intent|remember|search_intent|source_intent)"
    r"[a-zA-Z0-9_]*)\s*\((?P<params>[^)]*)\)",
    re.IGNORECASE,
)
FUNCTION_WITH_PARAMS = re.compile(
    r"\b(?:fn|function)\s+(?P<name>[a-zA-Z0-9_]+)\s*\((?P<params>[^)]*)\)",
    re.IGNORECASE,
)
FIXED_LITERAL_MATCH = re.compile(
    r"\.(?:contains|starts_with|ends_with|strip_prefix|strip_suffix|find|"
    r"eq_ignore_ascii_case|includes|startsWith|endsWith)"
    r"\s*\(\s*(?P<literal>\"(?:[^\"\\]|\\.)*\")"
    r"|\bRegex::new\s*\(\s*(?P<regex>\"(?:[^\"\\]|\\.)*\")",
    re.IGNORECASE,
)
CONTAINS_ANY_CALL = re.compile(
    r"\bcontains_any\s*\((?P<body>[^;{}]{0,1024})\)", re.DOTALL
)
FIXED_EQUALITY_MATCH = re.compile(
    r"\b[A-Za-z_][A-Za-z0-9_]*\b\s*(?:==|!=|===|!==)\s*"
    r"(?P<literal>\"(?:[^\"\\]|\\.)*\")"
)
RUST_STRING_MATCH = re.compile(
    r"\bmatch\s+[A-Za-z_][A-Za-z0-9_]*\s*\{(?P<body>[^{}]{0,2048})\}", re.DOTALL
)
TYPESCRIPT_STRING_SWITCH = re.compile(
    r"\bswitch\s*\(\s*[A-Za-z_$][A-Za-z0-9_$]*\s*\)\s*\{"
    r"(?P<body>[^{}]{0,2048})\}",
    re.DOTALL,
)
TYPESCRIPT_REGEX_OPERATION = re.compile(
    r"(?:\b[A-Za-z_$][A-Za-z0-9_$]*\s*\.\s*match\s*\(\s*/"
    r"(?P<method_body>(?:\\.|[^/])*)/[a-z]*\s*\)|/"
    r"(?P<test_body>(?:\\.|[^/])*)/[a-z]*\s*\.\s*test\s*\(\s*"
    r"[A-Za-z_$][A-Za-z0-9_$]*\s*\))",
    re.IGNORECASE,
)
STRING_LITERAL = re.compile(r'"(?P<value>(?:[^"\\]|\\.)*)"')
INLINE_TEST_MODULE = re.compile(
    r"^[ \t]*#\[cfg\(test\)\][ \t\r\n]*(?:#\[path\s*=\s*\"[^\"]+\"\][ \t\r\n]*)?"
    r"mod[ \t]+[A-Za-z0-9_]*tests[ \t]*\{",
    re.MULTILINE,
)


@dataclass(frozen=True)
class Violation:
    """One production source location that violates the prompt-routing policy."""

    path: Path
    line: int
    rule: str


def is_test_source(path: Path) -> bool:
    """Return whether a source path belongs to a physical test or generated surface."""
    name = path.name.lower()
    return (
        "tests" in path.parts
        or "e2e" in path.parts
        or "generated" in path.parts
        or "node_modules" in path.parts
        or "target" in path.parts
        or "dist" in path.parts
        or name.endswith(("_tests.rs", "_test_support.rs", ".test.ts", ".test.tsx"))
        or name.startswith("test_")
    )


def production_text(text: str) -> str:
    """Drop a trailing inline Rust test module from production scanning."""
    marker = INLINE_TEST_MODULE.search(text)
    return text[: marker.start()] if marker is not None else text


def decoded_literal(raw: str) -> str:
    """Return enough of a quoted source literal to classify explicit command syntax."""
    return raw[1:-1].replace(r"\"", '"').replace(r"\\", "\\")


def is_explicit_command_marker(value: str) -> bool:
    """Recognize syntax prefixes, not natural-language aliases."""
    return value.startswith(("/", "@", "$")) or value in {"", "-", "--"}


def regex_has_natural_phrase(value: str) -> bool:
    """Distinguish word-bearing regexes from punctuation-only command grammars."""
    without_escapes = re.sub(r"\\[A-Za-z]", "", value)
    return bool(re.search(r"[A-Za-z]{2,}|[^\x00-\x7f]", without_escapes))


def natural_literals(text: str) -> list[str]:
    """Return human-language-like literals from a candidate matching expression."""
    values = [match.group("value") for match in STRING_LITERAL.finditer(text)]
    return [
        value
        for value in values
        if not is_explicit_command_marker(value)
        and any(character.isalpha() or ord(character) > 127 for character in value)
    ]


def has_natural_phrase_match(text: str) -> bool:
    """Return whether fixed natural language is an operand of a match primitive."""
    for match in FIXED_LITERAL_MATCH.finditer(text):
        raw = match.group("literal") or match.group("regex")
        if natural_literals(raw):
            return True
    return any(
        natural_literals(match.group("body"))
        for match in CONTAINS_ANY_CALL.finditer(text)
    ) or any(
        natural_literals(match.group("literal"))
        for match in FIXED_EQUALITY_MATCH.finditer(text)
    ) or any(
        natural_literals(match.group("body"))
        for pattern in (RUST_STRING_MATCH, TYPESCRIPT_STRING_SWITCH)
        for match in pattern.finditer(text)
    ) or any(
        regex_has_natural_phrase(match.group("method_body") or match.group("test_body"))
        for match in TYPESCRIPT_REGEX_OPERATION.finditer(text)
    )


def semantic_helper_window(lines: list[str], start: int, limit: int = 96) -> str:
    """Return a bounded function window used only for conservative policy detection."""
    end = min(len(lines), start + limit)
    for index in range(start + 1, end):
        if re.match(
            r"^[ \t]*(?:(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn|function)"
            r"\s+[A-Za-z0-9_]+\s*\(",
            lines[index],
        ):
            end = index
            break
    return "\n".join(lines[start:end])


def scan_source(path: Path, relative: Path) -> list[Violation]:
    """Scan one production source file for prohibited semantic phrase routing."""
    text = production_text(path.read_text(encoding="utf-8"))
    lines = text.splitlines()
    violations: list[Violation] = []
    seen: set[tuple[int, str]] = set()

    for index, line in enumerate(lines):
        line_number = index + 1
        for match in DIRECT_PHRASE_MATCH.finditer(line):
            literal = decoded_literal(match.group("literal"))
            if is_explicit_command_marker(literal):
                continue
            key = (line_number, "direct_prompt_phrase_match")
            if key not in seen:
                seen.add(key)
                violations.append(Violation(relative, line_number, key[1]))
        for match in DIRECT_EQUALITY_MATCH.finditer(line):
            raw = match.group("right_literal") or match.group("left_literal")
            literal = decoded_literal(raw)
            if is_explicit_command_marker(literal):
                continue
            key = (line_number, "direct_prompt_phrase_match")
            if key not in seen:
                seen.add(key)
                violations.append(Violation(relative, line_number, key[1]))
        for match in DIRECT_REGEX_MATCH.finditer(line):
            regex = match.group("method_regex") or match.group("test_regex")
            if not regex_has_natural_phrase(regex):
                continue
            key = (line_number, "direct_prompt_phrase_match")
            if key not in seen:
                seen.add(key)
                violations.append(Violation(relative, line_number, key[1]))

        helper = SEMANTIC_HELPER.search(line)
        function = FUNCTION_WITH_PARAMS.search(line)
        if function is None:
            continue
        function_name = function.group("name").lower()
        function_params = function.group("params")
        has_prompt_parameter = PROMPT_VALUE.search(function_params) is not None
        has_query_parameter = re.search(
            r"\bquery\b", function_params, re.IGNORECASE
        ) is not None
        is_semantic_helper = helper is not None and (
            "intent_hint" in function_name
            or re.search(
                r"\b(?:prompt|guidance|query|input|text|message|request|user_input|user_message|"
                r"message_text|request_text|userInput|userMessage|messageText|requestText)\b",
                function_params,
                re.IGNORECASE,
            )
            is not None
        )
        window = semantic_helper_window(lines, index)
        query_builds_intent = has_query_parameter and re.search(
            r"\b[a-zA-Z0-9_]+_intent\s*:", window
        ) is not None
        if not has_prompt_parameter and not is_semantic_helper and not query_builds_intent:
            continue
        if has_natural_phrase_match(window):
            key = (line_number, "semantic_helper_phrase_match")
            if (line_number, "direct_prompt_phrase_match") in seen:
                continue
            if key not in seen:
                seen.add(key)
                violations.append(Violation(relative, line_number, key[1]))

    return violations


def production_sources(root: Path) -> list[Path]:
    """Return all checked production source files."""
    candidates: set[Path] = set()
    for source_root in (root / "crates", root / "apps" / "desktop"):
        if not source_root.is_dir():
            continue
        for path in source_root.rglob("*"):
            if path.is_file() and path.suffix in SOURCE_SUFFIXES:
                relative = path.relative_to(root)
                if not is_test_source(relative):
                    candidates.add(path)
    return sorted(candidates)


def prompt_phrase_routing_violations(root: Path) -> list[Violation]:
    """Return all hard-coded user-prompt semantic routing violations."""
    if not (root / "crates").is_dir():
        raise ValueError(f"crates directory is missing: {root / 'crates'}")
    violations: list[Violation] = []
    for path in production_sources(root):
        violations.extend(scan_source(path, path.relative_to(root)))
    return violations


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    try:
        violations = prompt_phrase_routing_violations(root)
    except (OSError, UnicodeError, ValueError) as error:
        print(f"prompt phrase routing check failed: {error}", file=sys.stderr)
        return 1

    if violations:
        print(
            "prompt phrase routing check failed: use a model-owned typed semantic decision; "
            "the host may validate durable identity and enum values but must not infer intent "
            "from natural-language phrases",
            file=sys.stderr,
        )
        for violation in violations:
            print(
                f"  {violation.path}:{violation.line}: {violation.rule}",
                file=sys.stderr,
            )
        return 1

    print("prompt phrase routing check passed: no hard-coded user-intent phrase matching")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
