from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


COMMIT_INSTRUCTION = (
    "When you are finished, commit every code change in /app. "
    "DeepSWE scores only the diff from the task base commit to final HEAD."
)

MODEL_RETRY_PATTERN = re.compile(
    r"run_agent_turn retry #(?P<attempt>\d+) after (?P<backoff_ms>\d+)ms "
    r"\(model=(?P<model>[^,]+), messages=(?P<messages>\d+), tools=(?P<tools>\d+), "
    r"estimated_input_tokens=(?P<estimated_input_tokens>\d+)\): (?P<error>.*)"
)


def build_instruction(raw: str) -> str:
    stripped = raw.rstrip()
    if COMMIT_INSTRUCTION in stripped:
        return stripped + "\n"
    return f"{stripped}\n\n{COMMIT_INSTRUCTION}\n"


def ensure_daemon_port(config_text: str, port: int) -> str:
    lines = config_text.splitlines(keepends=True)
    daemon_index = next(
        (index for index, line in enumerate(lines) if line.strip() == "[daemon]"),
        None,
    )
    if daemon_index is None:
        suffix = "" if config_text.endswith("\n") else "\n"
        return f"{config_text}{suffix}\n[daemon]\nport = {port}\n"

    section_end = next(
        (
            index
            for index, line in enumerate(lines[daemon_index + 1 :], start=daemon_index + 1)
            if line.strip().startswith("[") and line.strip().endswith("]")
        ),
        len(lines),
    )
    for index in range(daemon_index + 1, section_end):
        if re.match(r"^\s*port\s*=", lines[index]):
            newline = "\n" if lines[index].endswith("\n") else ""
            lines[index] = f"port = {port}{newline}"
            return "".join(lines)

    lines.insert(daemon_index + 1, f"port = {port}\n")
    return "".join(lines)


def patch_config_port(home: Path, port: int) -> None:
    config_path = home / "config" / "config.toml"
    if not config_path.exists():
        raise RuntimeError(
            f"Daat Locus config not found at {config_path}; pass daat_home or prepare the container home"
        )
    config_path.write_text(
        ensure_daemon_port(config_path.read_text(encoding="utf-8"), port),
        encoding="utf-8",
    )


def prepare_visible_home_logs(home: Path, agent_log_dir: Path) -> Path:
    mirror = agent_log_dir / "daat-locus-home-logs"
    mirror.mkdir(parents=True, exist_ok=True)

    home_logs = home / "logs"
    home_logs.parent.mkdir(parents=True, exist_ok=True)
    if home_logs.is_symlink():
        try:
            if home_logs.resolve() == mirror.resolve():
                return mirror
        except OSError:
            pass
        home_logs.unlink()
    elif home_logs.exists():
        try:
            if not any(home_logs.iterdir()):
                home_logs.rmdir()
        except OSError:
            pass

    if not home_logs.exists():
        try:
            home_logs.symlink_to(mirror, target_is_directory=True)
        except OSError:
            home_logs.mkdir(parents=True, exist_ok=True)
    return mirror


