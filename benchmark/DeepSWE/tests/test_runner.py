import argparse
import tempfile
import unittest
from pathlib import Path

from benchmark_deepswe.runner import (
    build_pier_command,
    find_daat_locus_source_root,
    patch_pier_proxy_keeps_allowlist_without_auth,
    patch_pier_proxy_script_newlines,
    prune_leftover_docker_state,
    resolve_benchmark_path,
    restore_pier_docker_cleanup_removes_images,
)


class RunnerTests(unittest.TestCase):
    def test_explicit_path_wins(self):
        args = argparse.Namespace(path="tasks/single", dry_run=True)
        self.assertEqual(resolve_benchmark_path(args, prepare=False), "tasks/single")

    def test_default_path_uses_managed_repo_tasks_dir(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
            )
            self.assertEqual(
                resolve_benchmark_path(args, prepare=False),
                str(repo / "tasks"),
            )

    def test_dry_run_command_does_not_require_explicit_path(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
                daemon_port=53825,
                daat_locus_bin=None,
                daat_locus_source="C:/src/DaatLocus",
                daat_home=None,
                startup_timeout_sec=90.0,
                send_timeout_sec=7200.0,
                source_build_timeout_sec=3600.0,
                agent_setup_timeout_multiplier=10.0,
                agent_timeout_multiplier=2.0,
                resume=None,
            )
            command = build_pier_command(args, ["--n-tasks", "1"])
            self.assertIn(str(repo / "tasks"), command)
            self.assertIn("--agent-import-path", command)
            self.assertIn("daat_locus_source=C:/src/DaatLocus", command)
            self.assertIn("--agent-setup-timeout-multiplier", command)
            self.assertIn("10.0", command)
            self.assertIn("--agent-timeout-multiplier", command)
            self.assertIn("2.0", command)

    def test_explicit_pier_agent_setup_timeout_multiplier_wins(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
                daemon_port=53825,
                daat_locus_bin=None,
                daat_locus_source="C:/src/DaatLocus",
                daat_home=None,
                startup_timeout_sec=90.0,
                send_timeout_sec=7200.0,
                source_build_timeout_sec=3600.0,
                agent_setup_timeout_multiplier=10.0,
                agent_timeout_multiplier=2.0,
                resume=None,
            )

            command = build_pier_command(
                args,
                ["--agent-setup-timeout-multiplier=2", "--n-tasks", "1"],
            )

            self.assertIn("--agent-setup-timeout-multiplier=2", command)
            self.assertNotIn("--agent-setup-timeout-multiplier", command)
            self.assertNotIn("10.0", command)

    def test_explicit_pier_agent_timeout_multiplier_wins(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
                daemon_port=53825,
                daat_locus_bin=None,
                daat_locus_source="C:/src/DaatLocus",
                daat_home=None,
                startup_timeout_sec=90.0,
                send_timeout_sec=7200.0,
                source_build_timeout_sec=3600.0,
                agent_setup_timeout_multiplier=10.0,
                agent_timeout_multiplier=2.0,
                resume=None,
            )

            command = build_pier_command(
                args,
                ["--agent-timeout-multiplier=3", "--n-tasks", "1"],
            )

            self.assertIn("--agent-timeout-multiplier=3", command)
            self.assertNotIn("--agent-timeout-multiplier", command)
            self.assertNotIn("2.0", command)

    def test_global_pier_timeout_multiplier_suppresses_agent_timeout_default(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
                daemon_port=53825,
                daat_locus_bin=None,
                daat_locus_source="C:/src/DaatLocus",
                daat_home=None,
                startup_timeout_sec=90.0,
                send_timeout_sec=7200.0,
                source_build_timeout_sec=3600.0,
                agent_setup_timeout_multiplier=10.0,
                agent_timeout_multiplier=2.0,
                resume=None,
            )

            command = build_pier_command(
                args,
                ["--timeout-multiplier=3", "--n-tasks", "1"],
            )

            self.assertIn("--timeout-multiplier=3", command)
            self.assertNotIn("--agent-timeout-multiplier", command)
            self.assertNotIn("2.0", command)

    def test_prebuilt_binary_skips_source_build_kwarg(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            repo = Path(temp_dir) / "deep-swe"
            args = argparse.Namespace(
                path=None,
                deep_swe_repo=str(repo),
                deep_swe_url="unused",
                refresh_deep_swe=False,
                dry_run=True,
                daemon_port=53825,
                daat_locus_bin="/host/daat-locus",
                daat_locus_source="C:/src/DaatLocus",
                daat_home=None,
                startup_timeout_sec=90.0,
                send_timeout_sec=7200.0,
                source_build_timeout_sec=3600.0,
                agent_setup_timeout_multiplier=10.0,
                agent_timeout_multiplier=2.0,
                resume=None,
            )
            command = build_pier_command(args, [])
            self.assertIn("daat_locus_bin=/host/daat-locus", command)
            self.assertNotIn("daat_locus_source=C:/src/DaatLocus", command)

    def test_resume_targets_existing_job_directory(self):
        args = argparse.Namespace(resume="jobs/2026-10-05__00-00-00")
        command = build_pier_command(args, ["--n-tasks", "1"])
        self.assertEqual(command[:4], ["pier", "job", "resume", "-p"])
        self.assertEqual(command[4], "jobs/2026-10-05__00-00-00")
        self.assertEqual(command[-2:], ["--n-tasks", "1"])

    def test_prune_skips_when_docker_is_missing(self):
        import unittest.mock as mock

        from benchmark_deepswe import runner as runner_module

        with mock.patch.object(runner_module.shutil, "which", return_value=None):
            prune_leftover_docker_state(build_cache=False)

    def test_finds_source_root_by_cargo_manifest_and_git_dir(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            nested = root / "benchmark" / "DeepSWE"
            nested.mkdir(parents=True)
            (root / "Cargo.toml").write_text("[package]\nname = \"x\"\n", encoding="utf-8")
            (root / ".git").mkdir()
            self.assertEqual(find_daat_locus_source_root(nested), root)

    def test_patches_pier_proxy_script_writer_to_lf(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            agent_setup = Path(temp_dir) / "agent_setup.py"
            agent_setup.write_text(
                '(proxy_dir / "start-squid.sh").write_text(squid_bootstrap_command())\r\n',
                encoding="utf-8",
            )

            self.assertTrue(patch_pier_proxy_script_newlines(agent_setup))
            patched = agent_setup.read_bytes()
            self.assertIn(b'newline="\\n"', patched)
            self.assertNotIn(b"\r\n", patched)
            self.assertFalse(patch_pier_proxy_script_newlines(agent_setup))

    def test_patches_pier_proxy_to_keep_allowlist_without_auth(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            agent_setup = Path(temp_dir) / "agent_setup.py"
            agent_setup.write_text(
                '''def squid_bootstrap_command():
    return r"""#!/usr/bin/env bash
set -eu

htpasswd -bc /tmp/squid.passwd agent "$PROXY_TOKEN"

cat > /tmp/squid.conf <<'EOF'
http_port 0.0.0.0:8080
pid_filename /tmp/squid.pid
coredump_dir /tmp

auth_param basic program /usr/lib/squid/basic_ncsa_auth /tmp/squid.passwd
auth_param basic realm PierPolicyProxy
acl authenticated proxy_auth REQUIRED

acl allowed_domains dstdomain "/tmp/allowed_domains.txt"
http_access allow authenticated allowed_domains
http_access deny all
EOF
"""
''',
                encoding="utf-8",
            )

            self.assertTrue(patch_pier_proxy_keeps_allowlist_without_auth(agent_setup))
            patched = agent_setup.read_text(encoding="utf-8")
            self.assertNotIn("htpasswd -bc", patched)
            self.assertNotIn("proxy_auth REQUIRED", patched)
            self.assertIn('acl allowed_domains dstdomain "/tmp/allowed_domains.txt"', patched)
            self.assertIn("http_access allow allowed_domains", patched)
            self.assertIn("http_access deny all", patched)
            self.assertFalse(patch_pier_proxy_keeps_allowlist_without_auth(agent_setup))

    def test_restores_pier_delete_cleanup_to_remove_images(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            docker_py = Path(temp_dir) / "docker.py"
            docker_py.write_text(
                'await run(["down", "--volumes", "--remove-orphans"])\n',
                encoding="utf-8",
            )

            self.assertTrue(restore_pier_docker_cleanup_removes_images(docker_py))
            patched = docker_py.read_text(encoding="utf-8")
            self.assertIn(
                '["down", "--rmi", "all", "--volumes", "--remove-orphans"]',
                patched,
            )
            self.assertFalse(restore_pier_docker_cleanup_removes_images(docker_py))


if __name__ == "__main__":
    unittest.main()
