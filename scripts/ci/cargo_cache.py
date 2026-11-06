"""在主站独占 target 中登记、按实际引用回收 Cargo 产物并审计磁盘占用。"""

import argparse
import json
import math
import os
from pathlib import Path
import re
import shutil
import sys
import tomllib
import uuid


GIB = 1024 ** 3
STATE_DIR = ".master-cache"
STAGES = ("clippy", "test", "package")
PACKAGES = {"src-tauri/Cargo.toml": "msk104master", "crates/iec104-core/Cargo.toml": "iec104-core"}
OWN_HASH = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_-]*-[a-f0-9]{16}\Z")
OWN_FILE = re.compile(r"(?:lib)?[A-Za-z0-9_]+-[a-f0-9]{16}(?:\.[A-Za-z0-9_-]+)+\Z")
# 包身份与路径检查保持不变；显式纳入协议核心新增的两个集成测试目标。
OWN_TARGETS = {"msk104master", "msk104master_lib", "iec104_core", "build_script_build", "wire", "session"}


class CapacityError(ValueError):
    def __init__(self, message, report):
        super().__init__(message)
        self.report = report


def _checked(path):
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("缓存操作需要无向上跳转的绝对路径")
    for entry in reversed((path, *path.parents)):
        try:
            stat = entry.lstat()
        except FileNotFoundError:
            continue
        if entry.is_symlink() or getattr(stat, "st_file_attributes", 0) & 1024:
            raise ValueError("缓存路径禁止符号链接及 Windows 重解析点：" + str(entry))
    return path.resolve()


def _workspace(root):
    root = _checked(root)
    if root == root.parent:
        raise ValueError("禁止使用磁盘根目录作为主站工作区")
    workspace = tomllib.loads(_checked(root / "Cargo.toml").read_text(encoding="utf-8"))
    members = workspace.get("workspace", {}).get("members", [])
    if not {"src-tauri", "crates/iec104-core"}.issubset(members):
        raise ValueError("工作区不是预期的主站工程")
    for relative, name in PACKAGES.items():
        manifest = _checked(root / relative)
        if tomllib.loads(manifest.read_text(encoding="utf-8")).get("package", {}).get("name") != name:
            raise ValueError("自有 crate 身份校验失败：" + relative)
    target = _checked(root / "target")
    if target.parent != root or target.name != "target":
        raise ValueError("缓存目录必须是本工作区独占的 target")
    return root, target


def _run():
    return os.environ.get("CI_PIPELINE_ID", "local") + ":" + os.environ.get("CI_JOB_ID", "local")


def _write(path, data):
    path = _checked(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + "." + uuid.uuid4().hex + ".tmp")
    temporary.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def _owned(target, relative):
    if not isinstance(relative, str) or not relative or "\\" in relative:
        return False
    parts = Path(relative).parts
    if Path(relative).is_absolute() or any(part in (".", "..") or ":" in part for part in parts):
        return False
    if parts[:1] == ("release",):
        tail = parts[1:]
    elif parts[:2] == ("x86_64-pc-windows-msvc", "release"):
        tail = parts[2:]
    else:
        return False
    if len(tail) == 2:
        if tail[0] in (".fingerprint", "build"):
            return bool(OWN_HASH.fullmatch(tail[1]))
        if tail[0] == "deps":
            return bool(OWN_FILE.fullmatch(tail[1])) or tail[1] in {"msk104master.exe", "msk104master.pdb", "msk104master.d"}
    return len(tail) == 1 and tail[0] in {
        "msk104master.exe", "msk104master.pdb", "msk104master.d",
        "libmsk104master_lib.rlib", "msk104master_lib.d", "msk104master_lib.pdb",
        "libiec104_core.rlib", "iec104_core.d", "iec104_core.pdb",
    }


def _artifact(target, path):
    path = _checked(path)
    try:
        relative = path.relative_to(target).as_posix()
    except ValueError as error:
        raise ValueError("产物超出本项目 target") from error
    if not _owned(target, relative):
        raise ValueError("产物不属于本项目的受管 Cargo 单元：" + relative)
    return path, relative


