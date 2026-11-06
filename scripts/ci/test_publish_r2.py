import copy
import hashlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from types import SimpleNamespace

from publish_r2 import artifact_root, current_head, list_objects, public_digest, validate_inputs, validate_policy


class ReleaseTests(unittest.TestCase):
    # 同版本内容一致时复用目录；安装包或签名变化时不可覆盖旧 immutable 地址。
    def test_artifact_root_is_bound_to_installer_and_signature(self):
        name = "master-setup.exe"
        installer = "installer/" + name
        checksums = {installer: hashlib.sha256(b"installer").hexdigest(),
                     installer + ".sig": hashlib.sha256(b"signature").hexdigest()}
        root = artifact_root("0.1.0-ci.12", name, checksums)
        self.assertRegex(root, r"^msk104master/ci/0\.1\.0-ci\.12/[0-9a-f]{64}$")
        self.assertEqual(root, artifact_root("0.1.0-ci.12", name, dict(checksums)))
        for filename in (installer, installer + ".sig"):
            changed = dict(checksums, **{filename: hashlib.sha256(b"rebuilt").hexdigest()})
            with self.subTest(filename=filename):
                self.assertNotEqual(root, artifact_root("0.1.0-ci.12", name, changed))

    # 公开校验请求声明发布器身份，并对完整响应计算摘要。
    def test_public_digest_uses_publisher_identity(self):
        with patch("publish_r2.urlopen", return_value=io.BytesIO(b"installer")) as request:
            self.assertEqual(public_digest("https://example.invalid/master.exe"), hashlib.sha256(b"installer").hexdigest())
        self.assertEqual(request.call_args.args[0].get_header("User-agent"), "msk104master-publisher/1")

    # 首次发布不存在旧清单时也必须显式请求 JSON，不受 Runner 的 CLI 默认格式影响。
    def test_empty_object_listing_requests_json(self):
        def run(command, **options):
            output = '{"KeyCount": 0}' if command[-2:] == ["--output", "json"] and "--no-paginate" in command else ""
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
                    "CI_PIPELINE_IID": "12", "CI_PIPELINE_URL": "https://ci/pipelines/12",
                    "GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": "CcooLcyy/Msk104Master",
                    "GITHUB_REF": "refs/heads/main", "GITHUB_EVENT_NAME": "push",
                    "R2_PUBLISH_ENABLED": "true"}
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
        validate_policy(dict(self.env, GITHUB_EVENT_NAME="workflow_dispatch", CI_PIPELINE_SOURCE="web"))
        for key, value in (("CI_COMMIT_BRANCH", "feature"), ("CI_COMMIT_REF_PROTECTED", "false"),
                           ("CI_PIPELINE_SOURCE", "merge_request_event"), ("R2_PREFIX", "slavesim"),
                           ("GITHUB_ACTIONS", "false"), ("GITHUB_REPOSITORY", "other/Msk104Master"),
                           ("GITHUB_REF", "refs/heads/feature"), ("GITHUB_EVENT_NAME", "pull_request"),
                           ("GITHUB_EVENT_NAME", "pull_request_target"), ("R2_PUBLISH_ENABLED", "false")):
            env = dict(self.env, **{key: value})
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                validate_policy(env)

    # GitLab 原有受保护 main 的上下文不能绕过新的发布入口与显式开关。
    def test_gitlab_only_and_missing_publish_switch_are_rejected(self):
        env = {key: value for key, value in self.env.items() if not key.startswith("GITHUB_")}
        with self.assertRaises(ValueError):
            validate_policy(env)
        env = dict(self.env)
        del env["R2_PUBLISH_ENABLED"]
        with self.assertRaises(ValueError):
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

    # 固定 GitHub API 地址并使用 Bearer token，精确匹配 main 当前提交。
    def test_current_head_matches_main(self):
        env = dict(self.env, GH_TOKEN="test-token", GITHUB_API_URL="https://untrusted.invalid",
                   CI_API_V4_URL="https://gitlab.example/api/v4", CI_JOB_TOKEN="old-token")
        response = io.BytesIO(json.dumps({"ref": "refs/heads/main", "object": {
            "type": "commit", "sha": self.env["CI_COMMIT_SHA"]}}).encode())
        with patch("publish_r2.urlopen", return_value=response) as request:
            current_head(env)
        self.assertEqual(request.call_args.args[0].full_url,
                         "https://api.github.com/repos/CcooLcyy/Msk104Master/git/ref/heads/main")
        self.assertEqual(request.call_args.args[0].get_header("Authorization"), "Bearer test-token")
        self.assertEqual(request.call_args.args[0].get_header("Accept"), "application/vnd.github+json")
        self.assertEqual(request.call_args.args[0].get_header("X-github-api-version"), "2022-11-28")
        self.assertIsNone(request.call_args.args[0].get_header("Job-token"))

    # main 已前进、引用不符及非法 JSON 结构均不能覆盖更新清单。
    def test_current_head_rejects_stale_or_missing_main(self):
        env = dict(self.env, GH_TOKEN="test-token")
        obj = {"type": "commit", "sha": self.env["CI_COMMIT_SHA"]}
        for data in ([], None, "invalid", {}, {"ref": "refs/heads/main", "object": None},
                     {"ref": "refs/heads/main", "object": []},
                     {"ref": "refs/heads/main-backup", "object": obj},
                     *({"ref": "refs/heads/main", "object": dict(obj, **change)} for change in (
                         {"sha": "b" * 40}, {"sha": "invalid"}, {"sha": 123}, {"type": "tag"}))):
            with self.subTest(data=data):
                response = io.BytesIO(json.dumps(data).encode())
                with patch("publish_r2.urlopen", return_value=response), self.assertRaises(ValueError):
                    current_head(env)

    # 缺失 API token 或无效本次 SHA 时先拒绝，不发送网络请求。
    def test_current_head_rejects_invalid_credentials_or_commit(self):
        for change in ({"GH_TOKEN": ""}, {"GH_TOKEN": " "},
                       {"GH_TOKEN": "test-token", "CI_COMMIT_SHA": ""},
                       {"GH_TOKEN": "test-token", "CI_COMMIT_SHA": "not-a-sha"}):
            with self.subTest(change=change), patch("publish_r2.urlopen") as request:
                with self.assertRaises(ValueError):
                    current_head(dict(self.env, **change))
                request.assert_not_called()


if __name__ == "__main__":
    unittest.main()
