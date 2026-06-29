#!/usr/bin/env python3
"""Unit tests for the staged coverage gate helpers."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).with_name("check-staged-coverage.py")
SPEC = importlib.util.spec_from_file_location("check_staged_coverage", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
check_staged_coverage = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = check_staged_coverage
SPEC.loader.exec_module(check_staged_coverage)


class StagedCoverageHelpersTests(unittest.TestCase):
    def test_business_rust_file_filter_keeps_only_business_sources(self) -> None:
        self.assertTrue(
            check_staged_coverage.is_business_rust_file("crates/sigil-mcp/src/lib.rs")
        )
        self.assertFalse(
            check_staged_coverage.is_business_rust_file(
                "crates/sigil-mcp/src/tests/lib_tests.rs"
            )
        )
        self.assertFalse(
            check_staged_coverage.is_business_rust_file(
                "crates/sigil-tui/src/runner/worker_loop.rs"
            )
        )
        self.assertFalse(
            check_staged_coverage.is_business_rust_file(
                "crates/sigil-runtime/src/agent_tools.rs"
            )
        )
        self.assertFalse(
            check_staged_coverage.is_business_rust_file(
                "crates/sigil-tui/src/runner/spawn.rs"
            )
        )
        self.assertFalse(
            check_staged_coverage.is_business_rust_file(
                "crates/sigil-tui/src/launcher.rs"
            )
        )
        self.assertFalse(check_staged_coverage.is_business_rust_file("README.md"))

    def test_package_names_for_staged_files_uses_crate_directories(self) -> None:
        packages = check_staged_coverage.package_names_for_staged_files(
            [
                "crates/sigil-tui/src/app.rs",
                "crates/sigil-kernel/src/session.rs",
                "crates/sigil-tui/src/ui/theme.rs",
                "README.md",
            ]
        )

        self.assertEqual(packages, ["sigil-kernel", "sigil-tui"])

    def test_rust_test_package_detects_same_package_test_files(self) -> None:
        self.assertEqual(
            check_staged_coverage.rust_test_package(
                "crates/sigil-kernel/src/tests/session_tests.rs"
            ),
            "sigil-kernel",
        )
        self.assertEqual(
            check_staged_coverage.rust_test_package(
                "crates/sigil-tui/src/app/tests/session_flow_tests.rs"
            ),
            "sigil-tui",
        )
        self.assertEqual(
            check_staged_coverage.rust_test_package("crates/sigil-http/src/lib.rs"),
            None,
        )

    def test_non_executable_classifier_accepts_rust_type_shapes(self) -> None:
        non_executable = [
            "Stale { capability: String },",
            "McpElicitationRequest, McpElicitationResponse, McpListChangedNotification,",
            "notification: McpProgressNotification,",
            "pub type ProviderMap = BTreeMap<String, Value>;",
            "pub(crate) mod agent_display;",
            "pub use crate::{",
            "impl TaskConfig {",
            "pub fn as_str(self) -> &'static str {",
            "pub(crate) const COMMAND_SPECS: &[UiCommandSpec] = &[",
            ")?;",
            ")]);",
            "pub struct OpenAiStreamEnvelope {",
            "pub enum ProviderMode {",
            "pub trait ExecutionBackend: Send + Sync {",
            "#[derive(Debug)]",
            "}",
            "use std::path::PathBuf;",
        ]
        for line in non_executable:
            with self.subTest(line=line):
                self.assertTrue(check_staged_coverage.is_non_executable_added_line(line))

    def test_non_executable_classifier_rejects_executable_lines(self) -> None:
        executable = [
            'return Ok(ToolResult::error(call_id, "tool", kind, message));',
            'params.insert("cursor".to_owned(), Value::String(cursor.to_owned()));',
            'let result = response.get("result").cloned();',
        ]
        for line in executable:
            with self.subTest(line=line):
                self.assertFalse(check_staged_coverage.is_non_executable_added_line(line))

    def test_declaration_line_map_marks_enum_body_only(self) -> None:
        source = """\