def _load(root, target):
    state_path = _checked(target / STATE_DIR / "state.json")
    identity = {"schema": 1, "project": "msk104master", "root": str(root), "target": str(target),
                "project_id": os.environ.get("CI_PROJECT_ID", "local")}
    if not state_path.exists():
        return {**identity, "runs": {}, "successes": []}
    state = json.loads(state_path.read_text(encoding="utf-8"))
    if any(state.get(key) != value for key, value in identity.items()):
        raise ValueError("缓存状态身份与当前主站工作区不一致")
    if not isinstance(state.get("runs"), dict) or not isinstance(state.get("successes"), list):
        raise ValueError("缓存状态格式无效")
    for run, record in state["runs"].items():
        if (not isinstance(record, dict) or record.get("status") not in ("running", "success", "failed")
                or not isinstance(record.get("artifacts"), list) or not isinstance(record.get("stages"), dict)):
            raise ValueError("构建归属记录无效：" + run)
        for relative in record["artifacts"]:
            if not _owned(target, relative):
                raise ValueError("缓存状态包含越界或未知产物")
            _artifact(target, target / relative)
    if any(run not in state["runs"] or state["runs"][run]["status"] != "success" for run in state["successes"]):
        raise ValueError("成功历史引用无效")
    return state


def _tree(path):
    """删除和计量之前验证每个节点，不跟随链接。"""
    path = _checked(path)
    if not path.exists():
        return 0
    if path.is_file():
        return path.stat().st_size
    if not path.is_dir():
        raise ValueError("缓存节点不是普通文件或目录：" + str(path))
    total = 0
    for entry in path.iterdir():
        total += _tree(entry)
    return total


def _delete(target, path, debug=False):
    path = _checked(path)
    if debug:
        if path != target / "debug":
            raise ValueError("迁移范围仅限本项目旧 Debug 目录")
    else:
        _artifact(target, path)
    _tree(path)
    if not path.exists():
        return
    print("回收本项目缓存：" + str(path), file=sys.stderr)
    if path.is_dir():
        shutil.rmtree(path)
    else:
        path.unlink()


def _save(target, state):
    _write(target / STATE_DIR / "state.json", state)


def _cleanup(target, state, current=True, complete=False):
    if not complete:
        return []
    protected = set(state["successes"][-1:])
    if current and _run() in state["runs"]:
        protected.add(_run())
    keep = {path for run in protected for path in state["runs"][run]["artifacts"]}
    expired = [run for run in state["runs"] if run not in protected]
    candidates = ({path for run in expired for path in state["runs"][run]["artifacts"]} | _confirmed_units(target)) - keep
    planned = []
    # 只有完整成功后才回收具有 Cargo 证据且本轮未引用的单元。
    for relative in sorted(candidates):
        path, _ = _artifact(target, target / relative)
        if any(relative == item or relative.startswith(item + "/") or item.startswith(relative + "/") for item in keep):
            continue
        if any(relative.startswith(parent + "/") for parent in planned):
            continue
        _tree(path)
        planned.append(relative)
    for relative in planned:
        _delete(target, target / relative)
    for run in expired:
        if run == _run() and state["runs"][run]["status"] == "failed":
            # after_script 可再次重放取消前的日志；保留空登记以便幂等收尾。
            state["runs"][run]["artifacts"] = []
        else:
            del state["runs"][run]
    state["successes"] = state["successes"][-1:]
    return planned


def _budgets():
    name = "MASTER_CARGO_MIN_FREE_GIB"
    try:
        value = float(os.environ.get(name, "10"))
    except ValueError as error:
        raise ValueError("磁盘最低剩余空间不是数字：" + name) from error
    if not math.isfinite(value) or value <= 0:
        raise ValueError("磁盘最低剩余空间必须是有限正数")
    return int(value * GIB)


def _profiles(target):
    return [target / "release", target / "x86_64-pc-windows-msvc/release"]


