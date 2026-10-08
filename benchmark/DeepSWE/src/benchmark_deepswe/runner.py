from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path


AGENT_IMPORT_PATH = "benchmark_deepswe.daat_locus_agent:DaatLocusAgent"
DEEPSWE_REPO_URL = "https://github.com/datacurve-ai/deep-swe.git"
PIER_PROXY_SCRIPT_WRITE = (
    '(proxy_dir / "start-squid.sh").write_text(squid_bootstrap_command())'
)
PIER_PROXY_SCRIPT_WRITE_LF = (
    '(proxy_dir / "start-squid.sh").write_text(squid_bootstrap_command(), newline="\\n")'
)
PIER_PROXY_AUTH_BLOCK = """htpasswd -bc /tmp/squid.passwd agent "$PROXY_TOKEN"

cat > /tmp/squid.conf <<'EOF'
http_port 0.0.0.0:8080
pid_filename /tmp/squid.pid
coredump_dir /tmp

auth_param basic program /usr/lib/squid/basic_ncsa_auth /tmp/squid.passwd
auth_param basic realm PierPolicyProxy
acl authenticated proxy_auth REQUIRED
"""
PIER_PROXY_NO_AUTH_BLOCK = """cat > /tmp/squid.conf <<'EOF'
http_port 0.0.0.0:8080
pid_filename /tmp/squid.pid
coredump_dir /tmp
"""
PIER_PROXY_AUTH_ALLOW = "http_access allow authenticated allowed_domains"
PIER_PROXY_NO_AUTH_ALLOW = "http_access allow allowed_domains"
PIER_DOCKER_DELETE_DOWN = '["down", "--rmi", "all", "--volumes", "--remove-orphans"]'
PIER_DOCKER_DELETE_DOWN_KEEP_IMAGES = '["down", "--volumes", "--remove-orphans"]'


def project_dir() -> Path:
    return Path(__file__).resolve().parents[2]


def default_deepswe_repo() -> Path:
    return project_dir() / ".cache" / "deep-swe"


def find_daat_locus_source_root(start: Path) -> Path | None:
    current = start.resolve()
    if current.is_file():
        current = current.parent
    for candidate in (current, *current.parents):
        if (candidate / "Cargo.toml").is_file() and (candidate / ".git").exists():
            return candidate
    return None


def default_daat_locus_source() -> str | None:
    if value := os.environ.get("DAAT_LOCUS_SOURCE"):
        return value
    root = find_daat_locus_source_root(project_dir())
    return None if root is None else str(root)


def default_daat_home() -> str:
    return os.environ.get("DAAT_LOCUS_HOME") or str(Path.home() / ".daat-locus")


def ensure_deepswe_repo(repo_dir: Path, repo_url: str, refresh: bool) -> None:
    if repo_dir.exists():
        if refresh:
            subprocess.run(["git", "-C", str(repo_dir), "pull", "--ff-only"], check=True)
        return

    repo_dir.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "clone", repo_url, str(repo_dir)], check=True)


def resolve_benchmark_path(args: argparse.Namespace, *, prepare: bool) -> str:
    if args.path:
        return args.path

    repo_dir = Path(args.deep_swe_repo).expanduser()
    if prepare:
        ensure_deepswe_repo(repo_dir, args.deep_swe_url, args.refresh_deep_swe)
    return str(repo_dir / "tasks")


def build_pier_command(args: argparse.Namespace, pier_args: list[str]) -> list[str]:
    if args.resume:
        return ["pier", "job", "resume", "-p", str(args.resume), *pier_args]
    command = [
        "pier",
        "run",
        "-p",
        resolve_benchmark_path(args, prepare=not args.dry_run),
        "--agent-import-path",
        AGENT_IMPORT_PATH,
        "--agent-kwarg",
        f"daemon_port={args.daemon_port}",
    ]
    if args.daat_locus_bin:
        command.extend(["--agent-kwarg", f"daat_locus_bin={args.daat_locus_bin}"])
    elif args.daat_locus_source:
        command.extend(
            ["--agent-kwarg", f"daat_locus_source={args.daat_locus_source}"]
        )
    if args.daat_home:
        command.extend(["--agent-kwarg", f"daat_home={args.daat_home}"])
    if args.startup_timeout_sec is not None:
        command.extend(
            ["--agent-kwarg", f"startup_timeout_sec={args.startup_timeout_sec}"]
        )
    if args.send_timeout_sec is not None:
        command.extend(["--agent-kwarg", f"send_timeout_sec={args.send_timeout_sec}"])
    if args.source_build_timeout_sec is not None:
        command.extend(
            [
                "--agent-kwarg",
                f"source_build_timeout_sec={args.source_build_timeout_sec}",
            ]
        )
    if args.agent_setup_timeout_multiplier is not None and not has_pier_option(
        pier_args, "--agent-setup-timeout-multiplier"
    ):
        command.extend(
            [
                "--agent-setup-timeout-multiplier",
                str(args.agent_setup_timeout_multiplier),
            ]
        )
    if (
        args.agent_timeout_multiplier is not None
        and not has_pier_option(pier_args, "--agent-timeout-multiplier")
        and not has_pier_option(pier_args, "--timeout-multiplier")
    ):
        command.extend(
            [
                "--agent-timeout-multiplier",
                str(args.agent_timeout_multiplier),
            ]
        )
    command.extend(pier_args)
    return command


