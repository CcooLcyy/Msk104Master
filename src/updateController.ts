export interface UpdateInfo { version: string; body: string | null; }

export interface UpdateViewState {
  candidate: UpdateInfo | null;
  busy: "checking" | "installing" | null;
  message: string;
  failed: boolean;
}

export const initialUpdateState: UpdateViewState = {
  candidate: null, busy: null, message: "", failed: false,
};

interface UpdateCommands {
  check: () => Promise<UpdateInfo | null>;
  install: () => Promise<void>;
}

export function createUpdateController(commands: UpdateCommands, onChange: (state: UpdateViewState) => void) {
  let state = initialUpdateState;
  let active = false;

  function publish(patch: Partial<UpdateViewState>) {
    state = { ...state, ...patch };
    if (active) onChange(state);
  }

  async function check() {
    // 同步记录忙碌状态，手动与定时触发不能在 React 重绘前重复发起请求。
    if (!active || state.busy !== null) return;
    publish({ busy: "checking", candidate: null, failed: false, message: "正在检查更新…" });
    console.info("开始检查主站更新");
    try {
      const candidate = await commands.check();
      const message = candidate ? `发现新版本 v${candidate.version}` : "当前已是最新版本";
      publish({ candidate, message });
      console.info(message);
    } catch (error: unknown) {
      const message = `检查更新失败：${String(error)}`;
      publish({ failed: true, message });
      console.warn(message);
    } finally {
      publish({ busy: null });
    }
    // 检查结束后立即进入安装，共用同步忙碌状态，避免定时检查或重复点击插入。
    if (state.candidate !== null) await install();
  }

  async function install() {
    if (!active || state.busy !== null || state.candidate === null) return;
    publish({ busy: "installing", failed: false, message: "正在下载并校验更新，完成后将退出程序并打开安装器…" });
    console.info("开始下载并安装主站更新，无需确认，不等待现有连接结束");
    try {
      await commands.install();
      publish({ candidate: null, message: "已交给安装器，请完成升级" });
    } catch (error: unknown) {
      const message = `安装更新失败：${String(error)}`;
      publish({ failed: true, message });
      console.warn(message);
    } finally {
      publish({ busy: null });
    }
  }

  function start() {
    active = true;
    console.info("已启用主站自动更新，间隔 60 秒，发现新版本立即下载安装");
    // 延迟到下一个事件循环，StrictMode 立即重建 effect 时可撤销首次检查。
    const initial = setTimeout(() => void check(), 0);
    const interval = setInterval(() => void check(), 60_000);
    return () => {
      active = false;
      clearTimeout(initial);
      clearInterval(interval);
      console.info("已停止主站自动检查更新");
    };
  }

  return { check, install, start };
}