def _confirmed_units(target):
    """只接管具有 Cargo 指纹 JSON 证据的单元；未知文件不能仅凭名字删除。"""
    found = set()
    for profile in _profiles(target):
        fingerprints = profile / ".fingerprint"
        if not fingerprints.exists():
            continue
        deps_by_hash = {}
        deps = profile / "deps"
        if deps.exists():
            for path in deps.iterdir():
                match = re.search(r"-([a-f0-9]{16})(?:\.|$)", path.name)
                if match:
                    deps_by_hash.setdefault(match[1], []).append(path)
        for folder in fingerprints.iterdir():
            _checked(folder)
            if not folder.is_dir() or not OWN_HASH.fullmatch(folder.name):
                continue
            valid = False
            stems = {folder.name.rsplit("-", 1)[0].replace("-", "_")}
            for metadata in folder.glob("*.json"):
                try:
                    value = json.loads(_checked(metadata).read_text(encoding="utf-8"))
                except (ValueError, OSError):
                    continue
                fingerprint_name = re.fullmatch(r"(?:(?:test-|bench-)?(?:lib|bin|example)-[A-Za-z0-9_-]+|(?:run-)?build-script-build-script-build)\.json", metadata.name)
                if (fingerprint_name and isinstance(value, dict) and type(value.get("rustc")) is int
                        and isinstance(value.get("deps"), list) and isinstance(value.get("local"), list)):
                    valid = True
                    match = re.fullmatch(r"(?:test-)?(?:lib|bin|example)-(.+)\.json", metadata.name)
                    if match:
                        stems.add(match[1].replace("-", "_"))
            if not valid:
                continue
            found.add(folder.relative_to(target).as_posix())
            build = profile / "build" / folder.name
            if build.exists():
                found.add(build.relative_to(target).as_posix())
            suffix = folder.name.rsplit("-", 1)[1]
            for path in deps_by_hash.get(suffix, []):
                if any(path.name.startswith(stem + "-" + suffix + ".") or path.name.startswith("lib" + stem + "-" + suffix + ".") for stem in stems):
                    if _owned(target, path.relative_to(target).as_posix()):
                        found.add(path.relative_to(target).as_posix())
    # 旧脚本可能已删除单元主产物和指纹，仅留下本工程 dep-info；须验证实际规则与源码根。
    orphan_name = re.compile(r"(msk104master(?:_lib)?|iec104_core)-([a-f0-9]{16})\.d\Z")
    for profile in _profiles(target):
        deps = profile / "deps"
        if not deps.exists():
            continue
        entries = list(deps.iterdir())
        for path in entries:
            match = orphan_name.fullmatch(path.name)
            if not match or not _checked(path).is_file():
                continue
            stem = path.stem
            if any(other != path and (other.name.startswith(stem + ".") or other.name.startswith("lib" + stem + ".")) for other in entries):
                continue
            package = "iec104-core" if match[1] == "iec104_core" else "msk104master"
            if (profile / ".fingerprint" / (package + "-" + match[2])).exists():
                continue
            text = re.sub(r"/+", "/", path.read_text(encoding="utf-8", errors="replace").replace("\\", "/"))
            source = r"(?:src-tauri/src|crates/iec104-core/src)/(?![^\s]*\.\./)[A-Za-z0-9_./-]+\.rs"
            rule = r"(?m)^.*" + re.escape(stem) + r"\.(?:d|rlib|rmeta|exe):[ \t]+[^\n]*" + source
            if re.search(rule, text):
                found.add(path.relative_to(target).as_posix())
    return found


