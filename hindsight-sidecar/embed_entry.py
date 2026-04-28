import json
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

if sys.platform == "win32":
    import msvcrt
    import hindsight_embed.profile_manager as profile_manager

    def _with_lock_cursor_at_start(file_obj, lock_mode):
        pos = file_obj.tell()
        file_obj.seek(0)
        try:
            msvcrt.locking(file_obj.fileno(), lock_mode, 1)
        finally:
            file_obj.seek(pos)

    def _lock_file(file_obj):
        _with_lock_cursor_at_start(file_obj, msvcrt.LK_LOCK)

    def _unlock_file(file_obj):
        _with_lock_cursor_at_start(file_obj, msvcrt.LK_UNLCK)

    def _save_metadata(self, metadata):
        self._ensure_directories()

        metadata_file = self._get_metadata_file()
        temp_file = metadata_file.with_suffix(".json.tmp")

        with open(temp_file, "w", encoding="utf-8") as file_obj:
            profile_manager.lock_file(file_obj)
            try:
                json.dump(
                    {"version": metadata.version, "profiles": metadata.profiles},
                    file_obj,
                    indent=2,
                )
                file_obj.flush()
                os.fsync(file_obj.fileno())
            finally:
                profile_manager.unlock_file(file_obj)

        temp_file.replace(metadata_file)

    profile_manager.lock_file = _lock_file
    profile_manager.unlock_file = _unlock_file
    profile_manager.ProfileManager._save_metadata = _save_metadata


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
