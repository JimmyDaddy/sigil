#!/usr/bin/env python3
"""Unit tests for the hard-coded prompt phrase routing gate."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("check-no-prompt-phrase-routing.py")
SPEC = importlib.util.spec_from_file_location("check_no_prompt_phrase_routing", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
checker = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = checker
SPEC.loader.exec_module(checker)


class PromptPhraseRoutingTests(unittest.TestCase):
    def scan(
        self, source_text: str, relative: str = "crates/example/src/lib.rs"
    ) -> list[checker.Violation]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates").mkdir()
            source = root / relative
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text(source_text, encoding="utf-8")
            return checker.prompt_phrase_routing_violations(root)

    def test_direct_prompt_phrase_match_is_rejected(self) -> None:
        violations = self.scan(
            'fn route(prompt: &str) -> bool { prompt.to_lowercase().contains("continue") }\n'
        )

    def test_direct_prompt_equality_is_rejected(self) -> None:
        violations = self.scan(
            'fn route(prompt: &str) -> bool { prompt == "continue" }\n'
        )

    def test_direct_prompt_regex_is_rejected(self) -> None:
        violations = self.scan(
            'function route(userInput: string) { return userInput.match(/go ahead|continue/i); }\n',
            "apps/desktop/src/route.ts",
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "direct_prompt_phrase_match")],
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "direct_prompt_phrase_match")],
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "direct_prompt_phrase_match")],
        )

    def test_semantic_intent_dictionary_is_rejected(self) -> None:
        violations = self.scan(
            "fn query_has_source_intent(query: &str) -> bool {\n"
            "    contains_any(query, &[\"source\", \"源码\"])\n"
            "}\n"
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "semantic_helper_phrase_match")],
        )

    def test_multiline_prompt_phrase_match_is_rejected(self) -> None:
        violations = self.scan(
            "fn route(prompt: &str) -> bool {\n"
            "    prompt\n"
            "        .trim()\n"
            "        .to_lowercase()\n"
            "        .contains(\"go ahead\")\n"
            "}\n"
        )

    def test_semantic_alias_does_not_hide_phrase_routing(self) -> None:
        violations = self.scan(
            "fn should_resume_task(text: &str) -> bool {\n"
            "    let normalized = text.to_lowercase();\n"
            "    normalized.contains(\"go ahead\")\n"
            "}\n"
        )

    def test_prompt_match_statement_is_rejected(self) -> None:
        violations = self.scan(
            "fn route(prompt: &str) -> bool {\n"
            "    match prompt { \"continue\" => true, _ => false }\n"
            "}\n"
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "semantic_helper_phrase_match")],
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "semantic_helper_phrase_match")],
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "semantic_helper_phrase_match")],
        )

    def test_inline_intent_field_phrase_table_is_rejected(self) -> None:
        violations = self.scan(
            "fn profile(query: &str) -> Profile {\n"
            "    Profile { source_intent: contains_any(query, &[\"source\", \"源码\"]) }\n"
            "}\n"
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "semantic_helper_phrase_match")],
        )

    def test_explicit_command_grammar_is_allowed(self) -> None:
        self.assertEqual(
            self.scan(
                'fn dispatch(prompt: &str) -> bool {\n'
                '    prompt.starts_with("/") || prompt.trim_start().starts_with("@")\n'
                '}\n'
            ),
            [],
        )

    def test_typescript_prompt_phrase_match_is_rejected(self) -> None:
        violations = self.scan(
            'function route(userInput: string) { return userInput.toLowerCase().includes("go ahead"); }\n',
            "apps/desktop/src/route.ts",
        )
        self.assertEqual(
            [(violation.line, violation.rule) for violation in violations],
            [(1, "direct_prompt_phrase_match")],
        )

    def test_typed_enum_parsing_is_allowed(self) -> None:
        self.assertEqual(
            self.scan(
                'fn parse_route_decision(action: &str) -> bool {\n'
                '    matches!(action, "resume_task" | "apply_current_request_as_guidance")\n'
                '}\n'
            ),
            [],
        )

    def test_explicit_literal_search_is_allowed(self) -> None:
        self.assertEqual(
            self.scan(
                "fn search(query: &str, candidate: &str) -> bool {\n"
                "    candidate.contains(query)\n"
                "}\n"
            ),
            [],
        )

    def test_physical_and_inline_tests_are_not_policy_inputs(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            production = root / "crates/example/src/lib.rs"
            physical_test = root / "crates/example/src/tests/lib_tests.rs"
            dependency = root / "apps/desktop/node_modules/example/index.ts"
            physical_test.parent.mkdir(parents=True)
            dependency.parent.mkdir(parents=True)
            production.write_text(
                "fn live() {}\n\n"
                "#[cfg(test)]\n"
                "mod routing_tests {\n"
                "    fn route(prompt: &str) -> bool { prompt.contains(\"continue\") }\n"
                "}\n",
                encoding="utf-8",
            )
            physical_test.write_text(
                'fn route(prompt: &str) -> bool { prompt.contains("continue") }\n',
                encoding="utf-8",
            )
            dependency.write_text(
                'function route(userInput: string) { return userInput.includes("continue"); }\n',
                encoding="utf-8",
            )
            self.assertEqual(checker.prompt_phrase_routing_violations(root), [])


if __name__ == "__main__":
    unittest.main()