def _audit(target, state):
    entry = state["runs"].get(_run(), {})
    current = set(entry.get("artifacts", []))
    cleanup_allowed = (entry.get("status") == "success" and entry.get("reference_schema") == 2
                       and set(entry.get("stages", {})) == set(STAGES)
                       and all(stage.get("complete") for stage in entry["stages"].values()))
    protected = {p for run in state["successes"][-1:] if run != _run() for p in state["runs"][run]["artifacts"]}
    confirmed = _confirmed_units(target) | {p for entry in state["runs"].values() for p in entry["artifacts"]}
    groups = {name: {"bytes": 0, "entries": []} for name in ("needed", "protected", "reclaimable", "pending", "unknown", "metadata", "delivery")}
    directories = {}
    def covered(relative, references):
        parts = relative.split("/")
        return any("/".join(parts[:index]) in references for index in range(1, len(parts) + 1))
    def visit(path):
        _checked(path)
        if path.is_dir():
            for entry in path.iterdir():
                visit(entry)
            return
        if not path.is_file():
            raise ValueError("缓存节点不是普通文件：" + str(path))
        relative = path.relative_to(target).as_posix()
        size = path.stat().st_size
        directory = "/".join(Path(relative).parts[:2])
        directories[directory] = directories.get(directory, 0) + size
        group = ("metadata" if relative.startswith(STATE_DIR + "/") or path.name in {".rustc_info.json", "CACHEDIR.TAG", ".cargo-lock", ".cargo-artifact-lock", ".cargo-build-lock"}
                 else "delivery" if "/bundle/" in relative or "/nsis/" in relative else "needed" if covered(relative, current)
                 else "protected" if covered(relative, protected) else ("reclaimable" if cleanup_allowed else "pending") if covered(relative, confirmed) else "unknown")
        groups[group]["bytes"] += size
        groups[group]["entries"].append(relative)
    if target.exists():
        visit(target)
    return {**groups, "directory_bytes": directories, "cleanup_allowed": cleanup_allowed,
            "pending_reason": "完整成功的当前引用图尚未确认，不能判断这些历史单元是否冗余",
            "unknown_reason": "缺少可验证的 Cargo 输出或指纹归属证据"}


def _report(target, state, **extra):
    audit = _audit(target, state)
    return {"run": _run(), "target": str(target), "target_bytes": sum(audit[x]["bytes"] for x in ("needed", "protected", "reclaimable", "pending", "unknown", "metadata", "delivery")),
            "free_bytes": shutil.disk_usage(target)[2], "min_free_bytes": _budgets(),
            "successes": state["successes"], "audit": audit, **extra}


def _capacity(report):
    if report["free_bytes"] < report["min_free_bytes"]:
        raise CapacityError("磁盘可用空间不足，保留必要缓存及其他工程数据；请按审计报告处理容量", report)
    return report


def prepare(root):
    root, target = _workspace(root)
    _budgets()
    state = _load(root, target)
    target.mkdir(parents=True, exist_ok=True)
    migrated = []
    debug = _checked(target / "debug")
    if debug.exists():
        _delete(target, debug, debug=True)
        migrated.append(str(debug))
    # 上轮硬中断只回收已登记且没有成功引用的产物。
    deleted = _cleanup(target, state)
    state["runs"].setdefault(_run(), {"status": "running", "stages": {}, "artifacts": []})
    _save(target, state)
    return _capacity(_report(target, state, status="prepared", migrated=migrated, deleted=deleted))


def check(root):
    root, target = _workspace(root)
    state = _load(root, target)
    target.mkdir(parents=True, exist_ok=True)
    deleted = _cleanup(target, state)
    _save(target, state)
    return _capacity(_report(target, state, status="checked", deleted=deleted))