def has_pier_option(pier_args: list[str], option: str) -> bool:
    return any(arg == option or arg.startswith(f"{option}=") for arg in pier_args)


def patch_pier_proxy_script_newlines(agent_setup_path: Path) -> bool:
    text = agent_setup_path.read_text(encoding="utf-8")
    if PIER_PROXY_SCRIPT_WRITE_LF in text:
        return False
    if PIER_PROXY_SCRIPT_WRITE not in text:
        raise RuntimeError(
            f"Pier agent_setup.py no longer contains the expected proxy script writer: {agent_setup_path}"
        )
    agent_setup_path.write_text(
        text.replace(PIER_PROXY_SCRIPT_WRITE, PIER_PROXY_SCRIPT_WRITE_LF),
        encoding="utf-8",
        newline="\n",
    )
    return True


def patch_pier_proxy_keeps_allowlist_without_auth(agent_setup_path: Path) -> bool:
    text = agent_setup_path.read_text(encoding="utf-8")
    if (
        PIER_PROXY_AUTH_BLOCK not in text
        and PIER_PROXY_AUTH_ALLOW not in text
        and PIER_PROXY_NO_AUTH_BLOCK in text
        and PIER_PROXY_NO_AUTH_ALLOW in text
    ):
        return False
    if PIER_PROXY_AUTH_BLOCK not in text or PIER_PROXY_AUTH_ALLOW not in text:
        raise RuntimeError(
            f"Pier agent_setup.py no longer contains the expected proxy auth block: {agent_setup_path}"
        )
    agent_setup_path.write_text(
        text.replace(PIER_PROXY_AUTH_BLOCK, PIER_PROXY_NO_AUTH_BLOCK).replace(
            PIER_PROXY_AUTH_ALLOW,
            PIER_PROXY_NO_AUTH_ALLOW,
        ),
        encoding="utf-8",
        newline="\n",
    )
    return True


def restore_pier_docker_cleanup_removes_images(docker_path: Path) -> bool:
    text = docker_path.read_text(encoding="utf-8")
    if PIER_DOCKER_DELETE_DOWN in text:
        return False
    if PIER_DOCKER_DELETE_DOWN_KEEP_IMAGES not in text:
        raise RuntimeError(
            f"Pier docker.py no longer contains a recognized delete cleanup command: {docker_path}"
        )
    docker_path.write_text(
        text.replace(PIER_DOCKER_DELETE_DOWN_KEEP_IMAGES, PIER_DOCKER_DELETE_DOWN),
        encoding="utf-8",
        newline="\n",
    )
    return True


def patch_installed_pier_for_runner() -> None:
    import pier.environments.agent_setup as agent_setup

    if os.name == "nt":
        patch_pier_proxy_script_newlines(Path(agent_setup.__file__).resolve())
    patch_pier_proxy_keeps_allowlist_without_auth(Path(agent_setup.__file__).resolve())

    import pier.environments.docker.docker as docker_environment

    restore_pier_docker_cleanup_removes_images(
        Path(docker_environment.__file__).resolve()
    )