pub enum ProviderMode {
    Fast,
    Slow(String),
    Stale { capability: String },
}

pub fn render(value: Result<(), Error>) -> Result<(), Error> {
    Ok(value?)
}
"""

        lines = check_staged_coverage.non_executable_declaration_lines(source)

        self.assertEqual(lines, {1, 2, 3, 4, 5, 7})
        self.assertNotIn(8, lines)

    def test_declaration_line_map_marks_use_const_and_function_signatures(self) -> None:
        source = """\
pub use crate::{
    Agent,
    TaskRunStatus,
};

pub(crate) const COMMAND_SPECS: &[UiCommandSpec] = &[
    UiCommandSpec {
        command: UiCommand::SubmitPlan,
        label: "Plan",
    },
];

impl TaskMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Plan => "plan",
        }
    }
}
"""

        lines = check_staged_coverage.non_executable_declaration_lines(source)

        self.assertTrue({1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 13, 14}.issubset(lines))
        self.assertNotIn(15, lines)
        self.assertNotIn(16, lines)

    def test_declaration_line_map_marks_trait_declarations(self) -> None:
        source = """\
pub trait ExecutionBackend: Send + Sync {
    fn kind(&self) -> ExecutionBackendKind;

    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionReceipt>;
}

pub fn run() {
    let receipt = execute();
    receipt
}
"""

        lines = check_staged_coverage.non_executable_declaration_lines(source)

        self.assertTrue({1, 2, 3, 4, 5}.issubset(lines))
        self.assertNotIn(8, lines)
        self.assertNotIn(9, lines)

    def test_parse_staged_added_lines_tracks_new_line_numbers(self) -> None:
        diff = """\
diff --git a/crates/example/src/lib.rs b/crates/example/src/lib.rs
--- a/crates/example/src/lib.rs
+++ b/crates/example/src/lib.rs
@@ -10,0 +11,2 @@
+let value = 1;
+Stale { capability: String },
@@ -20 +23 @@
-old();
+new();
"""

        added = check_staged_coverage.parse_staged_added_lines(diff)

        self.assertEqual(
            added["crates/example/src/lib.rs"],
            {
                11: "let value = 1;",
                12: "Stale { capability: String },",
                23: "new();",
            },
        )

    def test_compute_staged_coverage_reports_uncovered_lines(self) -> None:
        result = check_staged_coverage.compute_staged_coverage(
            ["crates/example/src/lib.rs"],
            {"crates/example/src/lib.rs": {10: "let a = 1;", 11: "let b = 2;"}},
            {"crates/example/src/lib.rs": {10: 1, 11: 0}},
            min_coverage=96.0,
        )

        self.assertEqual(result.checked_files, 1)
        self.assertEqual(result.checked_lines, 2)
        self.assertEqual(len(result.failures), 1)
        self.assertIn("50.00%", result.failures[0])
        self.assertIn("11", result.failures[0])

    def test_compute_staged_coverage_ignores_instrumented_enum_declarations(self) -> None:
        source = """\
pub enum ProviderMode {
    Fast,
    Slow(String),
}

pub fn run() {
    let value = load();
    value
}
"""
        result = check_staged_coverage.compute_staged_coverage(
            ["crates/example/src/lib.rs"],
            {
                "crates/example/src/lib.rs": {
                    2: "    Fast,",
                    3: "    Slow(String),",
                    7: "    let value = load();",
                }
            },
            {"crates/example/src/lib.rs": {2: 0, 3: 0, 7: 1}},
            min_coverage=96.0,
            staged_sources={"crates/example/src/lib.rs": source},
        )

        self.assertEqual(result.checked_files, 1)
        self.assertEqual(result.checked_lines, 1)
        self.assertEqual(result.failures, [])

    def test_compute_staged_coverage_keeps_executable_enum_constructor_calls(self) -> None:
        source = """\
