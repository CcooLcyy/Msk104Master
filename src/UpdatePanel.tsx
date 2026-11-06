import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface UpdateInfo { version: string; body: string | null; }

export default function UpdatePanel() {
  const [candidate, setCandidate] = useState<UpdateInfo | null>(null);
  const [busy, setBusy] = useState<"checking" | "installing" | null>(null);
  const [message, setMessage] = useState("");
  const [failed, setFailed] = useState(false);

  async function check() {
    setBusy("checking");
    setCandidate(null);
    setFailed(false);
    setMessage("正在检查更新…");
    try {
      const result = await invoke<UpdateInfo | null>("check_update");
      setCandidate(result);
      setMessage(result ? `发现新版本 v${result.version}` : "当前已是最新版本");
    } catch (error: unknown) {
      setFailed(true);
      setMessage(`检查更新失败：${String(error)}`);
    } finally {
      setBusy(null);
    }
  }

  async function install() {
    setBusy("installing");
    setFailed(false);
    setMessage("正在下载并校验更新，完成后将退出程序并打开安装器…");
    try {
      await invoke("install_update");
      setMessage("已交给安装器，请完成升级");
    } catch (error: unknown) {
      setFailed(true);
      setMessage(`安装更新失败：${String(error)}`);
    } finally {
      setBusy(null);
    }
  }

  return <section className="update-panel" aria-label="软件更新">
    <button className="button secondary compact" disabled={busy !== null} onClick={() => void check()}>
      {busy === "checking" ? "正在检查…" : "检查更新"}
    </button>
    {message && <p className={failed ? "update-error" : "update-status"} role={failed ? "alert" : "status"}>{message}</p>}
    {candidate && <>
      {candidate.body && <p className="update-notes">{candidate.body}</p>}
      <p>安装将关闭当前程序，完成后重新打开。</p>
      <button className="button primary compact" disabled={busy !== null} onClick={() => void install()}>
        {busy === "installing" ? "正在安装…" : "下载并安装更新"}
      </button>
    </>}
  </section>;
}
