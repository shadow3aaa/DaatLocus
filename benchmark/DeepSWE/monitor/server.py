#!/usr/bin/env python3
"""Local, dependency-free progress dashboard for DeepSWE/Pier jobs.

Serves a small shadcn-styled page plus a JSON progress API that reads the
selected job directory under ``benchmark/DeepSWE/jobs``.
"""

from __future__ import annotations

import argparse
import json
import locale
import shutil
import subprocess
import time
import tomllib
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

HERE = Path(__file__).resolve().parent
JOBS_DIR = HERE.parent / "jobs"
TASKS_DIR = HERE.parent / ".cache" / "deep-swe" / "tasks"
INDEX_PATH = HERE / "index.html"

# Probe run inside a sandbox container: talk to the in-sandbox daemon and return
# just the live bits we surface per running trial (current plan step, plan list,
# runtime status, recent activity). Keeping extraction inside the container means
# only a small JSON blob crosses the docker exec boundary.
PROBE_SCRIPT = """
import json, urllib.request
home = "/tmp/daat-locus-bench/home"
token = open(home + "/runtime/daemon.token").read().strip()

def get(path):
    req = urllib.request.Request(
        "http://127.0.0.1:53825" + path,
        headers={"Authorization": "Bearer " + token},
    )
    with urllib.request.urlopen(req, timeout=6) as resp:
        return json.loads(resp.read().decode("utf-8"))

out = {}
try:
    sessions = get("/sessions")
    sid = sessions[0]["session_id"] if sessions else None
    snap = get("/dashboard/snapshot?session_id=" + sid) if sid else {}
    step = snap.get("current_plan_step") or {}
    steps = []
    for line in (snap.get("status_output") or "").splitlines():
        text = line.strip()
        if text.startswith("\u2022"):
            steps.append(text.lstrip("\u2022 ").strip())
    activity = []
    fallback_kind = None
    for item in (snap.get("live_activity_events") or [])[:10]:
        event = item.get("event") or {}
        kind = str(next(iter(event), item.get("key")))
        payload = event.get(kind)
        payload = payload if isinstance(payload, dict) else {}
        detail = ""
        for field in ("content", "title", "text", "description"):
            value = payload.get(field)
            if isinstance(value, str) and value.strip():
                detail = " ".join(value.split())[-240:]
                break
        if not detail:
            for field in ("file", "path", "name", "command", "summary"):
                value = payload.get(field)
                if isinstance(value, str) and value.strip():
                    detail = value.strip()[:160]
                    break
        if detail:
            activity.append({"kind": kind, "detail": detail})
        elif fallback_kind is None:
            fallback_kind = kind
        if len(activity) >= 3:
            break
    if not activity and fallback_kind:
        activity.append({"kind": fallback_kind, "detail": ""})
    out = {
        "plan_step": {
            "step": (step.get("step") or "").strip(),
            "status": (step.get("status") or "").strip(),
        },
        "steps": steps[:14],
        "runtime_status": snap.get("runtime_status"),
        "activity": activity,
    }
except Exception as exc:  # noqa: BLE001 - best-effort live probe
    out = {"error": str(exc)}
# ASCII-only on the wire so the host-side decode can never depend on locale.
print(json.dumps(out))
"""

_PROBE_TTL_SECONDS = 6.0
_PROBE_CACHE: dict[str, tuple[float, object]] = {}


