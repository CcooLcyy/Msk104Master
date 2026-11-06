"""交付副本验证及打包中间文件回收边界，仅在 CI 执行。"""

import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import delivery_cache as cache


class DeliveryCacheTests(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name).resolve()
        for directory, name in (("src-tauri", "msk104master"), ("crates/iec104-core", "iec104-core")):
            path = self.root / directory
            path.mkdir(parents=True)
            (path / "Cargo.toml").write_text('[package]\nname = "' + name + '"\n', encoding="utf-8")
        (self.root / "Cargo.toml").write_text('[workspace]\nmembers = ["src-tauri", "crates/iec104-core"]\n', encoding="utf-8")
        self.env = {"CI_COMMIT_SHA": "a" * 40}
        self.name = "Msk104Master_0.1.0-ci.1_x64-setup.exe"
        self.info = {"commit": "a" * 40, "target": cache.TARGET, "signed": True, "installer": self.name}
        self.source = self.root / "target" / cache.BUNDLE / self.name
        self.destination = self.root / "package/installer" / self.name
        for path in (self.source, self.destination):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"installer")
            Path(str(path) + ".sig").write_bytes(b"signature")
        self.scripts = [self.root / "target" / cache.SCRIPT_ROOT / name for name in cache.SCRIPTS]
        for path in self.scripts:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"generated-script")
        self.save()

    def save(self):
        (self.root / "package/build-info.json").write_text(json.dumps(self.info), encoding="utf-8")

    # 同一提交、安装器和签名复制一致后，只删除明确重复项及四个生成脚本。
    def test_removes_verified_duplicates_and_keeps_delivery_and_unknown(self):
        unknown = self.source.parent / "keep.txt"
        unknown.write_bytes(b"keep")
        report = cache.cleanup(self.root, self.env)
        self.assertEqual(len(report["deleted"]), 6)
        self.assertEqual(report["deleted_bytes"], len(b"installer") + len(b"signature") + 4 * len(b"generated-script"))
        self.assertFalse(self.source.exists())
        self.assertFalse(Path(str(self.source) + ".sig").exists())
        self.assertTrue(self.destination.is_file())
        self.assertTrue(Path(str(self.destination) + ".sig").is_file())
        self.assertTrue(unknown.is_file())
        self.assertTrue(self.source.parent.is_dir())

    # 复制不完整或签名不一致时，完整删除计划均不得执行。
    def test_mismatch_or_missing_copy_preserves_entire_plan(self):
        for path in (self.destination, Path(str(self.destination) + ".sig")):
            with self.subTest(path=path):
                original = path.read_bytes()
                path.write_bytes(b"different")
                with self.assertRaises(ValueError):
                    cache.cleanup(self.root, self.env)
                self.assertTrue(self.source.is_file())
                self.assertTrue(all(script.is_file() for script in self.scripts))
                path.unlink()
                with self.assertRaises(ValueError):
                    cache.cleanup(self.root, self.env)
                self.assertTrue(self.source.is_file())
                path.write_bytes(original)

    # 非法名称、错误提交、错误架构及非布尔签名状态均不能扩大清理范围。
    def test_invalid_metadata_preserves_files(self):
        original = dict(self.info)
        for field, value in (("installer", "../outside.exe"), ("installer", "C:\\outside.exe"),
                             ("commit", "b" * 40), ("target", "aarch64"), ("signed", "false")):
            with self.subTest(field=field, value=value):
                self.info = dict(original, **{field: value})
                self.save()
                with self.assertRaises(ValueError):
                    cache.cleanup(self.root, self.env)
                self.assertTrue(self.source.is_file())
                self.assertTrue(all(script.is_file() for script in self.scripts))

    # 非本工程工作区不能因为存在同名 target 和交付记录而触发清理。
    def test_wrong_workspace_identity_preserves_files(self):
        (self.root / "src-tauri/Cargo.toml").write_text('[package]\nname = "other"\n', encoding="utf-8")
        with self.assertRaises(ValueError):
            cache.cleanup(self.root, self.env)
        self.assertTrue(self.source.is_file())
        self.assertTrue(all(script.is_file() for script in self.scripts))

    # 未签名构建不能删除没有比对过的旧签名文件。
    def test_unsigned_keeps_unverified_signature(self):
        self.info["signed"] = False
        self.save()
        signature = Path(str(self.source) + ".sig")
        signature.write_bytes(b"unverified-old-signature")
        report = cache.cleanup(self.root, self.env)
        self.assertEqual(len(report["deleted"]), 5)
        self.assertTrue(signature.is_file())

    # 删除计划末尾的脚本若为重解析点，也必须在删除安装器之前拒绝。
    def test_script_reparse_is_rejected_before_any_delete(self):
        victim = self.scripts[-1]
        original = Path.lstat

        def metadata(path, *arguments, **keywords):
            result = original(path, *arguments, **keywords)
            return SimpleNamespace(st_mode=result.st_mode, st_file_attributes=1024) if path == victim else result

        with patch.object(Path, "lstat", metadata), self.assertRaises(ValueError):
            cache.cleanup(self.root, self.env)
        self.assertTrue(self.source.is_file())
        self.assertTrue(all(script.is_file() for script in self.scripts))

    # 真实链接拒绝在 Linux CI 验证，Windows 无权限时明确报告缺口。
    def test_linked_installer_is_rejected(self):
        outside = self.root / "outside.exe"
        outside.write_bytes(b"installer")
        self.source.unlink()
        try:
            self.source.symlink_to(outside)
        except OSError:
            self.skipTest("当前系统不允许创建测试文件链接")
        with self.assertRaises(ValueError):
            cache.cleanup(self.root, self.env)
        self.assertTrue(outside.is_file())
        self.assertTrue(self.destination.is_file())


if __name__ == "__main__":
    unittest.main()
