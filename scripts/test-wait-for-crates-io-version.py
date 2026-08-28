#!/usr/bin/env python3
import importlib.util
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).with_name("wait-for-crates-io-version.py")
SPEC = importlib.util.spec_from_file_location("r70_registry_wait", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class RegistryWaitTests(unittest.TestCase):
    def test_version_visibility_requires_an_unyanked_exact_version(self):
        payload = {"versions": [{"num": "0.1.0", "yanked": False}]}
        self.assertTrue(MODULE.version_is_visible(payload, "0.1.0"))
        self.assertFalse(MODULE.version_is_visible(payload, "0.1.1"))
        self.assertFalse(
            MODULE.version_is_visible(
                {"versions": [{"num": "0.1.0", "yanked": True}]}, "0.1.0"
            )
        )

    def test_wait_retries_until_registry_index_is_updated(self):
        responses = iter(
            [
                {"versions": []},
                {"versions": [{"num": "0.1.0", "yanked": False}]},
            ]
        )
        sleeps = []
        self.assertEqual(
            MODULE.wait_for_version(
                "sigil-tui-core",
                "0.1.0",
                timeout_seconds=10,
                interval_seconds=2,
                request_timeout_seconds=1,
                fetch=lambda _name, _timeout: next(responses),
                sleep=sleeps.append,
                clock=iter([0, 1, 2]).__next__,
            ),
            (True, None),
        )
        self.assertEqual(sleeps, [2])

    def test_inputs_are_bounded(self):
        self.assertEqual(MODULE.validate_inputs("sigil-tui-core", "0.1.0"), [])
        self.assertTrue(MODULE.validate_inputs("../escape", "0.1.0"))
        self.assertTrue(MODULE.validate_inputs("sigil-tui-core", "latest"))


if __name__ == "__main__":
    unittest.main()
