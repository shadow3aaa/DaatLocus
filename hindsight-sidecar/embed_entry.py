import sys
from pathlib import Path

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
    main()