def request_json(
    method: str,
    url: str,
    *,
    token: str | None = None,
    body: dict[str, Any] | None = None,
    timeout_sec: float = 30.0,
) -> Any:
    data = None if body is None else json.dumps(body).encode("utf-8")
    headers = {"Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=timeout_sec) as response:
        raw = response.read()
    if not raw:
        return None
    return json.loads(raw.decode("utf-8"))


def unix_ms() -> int:
    return int(time.time() * 1000)


def elapsed_ms(start: float, end: float | None = None) -> int:
    return int(((time.monotonic() if end is None else end) - start) * 1000)


def safe_request_json(
    method: str,
    url: str,
    *,
    token: str | None = None,
    body: dict[str, Any] | None = None,
    timeout_sec: float = 30.0,
) -> dict[str, Any]:
    try:
        value = request_json(method, url, token=token, body=body, timeout_sec=timeout_sec)
        return {"ok": True, "value": value}
    except Exception as exc:  # noqa: BLE001 - diagnostics must not mask benchmark result
        return {"ok": False, "error": str(exc)}


def zero_usage() -> dict[str, int]:
    return {
        "input_tokens": 0,
        "cached_input_tokens": 0,
        "output_tokens": 0,
        "reasoning_output_tokens": 0,
        "total_tokens": 0,
    }


def add_usage(left: dict[str, int], right: dict[str, Any] | None) -> None:
    if not isinstance(right, dict):
        return
    for key in zero_usage():
        value = right.get(key, 0)
        if isinstance(value, int | float):
            left[key] += int(value)


def summarize_token_usage(token_usage: dict[str, Any] | None) -> dict[str, Any]:
    summary = {
        "main_model": None,
        "judge_model": None,
        "efficient_model": None,
        "main": zero_usage(),
        "judge": zero_usage(),
        "combined": zero_usage(),
    }
    if not isinstance(token_usage, dict):
        return summary

    summary["main_model"] = token_usage.get("main_model")
    summary["judge_model"] = token_usage.get("judge_model")
    summary["efficient_model"] = token_usage.get("efficient_model")
    for role in ("main", "judge"):
        info = token_usage.get(role)
        if isinstance(info, dict):
            total = info.get("total_token_usage")
            add_usage(summary[role], total)
            add_usage(summary["combined"], total)
    return summary


def extract_snapshot_token_usage(snapshot_result: dict[str, Any]) -> dict[str, Any] | None:
    if not snapshot_result.get("ok"):
        return None
    value = snapshot_result.get("value")
    if not isinstance(value, dict):
        return None
    token_usage = value.get("token_usage")
    return token_usage if isinstance(token_usage, dict) else None


def extract_status_summary_token_usage(
    status_result: dict[str, Any],
    session_id: str | None,
) -> dict[str, Any] | None:
    if not status_result.get("ok"):
        return None
    value = status_result.get("value")
    if not isinstance(value, dict):
        return None
    sessions = value.get("sessions")
    if not isinstance(sessions, list):
        return None
    selected = None
    for item in sessions:
        if not isinstance(item, dict):
            continue
        session = item.get("session")
        item_session_id = session.get("session_id") if isinstance(session, dict) else None
        if session_id is None or item_session_id == session_id:
            selected = item
            break
    if not isinstance(selected, dict):
        return None
    dashboard = selected.get("dashboard")
    if not isinstance(dashboard, dict):
        return None
    token_usage = dashboard.get("token_usage")
    return token_usage if isinstance(token_usage, dict) else None


def parse_model_retry_events(log_text: str) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    lines = log_text.splitlines()
    for index, line in enumerate(lines):
        match = MODEL_RETRY_PATTERN.search(line)
        if not match:
            continue
        error_lines = [match.group("error").rstrip()]
        cursor = index + 1
        while cursor < len(lines):
            next_line = lines[cursor]
            if re.match(r"^\d{4}-\d{2}-\d{2}T", next_line):
                break
            if next_line.strip():
                error_lines.append(next_line.rstrip())
            cursor += 1
        events.append(
            {
                "timestamp": line.split("Z", 1)[0] + "Z" if "Z" in line else None,
                "attempt": int(match.group("attempt")),
                "backoff_ms": int(match.group("backoff_ms")),
                "model": match.group("model"),
                "messages": int(match.group("messages")),
                "tools": int(match.group("tools")),
                "estimated_input_tokens": int(match.group("estimated_input_tokens")),
                "error": "\n".join(error_lines).strip(),
            }
        )
    return events


def collect_log_diagnostics(home: Path) -> dict[str, Any]:
    log_path = home / "logs" / "daat-locus.log"
    diagnostics: dict[str, Any] = {
        "log_path": str(log_path),
        "model_retry_events": [],
        "model_retry_count": 0,
        "stream_body_read_failures": 0,
        "request_failed_retry_warnings": 0,
    }
    if not log_path.exists():
        diagnostics["missing"] = True
        return diagnostics
    text = log_path.read_text(encoding="utf-8", errors="replace")
    retry_events = parse_model_retry_events(text)
    diagnostics["model_retry_events"] = retry_events
    diagnostics["model_retry_count"] = len(retry_events)
    diagnostics["stream_body_read_failures"] = text.count("streaming response body read failed")
    diagnostics["request_failed_retry_warnings"] = text.count("request failed; retry #")
    return diagnostics


def write_usage_report(path: Path, usage: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(usage, indent=2, ensure_ascii=False), encoding="utf-8")


def wait_for_status(base_url: str, timeout_sec: float) -> None:
    deadline = time.monotonic() + timeout_sec
    last_error = ""
    while time.monotonic() < deadline:
        try:
            status = request_json("GET", f"{base_url}/status", timeout_sec=5.0)
            state = status.get("state") if isinstance(status, dict) else None
            if state == "ready":
                return
            if state == "failed":
                raise RuntimeError("daemon reported failed state")
            last_error = f"daemon state is {state or 'unknown'}"
        except Exception as exc:  # noqa: BLE001 - diagnostic loop
            last_error = str(exc)
            time.sleep(0.5)
    raise RuntimeError(f"daemon did not become ready within {timeout_sec}s: {last_error}")


def wait_for_token(home: Path, timeout_sec: float) -> str:
    token_path = home / "runtime" / "daemon.token"
    deadline = time.monotonic() + timeout_sec
    while time.monotonic() < deadline:
        if token_path.exists():
            token = token_path.read_text(encoding="utf-8").strip()
            if token:
                return token
        time.sleep(0.2)
    raise RuntimeError(f"daemon token not created at {token_path}")


def run_checked(command: list[str], *, cwd: str | None = None) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        command,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"command failed with exit {result.returncode}: {' '.join(command)}\n{result.stdout}"
        )
    return result


