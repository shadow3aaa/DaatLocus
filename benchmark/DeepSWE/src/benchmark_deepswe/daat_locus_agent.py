from __future__ import annotations

import json
import logging
import os
import posixpath
import shutil
import shlex
import subprocess
import tarfile
import tempfile
import tomllib
import ipaddress
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from pier.agents.base import BaseAgent
from pier.agents.network import allowlist_from_urls, collect_url_values
from pier.environments.base import BaseEnvironment
from pier.models.agent.context import AgentContext
from pier.models.agent.network import NetworkAllowlist


BENCH_ROOT = "/tmp/daat-locus-bench"
SOURCE_TAR = f"{BENCH_ROOT}/daat-locus-source.tar"
SOURCE_DIR = f"{BENCH_ROOT}/source"

SOURCE_BUILD_DOMAINS = {
    ".githubusercontent.com",
    ".models.dev",
    ".openai.com",
    ".oaiusercontent.com",
    ".oaistatic.com",
    ".chatgpt.com",
    "127.0.0.1",
    "bun.sh",
    "chatgpt.com",
    "crates.io",
    "deb.debian.org",
    "deb.nodesource.com",
    "github.com",
    "index.crates.io",
    "localhost",
    "models.dev",
    "registry.npmjs.org",
    "security.debian.org",
    "sh.rustup.rs",
    "static.crates.io",
    "static.rust-lang.org",
}


def squid_safe_allowlist_domains(domains: Iterable[str]) -> list[str]:
    cleaned: set[str] = set()
    for domain in domains:
        normalized = domain.strip().lower().rstrip(".")
        if not normalized or normalized == "localhost":
            continue
        try:
            ipaddress.ip_address(normalized)
            continue
        except ValueError:
            pass
        cleaned.add(normalized)

    apex_domains = {domain for domain in cleaned if not domain.startswith(".")}
    return sorted(
        domain
        for domain in cleaned
        if not (domain.startswith(".") and domain[1:] in apex_domains)
    )


def find_daat_locus_source_root(start: Path | None = None) -> Path:
    current = (start or Path(__file__).resolve()).resolve()
    if current.is_file():
        current = current.parent
    for candidate in (current, *current.parents):
        if (candidate / "Cargo.toml").is_file() and (candidate / ".git").exists():
            return candidate
    raise RuntimeError(
        "could not find Daat Locus source root; pass daat_locus_source or set DAAT_LOCUS_SOURCE"
    )


