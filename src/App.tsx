import UpdatePanel from "./UpdatePanel";
import { useEffect, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AppInfo {
  name: string;
  version: string;
  protocol_ready: boolean;
}

const sections = [
  { id: "connection", title: "连接管理", subtitle: "配置主站连接参数", number: "01" },
  { id: "points", title: "实时点表", subtitle: "遥测、遥信与品质", number: "02" },
  { id: "commands", title: "命令操作", subtitle: "遥控与遥调", number: "03" },
  { id: "frames", title: "报文监视", subtitle: "收发记录与协议解析", number: "04" },
] as const;
type SectionId = (typeof sections)[number]["id"];

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function EmptyState({ title, children }: { title: string; children: string }) {
  return (
    <div className="empty-state">
      <div className="empty-symbol" aria-hidden="true">↔</div>
      <h3>{title}</h3>
      <p>{children}</p>
    </div>
  );
}

export default function App() {
  const [active, setActive] = useState<SectionId>("connection");
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [appError, setAppError] = useState("");
  const [config, setConfig] = useState({ host: "127.0.0.1", port: "2404", commonAddress: "1" });
  const [validation, setValidation] = useState<{ ok: boolean; message: string } | null>(null);
  const [validating, setValidating] = useState(false);
  const section = sections.find((item) => item.id === active)!;

  useEffect(() => {
    let disposed = false;
    invoke<AppInfo>("get_app_info")
      .then((result) => {
        if (!disposed) {
          setInfo(result);
          console.info("已读取主站应用信息，协议能力：", result.protocol_ready);
        }
      })
      .catch((error: unknown) => {
        console.error("读取主站应用信息失败：", error);
        if (!disposed) setAppError(errorMessage(error));
      });
    return () => { disposed = true; };
  }, []);

  function updateConfig(key: keyof typeof config, value: string) {
    setConfig((current) => ({ ...current, [key]: value }));
    setValidation(null);
  }

  async function validate(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setValidation(null);
    setValidating(true);
    try {
      await invoke("validate_connection", {
        config: {
          host: config.host.trim(),
          port: Number(config.port),
          commonAddress: Number(config.commonAddress),
        },
      });
      setValidation({ ok: true, message: "参数格式有效。未建立 TCP 连接，未发送任何协议报文。" });
      console.info("连接参数校验通过；未建立协议连接");
    } catch (error: unknown) {
      setValidation({ ok: false, message: `参数校验失败：${errorMessage(error)}` });
      console.error("连接参数校验失败：", error);
    } finally {
      setValidating(false);
    }
  }

  return (
    <div className="workspace">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">104</span>
          <div><strong>Msk104Master</strong><span>IEC104 模拟主站</span></div>
        </div>
        <p className="nav-label">主站工作台</p>
        <nav aria-label="主站功能">
          {sections.map((item) => (
            <button key={item.id} className={`nav-item ${active === item.id ? "selected" : ""}`}
              aria-current={active === item.id ? "page" : undefined}
              onClick={() => { setActive(item.id); console.info("切换主站页面：", item.title); }}>
              <span className="nav-number">{item.number}</span><span>{item.title}</span>
            </button>
          ))}
        </nav>
        <div className="sidebar-footer">
          <span className="foundation-label">工程骨架</span>
          <p>协议通信尚未实现</p>
          <span className="version">{info ? `v${info.version}` : "版本待读取"}</span>
          <UpdatePanel />
        </div>
      </aside>

      <main>
        <header className="topbar">
          <span>工作台 <span className="breadcrumb-divider">/</span> {section.title}</span>
          <span className="connection-state"><i />未连接</span>
        </header>
        <div className="page-content">
          <div className="page-heading">
            <div><p className="eyebrow">IEC 60870-5-104</p><h1>{section.title}</h1><p>{section.subtitle}</p></div>
            <span className="edition">Tauri + Rust</span>
          </div>

          {appError && <div className="notice error" role="alert"><strong>应用信息读取失败</strong><span>{appError}</span></div>}
          <div className="notice foundation"><span className="notice-icon" aria-hidden="true">i</span><div>
            <strong>当前为工程骨架</strong>
            <span>连接、总召、遥控和遥调尚未实现。参数校验仅检查格式，不代表设备可达或协议通信正常。</span>
          </div></div>

          {active === "connection" && <div className="connection-grid">
            <section className="panel">
              <div className="panel-heading"><h2>连接参数</h2><span>TCP 客户端 · 主站</span></div>
              <form onSubmit={validate}>
                <fieldset disabled={validating}>
                  <label>目标主机<input value={config.host} onChange={(event) => updateConfig("host", event.target.value)} required autoComplete="off" spellCheck={false} /><small>输入目标设备的 IP 地址或主机名</small></label>
                  <div className="field-row">
                    <label>TCP 端口<input type="number" min="1" max="65535" step="1" value={config.port} onChange={(event) => updateConfig("port", event.target.value)} required /><small>默认端口 2404</small></label>
                    <label>公共地址（CA）<input type="number" min="1" max="65534" step="1" value={config.commonAddress} onChange={(event) => updateConfig("commonAddress", event.target.value)} required /><small>十进制，须与从站配置一致</small></label>
                  </div>
                </fieldset>
                {validation && <p className={`validation ${validation.ok ? "valid" : "invalid"}`} role="status">{validation.message}</p>}
                <div className="form-actions"><button type="submit" className="button secondary" disabled={validating}>{validating ? "正在校验…" : "校验参数"}</button><button type="button" className="button primary" disabled title="104 协议通信尚未实现">建立连接</button></div>
              </form>
            </section>
            <section className="panel session-panel">
              <div className="panel-heading"><h2>会话状态</h2><span className="neutral-pill">未连接</span></div>
              <dl className="session-details"><div><dt>TCP 连接</dt><dd>未建立</dd></div><div><dt>数据传输</dt><dd>未启动</dd></div><div><dt>总召状态</dt><dd>未执行</dd></div><div><dt>最近接收</dt><dd>—</dd></div></dl>
              <p className="panel-note">当前参数仅保留在本次窗口中，不自动连接目标设备。</p>
            </section>
          </div>}

          {active === "points" && <section className="panel">
            <div className="panel-heading"><h2>接收点值</h2><button className="button secondary compact" disabled>发送总召</button></div>
            <div className="table-scroll"><table><thead><tr>{["公共地址", "信息地址", "类型", "值", "品质", "协议时标", "接收时间"].map((title) => <th key={title}>{title}</th>)}</tr></thead><tbody><tr><td colSpan={7}><EmptyState title="尚未接收点值">连接与接收能力实现后，将在这里展示设备实际上送的数据。</EmptyState></td></tr></tbody></table></div>
          </section>}

          {active === "commands" && <section className="panel">
            <div className="panel-heading"><h2>命令发送</h2><span>协议能力待实现</span></div>
            <div className="command-body"><fieldset disabled><div className="field-row"><label>命令类型<select><option>短浮点遥调</option><option>单点遥控</option><option>双点遥控</option></select></label><label>信息地址（IOA）<input placeholder="输入十进制地址" /></label></div><label>命令值<input placeholder="输入待下发值" /></label></fieldset><div className="form-actions"><button className="button primary" disabled>发送命令</button></div></div>
            <div className="section-divider" /><EmptyState title="暂无命令事务">尚未发送命令。后续将分别记录发送、协议确认及业务反馈。</EmptyState>
          </section>}

          {active === "frames" && <section className="panel">
            <div className="panel-heading"><h2>协议报文</h2><span>原始报文 · 解析事件</span></div>
            <div className="table-scroll"><table><thead><tr>{["接收 / 发送时间", "方向", "帧类型", "报文内容"].map((title) => <th key={title}>{title}</th>)}</tr></thead><tbody><tr><td colSpan={4}><EmptyState title="尚无收发报文">当前未建立协议连接，没有实际报文可供展示。</EmptyState></td></tr></tbody></table></div>
          </section>}
          <footer className="page-footer"><span>Msk104Master</span><span>独立主站 · 人工联调与自动化验收</span></footer>
        </div>
      </main>
    </div>
  );
}
