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

    # 完整成功后仅保留本轮必要引用，不永久保留历史成功变体。
    def test_keeps_current_success_and_preserves_unconfirmed_files(self):
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
        self.assertFalse(paths[1].exists())
        self.assertTrue(paths[2].is_file())
        self.assertEqual(report["successes"], ["3:3"])
        self.assertTrue(dependency.is_file())
        self.assertTrue(unknown.is_file())

    # 失败保留最后成功与本轮部分产物，方便后续重试。
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
        self.assertTrue(failed.is_file())
        self.assertEqual(report["successes"], ["1:1"])
        self.assertEqual(cache.finish(self.root, "failed")["successes"], ["1:1"])

    # 未完整成功前不能判定历史部分产物无用，prepare 不主动删除。
    def test_prepare_preserves_unconfirmed_abandoned_outputs(self):
        cache.prepare(self.root)
        for run in range(1, 4):
            path = self.file("release/deps/libmsk104master_lib-" + str(run) * 16 + ".rlib")
            cache.record(self.root, "test", [self.log([path], success=False)])
            self.next_run(run + 1)
            self.assertTrue(path.is_file())
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

    # 作业在成功收尾后取消时，补偿收尾必须保留已成功的缓存记录。
    def test_failed_replay_does_not_downgrade_success(self):
        cache.prepare(self.root)
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        self.record_all([path])
        cache.finish(self.root, "success")
        cache.record(self.root, "test", [self.log([path], success=False)])
        report = cache.finish(self.root, "failed")
        self.assertEqual(report["successes"], ["1:1"])
        self.assertTrue(path.is_file())

    # 收尾步骤重放失败日志时登记引用，供后续运行重试。
    def test_failure_cleanup_can_register_failed_run_for_retry(self):
        cache.prepare(self.root)
        cache.finish(self.root, "failed")
        path = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        cache.record(self.root, "test", [self.log([path], success=False)])
        self.assertEqual(cache.finish(self.root, "failed")["deleted"], [])
        self.assertTrue(path.is_file())

    # 不能确认身份的外部清单不能被当成已收齐的依赖引用。
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

    # 协议集成测试的检查及可执行产物可登记，未知目标仍须拒绝。
    def test_records_protocol_test_targets_and_rejects_unknown_target(self):
        cache.prepare(self.root)
        for name in ("wire", "session"):
            paths = [self.file("release/deps/" + name + "-" + "a" * 16 + extension)
                     for extension in (".rmeta", ".exe")]
            log = self.log(paths, package="iec104-core")
            messages = [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]
            messages[0]["target"] = {"name": name, "kind": ["test"]}
            log.write_text("\n".join(json.dumps(item) for item in messages) + "\n", encoding="utf-8")
            report = cache.record(self.root, "clippy", [log])
            for path in paths:
                self.assertIn(path.relative_to(self.target).as_posix(), report["artifacts"])
        messages[0]["target"]["name"] = "unknown_protocol_target"
        log.write_text("\n".join(json.dumps(item) for item in messages) + "\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "未知的编译目标"):
            cache.record(self.root, "clippy", [log])

    # build-script 输出目录与 fingerprint 一并登记，维持整单元引用。
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

    # 磁盘空间不足有明确报告，旧大小限制不影响保留未知数据。
    def test_capacity_failures_preserve_unknown_data(self):
        cache.prepare(self.root)
        unknown = self.file("release/deps/unknown.bin", b"1234")
        with patch("cargo_cache.shutil.disk_usage", return_value=(100, 99, 1)):
            with self.assertRaises(cache.CapacityError) as error:
                cache.check(self.root)
            self.assertEqual(error.exception.report["free_bytes"], 1)
        # 已撤销的旧大小限制即使存在于外部环境，也不能拒绝有效缓存。
        with patch.dict(os.environ, {"MASTER_CARGO_MAX_CACHE_GIB": "0.000000001"}):
            report = cache.check(self.root)
            self.assertGreater(report["target_bytes"], 1)
            self.assertNotIn("max_cache_bytes", report)
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


    # 具有真实 Cargo 指纹结构的旧第三方变体在完整成功后清理，当前旁文件保留。
    def test_sweeps_confirmed_old_dependency_and_keeps_sidecars(self):
        cache.prepare(self.root)
        old = self.file("release/deps/libtauri-" + "a" * 16 + ".rlib")
        old_fp = self.file("release/.fingerprint/tauri-" + "a" * 16 + "/lib-tauri.json",
                           b'{"rustc":1,"features":"[]","target":1,"profile":1,"deps":[],"local":[]}')
        current = self.file("release/deps/libmsk104master_lib-" + "b" * 16 + ".rlib")
        sidecar = self.file("release/deps/msk104master_lib-" + "b" * 16 + ".d")
        unknown = self.file("release/deps/libmystery-" + "c" * 16 + ".rlib")
        cache.check(self.root)
        self.assertTrue(old.exists())
        self.record_all([current])
        report = cache.finish(self.root, "success")
        self.assertFalse(old.exists())
        self.assertFalse(old_fp.exists())
        self.assertTrue(current.is_file())
        self.assertTrue(sidecar.is_file())
        self.assertTrue(unknown.is_file())
        self.assertIn(unknown.relative_to(self.target).as_posix(), report["audit"]["unknown"]["entries"])

    # 外部依赖由清单身份与锁文件确认，Fresh 消息同样能保护必要缓存。
    def test_records_locked_dependency_fresh_artifact(self):
        cache.prepare(self.root)
        manifest = self.root / "registry/serde/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nname="serde"\nversion="1.0.0"\n', encoding="utf-8")
        (self.root / "Cargo.lock").write_text('[[package]]\nname="serde"\nversion="1.0.0"\n', encoding="utf-8")
        artifact = self.file("release/deps/libserde-" + "a" * 16 + ".rlib")
        own = self.file("release/deps/libmsk104master_lib-" + "b" * 16 + ".rlib")
        msg = {"reason":"compiler-artifact", "manifest_path":str(manifest),
               "package_id":"registry+https://example.invalid#serde@1.0.0",
               "target":{"name":"serde"}, "filenames":[str(artifact)], "fresh":True}
        log = self.log([own], extra=[msg])
        for stage in cache.STAGES:
            cache.record(self.root, stage, [log])
        report = cache.finish(self.root, "success")
        self.assertTrue(artifact.is_file())
        self.assertIn(artifact.relative_to(self.target).as_posix(), report["audit"]["needed"]["entries"])


    # 仅名称和几个同名字段不足以证明 Cargo 归属，伪造指纹不能触发回收。
    def test_invalid_fingerprint_stays_unknown(self):
        cache.prepare(self.root)
        stale = self.file("release/deps/libtauri-" + "a" * 16 + ".rlib")
        self.file("release/.fingerprint/tauri-" + "a" * 16 + "/not-cargo.json",
                  b'{"rustc":1,"deps":[],"local":[]}')
        self.file("release/.fingerprint/tauri-" + "a" * 16 + "/lib-tauri.json",
                  b'{"rustc":"invalid","deps":[],"local":[]}')
        current = self.file("release/deps/libmsk104master_lib-" + "b" * 16 + ".rlib")
        self.record_all([current])
        report = cache.finish(self.root, "success")
        self.assertTrue(stale.is_file())
        self.assertIn(stale.relative_to(self.target).as_posix(), report["audit"]["unknown"]["entries"])


    # 完整日志中若出现身份无法确认的依赖，不能因 own target 成功就清扫该依赖。
    def test_unconfirmed_dependency_prevents_complete_sweep(self):
        cache.prepare(self.root)
        own = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        extra = [{"reason":"compiler-artifact", "manifest_path":str(self.root / "missing/Cargo.toml"),
                  "package_id":"unconfirmed", "target":{"name":"serde"}, "filenames":[]}]
        log = self.log([own], extra=extra)
        for stage in cache.STAGES:
            self.assertFalse(cache.record(self.root, stage, [log])["complete"])
        with self.assertRaises(ValueError):
            cache.finish(self.root, "success")
        self.assertTrue(own.is_file())


    # 阶段未齐时，已确认的历史 Cargo 单元只能列待确认，不能误报为可回收冗余。
    def test_partial_reference_graph_defers_redundancy_claim(self):
        cache.prepare(self.root)
        old = self.file("release/deps/libtauri-" + "a" * 16 + ".rlib")
        self.file("release/.fingerprint/tauri-" + "a" * 16 + "/lib-tauri.json",
                  b'{"rustc":1,"deps":[],"local":[]}')
        report = cache.check(self.root)
        self.assertFalse(report["audit"]["cleanup_allowed"])
        self.assertEqual(report["audit"]["reclaimable"]["entries"], [])
        self.assertIn(old.relative_to(self.target).as_posix(), report["audit"]["pending"]["entries"])
        self.assertTrue(old.is_file())


    # 旧 own-only 格式即使三阶段标完整也不能触发全依赖清扫。
    def test_legacy_reference_schema_cannot_promote_success(self):
        cache.prepare(self.root)
        state_path = self.target / cache.STATE_DIR / "state.json"
        state = json.loads(state_path.read_text(encoding="utf-8"))
        entry = state["runs"]["1:1"]
        entry["stages"] = {stage:{"complete":True} for stage in cache.STAGES}
        state_path.write_text(json.dumps(state), encoding="utf-8")
        with self.assertRaises(ValueError):
            cache.finish(self.root, "success")

    # 明确的原生搜索路径可以引用另一个 Cargo build 单元，必须保护整单元。
    def test_native_linked_path_protects_cross_build_unit(self):
        cache.prepare(self.root)
        own = self.file("release/deps/libmsk104master_lib-" + "a" * 16 + ".rlib")
        out = self.file("release/build/msk104master-" + "b" * 16 + "/out/current.rs")
        native = self.file("x86_64-pc-windows-msvc/release/build/native_sys-" + "c" * 16 + "/out/x64/native.lib")
        extra = [{"reason":"build-script-executed", "package_id":"path+file:///fixture#msk104master@0.1.0",
                  "out_dir":str(out.parent), "linked_paths":["native=" + str(native.parent)]}]
        report = cache.record(self.root, "test", [self.log([own], extra=extra)])
        self.assertTrue(report["complete"])
        self.assertIn(native.parent.parent.parent.relative_to(self.target).as_posix(), report["artifacts"])
        broad = {**extra[0], "linked_paths":["native=" + str(self.target / "release/deps")]}
        self.assertFalse(cache.record(self.root, "test", [self.log([own], extra=[broad])])["complete"])


    # Cargo 上提的主程序输出仍引用 deps 下同名原始程序与旁文件。
    def test_uplifted_binary_keeps_unhashed_deps_outputs_and_controls(self):
        cache.prepare(self.root)
        executable = self.file("x86_64-pc-windows-msvc/release/msk104master.exe")
        original = self.file("x86_64-pc-windows-msvc/release/deps/msk104master.exe")
        symbols = self.file("x86_64-pc-windows-msvc/release/deps/msk104master.pdb")
        info = self.file("x86_64-pc-windows-msvc/release/deps/msk104master.d")
        lock = self.file("release/.cargo-artifact-lock")
        nsis = self.file("x86_64-pc-windows-msvc/release/nsis/x64/installer.nsi")
        for stage in cache.STAGES:
            cache.record(self.root, stage, [self.log([executable])])
        report = cache.finish(self.root, "success")
        for path in (original, symbols, info):
            self.assertTrue(path.is_file())
            self.assertIn(path.relative_to(self.target).as_posix(), report["audit"]["needed"]["entries"])
        self.assertIn(lock.relative_to(self.target).as_posix(), report["audit"]["metadata"]["entries"])
        self.assertIn(nsis.relative_to(self.target).as_posix(), report["audit"]["delivery"]["entries"])

    # 只有本工程 Cargo dep-info 规则及 Rust 源码证据都确认的孤立 .d 才接管。
    def test_orphan_dep_info_requires_own_source_evidence(self):
        cache.prepare(self.root)
        stem = "msk104master_lib-" + "a" * 16
        old = self.file("release/deps/" + stem + ".d",
                        ("release/deps/" + stem + ".d: src-tauri/src/lib.rs\n").encode())
        invalid = self.file("release/deps/msk104master-" + "b" * 16 + ".d", b"unrelated data")
        foreign = self.file("release/deps/serde-" + "c" * 16 + ".d",
                            b"serde.rlib: src-tauri/src/lib.rs\n")
        current = self.file("release/deps/libmsk104master_lib-" + "d" * 16 + ".rlib")
        cache.check(self.root)
        self.assertTrue(old.is_file())
        self.record_all([current])
        report = cache.finish(self.root, "success")
        self.assertFalse(old.exists())
        self.assertTrue(invalid.is_file())
        self.assertTrue(foreign.is_file())
        self.assertIn(invalid.relative_to(self.target).as_posix(), report["audit"]["unknown"]["entries"])
        self.assertIn(foreign.relative_to(self.target).as_posix(), report["audit"]["unknown"]["entries"])


if __name__ == "__main__":
    unittest.main()
