"""校验本次 CI 的 Windows 安装包并最后发布 Tauri 更新清单。"""

import hashlib
import json
import os
import re
import subprocess
from pathlib import Path
from urllib.parse import quote
from urllib.request import Request, urlopen


BASE = "https://pub-19f3d71852b04011b120b1b814141c12.r2.dev"
PREFIX = "msk104master"


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def artifact_root(version, name, checksums):
    # 签名或安装包变化时使用新目录，避免同版本重跑覆盖 immutable 下载地址。
    fingerprint = hashlib.sha256((checksums["installer/" + name]
                                  + checksums["installer/" + name + ".sig"]).encode("ascii")).hexdigest()
    return PREFIX + "/ci/" + version + "/" + fingerprint


def validate_policy(env):
    if (env.get("GITHUB_ACTIONS") != "true"
            or env.get("GITHUB_REPOSITORY") != "CcooLcyy/Msk104Master"
            or env.get("GITHUB_REF") != "refs/heads/main"
            or env.get("GITHUB_EVENT_NAME") not in ("push", "workflow_dispatch")
            or env.get("R2_PUBLISH_ENABLED") != "true"):
        raise ValueError("仅允许已启用发布的 Msk104Master GitHub main 推送或手动工作流发布")
    if env.get("R2_PREFIX", PREFIX) != PREFIX:
        raise ValueError("禁止修改其他项目的 R2 前缀")
    if env.get("R2_PUBLIC_BASE_URL", BASE).rstrip("/") != BASE:
        raise ValueError("R2 公开地址与客户端更新源不一致")


def validate_inputs(folder, env):
    data = json.loads((folder / "build-info.json").read_text(encoding="utf-8"))
    version = data.get("version", "")
    if (data.get("signed") is not True or data.get("commit") != env["CI_COMMIT_SHA"]
            or data.get("pipeline") != env["CI_PIPELINE_URL"]
            or data.get("target") != "x86_64-pc-windows-msvc"
            or not re.fullmatch(r"\d+\.\d+\.\d+-ci\." + re.escape(env["CI_PIPELINE_IID"]), version)):
        raise ValueError("发布输入并非本次流水线的已签名 Windows 产物")
    name = data.get("installer", "")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*\.exe", name):
        raise ValueError("安装包名称无效")
    expected = {"msk104master.exe", "installer/" + name, "installer/" + name + ".sig"}
    checksums = {}
    for line in (folder / "SHA256SUMS.txt").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        if not match or match[2] not in expected or match[2] in checksums:
            raise ValueError("产物校验清单包含无效、重复或额外文件")
        checksum, filename = match.groups()
        path = folder / filename
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0 or digest(path) != checksum:
            raise ValueError("产物 SHA256 校验失败：" + filename)
        checksums[filename] = checksum
    if set(checksums) != expected:
        raise ValueError("缺少安装包、签名或可执行文件的 SHA256")
    return data, checksums


def current_head(env):
    # 不向日志写入请求头或凭据。
    token = env.get("GH_TOKEN", "")
    if not token.strip():
        raise ValueError("缺少 GH_TOKEN，无法核对 main 当前提交")
    expected_sha = env.get("CI_COMMIT_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", expected_sha):
        raise ValueError("本次提交 SHA 无效")
    # 固定可信 API 地址，避免自定义环境变量将 token 发送到其他主机。
    request = Request("https://api.github.com/repos/CcooLcyy/Msk104Master/git/ref/heads/main",
                      headers={"Authorization": "Bearer " + token,
                               "Accept": "application/vnd.github+json",
                               "X-GitHub-Api-Version": "2022-11-28",
                               "User-Agent": "msk104master-publisher/1"})
    with urlopen(request, timeout=60) as response:
        data = json.load(response)
    if (not isinstance(data, dict) or data.get("ref") != "refs/heads/main"
            or not isinstance(data.get("object"), dict)
            or data["object"].get("type") != "commit"
            or not isinstance(data["object"].get("sha"), str)
            or not re.fullmatch(r"[0-9a-f]{40}", data["object"]["sha"])):
        raise ValueError("GitHub main 引用响应格式无效")
    if data["object"]["sha"] != expected_sha:
        raise ValueError("main 已前进，拒绝过期提交覆盖更新清单")


