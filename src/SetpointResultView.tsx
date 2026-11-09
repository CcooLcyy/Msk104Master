import { displayTime, type SetpointResult } from "./protocol";

export function SetpointResultView({ result, columns }: { result: SetpointResult | null; columns: readonly string[] }) {
  const frames = result ? [
    { kind: "sent", label: "发送（COT 6）", time: result.sentAt, id: result.sentFrameId, raw: result.sentRaw },
    { kind: "confirmed", label: "确认", time: result.confirmedAt, id: result.confirmedFrameId, raw: result.confirmedRaw },
    { kind: "terminated", label: "终止", time: result.terminatedAt, id: result.terminatedFrameId, raw: result.terminatedRaw },
  ].filter((frame) => frame.raw != null) : [];

  return <>
    <div className="table-scroll"><table className="data-table" aria-label="遥调结果">
      <thead><tr>{columns.map((column) => <th key={column} scope="col">{column}</th>)}</tr></thead>
      <tbody><tr id="setpoint-result" data-state={result?.status ?? "idle"} data-operation-id={result?.operationId}
        data-ioa={result?.address} data-common-address={result?.commonAddress} data-value={result?.value}
        data-sent-at={result?.sentAt ?? undefined} data-sent-frame-id={result?.sentFrameId ?? undefined} data-sent-raw={result?.sentRaw ?? undefined}
        data-confirmed-at={result?.confirmedAt ?? undefined} data-confirmed-frame-id={result?.confirmedFrameId ?? undefined} data-confirmed-raw={result?.confirmedRaw ?? undefined}
        data-terminated-at={result?.terminatedAt ?? undefined} data-terminated-frame-id={result?.terminatedFrameId ?? undefined} data-terminated-raw={result?.terminatedRaw ?? undefined}>
        {result ? <><td>{result.commonAddress}</td><td>{result.address}</td><td>C_SE_NC_1</td>
          <td>{result.value}</td><td role="status">{result.message}</td><td>由业务侧核验</td>
          <td>{result.sentAt ? displayTime(result.sentAt) : "—"}</td></>
          : <td colSpan={columns.length} className="empty-cell">尚无遥调结果。</td>}
      </tr></tbody>
    </table></div>
    {frames.length > 0 && <details className="setpoint-evidence" open>
      <summary>遥调报文证据</summary>
      <dl>{frames.map((frame) => <div key={frame.kind}>
        <dt>{frame.label} · 帧 #{frame.id ?? "—"} · {frame.time ? displayTime(frame.time) : "—"}</dt>
        <dd><code>{frame.raw}</code></dd>
      </div>)}</dl>
    </details>}
  </>;
}