def record(root, stage, logs):
    if stage not in STAGES:
        raise ValueError("不支持的 Cargo 阶段")
    root, target = _workspace(root)
    state = _load(root, target)
    entry = state["runs"].get(_run())
    if not entry:
        raise ValueError("请先登记当前运行中的构建")
    if entry["status"] == "success":
        return {"run": _run(), "stage": stage, "complete": entry["stages"].get(stage, {}).get("complete", False),
                "artifacts": [], "status": "already-successful"}
    manifests = {root / relative for relative in PACKAGES}
    messages = []
    for path in logs:
        with Path(path).open(encoding="utf-8-sig", errors="replace") as stream:
            for line in stream:
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(value, dict):
                    messages.append(value)
    artifacts, package_ids = set(), set()
    own = 0
    unconfirmed = []
    sidecar_cache = {}
    lock = root / "Cargo.lock"
    dependencies = {(p["name"], p["version"]) for p in tomllib.loads(lock.read_text(encoding="utf-8")).get("package", [])} if lock.exists() else set()

    def add(path):
        path, relative = _artifact(target, path)
        if path.exists():
            _tree(path)
            artifacts.add(relative)

    def unit(directory):
        add(directory)
        fingerprint = directory.parent.parent / ".fingerprint" / directory.name
        if fingerprint.exists():
            add(fingerprint)

    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        manifest = Path(message.get("manifest_path", ""))
        own_manifest = manifest in manifests
        if not own_manifest:
            if not manifest.is_absolute() or not manifest.is_file():
                unconfirmed.append(message.get("package_id", "缺少包身份"))
                continue
            package = tomllib.loads(_checked(manifest).read_text(encoding="utf-8")).get("package", {})
            if (package.get("name"), package.get("version")) not in dependencies:
                unconfirmed.append(message.get("package_id", "缺少包身份"))
                continue
        _checked(manifest)
        name = message.get("target", {}).get("name", "").replace("-", "_")
        if own_manifest and name not in OWN_TARGETS:
            raise ValueError("自有 crate 报告未知的编译目标")
        if not re.fullmatch(r"[A-Za-z0-9_]+", name):
            raise ValueError("Cargo 编译目标名称无效")
        if not isinstance(message.get("package_id"), str) or not message["package_id"]:
            raise ValueError("自有 crate 缺少 Cargo 包身份")
        package_ids.add(message["package_id"])
        own += int(own_manifest)
        filenames = list(message.get("filenames", []))
        if message.get("executable"):
            filenames.append(message["executable"])
        for filename in set(filenames):
            path = _checked(filename)
            if path.parent.parent.name == "build" and OWN_HASH.fullmatch(path.parent.name):
                unit(path.parent)
                continue
            names = OWN_TARGETS if own_manifest else {name}
            if path.parent.name == "deps" and not any(path.name.startswith(crate + "-") or path.name.startswith("lib" + crate + "-") for crate in names):
                if not (own_manifest and path.name in {"msk104master.exe", "msk104master.pdb", "msk104master.d"}):
                    raise ValueError("Cargo 产物与当前编译目标名称不一致")
            add(path)
            if own_manifest and path.name == "msk104master.exe" and path.parent.name == "release":
                for filename in ("msk104master.exe", "msk104master.pdb", "msk104master.d"):
                    original = path.parent / "deps" / filename
                    if original.exists():
                        add(original)
            if path.parent.name != "deps":
                basename = path.name.split(".", 1)[0]
                aliases = {basename, basename[3:] if basename.startswith("lib") else basename}
                for alias in aliases:
                    for extension in (".d", ".pdb"):
                        sibling = path.parent / (alias + extension)
                        if sibling.exists() and _owned(target, sibling.relative_to(target).as_posix()):
                            add(sibling)
            match = re.search(r"-([a-f0-9]{16})(?:\.|$)", path.name)
            if match:
                stem = re.sub(r"(?:\.[A-Za-z0-9_-]+)+$", "", path.name)
                stems = {stem, stem[3:] if stem.startswith("lib") else stem}
                if path.parent not in sidecar_cache:
                    sidecar_cache[path.parent] = list(path.parent.iterdir())
                for sidecar in sidecar_cache[path.parent]:
                    if any(sidecar.name.startswith(item + ".") for item in stems) and _owned(target, sidecar.relative_to(target).as_posix()):
                        add(sidecar)
                profile = path.parent.parent if path.parent.name == "deps" else path.parent
                package_name = PACKAGES[manifest.relative_to(root).as_posix()] if own_manifest else tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]["name"]
                fingerprint = profile / ".fingerprint" / (package_name + "-" + match[1])
                if fingerprint.exists():
                    add(fingerprint)
    for message in messages:
        if message.get("reason") == "build-script-executed":
            if message.get("package_id") in package_ids:
                unit(_checked(message["out_dir"]).parent)
                for linked in message.get("linked_paths", []):
                    # native=、framework= 等是 Cargo 的搜索类型前缀，路径本身仍需逐层验证。
                    location = linked.split("=", 1)[1] if "=" in linked else linked
                    search = _checked(location)
                    try:
                        relative = search.relative_to(target)
                    except ValueError:
                        # 系统 SDK 和 target 外的搜索目录不属于本缓存清理范围。
                        continue
                    parts = relative.parts
                    build_index = 1 if parts[:1] == ("release",) else 2 if parts[:2] == ("x86_64-pc-windows-msvc", "release") else -1
                    if (build_index >= 0 and len(parts) > build_index + 1
                            and parts[build_index] == "build" and OWN_HASH.fullmatch(parts[build_index + 1])):
                        unit(target.joinpath(*parts[:build_index + 2]))
                    else:
                        unconfirmed.append("无法精确归属的原生搜索目录：" + str(search))
            else:
                unconfirmed.append(message.get("package_id", "缺少构建脚本身份"))
    complete = bool(own and artifacts and not unconfirmed) and any(
        message.get("reason") == "build-finished" and message.get("success") is True for message in messages)
    entry["reference_schema"] = 2
    entry["stages"][stage] = {"complete": complete, "unconfirmed": sorted(set(unconfirmed))}
    entry["artifacts"] = sorted(set(entry["artifacts"]) | artifacts)
    _save(target, state)
    return {"run": _run(), "stage": stage, "complete": complete, "artifacts": sorted(artifacts), "unconfirmed": sorted(set(unconfirmed))}