def daat_locus_source_files(source_root: Path) -> list[Path]:
    result = subprocess.run(
        ["git", "-C", str(source_root), "ls-files", "-co", "--exclude-standard", "-z"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            "git ls-files failed while collecting Daat Locus source package:\n"
            f"{result.stderr.decode('utf-8', errors='replace')}"
        )
    return [
        Path(name)
        for name in result.stdout.decode("utf-8", errors="surrogateescape").split("\0")
        if name
    ]


def create_source_archive(source_root: Path, archive_path: Path) -> int:
    files = daat_locus_source_files(source_root)
    with tarfile.open(archive_path, "w") as archive:
        for relative in files:
            source = source_root / relative
            if source.is_file() or source.is_symlink():
                archive.add(source, arcname=relative.as_posix(), recursive=False)
    return len(files)


def _replace_section_auth_file(text: str, provider: str, new_path: str) -> str:
    """Repoint a provider's ``auth_file`` at ``new_path`` inside its TOML section."""
    headers = {
        f"providers.{provider}",
        f'providers."{provider}"',
        f"providers.'{provider}'",
    }
    inside = False
    result: list[str] = []
    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            inside = stripped[1:-1].strip() in headers
            result.append(line)
            continue
        if (
            inside
            and "=" in stripped
            and stripped.split("=", 1)[0].strip() == "auth_file"
        ):
            indent = line[: len(line) - len(line.lstrip())]
            newline = "\n" if line.endswith("\n") else ""
            result.append(f'{indent}auth_file = "{new_path}"{newline}')
            continue
        result.append(line)
    return "".join(result)


class DaatLocusAgent(BaseAgent):
    SUPPORTS_WINDOWS = False

    @staticmethod
    def name() -> str:
        return "daat-locus"

    def __init__(
        self,
        logs_dir: Path,
        model_name: str | None = None,
        logger: logging.Logger | None = None,
        extra_env: dict[str, str] | None = None,
        daat_locus_bin: str | None = None,
        daat_locus_source: str | None = None,
        daat_home: str | None = None,
        container_bin: str = "/tmp/daat-locus-bench/bin/daat-locus",
        container_home: str = "/tmp/daat-locus-bench/home",
        daemon_port: int = 53825,
        startup_timeout_sec: float = 90.0,
        send_timeout_sec: float = 7200.0,
        source_build_timeout_sec: float = 3600.0,
        forward_env: str = "OPENAI_API_KEY,CODEX_REFRESH_TOKEN_URL_OVERRIDE,CODEX_CLIENT_VERSION_OVERRIDE",
        **kwargs: Any,
    ) -> None:
        super().__init__(logs_dir=logs_dir, model_name=model_name, logger=logger, **kwargs)
        self._extra_env = extra_env or {}
        self._host_bin = daat_locus_bin
        self._host_source = daat_locus_source or os.environ.get("DAAT_LOCUS_SOURCE")
        self._host_home = daat_home or os.environ.get("DAAT_LOCUS_HOME") or str(
            Path.home() / ".daat-locus"
        )
        self._container_bin = container_bin
        self._container_home = container_home
        self._daemon_port = int(daemon_port)
        self._startup_timeout_sec = float(startup_timeout_sec)
        self._send_timeout_sec = float(send_timeout_sec)
        self._source_build_timeout_sec = float(source_build_timeout_sec)
        self._forward_env_names = [
            name.strip() for name in forward_env.split(",") if name.strip()
        ]
        self._tmp_home: tempfile.TemporaryDirectory[str] | None = None
        self._python_cmd = "python3"

    def version(self) -> str:
        return "local"

    def network_allowlist(self) -> NetworkAllowlist:
        url_values = list(self._extra_env.values())
        url_values.extend(self._config_url_values())
        for name in self._forward_env_names:
            if name in os.environ:
                url_values.append(os.environ[name])
        allowlist = allowlist_from_urls(
            url_values,
            default_domains=SOURCE_BUILD_DOMAINS,
        )
        return NetworkAllowlist(domains=squid_safe_allowlist_domains(allowlist.domains))

    async def setup(self, environment: BaseEnvironment) -> None:
        self.logs_dir.mkdir(parents=True, exist_ok=True)
        await self._ensure_python(environment)
        await self._install_binary(environment)
        await self._upload_home(environment)
        await self._upload_bridge(environment)

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        env_paths = environment.env_paths
        host_instruction = self.logs_dir / "instruction.md"
        host_reply = self.logs_dir / "reply.txt"
        host_metadata = self.logs_dir / "daat-locus-metadata.json"
        host_usage = self.logs_dir / "daat-locus-usage.json"
        host_stdout = self.logs_dir / "daat-locus-bridge-output.txt"
        host_home_logs = self.logs_dir / "daat-locus-home-logs"
        host_instruction.write_text(instruction, encoding="utf-8")

        container_instruction = str(env_paths.agent_dir / "instruction.md")
        container_reply = str(env_paths.agent_dir / "reply.txt")
        container_metadata = str(env_paths.agent_dir / "daat-locus-metadata.json")
        container_usage = str(env_paths.agent_dir / "daat-locus-usage.json")
        container_daemon_log = str(env_paths.agent_dir / "daat-locus-daemon.log")
        container_pid = "/tmp/daat-locus-bench/daemon.pid"

        await environment.upload_file(host_instruction, container_instruction)

        command = " ".join(
            shlex.quote(part)
            for part in [
                self._python_cmd,
                "/tmp/daat-locus-bench/bridge.py",
                "--daemon-bin",
                self._container_bin,
                "--home",
                self._container_home,
                "--port",
                str(self._daemon_port),
                "--project-dir",
                "/app",
                "--title",
                "DeepSWE",
                "--instruction-file",
                container_instruction,
                "--reply-file",
                container_reply,
                "--metadata-file",
                container_metadata,
                "--usage-file",
                container_usage,
                "--daemon-log",
                container_daemon_log,
                "--pid-file",
                container_pid,
                "--startup-timeout-sec",
                str(self._startup_timeout_sec),
                "--send-timeout-sec",
                str(self._send_timeout_sec),
            ]
        )

        try:
            result = await environment.exec(
                command=command,
                cwd="/app",
                env=self._agent_env(environment),
                timeout_sec=int(self._send_timeout_sec + self._startup_timeout_sec + 60),
            )
        finally:
            await self._download_dir_if_possible(
                environment,
                f"{self._container_home.rstrip('/')}/logs",
                host_home_logs,
            )
        host_stdout.write_text(
            f"return_code={result.return_code}\n\nstdout:\n{result.stdout or ''}\n\nstderr:\n{result.stderr or ''}\n",
            encoding="utf-8",
        )

        await self._download_if_possible(environment, container_reply, host_reply)
        await self._download_if_possible(environment, container_metadata, host_metadata)
        await self._download_if_possible(environment, container_usage, host_usage)

        metadata: dict[str, Any] = {}
        if host_metadata.exists():
            metadata = json.loads(host_metadata.read_text(encoding="utf-8"))
        context.metadata = {
            "daat_locus": metadata,
            "reply_path": str(host_reply),
            "bridge_output_path": str(host_stdout),
            "usage_path": str(host_usage),
        }

        if result.return_code != 0:
            output = result.stdout or result.stderr or "no output"
            raise RuntimeError(f"Daat Locus DeepSWE bridge failed: {output}")

    async def _ensure_python(self, environment: BaseEnvironment) -> None:
        result = await environment.exec(
            "command -v python3 || command -v python",
            env=self._agent_env(environment),
            timeout_sec=30,
            user="root",
        )
        if result.return_code == 0 and result.stdout:
            self._python_cmd = result.stdout.strip().splitlines()[-1]
            return

        install = (
            "if command -v apk >/dev/null 2>&1; then "
            "apk add --no-cache python3; "
            "elif command -v apt-get >/dev/null 2>&1; then "
            "apt-get update && apt-get install -y python3; "
            "elif command -v yum >/dev/null 2>&1; then "
            "yum install -y python3; "
            "else exit 127; fi"
        )
        installed = await environment.exec(
            install,
            env=self._agent_env(environment),
            timeout_sec=180,
            user="root",
        )
        if installed.return_code != 0:
            raise RuntimeError(
                "python is required in the DeepSWE container and automatic install failed: "
                f"{installed.stdout or installed.stderr or 'no output'}"
            )
        self._python_cmd = "python3"

    def _config_url_values(self) -> list[str]:
        config_path = Path(self._host_home).expanduser() / "config" / "config.toml"
        if not config_path.exists():
            return []
        try:
            data = tomllib.loads(config_path.read_text(encoding="utf-8"))
        except Exception as exc:  # noqa: BLE001 - config parsing is best-effort for network setup
            self.logger.debug("failed to parse Daat Locus config for network allowlist: %s", exc)
            return []
        return collect_url_values(data)

    async def _install_binary(self, environment: BaseEnvironment) -> None:
        await environment.exec(
            f"mkdir -p {shlex.quote(posixpath.dirname(self._container_bin))}",
            env=self._agent_env(environment),
            timeout_sec=30,
            user="root",
        )
        if self._host_bin:
            host_bin = Path(self._host_bin)
            if not host_bin.is_file():
                raise RuntimeError(f"Daat Locus binary path not found: {host_bin}")
            await environment.upload_file(host_bin, self._container_bin)
            await environment.exec(
                f"chmod +x {shlex.quote(self._container_bin)}",
                env=self._agent_env(environment),
                timeout_sec=30,
                user="root",
            )
            return

        await self._install_from_source(environment)

    async def _install_from_source(self, environment: BaseEnvironment) -> None:
        source_root = (
            Path(self._host_source).expanduser().resolve()
            if self._host_source
            else find_daat_locus_source_root()
        )
        if not (source_root / "Cargo.toml").is_file():
            raise RuntimeError(
                f"Daat Locus source root does not contain Cargo.toml: {source_root}"
            )

        with tempfile.NamedTemporaryFile(
            prefix="daat-locus-source-", suffix=".tar", delete=False
        ) as temp_archive:
            archive_path = Path(temp_archive.name)
        try:
            file_count = create_source_archive(source_root, archive_path)
            self.logger.info(
                "uploading Daat Locus source package from %s (%s files)",
                source_root,
                file_count,
            )
            await environment.exec(
                f"rm -rf {shlex.quote(SOURCE_DIR)} {shlex.quote(SOURCE_TAR)} && mkdir -p {shlex.quote(BENCH_ROOT)} {shlex.quote(posixpath.dirname(self._container_bin))}",
                env=self._agent_env(environment),
                timeout_sec=60,
                user="root",
            )
            await environment.upload_file(archive_path, SOURCE_TAR)
        finally:
            archive_path.unlink(missing_ok=True)

        await environment.exec(
            f"mkdir -p {shlex.quote(SOURCE_DIR)} && tar -xf {shlex.quote(SOURCE_TAR)} -C {shlex.quote(SOURCE_DIR)}",
            env=self._agent_env(environment),
            timeout_sec=180,
            user="root",
        )
        await self._ensure_source_build_tools(environment)

        build = (
            "set -e; "
            'export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"; '
            f"cd {shlex.quote(SOURCE_DIR)}; "
            "cargo build --release --locked; "
            f"cp target/release/daat-locus {shlex.quote(self._container_bin)}; "
            f"chmod +x {shlex.quote(self._container_bin)}"
        )
        built = await environment.exec(
            build,
            env=self._agent_env(environment),
            timeout_sec=int(self._source_build_timeout_sec),
            user="root",
        )
        if built.return_code != 0:
            raise RuntimeError(
                "building Daat Locus from source failed:\n"
                f"{built.stdout or built.stderr or 'no output'}"
            )

    async def _ensure_source_build_tools(self, environment: BaseEnvironment) -> None:
        setup = r"""
set -e
need_packages=0
for cmd in curl git tar bash; do
  command -v "$cmd" >/dev/null 2>&1 || need_packages=1
done
if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
  need_packages=1
fi
if ! command -v pkg-config >/dev/null 2>&1 && ! command -v pkgconf >/dev/null 2>&1; then
  need_packages=1
fi
if command -v pkg-config >/dev/null 2>&1 && ! pkg-config --exists glib-2.0 gtk+-3.0 ayatana-appindicator3-0.1; then
  need_packages=1
fi
if [ "$need_packages" -eq 1 ]; then
  if command -v apk >/dev/null 2>&1; then
    apk add --no-cache bash ca-certificates curl git tar unzip build-base pkgconf openssl-dev
  elif command -v apt-get >/dev/null 2>&1; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y bash ca-certificates curl git tar unzip build-essential pkg-config libssl-dev libgtk-3-dev libayatana-appindicator3-dev
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y bash ca-certificates curl git tar unzip gcc gcc-c++ make pkgconf-pkg-config openssl-devel
  elif command -v yum >/dev/null 2>&1; then
    yum install -y bash ca-certificates curl git tar unzip gcc gcc-c++ make pkgconfig openssl-devel
  else
    echo "missing required build tools and no supported package manager was found"
    exit 127
  fi
fi
export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
export PATH="$HOME/.cargo/bin:$HOME/.bun/bin:$PATH"
if ! command -v bun >/dev/null 2>&1; then
  curl -fsSL https://bun.sh/install | bash
fi
"""
        result = await environment.exec(
            setup,
            env=self._agent_env(environment),
            timeout_sec=600,
            user="root",
        )
        if result.return_code != 0:
            raise RuntimeError(
                "installing Daat Locus source-build dependencies failed:\n"
                f"{result.stdout or result.stderr or 'no output'}"
            )

    async def _upload_home(self, environment: BaseEnvironment) -> None:
        host_home = Path(self._host_home).expanduser()
        if not host_home.exists():
            await environment.exec(
                f"mkdir -p {shlex.quote(self._container_home)}",
                env=self._agent_env(environment),
                timeout_sec=30,
                user="root",
            )
            return

        slim_home = self._build_slim_home(host_home)
        await environment.exec(
            f"rm -rf {shlex.quote(self._container_home)} && mkdir -p {shlex.quote(posixpath.dirname(self._container_home.rstrip('/')))}",
            env=self._agent_env(environment),
            timeout_sec=30,
            user="root",
        )
        await environment.upload_dir(slim_home, self._container_home)

    async def _upload_bridge(self, environment: BaseEnvironment) -> None:
        bridge_path = Path(__file__).with_name("bridge.py")
        await environment.exec(
            "mkdir -p /tmp/daat-locus-bench",
            env=self._agent_env(environment),
            timeout_sec=30,
            user="root",
        )
        await environment.upload_file(bridge_path, "/tmp/daat-locus-bench/bridge.py")

    def _build_slim_home(self, host_home: Path) -> Path:
        self._tmp_home = tempfile.TemporaryDirectory(prefix="daat-locus-deepswe-home-")
        target = Path(self._tmp_home.name)
        for name in ("config", "skills"):
            source = host_home / name
            if source.exists():
                shutil.copytree(source, target / name, dirs_exist_ok=True)
        models_cache = host_home / "cache" / "models-dev-api.json"
        if models_cache.exists():
            (target / "cache").mkdir(parents=True, exist_ok=True)
            shutil.copy2(models_cache, target / "cache" / "models-dev-api.json")
        self._rewrite_provider_auth_files(host_home, target)
        return target

    def _rewrite_provider_auth_files(self, host_home: Path, target: Path) -> None:
        """Copy provider credential files into the sandbox and repoint ``auth_file``.

        Sandboxes run on Linux with a slim home, so a host credential path (for
        example one under the Windows user profile) is unreachable. Without this,
        the daemon rejects the whole config because a configured-but-unusable
        provider makes readiness incomplete, even when the run never uses it.
        """
        config_path = target / "config" / "config.toml"
        if not config_path.is_file():
            return
        text = config_path.read_text(encoding="utf-8")
        try:
            data = tomllib.loads(text)
        except tomllib.TOMLDecodeError:
            return
        providers = data.get("providers")
        if not isinstance(providers, dict):
            return
        for name, provider in providers.items():
            if not isinstance(provider, dict):
                continue
            auth_file = provider.get("auth_file")
            if not isinstance(auth_file, str) or not auth_file.strip():
                continue
            host_path = Path(auth_file.strip()).expanduser()
            if not host_path.is_absolute():
                host_path = host_home / host_path
            if not host_path.is_file():
                continue
            destination = target / "auth" / name / host_path.name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(host_path, destination)
            container_path = (
                f"{self._container_home.rstrip('/')}/auth/{name}/{host_path.name}"
            )
            text = _replace_section_auth_file(text, name, container_path)
        config_path.write_text(text, encoding="utf-8")

    def _run_env(self) -> dict[str, str]:
        env = dict(self._extra_env)
        env["DAAT_LOCUS_HOME"] = self._container_home
        for name in self._forward_env_names:
            if name not in env and name in os.environ:
                env[name] = os.environ[name]
        return env

    def _agent_env(
        self,
        environment: BaseEnvironment,
        extra: dict[str, str] | None = None,
    ) -> dict[str, str] | None:
        env = self._run_env()
        if extra:
            env.update(extra)
        return environment.agent_process_env(env)

    async def _download_if_possible(
        self,
        environment: BaseEnvironment,
        source_path: str,
        target_path: Path,
    ) -> None:
        try:
            await environment.download_file(source_path, target_path)
        except Exception as exc:  # noqa: BLE001 - logs may already be mounted
            self.logger.debug("download %s failed: %s", source_path, exc)

    async def _download_dir_if_possible(
        self,
        environment: BaseEnvironment,
        source_path: str,
        target_path: Path,
    ) -> None:
        try:
            if target_path.exists() and not target_path.is_dir():
                target_path.unlink()
            await environment.download_dir(source_path, target_path)
        except Exception as exc:  # noqa: BLE001 - diagnostics should not mask agent errors
            self.logger.debug("download dir %s failed: %s", source_path, exc)