def docker_binary() -> str | None:
    found = shutil.which("docker")
    if found:
        return found
    for candidate in (
        Path(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe"),
        Path("/usr/local/bin/docker"),
        Path("/usr/bin/docker"),
    ):
        if candidate.is_file():
            return str(candidate)
    return None


def probe_trial(name: str) -> dict | None:
    """Return the sandbox's live plan/activity for a running trial, cached briefly."""
    now = time.time()
    cached = _PROBE_CACHE.get(name)
    if cached is not None and now - cached[0] < _PROBE_TTL_SECONDS:
        return cached[1]  # type: ignore[return-value]
    docker = docker_binary()
    data = None
    if docker is not None:
        try:
            completed = subprocess.run(
                [docker, "exec", f"{name.lower()}-main-1", "python3", "-c", PROBE_SCRIPT],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                encoding="utf-8",
                errors="replace",
                timeout=15,
                check=False,
            )
            output = completed.stdout or ""
            if completed.returncode == 0 and output.strip():
                parsed = json.loads(output)
                data = None if "error" in parsed else parsed
        except Exception:  # noqa: BLE001 - the live probe is strictly best-effort
            data = None
    _PROBE_CACHE[name] = (now, data)
    return data


def resolve_task_name(token: str) -> str:
    """Map a trial's (possibly truncated) task token back to the real task name."""
    if not TASKS_DIR.is_dir():
        return token
    if (TASKS_DIR / token).is_dir():
        return token
    matches = [
        p.name
        for p in TASKS_DIR.iterdir()
        if p.is_dir() and p.name.startswith(token)
    ]
    return matches[0] if len(matches) == 1 else token


_TASK_CATALOG: dict[str, tuple[str, str]] | None = None


def task_catalog() -> dict[str, tuple[str, str]]:
    """Map every task name to its (language, category) from ``task.toml``."""
    global _TASK_CATALOG
    if _TASK_CATALOG is not None:
        return _TASK_CATALOG
    catalog: dict[str, tuple[str, str]] = {}
    if TASKS_DIR.is_dir():
        for path in TASKS_DIR.iterdir():
            if not path.is_dir():
                continue
            language, category = "?", "?"
            try:
                data = tomllib.loads((path / "task.toml").read_text(encoding="utf-8"))
                meta = data.get("metadata") or {}
                language = str(meta.get("language") or "?")
                category = str(meta.get("category") or "?")
            except (OSError, ValueError):
                pass
            catalog[path.name] = (language, category)
    _TASK_CATALOG = catalog
    return catalog


def summarize_by(trials: list[dict], catalog: dict, index: int) -> list[dict]:
    """Group trial statuses by catalog field (0 = language, 1 = category)."""
    buckets: dict[str, dict] = {}

    def bucket(key: str) -> dict:
        return buckets.setdefault(
            key,
            {
                "key": key,
                "passed": 0,
                "failed": 0,
                "error": 0,
                "running": 0,
                "pending": 0,
            },
        )

    seen: set[str] = set()
    for trial in trials:
        task = trial["task"]
        seen.add(task)
        bucket(catalog.get(task, ("?", "?"))[index])[trial["status"]] += 1
    for task, meta in catalog.items():
        if task not in seen:
            bucket(meta[index])["pending"] += 1

    result = []
    for item in buckets.values():
        item["total"] = sum(
            item[key] for key in ("passed", "failed", "error", "running", "pending")
        )
        item["evaluated"] = item["passed"] + item["failed"]
        item["pass_rate"] = (
            round(item["passed"] / item["evaluated"], 3) if item["evaluated"] else None
        )
        result.append(item)
    result.sort(key=lambda item: (-item["evaluated"], -item["total"], item["key"]))
    return result


def summarize_errors(trials: list[dict]) -> list[dict]:
    """Group errored trials by exception type."""
    groups: dict[str, list[str]] = {}
    for trial in trials:
        if trial["status"] != "error":
            continue
        groups.setdefault(trial["exception_type"] or "Unknown", []).append(
            trial["task"]
        )
    return [
        {"type": key, "count": len(tasks), "tasks": tasks}
        for key, tasks in sorted(groups.items(), key=lambda kv: (-len(kv[1]), kv[0]))
    ]


def parse_iso(value):
    if not value:
        return None
    text = value.replace("Z", "+00:00")
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        # Pier writes job-level timestamps as naive local time, while trial
        # timestamps carry an explicit UTC offset.
        parsed = parsed.replace(tzinfo=datetime.now().astimezone().tzinfo)
    return parsed


def load_json(path: Path):
    # Pier writes JSON files with the platform's default text encoding, so on a
    # CP936 Windows host an agent reply containing CJK text lands as GBK bytes
    # and plain UTF-8 decoding fails. Decode leniently instead of giving up.
    try:
        raw = path.read_bytes()
    except OSError:
        return None
    fallback = locale.getpreferredencoding(False)
    for encoding in ("utf-8", fallback, "latin-1"):
        try:
            return json.loads(raw.decode(encoding))
        except (UnicodeDecodeError, ValueError):
            continue
    return None


def list_jobs() -> list[str]:
    if not JOBS_DIR.is_dir():
        return []
    jobs = [p.name for p in JOBS_DIR.iterdir() if p.is_dir()]
    jobs.sort(reverse=True)
    return jobs


def newest_job() -> str | None:
    if not JOBS_DIR.is_dir():
        return None
    dirs = [p for p in JOBS_DIR.iterdir() if p.is_dir()]
    if not dirs:
        return None
    return max(dirs, key=lambda p: p.stat().st_mtime).name


def trial_snapshot(path: Path) -> dict:
    entry = {
        "name": path.name,
        "task": resolve_task_name(path.name.rsplit("__", 1)[0]),
        "status": "running",
        "reward": None,
        "f2p": None,
        "p2p": None,
        "f2p_passed": None,
        "f2p_total": None,
        "p2p_passed": None,
        "p2p_total": None,
        "duration_s": None,
        "finished_at": None,
        "exception_type": None,
        "exception_message": None,
    }
    result = load_json(path / "result.json")
    if result is None:
        return entry

    rewards = (result.get("verifier_result") or {}).get("rewards") or {}
    entry["reward"] = rewards.get("reward")
    entry["f2p"] = rewards.get("f2p")
    entry["p2p"] = rewards.get("p2p")
    entry["f2p_passed"] = rewards.get("f2p_passed")
    entry["f2p_total"] = rewards.get("f2p_total")
    entry["p2p_passed"] = rewards.get("p2p_passed")
    entry["p2p_total"] = rewards.get("p2p_total")
    entry["finished_at"] = result.get("finished_at")

    start = parse_iso(result.get("started_at"))
    end = parse_iso(result.get("finished_at"))
    if start and end:
        entry["duration_s"] = round((end - start).total_seconds(), 1)

    info = result.get("exception_info")
    if info:
        entry["status"] = "error"
        entry["exception_type"] = info.get("exception_type")
        entry["exception_message"] = (info.get("exception_message") or "")[:500]
    elif entry["reward"] == 1:
        entry["status"] = "passed"
    else:
        entry["status"] = "failed"
    return entry


def job_progress(job_name: str | None) -> dict:
    if not job_name:
        job_name = newest_job()
    if not job_name:
        return {"job": None, "available_jobs": list_jobs(), "trials": []}

    job_dir = JOBS_DIR / job_name
    if not job_dir.is_dir():
        return {
            "job": job_name,
            "error": f"job directory not found: {job_dir}",
            "available_jobs": list_jobs(),
            "trials": [],
        }

    job_result = load_json(job_dir / "result.json") or {}
    stats = job_result.get("stats") or {}

    trials = [
        trial_snapshot(child)
        for child in sorted(job_dir.iterdir())
        if child.is_dir()
    ]

    total = job_result.get("n_total_trials")
    if not total:
        total = len(trials) + (stats.get("n_pending_trials") or 0)

    # Status comes from each trial's own result file. Pier's job-level
    # `n_completed_trials` also counts errored trials, so mixing it into the
    # failed bucket double-counts; only `pending` is taken from the stats.
    counts = {"passed": 0, "failed": 0, "error": 0, "running": 0, "pending": 0}
    for trial in trials:
        status = trial["status"]
        if status in counts:
            counts[status] += 1
    pending = stats.get("n_pending_trials")
    counts["pending"] = (
        pending if isinstance(pending, int) else max(int(total) - len(trials), 0)
    )
    counts["running"] = stats.get("n_running_trials", counts["running"])
    counts["pending"] = stats.get(
        "n_pending_trials", max(int(total) - len(trials), 0)
    )

    rewards = [t["reward"] for t in trials if isinstance(t["reward"], (int, float))]
    mean_reward = round(sum(rewards) / len(rewards), 3) if rewards else None
    durations = [
        t["duration_s"] for t in trials if isinstance(t["duration_s"], (int, float))
    ]
    avg_duration = round(sum(durations) / len(durations), 1) if durations else None

    started = parse_iso(job_result.get("started_at"))
    finished = parse_iso(job_result.get("finished_at"))
    now = datetime.now(timezone.utc)
    elapsed_s = (
        round(((finished or now) - started).total_seconds(), 1) if started else None
    )

    concurrency = (load_json(job_dir / "config.json") or {}).get(
        "n_concurrent_trials"
    ) or 1
    remaining = counts["pending"] + counts["running"]
    eta_s = None
    if avg_duration and remaining:
        eta_s = round(avg_duration * remaining / concurrency, 1)

    trials.sort(key=lambda t: (t["status"] != "running", t["name"]))

    catalog = task_catalog()
    for trial in trials:
        language, category = catalog.get(trial["task"], ("?", "?"))
        trial["language"] = language
        trial["category"] = category
        if trial["status"] == "running":
            trial["live"] = probe_trial(trial["name"])

    evaluated = counts["passed"] + counts["failed"]
    finished_count = evaluated + counts["error"]
    pass_rate = round(counts["passed"] / evaluated, 4) if evaluated else None
    throughput = None
    if elapsed_s and elapsed_s > 0 and finished_count:
        throughput = round(finished_count / (elapsed_s / 3600), 2)

    return {
        "job": job_name,
        "available_jobs": list_jobs(),
        "job_id": job_result.get("id"),
        "started_at": job_result.get("started_at"),
        "updated_at": job_result.get("updated_at"),
        "finished_at": job_result.get("finished_at"),
        "complete": job_result.get("finished_at") is not None
        or (counts["pending"] == 0 and counts["running"] == 0),
        "total": int(total),
        "counts": counts,
        "evaluated": evaluated,
        "mean_reward": mean_reward,
        "avg_duration_s": avg_duration,
        "elapsed_s": elapsed_s,
        "eta_s": eta_s,
        "concurrency": concurrency,
        "pass_rate": pass_rate,
        "throughput_per_hour": throughput,
        "by_language": summarize_by(trials, catalog, 0),
        "by_category": summarize_by(trials, catalog, 1),
        "errors": summarize_errors(trials),
        "server_time": now.isoformat(),
        "trials": trials,
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parsed = urlparse(self.path)
        if parsed.path in ("/", "/index.html"):
            try:
                html = INDEX_PATH.read_bytes()
            except OSError:
                self._send(500, b"index.html not found", "text/plain; charset=utf-8")
                return
            self._send(200, html, "text/html; charset=utf-8")
            return
        if parsed.path == "/api/progress":
            query = parse_qs(parsed.query)
            job = (query.get("job") or [None])[0]
            payload = json.dumps(job_progress(job), ensure_ascii=False)
            self._send(200, payload.encode("utf-8"), "application/json; charset=utf-8")
            return
        self._send(404, b"not found", "text/plain; charset=utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8770)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument(
        "--job", default=None, help="Job directory name (defaults to newest)."
    )
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    url = f"http://{args.host}:{args.port}/"
    print(f"DeepSWE monitor listening on {url} (jobs dir: {JOBS_DIR})")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