def fallback_commit(project_dir: str, message: str) -> dict[str, Any]:
    run_checked(["git", "config", "--global", "--add", "safe.directory", project_dir])
    run_checked(["git", "config", "user.name", "Daat Locus"], cwd=project_dir)
    run_checked(["git", "config", "user.email", "daat-locus@local"], cwd=project_dir)
    run_checked(["git", "add", "-A"], cwd=project_dir)
    diff = subprocess.run(
        ["git", "diff", "--cached", "--quiet"],
        cwd=project_dir,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    if diff.returncode == 0:
        head = run_checked(["git", "rev-parse", "--short", "HEAD"], cwd=project_dir)
        return {"created": False, "head": head.stdout.strip()}
    commit = subprocess.run(
        ["git", "commit", "--no-verify", "-m", message],
        cwd=project_dir,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    if commit.returncode != 0:
        raise RuntimeError(f"fallback commit failed with exit {commit.returncode}:\n{commit.stdout}")
    head = run_checked(["git", "rev-parse", "--short", "HEAD"], cwd=project_dir)
    return {"created": True, "head": head.stdout.strip(), "output": commit.stdout}


def start_daemon(args: argparse.Namespace) -> subprocess.Popen[bytes]:
    env = os.environ.copy()
    env["DAAT_LOCUS_HOME"] = str(args.home)
    log = open(args.daemon_log, "ab", buffering=0)
    process = subprocess.Popen(
        [str(args.daemon_bin), "serve"],
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        env=env,
        start_new_session=True,
    )
    Path(args.pid_file).write_text(str(process.pid), encoding="utf-8")
    return process


def shutdown_daemon(base_url: str, token: str, process: subprocess.Popen[bytes] | None) -> None:
    try:
        request_json("POST", f"{base_url}/shutdown", token=token, timeout_sec=5.0)
    except Exception:
        pass
    if process is None:
        return
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except Exception:
            process.terminate()


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--daemon-bin", required=True)
    parser.add_argument("--home", required=True, type=Path)
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--project-dir", default="/app")
    parser.add_argument("--title", default="DeepSWE")
    parser.add_argument("--instruction-file", required=True)
    parser.add_argument("--reply-file", required=True)
    parser.add_argument("--metadata-file", required=True)
    parser.add_argument("--usage-file", required=True)
    parser.add_argument("--daemon-log", required=True)
    parser.add_argument("--pid-file", required=True)
    parser.add_argument("--startup-timeout-sec", type=float, default=90.0)
    parser.add_argument("--send-timeout-sec", type=float, default=7200.0)
    parser.add_argument("--commit-message", default="Daat Locus solution")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    bridge_started_monotonic = time.monotonic()
    bridge_started_at_ms = unix_ms()
    args.home.mkdir(parents=True, exist_ok=True)
    Path(args.daemon_log).parent.mkdir(parents=True, exist_ok=True)

    patch_config_port(args.home, args.port)

    daemon_process: subprocess.Popen[bytes] | None = None
    metadata: dict[str, Any] = {
        "project_dir": args.project_dir,
        "daemon_port": args.port,
    }
    usage_report: dict[str, Any] = {
        "version": 1,
        "project_dir": args.project_dir,
        "daemon_port": args.port,
        "started_at_ms": bridge_started_at_ms,
        "finished_at_ms": None,
        "elapsed_ms": None,
        "phases": {},
        "session_id": None,
        "token_usage": {
            "snapshot": None,
            "status_summary": None,
            "summary": summarize_token_usage(None),
        },
        "diagnostics": {},
    }
    try:
        metadata["home_logs_dir"] = str(
            prepare_visible_home_logs(args.home, Path(args.daemon_log).parent)
        )
        phase_start = time.monotonic()
        daemon_process = start_daemon(args)
        base_url = f"http://localhost:{args.port}"
        wait_for_status(base_url, args.startup_timeout_sec)
        token = wait_for_token(args.home, args.startup_timeout_sec)
        usage_report["phases"]["startup_ms"] = elapsed_ms(phase_start)

        phase_start = time.monotonic()
        session = request_json(
            "POST",
            f"{base_url}/sessions",
            token=token,
            body={"project_dir": args.project_dir, "title": args.title},
            timeout_sec=30.0,
        )
        session_id = str(session["session_id"])
        metadata["session_id"] = session_id
        usage_report["session_id"] = session_id
        usage_report["phases"]["session_create_ms"] = elapsed_ms(phase_start)

        raw_instruction = Path(args.instruction_file).read_text(encoding="utf-8")
        phase_start = time.monotonic()
        response = request_json(
            "POST",
            f"{base_url}/send",
            token=token,
            body={
                "session_id": session_id,
                "message": build_instruction(raw_instruction),
            },
            timeout_sec=args.send_timeout_sec,
        )
        usage_report["phases"]["send_ms"] = elapsed_ms(phase_start)
        metadata["send_response"] = response
        reply = (response or {}).get("reply_message") or ""
        Path(args.reply_file).write_text(reply, encoding="utf-8")

        phase_start = time.monotonic()
        snapshot_result = safe_request_json(
            "GET",
            f"{base_url}/dashboard/snapshot?session_id={session_id}",
            token=token,
            timeout_sec=30.0,
        )
        status_result = safe_request_json(
            "GET",
            f"{base_url}/status/summary?session_id={session_id}",
            token=token,
            timeout_sec=30.0,
        )
        usage_report["phases"]["usage_collect_ms"] = elapsed_ms(phase_start)
        snapshot_token_usage = extract_snapshot_token_usage(snapshot_result)
        status_token_usage = extract_status_summary_token_usage(status_result, session_id)
        usage_report["token_usage"] = {
            "snapshot": snapshot_token_usage,
            "status_summary": status_token_usage,
            "summary": summarize_token_usage(snapshot_token_usage or status_token_usage),
            "snapshot_request": snapshot_result if not snapshot_result.get("ok") else {"ok": True},
            "status_summary_request": status_result
            if not status_result.get("ok")
            else {"ok": True},
        }
        metadata["usage_file"] = str(args.usage_file)

        phase_start = time.monotonic()
        metadata["fallback_commit"] = fallback_commit(args.project_dir, args.commit_message)
        usage_report["phases"]["fallback_commit_ms"] = elapsed_ms(phase_start)
        Path(args.metadata_file).write_text(json.dumps(metadata, indent=2), encoding="utf-8")
        return 0
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        metadata["error"] = f"HTTP {exc.code}: {body}"
        Path(args.metadata_file).write_text(json.dumps(metadata, indent=2), encoding="utf-8")
        print(metadata["error"], file=sys.stderr)
        return 1
    except Exception as exc:  # noqa: BLE001 - bridge is a process boundary
        metadata["error"] = str(exc)
        Path(args.metadata_file).write_text(json.dumps(metadata, indent=2), encoding="utf-8")
        print(str(exc), file=sys.stderr)
        return 1
    finally:
        usage_report["finished_at_ms"] = unix_ms()
        usage_report["elapsed_ms"] = elapsed_ms(bridge_started_monotonic)
        usage_report["diagnostics"] = collect_log_diagnostics(args.home)
        write_usage_report(Path(args.usage_file), usage_report)
        token_path = args.home / "runtime" / "daemon.token"
        if token_path.exists():
            shutdown_daemon(f"http://localhost:{args.port}", token_path.read_text().strip(), daemon_process)


if __name__ == "__main__":
    raise SystemExit(main())
