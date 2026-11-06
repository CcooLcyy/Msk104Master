"""交付副本验证成功后回收主站 NSIS 打包重复文件，禁止递归清理。"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys

from cargo_cache import _checked, _workspace


TARGET = "x86_64-pc-windows-msvc"
BUNDLE = Path(TARGET) / "release/bundle/nsis"
SCRIPT_ROOT = Path(TARGET) / "release/nsis/x64"
SCRIPTS = ("FileAssociation.nsh", "installer.nsi", "SimpChinese.nsh", "utils.nsh")


def _file(path):
    path = _checked(path)
    if not path.is_file():
        raise ValueError("打包文件缺失或不是普通文件：" + str(path))
    return path


def _digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def cleanup(root, env=None):
    env = os.environ if env is None else env
    root, target = _workspace(root)
    info = json.loads(_file(root / "package/build-info.json").read_text(encoding="utf-8"))
    commit = env.get("CI_COMMIT_SHA", "")
    if not re.fullmatch(r"[a-f0-9]{40}", commit) or info.get("commit") != commit:
        raise ValueError("交付记录不是当前 CI 提交，拒绝清理打包副本")
    if info.get("target") != TARGET or type(info.get("signed")) is not bool:
        raise ValueError("交付架构或签名状态无效")
    name = info.get("installer", "")
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*\.exe", name):
        raise ValueError("安装包名称不是合法的普通文件名")
    planned = []
    for suffix in ("", ".sig") if info["signed"] else ("",):
        source = _file(target / BUNDLE / (name + suffix))
        destination = _file(root / "package/installer" / (name + suffix))
        if source.stat().st_size == 0 or destination.stat().st_size == 0 or _digest(source) != _digest(destination):
            raise ValueError("交付副本与打包文件不一致，拒绝清理：" + name + suffix)
        planned.append(source)
    for script in SCRIPTS:
        path = _checked(target / SCRIPT_ROOT / script)
        if path.exists():
            planned.append(_file(path))
    # 所有路径与副本先校验，保证任一失败时不执行删除计划。
    sizes = {path: path.stat().st_size for path in planned}
    for path in planned:
        _file(path).unlink()
    return {"commit": commit, "deleted": [path.relative_to(target).as_posix() for path in planned],
            "deleted_bytes": sum(sizes.values()), "installer": name}


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    args = parser.parse_args(arguments)
    try:
        report = cleanup(args.root)
        print(json.dumps(report, ensure_ascii=False))
        return 0
    except (ValueError, OSError, TypeError, AttributeError) as error:
        print(json.dumps({"error": str(error)}, ensure_ascii=False))
        print("主站打包副本回收失败：" + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
