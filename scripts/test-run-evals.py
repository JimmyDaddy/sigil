#!/usr/bin/env python3
"""Contract tests for the explicit model-eval shell adapter."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
RUNNER = REPOSITORY_ROOT / "scripts/run-evals.sh"


class RunEvalsTests(unittest.TestCase):
    def test_model_mode_forwards_exact_orchestration_route_contract(self) -> None:
        with tempfile.TemporaryDirectory(prefix="sigil-run-evals-") as raw_temp:
            temp = Path(raw_temp)
            config_path = temp / "sigil.toml"
            config_path.write_text("", encoding="utf-8")
            route_contract = temp / "route.toml"
            route_contract.write_text("schema_version = 1\n", encoding="utf-8")
            output_dir = temp / "campaign"
            captured_args = temp / "args.json"
            fake_binary = temp / "fake-sigil"
            fake_binary.write_text(
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    args = sys.argv[1:]
                    output_dir = Path(args[args.index("--output-dir") + 1])
                    output_dir.mkdir()
                    (output_dir / "results.jsonl").write_text(
                        '{"report_schema_version":3}\\n',
                        encoding="utf-8",
                    )
                    (output_dir / "manifest.json").write_text(
                        '{"report_schema_version": 3}\\n',
                        encoding="utf-8",
                    )
                    (output_dir / "summary.md").write_text("model\\n", encoding="utf-8")
                    orchestration = output_dir / "orchestration"
                    orchestration.mkdir()
                    (orchestration / "results.jsonl").write_text(
                        '{"report_schema_version":1}\\n',
                        encoding="utf-8",
                    )
                    (orchestration / "manifest.json").write_text(
                        '{"report_schema_version": 1}\\n',
                        encoding="utf-8",
                    )
                    (orchestration / "summary.md").write_text(
                        "orchestration\\n",
                        encoding="utf-8",
                    )
                    Path(os.environ["SIGIL_CAPTURED_ARGS"]).write_text(
                        json.dumps(args),
                        encoding="utf-8",
                    )
                    """
                ),
                encoding="utf-8",
            )
            fake_binary.chmod(0o700)
            environment = os.environ.copy()
            environment.update(
                {
                    "SIGIL_MODEL_EVAL_BIN": str(fake_binary),
                    "SIGIL_CAPTURED_ARGS": str(captured_args),
                }
            )

            completed = subprocess.run(
                [
                    "bash",
                    str(RUNNER),
                    "--model",
                    "--config",
                    str(config_path),
                    "--case",
                    "orchestration-v1",
                    "--repetitions",
                    "3",
                    "--max-cost-usd",
                    "1.00",
                    "--timeout-secs",
                    "600",
                    "--output-dir",
                    str(output_dir),
                    "--orchestration-route-contract",
                    str(route_contract),
                ],
                cwd=REPOSITORY_ROOT,
                env=environment,
                text=True,
                capture_output=True,
                check=False,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            args = json.loads(captured_args.read_text(encoding="utf-8"))
            contract_index = args.index("--orchestration-route-contract")
            self.assertEqual(args[contract_index + 1], str(route_contract))
            self.assertIn("orchestration-v1", args)
            self.assertIn(
                f"wrote {output_dir}/orchestration/manifest.json",
                completed.stdout,
            )


if __name__ == "__main__":
    unittest.main()
