"""主站缓存归属、保留与空间边界；仅由 CI 执行。"""

import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import cargo_cache as cache


class CargoCacheTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name).resolve()
        (self.root / "src-tauri").mkdir()
        (self.root / "crates/iec104-core").mkdir(parents=True)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["src-tauri", "crates/iec104-core"]\n', encoding="utf-8")
        for directory, name in (("src-tauri", "msk104master"), ("crates/iec104-core", "iec104-core")):
            (self.root / directory / "Cargo.toml").write_text(
                '[package]\nname = "' + name + '"\n', encoding="utf-8")
        self.target = self.root / "target"
        self.environ = patch.dict(os.environ, {"CI_PIPELINE_ID": "1", "CI_JOB_ID": "1",
                                  "CI_PROJECT_ID": "11", "MASTER_CARGO_MIN_FREE_GIB": "10",
                                  "MASTER_CARGO_MAX_CACHE_GIB": "20"})
        self.environ.start()
        self.addCleanup(self.environ.stop)
        self.disk = patch("cargo_cache.shutil.disk_usage", return_value=(100 * cache.GIB, 0, 100 * cache.GIB))
        self.disk.start()
        self.addCleanup(self.disk.stop)

    def file(self, relative, data=b"artifact"):
        path = self.target / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def log(self, filenames, package="msk104master", success=True, extra=None):
        directory = "src-tauri" if package == "msk104master" else "crates/iec104-core"
        messages = [{"reason": "compiler-artifact", "manifest_path": str(self.root / directory / "Cargo.toml"),
                     "package_id": "path+file:///fixture#" + package + "@0.1.0",
                     "target": {"name": "msk104master_lib" if package == "msk104master" else "iec104_core"},
                     "filenames": [str(path) for path in filenames], "fresh": True}]
        messages.extend(extra or [])
        messages.append({"reason": "build-finished", "success": success})
        path = self.root / "cargo.jsonl"
        path.write_text("\n".join(json.dumps(item) for item in messages) + "\n", encoding="utf-8")
        return path

    def record_all(self, filenames, success=True):
        for stage in cache.STAGES:
            cache.record(self.root, stage, [self.log(filenames, success=success)])

    def next_run(self, run):
        os.environ["CI_PIPELINE_ID"] = str(run)
        os.environ["CI_JOB_ID"] = str(run)
        return cache.prepare(self.root)

    # 只迁移本工作区旧 Debug，Release、第三方和其他工程文件均保留。
    def test_prepare_migrates_debug_only(self):
        debug = self.file("debug/deps/legacy.rlib")
        dependency = self.file("release/deps/libserde-" + "a" * 16 + ".rlib")
        outside = self.root / "other-project/debug/important.txt"
        outside.parent.mkdir(parents=True)
        outside.write_text("保留", encoding="utf-8")
        report = cache.prepare(self.root)
        self.assertFalse(debug.exists())
        self.assertTrue(dependency.is_file())
        self.assertTrue(outside.is_file())
        self.assertIn(str(self.target / "debug"), report["migrated"])

    # 身份不符或 root 向上跳转时，绝不能碰调试缓存。
    def test_prepare_rejects_wrong_identity_and_traversal(self):
        debug = self.file("debug/keep.bin")
        manifest = self.root / "src-tauri/Cargo.toml"
        manifest.write_text('[package]\nname = "other"\n', encoding="utf-8")
        with self.assertRaises(ValueError):
            cache.prepare(self.root)
        self.assertTrue(debug.exists())
        with self.assertRaises(ValueError):
            cache.prepare(self.root / "child/..")

    # 工作区清单自身为 Windows 重解析点时也应拒绝，且不依赖测试账号的链接权限。
    def test_prepare_rejects_workspace_manifest_reparse(self):
        debug = self.file("debug/keep.bin")
        manifest = self.root / "Cargo.toml"
        original = Path.lstat

        def metadata(path, *arguments, **keywords):
            result = original(path, *arguments, **keywords)
            if path == manifest:
                return SimpleNamespace(st_mode=result.st_mode, st_file_attributes=1024)
            return result

        with patch.object(Path, "lstat", metadata), self.assertRaises(ValueError):
            cache.prepare(self.root)
        self.assertTrue(debug.is_file())

    # 链接检测在删除前完成，禁止跟随 Debug 内指向外部的链接。
    def test_prepare_rejects_debug_link(self):
        self.target.mkdir()
        (self.target / "debug").mkdir()
        outside = self.root / "outside"
        outside.mkdir()
        victim = outside / "important.txt"
        victim.write_text("保留", encoding="utf-8")
        try:
            (self.target / "debug/link").symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("当前系统不允许创建测试目录链接")
        with self.assertRaises(ValueError):
            cache.prepare(self.root)
        self.assertTrue(victim.is_file())

    # target 自身不能是链接，避免将清理范围引到其他工程。
    def test_prepare_rejects_target_link(self):
        outside = self.root / "outside"
        outside.mkdir()
        try:
            self.target.symlink_to(outside, target_is_directory=True)
        except OSError:
            self.skipTest("当前系统不允许创建测试目录链接")
        with self.assertRaises(ValueError):
            cache.prepare(self.root)

    # 最近两个成功引用受保护，第三次成功才回收不再引用的自有旧产物。
    def test_keeps_two_successes_and_preserves_dependencies(self):
        cache.prepare(self.root)
        paths = []
        dependency = self.file("release/deps/libserde-" + "e" * 16 + ".rlib")
        unknown = self.file("release/deps/unknown.bin")
        for run in range(1, 4):
            if run > 1:
                self.next_run(run)
            paths.append(self.file("release/deps/libmsk104master_lib-" + str(run) * 16 + ".rlib"))
            self.record_all([paths[-1]])
            report = cache.finish(self.root, "success")
        self.assertFalse(paths[0].exists())
        self.assertTrue(paths[1].is_file())
        self.assertTrue(paths[2].is_file())
        self.assertEqual(report["successes"], ["2:2", "3:3"])
        self.assertTrue(dependency.is_file())
        self.assertTrue(unknown.is_file())

    # 失败只回收失败产物，不能删除前次成功或双方共同引用的文件。
    def test_failed_finish_preserves_success_and_shared_artifact(self):
        cache.prepare(self.root)
        shared = self.file("release/deps/libiec104_core-" + "a" * 16 + ".rlib")
        self.record_all([shared])
        cache.finish(self.root, "success")
        self.next_run(2)
        failed = self.file("release/deps/msk104master-" + "b" * 16 + ".exe")
        cache.record(self.root, "test", [self.log([shared, failed], success=False)])
        report = cache.finish(self.root, "failed")
        self.assertTrue(shared.is_file())
        self.assertFalse(failed.exists())
        self.assertEqual(report["successes"], ["1:1"])
        self.assertEqual(cache.finish(self.root, "failed")["successes"], ["1:1"])

    # 硬中断没有收尾时，下次 prepare 回收已登记孤立产物，连续失败不累积。
    def test_prepare_recovers_abandoned_run(self):
        cache.prepare(self.root)
        for run in range(1, 4):
            path = self.file("release/deps/libmsk104master_lib-" + str(run) * 16 + ".rlib")
            cache.record(self.root, "test", [self.log([path], success=False)])
            self.next_run(run + 1)
            self.assertFalse(path.exists())
        self.assertEqual(cache.check(self.root)["successes"], [])

    # 缺阶段或 Cargo 失败消息均不能将构建晋升成成功保留记录。
    def test_success_requires_complete_stages(self):
        cache.prepare(self.root)
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        cache.record(self.root, "test", [self.log([path])])
        with self.assertRaises(ValueError):
            cache.finish(self.root, "success")
        self.record_all([path], success=False)
        with self.assertRaises(ValueError):
            cache.finish(self.root, "success")
        self.assertEqual(cache.check(self.root)["successes"], [])

    # GitLab 在成功收尾后取消作业时，补偿收尾不得降级已成功的缓存记录。
    def test_failed_replay_does_not_downgrade_success(self):
        cache.prepare(self.root)
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        self.record_all([path])
        cache.finish(self.root, "success")
        cache.record(self.root, "test", [self.log([path], success=False)])
        report = cache.finish(self.root, "failed")
        self.assertEqual(report["successes"], ["1:1"])
        self.assertTrue(path.is_file())

    # 主脚本已经失败收尾后，after_script 仍能重放迟到消息再幂等回收。
    def test_after_script_can_replay_failed_run(self):
        cache.prepare(self.root)
        cache.finish(self.root, "failed")
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        cache.record(self.root, "test", [self.log([path], success=False)])
        self.assertIn(path.relative_to(self.target).as_posix(), cache.finish(self.root, "failed")["deleted"])
        self.assertFalse(path.exists())

    # 即使产物名类似，其他清单中的消息也不能登记为本工程所有。
    def test_record_ignores_external_manifest(self):
        cache.prepare(self.root)
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        log = self.log([path])
        content = log.read_text(encoding="utf-8").replace(str(self.root).replace("\\", "\\\\"), "external")
        log.write_text(content, encoding="utf-8")
        report = cache.record(self.root, "test", [log])
        self.assertFalse(report["complete"])
        self.assertEqual(report["artifacts"], [])

    # 自有清单也不能将 target 之外的文件或其他 crate 文件交给回收器。
    def test_record_rejects_external_and_dependency_artifacts(self):
        cache.prepare(self.root)
        external = self.root / "outside.exe"
        external.write_bytes(b"keep")
        dependency = self.file("release/deps/libserde-" + "e" * 16 + ".rlib")
        for path in (external, dependency):
            with self.subTest(path=path), self.assertRaises(ValueError):
                cache.record(self.root, "test", [self.log([path])])
        self.assertTrue(external.is_file())
        self.assertTrue(dependency.is_file())

    # 自有 build-script 输出目录与 fingerprint 一并登记，第三方 build 不登记。
    def test_records_owned_build_script_units(self):
        cache.prepare(self.root)
        unit = "msk104master-" + "a" * 16
        out = self.file("x86_64-pc-windows-msvc/release/build/" + unit + "/out/config.rs")
        fingerprint = self.file("x86_64-pc-windows-msvc/release/.fingerprint/" + unit + "/marker")
        executable = self.file("release/build/" + unit + "/build-script-build.exe")
        extra = [{"reason": "build-script-executed", "package_id": "path+file:///fixture#msk104master@0.1.0",
                  "out_dir": str(out.parent)}]
        report = cache.record(self.root, "package", [self.log([executable], extra=extra)])
        self.assertIn(out.parent.parent.relative_to(self.target).as_posix(), report["artifacts"])
        self.assertIn(fingerprint.parent.relative_to(self.target).as_posix(), report["artifacts"])

    # 篡改状态里的路径不能导致越界删除，错误发生在回收之前。
    def test_tampered_state_is_rejected(self):
        cache.prepare(self.root)
        state_path = self.target / cache.STATE_DIR / "state.json"
        state = json.loads(state_path.read_text(encoding="utf-8"))
        state["runs"]["1:1"]["artifacts"] = ["../outside.exe"]
        state_path.write_text(json.dumps(state), encoding="utf-8")
        with self.assertRaises(ValueError):
            cache.finish(self.root, "failed")

    # 空间不足与项目预算超限有明确报告，不删除未知大文件。
    def test_capacity_failures_preserve_unknown_data(self):
        cache.prepare(self.root)
        unknown = self.file("release/deps/unknown.bin", b"1234")
        with patch("cargo_cache.shutil.disk_usage", return_value=(100, 99, 1)):
            with self.assertRaises(cache.CapacityError) as error:
                cache.check(self.root)
            self.assertEqual(error.exception.report["free_bytes"], 1)
        with patch.dict(os.environ, {"MASTER_CARGO_MAX_CACHE_GIB": "0.000000001"}):
            with self.assertRaises(cache.CapacityError) as error:
                cache.check(self.root)
            self.assertGreater(error.exception.report["target_bytes"], 1)
        self.assertTrue(unknown.is_file())

    # 非法预算值需显式失败，不能被解释为禁用空间检查。
    def test_rejects_invalid_budget(self):
        for value in ("0", "-1", "nan", "inf", "not-a-number"):
            with self.subTest(value=value), patch.dict(os.environ, {"MASTER_CARGO_MIN_FREE_GIB": value}):
                with self.assertRaises(ValueError):
                    cache.prepare(self.root)

    # CLI 即使因容量不足失败，也输出可归档的 JSON 报告。
    def test_cli_preserves_json_capacity_report(self):
        cache.prepare(self.root)
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch("cargo_cache.shutil.disk_usage", return_value=(100, 99, 1)), patch("sys.stdout", stdout), patch("sys.stderr", stderr):
            code = cache.main(["check", "--root", str(self.root)])
        self.assertEqual(code, 1)
        self.assertIn("error", json.loads(stdout.getvalue()))
        self.assertEqual(json.loads(stdout.getvalue())["report"]["free_bytes"], 1)


if __name__ == "__main__":
    unittest.main()
