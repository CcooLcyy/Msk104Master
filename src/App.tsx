import UpdatePanel from "./UpdatePanel";
import { useEffect, useRef, useState, type CSSProperties, type FormEvent, type KeyboardEvent, type PointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";

interface AppInfo { name: string; version: string; protocol_ready: boolean }

const pointTypes = [
  { id: "signals", label: "遥信", columns: ["公共地址", "信息地址", "类型", "状态", "品质", "协议时标", "接收时间"] },
  { id: "measurements", label: "遥测", columns: ["公共地址", "信息地址", "类型", "数值", "品质", "协议时标", "接收时间"] },
  { id: "controls", label: "遥控", columns: ["公共地址", "信息地址", "命令类型", "命令值", "协议确认", "业务反馈", "时间"] },
  { id: "setpoints", label: "遥调", columns: ["公共地址", "信息地址", "命令类型", "设定值", "协议确认", "业务反馈", "时间"] },
  { id: "energy", label: "电量", columns: ["公共地址", "信息地址", "累计值", "序号", "品质", "协议时标", "接收时间"] },
  { id: "events", label: "SOE", columns: ["公共地址", "信息地址", "事件状态", "品质", "协议时标", "接收时间"] },
] as const;
type PointType = (typeof pointTypes)[number]["id"];
const protocolUnavailable = "104 协议通信尚未实现";

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function EmptyTable({ columns, message, label }: { columns: readonly string[]; message: string; label: string }) {
  return <div className="table-scroll"><table aria-label={label}>
    <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
    <tbody><tr><td colSpan={columns.length} className="empty-cell">{message}</td></tr></tbody>
  </table></div>;
}

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [appError, setAppError] = useState("");
  const [config, setConfig] = useState({ host: "127.0.0.1", port: "2404", commonAddress: "1" });
  const [validation, setValidation] = useState<{ ok: boolean; message: string } | null>(null);
  const [validating, setValidating] = useState(false);
  const validationBusy = useRef(false);
  const [statusOpen, setStatusOpen] = useState(true);
  const [settingsOpen, setSettingsOpen] = useState(true);
  const [paused, setPaused] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const [activePoint, setActivePoint] = useState<PointType>("signals");
  const [detailMode, setDetailMode] = useState<"raw" | "parsed">("raw");
  const [displayMessage, setDisplayMessage] = useState("尚无实际报文，等待协议能力接入");
  const [split, setSplit] = useState(42);
  const monitorRef = useRef<HTMLDivElement>(null);
  const dragPointer = useRef<number | null>(null);
  const selectedPoint = pointTypes.find((point) => point.id === activePoint)!;

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
    if (validationBusy.current) return;
    setConfig((current) => ({ ...current, [key]: value }));
    setValidation(null);
  }

  async function validate(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    // 同步守卫避免重渲染前重复提交，校验期间禁止修改同一份参数。
    if (validationBusy.current) return;
    validationBusy.current = true;
    setValidation(null);
    setValidating(true);
    try {
      await invoke("validate_connection", {
        config: { host: config.host.trim(), port: Number(config.port), commonAddress: Number(config.commonAddress) },
      });
      setValidation({ ok: true, message: "参数格式有效。未建立 TCP 连接，未发送任何协议报文。" });
      console.info("连接参数校验通过；未建立协议连接");
    } catch (error: unknown) {
      setValidation({ ok: false, message: `参数校验失败：${errorMessage(error)}` });
      console.error("连接参数校验失败：", error);
    } finally {
      validationBusy.current = false;
      setValidating(false);
    }
  }

  function changeSplit(value: number) {
    setSplit(Math.max(25, Math.min(65, Math.round(value))));
  }

  function moveSplitter(event: PointerEvent<HTMLDivElement>) {
    if (dragPointer.current !== event.pointerId || !monitorRef.current) return;
    const bounds = monitorRef.current.getBoundingClientRect();
    changeSplit((event.clientY - bounds.top) / bounds.height * 100);
  }

  function splitterKey(event: KeyboardEvent<HTMLDivElement>) {
    const changes: Record<string, number> = { ArrowUp: split - 2, ArrowDown: split + 2, Home: 25, End: 65 };
    if (!(event.key in changes)) return;
    event.preventDefault();
    changeSplit(changes[event.key]);
    console.info("键盘调整报文分区比例");
  }

  function selectPoint(id: PointType) {
    setActivePoint(id);
    console.info("切换分类点表：", pointTypes.find((point) => point.id === id)!.label);
  }

  function pointTabKey(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    let next: number;
    if (event.key === "ArrowRight") next = (index + 1) % pointTypes.length;
    else if (event.key === "ArrowLeft") next = (index + pointTypes.length - 1) % pointTypes.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = pointTypes.length - 1;
    else return;
    event.preventDefault();
    selectPoint(pointTypes[next].id);
    document.getElementById(`tab-${pointTypes[next].id}`)?.focus();
  }

  return <div className="workspace">
    <header className="topbar">
      <div className="brand"><span className="brand-mark" aria-hidden="true">104</span>
        <div><h1>Msk104Master</h1><span>IEC104 模拟主站 · 调试工作台</span></div>
      </div>
      <div className="toolbar" role="group" aria-label="主站操作">
        <button className="button" disabled title={`建立连接：${protocolUnavailable}`}>连接</button>
        <button className="button" disabled title={`断开连接：${protocolUnavailable}`}>断开</button>
        <span className="toolbar-divider" aria-hidden="true" />
        <button className="button" disabled title={protocolUnavailable}>总召</button>
        <button className="button" disabled title={protocolUnavailable}>对时</button>
        <span className="toolbar-divider" aria-hidden="true" />
        <button className="button" onClick={() => { setDisplayMessage("已清空显示；当前没有实际报文或点值"); console.info("已清空工作台显示，无实际数据"); }}>清空显示</button>
        <button className="button" aria-pressed={paused} title="仅暂停界面显示，不停止协议传输" onClick={() => {
          setPaused(!paused); console.info(paused ? "已恢复报文显示" : "已暂停报文显示；不影响协议传输");
        }}>{paused ? "恢复显示" : "暂停显示"}</button>
        <label className="check-label"><input type="checkbox" checked={autoScroll} onChange={(event) => {
          setAutoScroll(event.target.checked); console.info(event.target.checked ? "已启用自动滚动" : "已关闭自动滚动");
        }} />自动滚动</label>
        <button className="button" disabled title="报文导出尚未实现，当前也无实际报文">导出报文</button>
      </div>
      <details className="update-menu"><summary>软件更新 <span className="version">{info ? `v${info.version}` : "版本待读取"}</span></summary><UpdatePanel /></details>
    </header>

    <div className="capability-note">协议通信尚未实现；连接与命令操作不可用。参数校验仅检查格式，不代表设备可达或通信成功。</div>
    {appError && <div className="notice error" role="alert">应用信息读取失败：{appError}</div>}

    <main className={`workbench ${statusOpen ? "" : "status-collapsed"} ${settingsOpen ? "" : "settings-collapsed"}`}>
      <aside className={`side-panel status-panel ${statusOpen ? "" : "collapsed"}`} aria-label="运行状态侧栏">
        <div className="panel-heading"><h2 id="status-heading">运行状态</h2><button className="collapse-button" aria-label={statusOpen ? "折叠运行状态" : "展开运行状态"} aria-expanded={statusOpen} aria-controls="status-content" onClick={() => {
          setStatusOpen(!statusOpen); console.info(statusOpen ? "已折叠运行状态" : "已展开运行状态");
        }}><span aria-hidden="true">‹</span></button></div>
        <div className="side-content" id="status-content" inert={!statusOpen}>
          <div className="session-state"><i aria-hidden="true" />未连接</div>
          <dl className="session-details">
            {[["TCP 连接", "未建立"], ["数据传输", "未启动"], ["总召状态", "未执行"], ["最近接收", "无数据"]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd>{value}</dd></div>)}
          </dl>
          <h3 className="minor-heading">收发统计</h3>
          <dl className="session-details">
            {[["接收帧", "0"], ["发送帧", "0"], ["接收字节", "0 B"], ["发送字节", "0 B"], ["点值数量", "0"]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd className="numeric">{value}</dd></div>)}
          </dl>
          <p className="panel-note">统计仅来自真实会话。当前无协议连接、无点值数据。</p>
        </div>
      </aside>

      <div className="monitor" ref={monitorRef} style={{ "--frame-share": `${split}fr`, "--lower-share": `${100 - split}fr` } as CSSProperties}>
        <section className="panel frame-panel" id="frame-monitor" aria-labelledby="frame-heading">
          <div className="panel-heading"><h2 id="frame-heading">报文监视</h2><span>{paused ? "显示已暂停" : "实时显示"} · 0 条</span></div>
          <EmptyTable label="收发报文" columns={["时间", "方向", "帧类型", "长度", "摘要"]} message="尚无收发报文，当前未建立协议连接。" />
          <div className="display-status" role="status">{displayMessage}{paused ? " · 仅暂停显示" : ""}</div>
        </section>
        <div className="splitter" role="separator" tabIndex={0} aria-label="调整报文与详情点表区域高度" aria-orientation="horizontal" aria-valuemin={25} aria-valuemax={65} aria-valuenow={split} aria-valuetext={`报文区域 ${split}%`} aria-controls="frame-monitor lower-monitor" onKeyDown={splitterKey}
          onPointerDown={(event) => { if (event.button !== 0) return; dragPointer.current = event.pointerId; event.currentTarget.setPointerCapture(event.pointerId); event.preventDefault(); }}
          onPointerMove={moveSplitter}
          onPointerUp={(event) => { if (dragPointer.current === event.pointerId) { dragPointer.current = null; event.currentTarget.releasePointerCapture(event.pointerId); console.info("已调整报文分区比例"); } }}
          onPointerCancel={() => { dragPointer.current = null; }} onLostPointerCapture={() => { dragPointer.current = null; }} title="拖动调整；也可聚焦后使用上下方向键、Home、End"><span /></div>
        <div className="lower-monitor" id="lower-monitor">
          <section className="panel detail-panel" aria-labelledby="detail-heading">
            <div className="panel-heading"><h2 id="detail-heading">报文详情</h2><div className="segmented" role="group" aria-label="详情显示方式">
              <button aria-pressed={detailMode === "raw"} onClick={() => { setDetailMode("raw"); console.info("切换至原始报文详情"); }}>原始报文</button>
              <button aria-pressed={detailMode === "parsed"} onClick={() => { setDetailMode("parsed"); console.info("切换至报文解析详情"); }}>解析详情</button>
            </div></div>
            <div className={`detail-content ${detailMode === "raw" ? "hex-content" : ""}`}>{detailMode === "raw" ? "尚无选中报文，无原始字节可显示。" : "尚无选中报文，无协议解析事件可显示。"}</div>
          </section>
          <section className="panel point-panel" aria-label="分类点表">
            <div className="point-tabs" role="tablist" aria-label="点表分类">
              {pointTypes.map((point, index) => <button key={point.id} role="tab" id={`tab-${point.id}`} aria-controls={`points-${point.id}`} aria-selected={activePoint === point.id} tabIndex={activePoint === point.id ? 0 : -1} onClick={() => selectPoint(point.id)} onKeyDown={(event) => pointTabKey(event, index)}>{point.label}</button>)}
              <span className="point-count">0 点</span>
            </div>
            {pointTypes.map((point) => <div key={point.id} id={`points-${point.id}`} role="tabpanel" aria-labelledby={`tab-${point.id}`} hidden={activePoint !== point.id} tabIndex={0} className="point-tabpanel">
              {activePoint === point.id && <>
                {(point.id === "controls" || point.id === "setpoints") && <div className="command-area">
                  <fieldset disabled title={protocolUnavailable}><legend>{point.label}操作 · 协议能力待实现</legend><div className="command-fields">
                    <label>信息地址<input type="number" placeholder="IOA" /></label>
                    <label>命令值<input placeholder={point.id === "controls" ? "开 / 合" : "设定值"} /></label>
                    <button className="button" disabled title={protocolUnavailable}>发送{point.label}</button>
                  </div></fieldset>
                </div>}
                <EmptyTable label={`${point.label}点表`} columns={point.columns} message={`尚无${point.label}数据，等待设备实际上送或命令事务。`} />
              </>}
            </div>)}
          </section>
        </div>
      </div>

      <aside className={`side-panel settings-panel ${settingsOpen ? "" : "collapsed"}`} aria-label="通信设置侧栏">
        <div className="panel-heading"><h2>通信设置</h2><button className="collapse-button" aria-label={settingsOpen ? "折叠通信设置" : "展开通信设置"} aria-expanded={settingsOpen} aria-controls="settings-content" onClick={() => {
          setSettingsOpen(!settingsOpen); console.info(settingsOpen ? "已折叠通信设置" : "已展开通信设置");
        }}><span aria-hidden="true">›</span></button></div>
        <div className="side-content" id="settings-content" inert={!settingsOpen}>
          <form onSubmit={validate} aria-busy={validating}>
            <fieldset disabled={validating}>
              <label>目标主机<input value={config.host} onChange={(event) => updateConfig("host", event.target.value)} required autoComplete="off" spellCheck={false} /></label>
              <label>TCP 端口<input type="number" min="1" max="65535" step="1" value={config.port} onChange={(event) => updateConfig("port", event.target.value)} required /></label>
              <label>公共地址（CA）<input type="number" min="1" max="65534" step="1" value={config.commonAddress} onChange={(event) => updateConfig("commonAddress", event.target.value)} required /></label>
            </fieldset>
            <button type="submit" className="button secondary" disabled={validating}>{validating ? "正在校验…" : "校验参数"}</button>
            {validation && <p className={`validation ${validation.ok ? "valid" : "invalid"}`} role="status">{validation.message}</p>}
          </form>
          <p className="panel-note">TCP 客户端 · 主站<br />参数仅保留在本次窗口，不自动建立连接。</p>
          <div className="protocol-settings"><h3 className="minor-heading">协议参数</h3><dl className="session-details"><div><dt>协议</dt><dd>IEC 60870-5-104</dd></div><div><dt>命令发送</dt><dd>尚未实现</dd></div></dl></div>
        </div>
      </aside>
    </main>
    <footer className="statusbar"><span>会话：未连接 · {config.host}:{config.port} · CA {config.commonAddress}</span><span>{selectedPoint.label} · {paused ? "显示已暂停" : "显示正常"} · 自动滚动{autoScroll ? "开启" : "关闭"}</span></footer>
  </div>;
}