pub fn render(value: Result<(), Error>) -> Result<(), Error> {
    Ok(value?)
}
"""
        result = check_staged_coverage.compute_staged_coverage(
            ["crates/example/src/lib.rs"],
            {"crates/example/src/lib.rs": {2: "    Ok(value?)"}},
            {"crates/example/src/lib.rs": {2: 0}},
            min_coverage=96.0,
            staged_sources={"crates/example/src/lib.rs": source},
        )

        self.assertEqual(result.checked_files, 1)
        self.assertEqual(result.checked_lines, 1)
        self.assertEqual(len(result.failures), 1)
        self.assertIn("0.00%", result.failures[0])

    def test_compute_staged_coverage_ignores_non_executable_no_data_files(self) -> None:
        result = check_staged_coverage.compute_staged_coverage(
            ["crates/example/src/protocol.rs"],
            {"crates/example/src/protocol.rs": {5: "Stale { capability: String },"}},
            {},
            min_coverage=96.0,
        )

        self.assertEqual(result.checked_lines, 0)
        self.assertEqual(result.failures, [])

    def test_compute_staged_coverage_fails_business_lines_without_coverage_data(self) -> None:
        result = check_staged_coverage.compute_staged_coverage(
            ["crates/example/src/lib.rs"],
            {"crates/example/src/lib.rs": {10: "let value = run();"}},
            {},
            min_coverage=96.0,
        )

        self.assertEqual(result.checked_lines, 0)
        self.assertEqual(
            result.failures,
            ["crates/example/src/lib.rs: no coverage data for staged business-code additions"],
        )

    def test_compute_staged_test_evidence_accepts_same_package_tests(self) -> None:
        result = check_staged_coverage.compute_staged_test_evidence(
            ["crates/sigil-kernel/src/session.rs"],
            [
                "crates/sigil-kernel/src/session.rs",
                "crates/sigil-kernel/src/tests/session_tests.rs",
            ],
            {"crates/sigil-kernel/src/session.rs": {10: "let value = run();"}},
            {"crates/sigil-kernel/src/session.rs": "pub fn run() {\n    let value = run();\n}\n"},
        )

        self.assertEqual(result.checked_packages, 1)
        self.assertEqual(result.checked_files, 1)
        self.assertEqual(
            result.evidence_files,
            ["crates/sigil-kernel/src/tests/session_tests.rs"],
        )
        self.assertEqual(result.failures, [])

    def test_compute_staged_test_evidence_rejects_missing_same_package_tests(self) -> None:
        result = check_staged_coverage.compute_staged_test_evidence(
            ["crates/sigil-kernel/src/session.rs"],
            [
                "crates/sigil-kernel/src/session.rs",
                "crates/sigil-tui/src/app/tests/session_flow_tests.rs",
            ],
            {"crates/sigil-kernel/src/session.rs": {10: "let value = run();"}},
            {"crates/sigil-kernel/src/session.rs": "pub fn run() {\n    let value = run();\n}\n"},
        )

        self.assertEqual(result.checked_packages, 1)
        self.assertEqual(result.checked_files, 1)
        self.assertEqual(len(result.failures), 1)
        self.assertIn("sigil-kernel", result.failures[0])

    def test_compute_staged_test_evidence_ignores_non_executable_additions(self) -> None:
        result = check_staged_coverage.compute_staged_test_evidence(
            ["crates/sigil-kernel/src/session.rs"],
            ["crates/sigil-kernel/src/session.rs"],
            {"crates/sigil-kernel/src/session.rs": {1: "pub enum Mode {", 2: "    Fast,"}},
            {"crates/sigil-kernel/src/session.rs": "pub enum Mode {\n    Fast,\n}\n"},
        )

        self.assertEqual(result.checked_packages, 0)
        self.assertEqual(result.checked_files, 0)
        self.assertEqual(result.failures, [])


if __name__ == "__main__":
    unittest.main()
