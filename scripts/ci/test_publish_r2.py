import copy
import hashlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from types import SimpleNamespace

from publish_r2 import current_head, list_objects, validate_inputs, validate_policy


class ReleaseTests(unittest.TestCase):
    # 首次发布不存在旧清单时也必须显式请求 JSON，不受 Runner 的 CLI 默认格式影响。
    def test_empty_object_listing_requests_json(self):
        def run(command, **options):
            output = '{"KeyCount": 0}' if command[-2:] == ["--output", "json"] else ""
            return SimpleNamespace(stdout=output)
        with patch("publish_r2.subprocess.run", side_effect=run):
            self.assertEqual(list_objects("a" * 32, "mskdsp-update", "msk104master/ci", {}), [])

    # 非法对象列表不能被当成首次发布而忽略已有版本保护。
    def test_invalid_object_listing_is_rejected(self):
        for value in ('[]', '{"Contents": null}', '{"Contents": ["invalid"]}'):
            with self.subTest(value=value), patch("publish_r2.subprocess.run", return_value=SimpleNamespace(stdout=value)):
                with self.assertRaises(ValueError):
                    list_objects("a" * 32, "mskdsp-update", "msk104master/ci", {})

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        (self.root / "installer").mkdir()
        self.env = {"CI_COMMIT_BRANCH": "main", "CI_COMMIT_REF_PROTECTED": "true",
                    "CI_PIPELINE_SOURCE": "push", "CI_COMMIT_SHA": "a" * 40,
                    "CI_PIPELINE_IID": "12", "CI_PIPELINE_URL": "https://ci/pipelines/12"}
        self.info = {"version": "0.1.0-ci.12", "signed": True, "installer": "master-setup.exe",
                     "commit": "a" * 40, "pipeline": self.env["CI_PIPELINE_URL"],
                     "target": "x86_64-pc-windows-msvc", "built_at": "2026-11-06T00:00:00Z"}
        for name, data in (("msk104master.exe", b"exe"),
                           ("installer/master-setup.exe", b"installer"),
                           ("installer/master-setup.exe.sig", b"signature")):
            (self.root / name).write_bytes(data)
        self.save()

    def save(self):
        (self.root / "build-info.json").write_text(json.dumps(self.info), encoding="utf-8")
        files = ["msk104master.exe", "installer/master-setup.exe", "installer/master-setup.exe.sig"]
        (self.root / "SHA256SUMS.txt").write_text("".join(
            hashlib.sha256((self.root / name).read_bytes()).hexdigest() + "  " + name + "\n"
            for name in files), encoding="utf-8")

    # 只允许受保护 main 的受支持来源发布。
    def test_policy(self):
        validate_policy(self.env)
        for key, value in (("CI_COMMIT_BRANCH", "feature"), ("CI_COMMIT_REF_PROTECTED", "false"),
                           ("CI_PIPELINE_SOURCE", "merge_request_event"), ("R2_PREFIX", "slavesim")):
            env = dict(self.env, **{key: value})
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_policy(env)

    # 输入需属于本次流水线，且安装包完整；替换包和回退元数据均被拒绝。
    def test_inputs(self):
        validate_inputs(self.root, self.env)
        (self.root / "installer/master-setup.exe").write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            validate_inputs(self.root, self.env)
        self.save()
        original = copy.deepcopy(self.info)
        for key, value in (("signed", False), ("commit", "b" * 40),
                           ("version", "0.1.0-ci.11"), ("installer", "../outside.exe")):
            self.info = dict(original, **{key: value})
            self.save()
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_inputs(self.root, self.env)

    # JOB-TOKEN 只请求授权的分支列表接口，精确匹配 main 当前提交。
    def test_current_head_matches_main(self):
        env = dict(self.env, CI_API_V4_URL="https://gitlab.example/api/v4",
                   CI_PROJECT_ID="11", CI_JOB_TOKEN="test-token")
        response = io.BytesIO(json.dumps([
            {"name": "main-backup", "commit": {"id": "b" * 40}},
            {"name": "main", "commit": {"id": self.env["CI_COMMIT_SHA"]}},
        ]).encode())
        with patch("publish_r2.urlopen", return_value=response) as request:
            current_head(env)
        self.assertEqual(request.call_args.args[0].full_url,
                         "https://gitlab.example/api/v4/projects/11/repository/branches?search=%5Emain%24&per_page=100")
        self.assertEqual(request.call_args.args[0].get_header("Job-token"), "test-token")

    # main 已前进、缺失或响应重复时均不能覆盖更新清单。
    def test_current_head_rejects_stale_or_missing_main(self):
        env = dict(self.env, CI_API_V4_URL="https://gitlab.example/api/v4",
                   CI_PROJECT_ID="11", CI_JOB_TOKEN="test-token")
        main = {"name": "main", "commit": {"id": self.env["CI_COMMIT_SHA"]}}
        for branches in ([{"name": "main", "commit": {"id": "b" * 40}}],
                         [], [{"name": "main-backup", "commit": main["commit"]}],
                         [main, main]):
            with self.subTest(branches=branches):
                response = io.BytesIO(json.dumps(branches).encode())
                with patch("publish_r2.urlopen", return_value=response), self.assertRaises(ValueError):
                    current_head(env)


if __name__ == "__main__":
    unittest.main()
