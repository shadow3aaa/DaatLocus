import os
import sys
from pathlib import Path

os.environ.setdefault("PYTHONUTF8", "1")
os.environ.setdefault("PYTHONIOENCODING", "utf-8")

for stream_name in ("stdout", "stderr"):
    stream = getattr(sys, stream_name, None)
    if hasattr(stream, "reconfigure"):
        stream.reconfigure(encoding="utf-8", errors="replace")

from hindsight_embed.daemon_embed_manager import DaemonEmbedManager

def _find_bundled_api_command(self):
    exe_name = "hindsight-api.exe" if sys.platform == "win32" else "hindsight-api"
    candidate = Path(sys.executable).resolve().parent / exe_name
    if candidate.exists():
        return [str(candidate)]
    return [exe_name]


DaemonEmbedManager._find_api_command = _find_bundled_api_command

from hindsight_embed.cli import main


if __name__ == "__main__":
    import multiprocessing

    multiprocessing.freeze_support()
    main()