def finish(root, status):
    if status not in ("success", "failed"):
        raise ValueError("构建结束状态无效")
    root, target = _workspace(root)
    state = _load(root, target)
    target.mkdir(parents=True, exist_ok=True)
    entry = state["runs"].get(_run())
    if entry and entry["status"] == "success":
        return _report(target, state, status="already-successful", deleted=[])
    if entry:
        if status == "success":
            if entry["status"] != "running":
                raise ValueError("失败构建不能重新晋升为成功")
            if entry.get("reference_schema") != 2:
                raise ValueError("旧版自有产物记录不包含完整依赖引用，不能执行全量回收")
            if set(entry["stages"]) != set(STAGES) or not all(value.get("complete") for value in entry["stages"].values()):
                raise ValueError("成功构建必须完整记录 Release 检查、测试和打包阶段")
            state["successes"] = [run for run in state["successes"] if run != _run()] + [_run()]
        entry["status"] = status
    elif status == "success":
        raise ValueError("当前构建没有登记，不能标记成功")
    deleted = _cleanup(target, state, current=status == "success", complete=status == "success")
    _save(target, state)
    return _report(target, state, status=status if entry else "unregistered", deleted=deleted)


def main(arguments=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("prepare", "check", "record", "finish"):
        command = commands.add_parser(name)
        command.add_argument("--root", required=True)
        if name == "record":
            command.add_argument("--stage", choices=STAGES, required=True)
            command.add_argument("--log", action="append", required=True)
        if name == "finish":
            command.add_argument("--status", choices=("success", "failed"), required=True)
    args = parser.parse_args(arguments)
    try:
        if args.command == "record":
            result = record(args.root, args.stage, args.log)
        elif args.command == "finish":
            result = finish(args.root, args.status)
        else:
            result = {"prepare": prepare, "check": check}[args.command](args.root)
        print(json.dumps(result, ensure_ascii=False))
        return 0
    except (ValueError, OSError, KeyError, TypeError) as error:
        result = {"error": str(error)}
        if isinstance(error, CapacityError):
            result["report"] = error.report
        print(json.dumps(result, ensure_ascii=False))
        print("主站缓存操作失败：" + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