def public_digest(url):
    request = Request(url, headers={"Cache-Control": "no-cache", "User-Agent": "msk104master-publisher/1"})
    with urlopen(request, timeout=120) as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def list_objects(account, bucket, channel, publish_env):
    result = subprocess.run(["aws", "--endpoint-url", f"https://{account}.r2.cloudflarestorage.com",
                             "s3api", "list-objects-v2", "--bucket", bucket,
                             "--prefix", channel + "/latest.json", "--no-paginate", "--output", "json"],
                            env=publish_env, check=True, capture_output=True, text=True)
    data = json.loads(result.stdout)
    objects = data.get("Contents", []) if isinstance(data, dict) else None
    if not isinstance(objects, list) or any(not isinstance(item, dict) for item in objects):
        raise ValueError("R2 对象列表响应格式无效")
    return objects


def main():
    env = dict(os.environ)
    validate_policy(env)
    folder = Path("package")
    data, checksums = validate_inputs(folder, env)
    account = env.get("R2_ACCOUNT_ID", "")
    if not re.fullmatch(r"[0-9a-f]{32}", account):
        raise ValueError("缺少或无效的 R2_ACCOUNT_ID")
    for name in ("R2_ACCESS_KEY_ID", "R2_SECRET_ACCESS_KEY"):
        if not env.get(name):
            raise ValueError("缺少 CI 变量：" + name)
    bucket = env.get("R2_BUCKET", "mskdsp-update")
    if bucket != "mskdsp-update":
        raise ValueError("R2 存储桶与既有发布方案不一致")
    publish_env = dict(env, AWS_ACCESS_KEY_ID=env["R2_ACCESS_KEY_ID"],
                       AWS_SECRET_ACCESS_KEY=env["R2_SECRET_ACCESS_KEY"],
                       AWS_DEFAULT_REGION="auto", AWS_EC2_METADATA_DISABLED="true",
                       AWS_REQUEST_CHECKSUM_CALCULATION="when_required",
                       AWS_RESPONSE_CHECKSUM_VALIDATION="when_required")

    def upload(path, key, cache):
        subprocess.run(["aws", "--endpoint-url", f"https://{account}.r2.cloudflarestorage.com",
                        "s3", "cp", str(path), f"s3://{bucket}/{key}",
                        "--cache-control", cache, "--only-show-errors"], env=publish_env, check=True)

    current_head(env)
    channel = PREFIX + "/ci"
    name = data["installer"]
    root = artifact_root(data["version"], name, checksums)
    signature = (folder / "installer" / (name + ".sig")).read_text(encoding="utf-8").strip()
    manifest = dict(version=data["version"], notes="IEC104 模拟主站 CI 更新；提交 " + data["commit"][:12],
                    pub_date=data["built_at"], commit=data["commit"], pipeline_iid=int(env["CI_PIPELINE_IID"]),
                    platforms={"windows-x86_64": dict(signature=signature, url=BASE + "/" + root + "/" + quote(name))})
    # 同一提交重跑也不能让旧流水线回滚较新版本；工作流并发组使检查和切换串行。
    existing = folder / "previous-latest.json"
    objects = list_objects(account, bucket, channel, publish_env)
    if any(item.get("Key") == channel + "/latest.json" for item in objects):
        subprocess.run(["aws", "--endpoint-url", f"https://{account}.r2.cloudflarestorage.com",
                        "s3", "cp", f"s3://{bucket}/{channel}/latest.json", str(existing), "--only-show-errors"],
                       env=publish_env, check=True)
        previous = json.loads(existing.read_text(encoding="utf-8"))
        if previous.get("pipeline_iid", 0) > int(env["CI_PIPELINE_IID"]):
            raise ValueError("拒绝旧流水线回滚已发布更新")
    for filename in ("installer/" + name, "installer/" + name + ".sig"):
        key = root + "/" + Path(filename).name
        upload(folder / filename, key, "public,max-age=31536000,immutable")
        if public_digest(BASE + "/" + key) != checksums[filename]:
            raise ValueError("R2 公开下载内容与构建输入不一致")
    current_head(env)
    latest = folder / "latest.json"
    latest.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    upload(latest, channel + "/latest.json", "no-store,max-age=0")
    if public_digest(BASE + "/" + channel + "/latest.json") != digest(latest):
        raise ValueError("R2 公开更新清单验收失败")
    print(f"R2 更新发布完成：{BASE}/{channel}/latest.json；版本 {data['version']}")


if __name__ == "__main__":
    main()
