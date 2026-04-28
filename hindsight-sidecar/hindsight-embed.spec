# -*- mode: python ; coding: utf-8 -*-

from PyInstaller.utils.hooks import collect_data_files, collect_submodules, copy_metadata

block_cipher = None
spec_dir = SPECPATH
hiddenimports = collect_submodules("tiktoken_ext")
hiddenimports += collect_submodules("sentence_transformers")
datas = (
    copy_metadata("fastmcp", recursive=True)
    + copy_metadata("sentence-transformers", recursive=True)
    + collect_data_files("magika", includes=["config/**", "models/**"])
    + collect_data_files("pg0", includes=["bin/*"])
    + collect_data_files("hindsight_api", includes=["alembic/**"], include_py_files=True)
)

embed = Analysis(
    [spec_dir + "/embed_entry.py"],
    pathex=[spec_dir],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)
embed_pyz = PYZ(embed.pure, embed.zipped_data, cipher=block_cipher)
embed_exe = EXE(
    embed_pyz,
    embed.scripts,
    [],
    exclude_binaries=True,
    name="hindsight-embed",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)

api = Analysis(
    [spec_dir + "/api_entry.py"],
    pathex=[spec_dir],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)
api_pyz = PYZ(api.pure, api.zipped_data, cipher=block_cipher)
api_exe = EXE(
    api_pyz,
    api.scripts,
    [],
    exclude_binaries=True,
    name="hindsight-api",
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)

coll = COLLECT(
    embed_exe,
    api_exe,
    embed.binaries,
    embed.datas,
    api.binaries,
    api.datas,
    strip=False,
    upx=True,
    upx_exclude=[],
    name="hindsight-embed",
)