def prune_leftover_docker_state(*, build_cache: bool) -> None:
    """Drop bench-only Docker leftovers so long batches do not fill the disk.

    Pier tears down each trial's task image via `docker compose down --rmi all`,
    but the per-job egress-proxy images are not always removed (~300 MB each),
    and the BuildKit cache grows monotonically. Both are safe to prune between
    batches because they are rebuilt on demand.
    """

    docker = shutil.which("docker")
    if docker is None:
        print("docker not found on PATH; skipping prune", file=sys.stderr)
        return
    listed = subprocess.run(
        [docker, "images", "--filter", "reference=*pier-egress-proxy*", "-q"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    image_ids = listed.stdout.split()
    if image_ids:
        subprocess.run([docker, "rmi", "-f", *image_ids], check=False)
        print(f"pruned {len(image_ids)} leftover egress-proxy image(s)")
    if build_cache:
        subprocess.run([docker, "builder", "prune", "-f"], check=False)


def parse_args(argv: list[str]) -> tuple[argparse.Namespace, list[str]]:
    parser = argparse.ArgumentParser(
        description="Run DeepSWE/Pier with the Daat Locus custom agent."
    )
    parser.add_argument(
        "-p",
        "--path",
        help="DeepSWE task or tasks path. Defaults to the managed clone's tasks directory.",
    )
    parser.add_argument(
        "--deep-swe-repo",
        default=str(default_deepswe_repo()),
        help="Managed DeepSWE checkout path used when --path is omitted.",
    )
    parser.add_argument(
        "--deep-swe-url",
        default=DEEPSWE_REPO_URL,
        help="Repository URL cloned into --deep-swe-repo when needed.",
    )
    parser.add_argument(
        "--refresh-deep-swe",
        action="store_true",
        help="Run git pull --ff-only in the managed DeepSWE checkout before Pier.",
    )
    parser.add_argument(
        "--daat-locus-bin",
        default=None,
        help="Optional host path to a Linux daat-locus binary. If omitted, the local source tree is packaged and built in the sandbox.",
    )
    parser.add_argument(
        "--daat-locus-source",
        default=default_daat_locus_source(),
        help="Host Daat Locus source tree packaged when --daat-locus-bin is omitted. Defaults to DAAT_LOCUS_SOURCE or the current repository.",
    )
    parser.add_argument(
        "--daat-home",
        default=default_daat_home(),
        help="Host Daat Locus home copied into each sandbox as a slim config home.",
    )
    parser.add_argument("--daemon-port", type=int, default=53825)
    parser.add_argument("--startup-timeout-sec", type=float, default=90.0)
    parser.add_argument("--send-timeout-sec", type=float, default=7200.0)
    parser.add_argument("--source-build-timeout-sec", type=float, default=3600.0)
    parser.add_argument(
        "--agent-setup-timeout-multiplier",
        type=float,
        default=10.0,
        help="Default Pier agent setup timeout multiplier. Use the Pier argument after -- to override.",
    )
    parser.add_argument(
        "--agent-timeout-multiplier",
        type=float,
        default=2.0,
        help="Default Pier agent execution timeout multiplier. Use the Pier argument after -- to override.",
    )
    parser.add_argument(
        "--resume",
        default=None,
        help="Resume an existing Pier job directory instead of starting a new run.",
    )
    parser.add_argument(
        "--prune",
        action="store_true",
        help="Remove leftover egress-proxy images before and after the run.",
    )
    parser.add_argument(
        "--prune-build-cache",
        action="store_true",
        help="Also prune the Docker build cache after the run (implies --prune).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the pier command instead of running it.",
    )
    args, rest = parser.parse_known_args(argv)
    if rest and rest[0] == "--":
        rest = rest[1:]
    return args, rest


def main(argv: list[str] | None = None) -> int:
    args, pier_args = parse_args(sys.argv[1:] if argv is None else argv)
    command = build_pier_command(args, pier_args)
    if args.dry_run:
        print(" ".join(command))
        return 0
    patch_installed_pier_for_runner()
    # Pier persists trial results with the platform text encoding. On a CP936
    # host an agent reply containing characters outside GBK (for example "↔")
    # makes `Path.write_text` raise `UnicodeEncodeError` inside trial cleanup,
    # which aborts the whole job. Force UTF-8 for the pier subprocess so these
    # files are written (and read back) as UTF-8.
    os.environ["PYTHONUTF8"] = "1"
    do_prune = args.prune or args.prune_build_cache
    if do_prune:
        prune_leftover_docker_state(build_cache=False)
    try:
        return subprocess.run(command, check=False).returncode
    finally:
        if do_prune:
            prune_leftover_docker_state(build_cache=args.prune_build_cache)


if __name__ == "__main__":
    raise SystemExit(main())
