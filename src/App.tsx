import UpdatePanel from "./UpdatePanel";
import { useEffect, useRef, useState, type CSSProperties, type FormEvent, type KeyboardEvent, type PointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { directionLabel, displayTime, errorMessage, isSetpointSendLocked, stateLabels, useProtocol, type PointRow } from "./protocol";
import { SetpointResultView } from "./SetpointResultView";

interface AppInfo { name: string; version: string; protocol_ready: boolean; test_session: boolean }

const pointTypes = [
  { id: "signals", label: "遥信", columns: ["公共地址", "信息地址", "类型", "状态", "品质", "协议时标", "接收时间"] },
  { id: "measurements", label: "遥测", columns: ["公共地址", "信息地址", "类型", "数值", "品质", "协议时标", "接收时间"] },
  { id: "controls", label: "遥控", columns: ["公共地址", "信息地址", "命令类型", "命令值", "协议确认", "业务反馈", "时间"] },
  { id: "setpoints", label: "遥调", columns: ["公共地址", "信息地址", "命令类型", "设定值", "协议确认", "业务反馈", "时间"] },
  { id: "energy", label: "电量", columns: ["公共地址", "信息地址", "累计值", "序号", "品质", "协议时标", "接收时间"] },
  { id: "events", label: "SOE", columns: ["公共地址", "信息地址", "事件状态", "品质", "协议时标", "接收时间"] },
] as const;
type PointType = (typeof pointTypes)[number]["id"];
const signalTypes = new Set([1, 3, 30, 31]);
const measurementTypes = new Set([9, 11, 13, 34, 35, 36]);
const frameColumns = ["时间", "方向", "帧类型", "长度", "摘要"];

function EmptyTable({ columns, message, label }: { columns: readonly string[]; message: string; label: string }) {
  return <div className="table-scroll"><table aria-label={label}>
    <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
    <tbody><tr><td colSpan={columns.length} className="empty-cell">{message}</td></tr></tbody>
  </table></div>;
}

function PointTable({ rows, columns, label, events }: { rows: PointRow[]; columns: readonly string[]; label: string; events: boolean }) {
  if (!rows.length) return <EmptyTable label={`${label}点表`} columns={columns} message={`尚无${label}数据，等待设备实际上送。`} />;
  return <div className="table-scroll"><table className="data-table" aria-label={`${label}点表`}>
    <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
    <tbody>{rows.map((row) => <tr key={row.id} data-received-at={row.time}
      data-point-type={row.typeId} data-received-frame-id={row.receivedFrameId}>
      <td>{row.commonAddress}</td><td>{row.address}</td>{!events && <td>{row.typeId}</td>}
      <td>{row.value}</td><td>{row.quality}</td><td>{row.timestamp || "—"}</td><td>{displayTime(row.time)}</td>
    </tr>)}</tbody>
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
  const { snapshot, display, paused, togglePause, pending, actionBusy, actionError, readError, displayRevision, run } = useProtocol();
  const [setpointAddress, setSetpointAddress] = useState("");
  const [setpointValue, setSetpointValue] = useState("");
  const [setpointError, setSetpointError] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const [activePoint, setActivePoint] = useState<PointType>("signals");
  const [detailMode, setDetailMode] = useState<"raw" | "parsed">("raw");
  const [selection, setSelection] = useState<{ id: number; revision: number } | null>(null);
  const [split, setSplit] = useState(42);
  const monitorRef = useRef<HTMLDivElement>(null);
  const frameScrollRef = useRef<HTMLDivElement>(null);
  const configFormRef = useRef<HTMLFormElement>(null);
  const dragPointer = useRef<number | null>(null);
  const selectedPoint = pointTypes.find((point) => point.id === activePoint)!;
  const selectedFrame = selection?.revision === displayRevision
    ? display.frames.find((frame) => frame.id === selection.id)
    : undefined;
  const pointRows = activePoint === "events" ? display.events : display.points.filter((point) =>
    (activePoint === "signals" && signalTypes.has(point.typeId))
    || (activePoint === "measurements" && measurementTypes.has(point.typeId)),
  );
  const state = pending === "connect" && snapshot.state === "disconnected" ? "connecting" : snapshot.state;
  const configLocked = state !== "disconnected";
  const setpoint = snapshot.setpoint;
  const setpointDisabled = state !== "started" || !!pending || !!readError || isSetpointSendLocked(setpoint);
  const lastFrameId = display.frames.at(-1)?.id;

  useEffect(() => {
    const list = frameScrollRef.current;
    if (autoScroll && !paused && list) list.scrollTop = list.scrollHeight;
  }, [autoScroll, paused, lastFrameId, displayRevision]);

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
    if (validationBusy.current || actionBusy.current || configLocked) return;
    setConfig((current) => ({ ...current, [key]: value }));
    setValidation(null);
  }

  async function validate(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    // 同步守卫避免重渲染前重复提交，校验期间禁止修改同一份参数。
    if (validationBusy.current || actionBusy.current || configLocked) return;
    validationBusy.current = true;
    setValidation(null);
    setValidating(true);
    try {
      await invoke("validate_connection", { config: connectionConfig() });
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

  function connectionConfig() {
    return { host: config.host.trim(), port: Number(config.port), commonAddress: Number(config.commonAddress) };
  }

  function connect() {
    if (validationBusy.current || actionBusy.current || configLocked) return;
    if (configFormRef.current && !configFormRef.current.checkValidity()) {
      setSettingsOpen(true);
      setValidation({ ok: false, message: "请填写目标主机、有效的 TCP 端口（1～65535）和公共地址（1～65534）。" });
      return;
    }
    setValidation(null);
    void run({ kind: "connect", config: connectionConfig() });
  }

  function sendSetpoint(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (setpointDisabled || actionBusy.current) return;
    const address = Number(setpointAddress);
    const number = Number(setpointValue);
    const value = Math.fround(number);
    if (!setpointAddress.trim() || !Number.isInteger(address) || address < 0 || address > 0xff_ffff
      || !setpointValue.trim() || !Number.isFinite(value) || (number !== 0 && value === 0)) {
      setSetpointError("请填写有效的三字节信息地址和可表示的短浮点值。");
      return;
    }
    setSetpointError("");
    void run({ kind: "setpoint", address, value });
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

  return <div className="workspace" data-test-session={info?.test_session === true ? "true" : undefined}>
    <header className="topbar">
      <div className="brand"><span className="brand-mark" aria-hidden="true">104</span>
        <div><h1>Msk104Master</h1><span>IEC104 模拟主站 · 调试工作台</span></div>
      </div>
      <div className="toolbar" role="group" aria-label="主站操作">
        <button className="button" disabled={configLocked || !!pending || validating} onClick={connect}>连接</button>
        <button className="button" disabled={!configLocked || state === "disconnecting" || !!pending} onClick={() => void run({ kind: "disconnect" })}>断开</button>
        <span className="toolbar-divider" aria-hidden="true" />
        <button className="button" disabled={state !== "started" || snapshot.interrogationPending || !!pending} onClick={() => void run({ kind: "interrogate" })}>总召</button>
        <button className="button" disabled title="对时尚未实现">对时</button>
        <span className="toolbar-divider" aria-hidden="true" />
        <button className="button" disabled={!!pending} onClick={() => void run({ kind: "clear" })}>清空显示</button>
        <button className="button" aria-pressed={paused} title="仅暂停界面显示，不停止协议传输" onClick={togglePause}>{paused ? "恢复显示" : "暂停显示"}</button>
        <label className="check-label"><input type="checkbox" checked={autoScroll} onChange={(event) => {
          setAutoScroll(event.target.checked); console.info(event.target.checked ? "已启用自动滚动" : "已关闭自动滚动");
        }} />自动滚动</label>
        <button className="button" disabled title="报文导出尚未实现">导出报文</button>
      </div>
      <details className="update-menu"><summary>软件更新 <span className="version">{info ? `v${info.version}` : "版本待读取"}</span></summary>
        {info?.test_session ? <p className="update-panel">测试会话已禁用更新。</p> : <UpdatePanel enabled={info?.test_session === false} />}
      </details>
    </header>

    <div className="capability-note">连接后自动启动数据传输，收到 STARTDT 确认后可总召及短浮点遥调。对时、遥控、电量和报文导出尚未实现。</div>
    {appError && <div className="notice error" role="alert">应用信息读取失败：{appError}</div>}
    {actionError && <div className="notice error" role="alert">{actionError}</div>}
    {readError && <div className="notice error" role="alert">{readError}</div>}
    {snapshot.error && <div className="notice error" role="alert">协议会话异常：{snapshot.error}</div>}

    <main className={`workbench ${statusOpen ? "" : "status-collapsed"} ${settingsOpen ? "" : "settings-collapsed"}`}>
      <aside className={`side-panel status-panel ${statusOpen ? "" : "collapsed"}`} aria-label="运行状态侧栏">
        <div className="panel-heading"><h2 id="status-heading">运行状态</h2><button className="collapse-button" aria-label={statusOpen ? "折叠运行状态" : "展开运行状态"} aria-expanded={statusOpen} aria-controls="status-content" onClick={() => {
          setStatusOpen(!statusOpen); console.info(statusOpen ? "已折叠运行状态" : "已展开运行状态");
        }}><span aria-hidden="true">‹</span></button></div>
        <div className="side-content" id="status-content" inert={!statusOpen}>
          <div className="session-state" data-state={state} role="status"><i aria-hidden="true" />{stateLabels[state]}</div>
          <dl className="session-details">
            <div><dt>TCP 连接</dt><dd>{state === "started" || state === "starting" ? "已建立" : state === "disconnecting" ? "正在关闭" : state === "connecting" ? "正在建立" : "未建立"}</dd></div>
            <div><dt>数据传输</dt><dd>{state === "started" ? "已启动" : state === "starting" ? "等待 STARTDT 确认" : state === "disconnecting" ? "正在关闭" : "未启动"}</dd></div>
            <div><dt>总召状态</dt><dd aria-live="polite">{snapshot.interrogation || "未执行"}</dd></div>
            <div><dt>最近接收</dt><dd>{snapshot.lastReceive ? displayTime(snapshot.lastReceive) : "无数据"}</dd></div>
          </dl>
          <h3 className="minor-heading">收发统计</h3>
          <dl className="session-details">
            {[["接收帧", snapshot.rxFrames], ["发送帧", snapshot.txFrames], ["接收字节", `${snapshot.rxBytes} B`], ["发送字节", `${snapshot.txBytes} B`], ["点值数量", snapshot.points.length]].map(([label, value]) => <div key={label}><dt>{label}</dt><dd className="numeric">{value}</dd></div>)}
          </dl>
          <p className="panel-note">统计来自实际会话，暂停显示时继续更新。</p>
        </div>
      </aside>

      <div className="monitor" ref={monitorRef} style={{ "--frame-share": `${split}fr`, "--lower-share": `${100 - split}fr` } as CSSProperties}>
        <section className="panel frame-panel" id="frame-monitor" aria-labelledby="frame-heading">
          <div className="panel-heading"><h2 id="frame-heading">报文监视</h2><span>{paused ? "显示已暂停" : "实时显示"} · {display.frames.length} 条</span></div>
          <div className="table-scroll" ref={frameScrollRef}>
            <table className="data-table frame-table" aria-label="收发报文">
              <thead><tr>{frameColumns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
              <tbody>{display.frames.length ? display.frames.map((frame) => {
                const select = () => setSelection({ id: frame.id, revision: displayRevision });
                const selected = selectedFrame?.id === frame.id;
                return <tr key={frame.id} data-selected={selected} onClick={select}>
                  <td>{displayTime(frame.time)}</td><td>{directionLabel(frame.direction)}</td><td>{frame.kind}</td><td>{frame.length} B</td>
                  <td><button className="frame-selection" aria-pressed={selected} aria-controls="frame-detail">{frame.summary || "查看报文"}</button></td>
                </tr>;
              }) : <tr><td colSpan={frameColumns.length} className="empty-cell">尚无收发报文。</td></tr>}</tbody>
            </table>
          </div>
          <div className="display-status" role="status">{paused ? "显示已暂停，协议会话仍继续运行。" : "选择报文可查看原始字节和解析详情。"}</div>
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
            <div id="frame-detail" className={`detail-content ${detailMode === "raw" ? "hex-content" : ""} ${selectedFrame ? "has-detail" : ""}`} tabIndex={selectedFrame ? 0 : undefined}>
              {selectedFrame ? (detailMode === "raw" ? selectedFrame.raw || "无原始字节。" : selectedFrame.parsed || "无解析结果。") : detailMode === "raw" ? "尚无选中报文，无原始字节可显示。" : "尚无选中报文，无协议解析事件可显示。"}
            </div>
          </section>
          <section className="panel point-panel" aria-label="分类点表">
            <div className="point-tabs" role="tablist" aria-label="点表分类">
              {pointTypes.map((point, index) => <button key={point.id} role="tab" id={`tab-${point.id}`} aria-controls={`points-${point.id}`} aria-selected={activePoint === point.id} tabIndex={activePoint === point.id ? 0 : -1} onClick={() => selectPoint(point.id)} onKeyDown={(event) => pointTabKey(event, index)}>{point.label}</button>)}
              <span className="point-count">{pointRows.length} 点</span>
            </div>
            {pointTypes.map((point) => <div key={point.id} id={`points-${point.id}`} role="tabpanel" aria-labelledby={`tab-${point.id}`} hidden={activePoint !== point.id} tabIndex={0} className="point-tabpanel">
              {activePoint === point.id && <>
                {point.id === "controls" && <div className="command-area">
                  <fieldset disabled title="遥控尚未实现"><legend>遥控操作 · 协议能力待实现</legend><div className="command-fields">
                    <label>信息地址<input type="number" placeholder="IOA" /></label>
                    <label>命令值<input placeholder="开 / 合" /></label>
                    <button className="button" disabled title="遥控尚未实现">发送遥控</button>
                  </div></fieldset>
                </div>}
                {point.id === "setpoints" && <form className="command-area" onSubmit={sendSetpoint}>
                  <fieldset disabled={setpointDisabled}><legend>短浮点遥调 · 直接执行</legend><div className="command-fields">
                    <label>信息地址<input id="setpoint-ioa" type="number" min="0" max="16777215" step="1" value={setpointAddress}
                      onChange={(event) => setSetpointAddress(event.target.value)} required /></label>
                    <label>设定值<input id="setpoint-value" type="number" step="any" value={setpointValue}
                      onChange={(event) => setSetpointValue(event.target.value)} required /></label>
                    <button id="send-setpoint" type="submit" className="button" disabled={setpointDisabled}>发送遥调</button>
                  </div></fieldset>
                  {setpointError && <p className="validation invalid" role="alert">{setpointError}</p>}
                  {(setpoint?.status === "timeout" || setpoint?.status === "failed")
                    && <p className="setpoint-note">重新连接后可发送下一笔遥调。</p>}
                </form>}
                {point.id === "signals" || point.id === "measurements" || point.id === "events"
                  ? <PointTable rows={pointRows} columns={point.columns} label={point.label} events={point.id === "events"} />
                  : point.id === "setpoints"
                  ? <SetpointResultView result={setpoint} columns={point.columns} />
                  : <EmptyTable label={`${point.label}点表`} columns={point.columns} message={`${point.label}尚未实现。`} />}
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
          <form ref={configFormRef} onSubmit={validate} aria-busy={validating}>
            <fieldset disabled={validating || configLocked || !!pending}>
              <label>目标主机<input value={config.host} onChange={(event) => updateConfig("host", event.target.value)} required autoComplete="off" spellCheck={false} /></label>
              <label>TCP 端口<input type="number" min="1" max="65535" step="1" value={config.port} onChange={(event) => updateConfig("port", event.target.value)} required /></label>
              <label>公共地址（CA）<input type="number" min="1" max="65534" step="1" value={config.commonAddress} onChange={(event) => updateConfig("commonAddress", event.target.value)} required /></label>
            </fieldset>
            <button type="submit" className="button secondary" disabled={validating || configLocked || !!pending}>{validating ? "正在校验…" : "校验参数"}</button>
            {validation && <p className={`validation ${validation.ok ? "valid" : "invalid"}`} role="status">{validation.message}</p>}
          </form>
          <p className="panel-note">TCP 客户端 · 主站<br />参数仅保留在本次窗口，不自动建立连接。</p>
          <div className="protocol-settings"><h3 className="minor-heading">协议参数</h3><dl className="session-details"><div><dt>协议</dt><dd>IEC 60870-5-104</dd></div><div><dt>命令发送</dt><dd>短浮点直接执行</dd></div></dl></div>
        </div>
      </aside>
    </main>
    <footer className="statusbar"><span>会话：{stateLabels[state]} · {config.host}:{config.port} · CA {config.commonAddress}</span><span>{selectedPoint.label} · {paused ? "显示已暂停" : "显示正常"} · 自动滚动{autoScroll ? "开启" : "关闭"}</span></footer>
  </div>;
}
