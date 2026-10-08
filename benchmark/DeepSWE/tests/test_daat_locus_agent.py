import ast
import tempfile
import unittest
from pathlib import Path

from benchmark_deepswe.daat_locus_agent import (
    DaatLocusAgent,
    _replace_section_auth_file,
)


class FakeEnvironment:
    def __init__(self):
        self.received_env = None

    def agent_process_env(self, env):
        self.received_env = dict(env)
        merged = dict(env)
        merged["HTTPS_PROXY"] = "http://agent:token@pier-egress-proxy:8080"
        return merged


class DaatLocusAgentTests(unittest.TestCase):
    def test_agent_env_passes_through_pier_agent_process_env(self):
        environment = FakeEnvironment()
        agent = DaatLocusAgent(
            logs_dir=Path("logs"),
            extra_env={"EXTRA": "value"},
            forward_env="",
            container_home="/tmp/daat-home",
        )

        env = agent._agent_env(environment, {"LOCAL": "value"})

        self.assertEqual(environment.received_env["DAAT_LOCUS_HOME"], "/tmp/daat-home")
        self.assertEqual(environment.received_env["EXTRA"], "value")
        self.assertEqual(environment.received_env["LOCAL"], "value")
        self.assertEqual(
            env["HTTPS_PROXY"],
            "http://agent:token@pier-egress-proxy:8080",
        )

    def test_environment_exec_calls_explicitly_pass_env(self):
        source = (
            Path(__file__).parents[1]
            / "src"
            / "benchmark_deepswe"
            / "daat_locus_agent.py"
        )
        tree = ast.parse(source.read_text(encoding="utf-8"))
        missing_env_lines = []
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            if not isinstance(node.func, ast.Attribute):
                continue
            if node.func.attr != "exec":
                continue
            if not isinstance(node.func.value, ast.Name):
                continue
            if node.func.value.id != "environment":
                continue
            if not any(keyword.arg == "env" for keyword in node.keywords):
                missing_env_lines.append(node.lineno)

        self.assertEqual(missing_env_lines, [])

    def test_agent_wires_usage_artifact_to_bridge_and_metadata(self):
        source = (
            Path(__file__).parents[1]
            / "src"
            / "benchmark_deepswe"
            / "daat_locus_agent.py"
        ).read_text(encoding="utf-8")

        self.assertIn('"--usage-file"', source)
        self.assertIn('"daat-locus-usage.json"', source)
        self.assertIn('"usage_path"', source)


    def test_replace_section_auth_file_only_touches_target_provider(self):
        text = (
            "[providers.a]\n"
            "auth_file = 'old-a'\n"
            "[providers.b]\n"
            'auth_file = "old-b"\n'
        )

        updated = _replace_section_auth_file(text, "b", "/new/b.json")

        self.assertIn("auth_file = 'old-a'", updated)
        self.assertIn('auth_file = "/new/b.json"', updated)

    def test_slim_home_copies_provider_auth_file_and_repoints_path(self):
        with (
            tempfile.TemporaryDirectory() as host_dir,
            tempfile.TemporaryDirectory() as auth_dir,
        ):
            host_home = Path(host_dir)
            (host_home / "config").mkdir()
            auth_file = Path(auth_dir) / "oauth.json"
            auth_file.write_text("{}", encoding="utf-8")
            (host_home / "config" / "config.toml").write_text(
                'main_model = "m"\n'
                "[providers.opencode-go]\n"
                'api_key = "k"\n'
                "[providers.opencode-console]\n"
                f"auth_file = '{auth_file}'\n",
                encoding="utf-8",
            )

            agent = DaatLocusAgent(
                logs_dir=Path("logs"),
                forward_env="",
                container_home="/tmp/bench/home",
            )
            target = agent._build_slim_home(host_home)
            try:
                copied = (target / "config" / "config.toml").read_text(
                    encoding="utf-8"
                )
                self.assertIn('api_key = "k"', copied)
                self.assertIn(
                    'auth_file = "/tmp/bench/home/auth/opencode-console/oauth.json"',
                    copied,
                )
                self.assertTrue(
                    (target / "auth" / "opencode-console" / "oauth.json").is_file()
                )
            finally:
                agent._tmp_home.cleanup()


if __name__ == "__main__":
    unittest.main()
