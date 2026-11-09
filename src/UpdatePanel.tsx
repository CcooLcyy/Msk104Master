import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createUpdateController, initialUpdateState, type UpdateInfo } from "./updateController";

export default function UpdatePanel({ enabled }: { enabled: boolean }) {
  const [{ candidate, busy, message, failed }, setState] = useState(initialUpdateState);
  const [controller] = useState(() => createUpdateController({
    check: () => invoke<UpdateInfo | null>("check_update"),
    install: () => invoke<void>("install_update"),
  }, setState));

  useEffect(() => {
    if (enabled) return controller.start();
  }, [controller, enabled]);

  return <section className="update-panel" aria-label="软件更新">
    <p className="update-status">每分钟自动检查，发现新版本立即下载安装；安装会关闭程序并中断现有连接。</p>
    <button className="button secondary compact" disabled={!enabled || busy !== null} onClick={() => void controller.check()}>
      {busy === "checking" ? "正在检查…" : "检查更新"}
    </button>
    {message && <p className={failed ? "update-error" : "update-status"} role={failed ? "alert" : "status"}>{message}</p>}
    {candidate && <>
      {candidate.body && <p className="update-notes">{candidate.body}</p>}
      <p>安装将关闭当前程序，完成后重新打开。</p>
      {failed && <button className="button primary compact" disabled={busy !== null} onClick={() => void controller.install()}>
        重试下载安装
      </button>}
    </>}
  </section>;
}
