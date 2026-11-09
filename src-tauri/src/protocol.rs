//! 桌面工作台与协议会话之间的命令和快照。

use iec104_core::session::{
    ConnectionState, Direction, Event, Session, SessionOptions, TransferState,
};
use iec104_core::wire::{
    parse_asdu, short_float_setpoint, Asdu, Decoder, Frame, InformationObject, PointValue,
    UFunction, SHORT_FLOAT_SETPOINT_TYPE,
};
use iec104_core::ConnectionConfig;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FRAME_LIMIT: usize = 500;
const POINT_LIMIT: usize = 2_000;
const EVENT_LIMIT: usize = 500;
const INTERROGATION_TIMEOUT: Duration = Duration::from_secs(15);
const SETPOINT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum LinkState {
    Disconnected,
    Connecting,
    Starting,
    Started,
    Disconnecting,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FrameRow {
    id: u64,
    time: String,
    direction: &'static str,
    kind: &'static str,
    length: usize,
    raw: String,
    summary: String,
    parsed: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PointRow {
    id: String,
    time: String,
    received_frame_id: u64,
    common_address: u16,
    address: u32,
    type_id: u8,
    value: String,
    quality: String,
    timestamp: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum SetpointStatus {
    Queued,
    Sent,
    Confirmed,
    Terminated,
    Rejected,
    Failed,
    Timeout,
}

impl SetpointStatus {
    fn pending(self) -> bool {
        matches!(self, Self::Queued | Self::Sent | Self::Confirmed)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetpointResult {
    operation_id: u64,
    common_address: u16,
    address: u32,
    value: f32,
    status: SetpointStatus,
    message: String,
    sent_at: Option<String>,
    sent_frame_id: Option<u64>,
    sent_raw: Option<String>,
    confirmed_at: Option<String>,
    confirmed_frame_id: Option<u64>,
    confirmed_raw: Option<String>,
    terminated_at: Option<String>,
    terminated_frame_id: Option<u64>,
    terminated_raw: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProtocolSnapshot {
    state: LinkState,
    error: Option<String>,
    interrogation: String,
    interrogation_pending: bool,
    setpoint: Option<SetpointResult>,
    rx_frames: u64,
    tx_frames: u64,
    rx_bytes: u64,
    tx_bytes: u64,
    last_receive: Option<String>,
    frames: VecDeque<FrameRow>,
    points: VecDeque<PointRow>,
    events: VecDeque<PointRow>,
}

impl Default for ProtocolSnapshot {
    fn default() -> Self {
        Self {
            state: LinkState::Disconnected,
            error: None,
            interrogation: "未执行".to_owned(),
            interrogation_pending: false,
            setpoint: None,
            rx_frames: 0,
            tx_frames: 0,
            rx_bytes: 0,
            tx_bytes: 0,
            last_receive: None,
            frames: VecDeque::new(),
            points: VecDeque::new(),
            events: VecDeque::new(),
        }
    }
}

impl ProtocolSnapshot {
    fn clear(&mut self) {
        self.rx_frames = 0;
        self.tx_frames = 0;
        self.rx_bytes = 0;
        self.tx_bytes = 0;
        self.last_receive = None;
        self.frames.clear();
        self.points.clear();
        self.events.clear();
    }
}

#[derive(Default)]
pub(crate) struct ProtocolState {
    runtime: Arc<Mutex<Runtime>>,
}

enum Action {
    Interrogate,
    Setpoint { address: u32, value: f32 },
    Disconnect,
}

struct Interrogation {
    common_address: u16,
    deadline: Instant,
    confirmed: bool,
}

#[derive(Default)]
struct Runtime {
    snapshot: ProtocolSnapshot,
    worker: Option<mpsc::Sender<Action>>,
    interrogation: Option<Interrogation>,
    common_address: u16,
    setpoint_deadline: Option<Instant>,
    next_setpoint: u64,
    next_frame: u64,
    next_event: u64,
}

impl Runtime {
    fn ensure_setpoint_available(&self) -> Result<(), String> {
        match self.snapshot.setpoint.as_ref().map(|result| result.status) {
            Some(status) if status.pending() => Err("已有遥调正在等待响应".to_owned()),
            Some(SetpointStatus::Timeout | SetpointStatus::Failed) => {
                Err("请重新连接后发送遥调".to_owned())
            }
            _ => Ok(()),
        }
    }

    fn queue_setpoint(&mut self, address: u32, value: f32) {
        self.next_setpoint += 1;
        self.snapshot.setpoint = Some(SetpointResult {
            operation_id: self.next_setpoint,
            common_address: self.common_address,
            address,
            value,
            status: SetpointStatus::Queued,
            message: "遥调已排队".to_owned(),
            sent_at: None,
            sent_frame_id: None,
            sent_raw: None,
            confirmed_at: None,
            confirmed_frame_id: None,
            confirmed_raw: None,
            terminated_at: None,
            terminated_frame_id: None,
            terminated_raw: None,
        });
    }

    fn setpoint_status(&mut self, status: SetpointStatus, message: &str) {
        if let Some(result) = self.snapshot.setpoint.as_mut() {
            result.status = status;
            result.message = message.to_owned();
        }
        if !matches!(status, SetpointStatus::Sent | SetpointStatus::Confirmed) {
            self.setpoint_deadline = None;
        }
    }

    fn expire_setpoint(&mut self, now: Instant) {
        if self
            .setpoint_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            let message = if self
                .snapshot
                .setpoint
                .as_ref()
                .is_some_and(|result| result.status == SetpointStatus::Confirmed)
            {
                "遥调等待终止超时，请重新连接"
            } else {
                "遥调等待确认超时，请重新连接"
            };
            self.setpoint_status(SetpointStatus::Timeout, message);
        }
    }

    fn record_setpoint_sent(&mut self, frame: &FrameRow, raw: &[u8], now: Instant) {
        let Some(result) = self.snapshot.setpoint.as_mut() else {
            return;
        };
        if result.status != SetpointStatus::Queued {
            return;
        }
        let Ok(asdu) = short_float_setpoint(result.common_address, result.address, result.value)
        else {
            return;
        };
        if raw.get(6..) != Some(asdu.as_slice()) {
            return;
        }
        result.sent_at = Some(frame.time.clone());
        result.sent_frame_id = Some(frame.id);
        result.sent_raw = Some(frame.raw.clone());
        self.setpoint_deadline = Some(now + SETPOINT_TIMEOUT);
        self.setpoint_status(SetpointStatus::Sent, "遥调已发送，等待确认");
    }

    fn update_setpoint(&mut self, asdu: &Asdu, now: Instant) {
        self.expire_setpoint(now);
        let Some(result) = self.snapshot.setpoint.as_mut() else {
            return;
        };
        if asdu.type_id != SHORT_FLOAT_SETPOINT_TYPE
            || asdu.test
            || asdu.originator != 0
            || asdu.common_address != result.common_address
            || !matches!(asdu.objects.as_slice(), [object]
                if object.address == result.address && object.quality == 0
                    && matches!(&object.value, PointValue::Float(value)
                        if value.to_bits() == result.value.to_bits()))
        {
            return;
        }
        let (status, message) = match (result.status, asdu.cause, asdu.negative) {
            (SetpointStatus::Sent, 7, false) => (SetpointStatus::Confirmed, "遥调已确认（COT 7）"),
            (SetpointStatus::Confirmed, 10, false) => {
                (SetpointStatus::Terminated, "遥调已终止（COT 10）")
            }
            (SetpointStatus::Sent, 7, true) | (SetpointStatus::Confirmed, 10, true) => {
                (SetpointStatus::Rejected, "遥调被对端否定")
            }
            (SetpointStatus::Sent | SetpointStatus::Confirmed, 44..=47, _) => {
                (SetpointStatus::Rejected, "遥调被对端拒绝")
            }
            _ => return,
        };
        // 调用点已核对这是紧邻本次解析事件的接收 I 帧。
        let Some(frame) = self.snapshot.frames.back() else {
            return;
        };
        if result.status == SetpointStatus::Sent {
            result.confirmed_at = Some(frame.time.clone());
            result.confirmed_frame_id = Some(frame.id);
            result.confirmed_raw = Some(frame.raw.clone());
        } else {
            result.terminated_at = Some(frame.time.clone());
            result.terminated_frame_id = Some(frame.id);
            result.terminated_raw = Some(frame.raw.clone());
        }
        self.setpoint_status(status, message);
        if status == SetpointStatus::Confirmed {
            self.setpoint_deadline = Some(now + SETPOINT_TIMEOUT);
        }
    }

    fn begin_interrogation(&mut self, common_address: u16, now: Instant) {
        self.interrogation = Some(Interrogation {
            common_address,
            deadline: now + INTERROGATION_TIMEOUT,
            confirmed: false,
        });
        self.snapshot.interrogation = "总召已发送".to_owned();
        self.snapshot.interrogation_pending = true;
    }

    fn end_interrogation(&mut self, status: &str) {
        self.interrogation = None;
        self.snapshot.interrogation = status.to_owned();
        self.snapshot.interrogation_pending = false;
    }

    fn expire_interrogation(&mut self, now: Instant) {
        if self
            .interrogation
            .as_ref()
            .is_some_and(|request| now >= request.deadline)
        {
            self.end_interrogation("总召等待超时");
        }
    }

    fn update_interrogation(&mut self, asdu: &Asdu, now: Instant) {
        self.expire_interrogation(now);
        let Some(request) = self.interrogation.as_mut() else {
            return;
        };
        if asdu.type_id != 100
            || asdu.test
            || asdu.common_address != request.common_address
            || !matches!(asdu.objects.as_slice(), [object] if object.value == PointValue::Interrogation(20))
        {
            return;
        }
        match asdu.cause {
            7 if asdu.negative => self.end_interrogation("总召被对端否定"),
            7 if !request.confirmed => {
                request.confirmed = true;
                request.deadline = now + INTERROGATION_TIMEOUT;
                self.snapshot.interrogation = "总召已确认，等待终止".to_owned();
            }
            10 if request.confirmed && asdu.negative => {
                self.end_interrogation("总召被对端否定");
            }
            10 if request.confirmed => self.end_interrogation("总召已终止"),
            44..=47 => self.end_interrogation("总召被对端拒绝"),
            _ => {}
        }
    }

    fn ingest(&mut self, events: Vec<Event>, now: Instant) {
        let time = timestamp();
        let mut receiving_i = false;
        for event in events {
            match event {
                Event::Frame { direction, raw } => {
                    let row = self.frame_row(direction, &raw, &time);
                    receiving_i = direction == Direction::Receive && row.kind == "I";
                    if direction == Direction::Send && row.kind == "I" {
                        self.record_setpoint_sent(&row, &raw, now);
                    }
                    push_bounded(&mut self.snapshot.frames, row, FRAME_LIMIT);
                }
                Event::SendFailure { raw, error } => {
                    receiving_i = false;
                    let mut row = self.new_frame("sendFailed", &raw, &time);
                    log::debug!("IEC104 发送失败，尝试发送的报文：{}", row.raw);
                    row.summary = "发送失败".to_owned();
                    row.parsed = error;
                    push_bounded(&mut self.snapshot.frames, row, FRAME_LIMIT);
                }
                Event::Asdu(asdu) if receiving_i => {
                    if let Some(row) = self.snapshot.frames.back_mut() {
                        add_asdu_details(row, &asdu);
                    }
                    self.update_interrogation(&asdu, now);
                    self.update_setpoint(&asdu, now);
                    if !asdu.test && !asdu.negative && asdu.type_id != SHORT_FLOAT_SETPOINT_TYPE {
                        self.update_points(&asdu, &time);
                    }
                    receiving_i = false;
                }
                Event::ParseError(error) if receiving_i => {
                    if let Some(row) = self.snapshot.frames.back_mut() {
                        row.parsed.push_str(&format!("\nASDU 解析失败：{error}"));
                    }
                    receiving_i = false;
                }
                Event::Error(error) if receiving_i => {
                    if let Some(row) = self.snapshot.frames.back_mut() {
                        row.parsed.push_str(&format!("\n会话错误：{error}"));
                    }
                    receiving_i = false;
                }
                _ => {}
            }
        }
        self.expire_interrogation(now);
        self.expire_setpoint(now);
    }

    fn new_frame(&mut self, direction: &'static str, raw: &[u8], time: &str) -> FrameRow {
        self.next_frame += 1;
        FrameRow {
            id: self.next_frame,
            time: time.to_owned(),
            direction,
            kind: "异常",
            length: raw.len(),
            raw: hex(raw),
            summary: "无效或未收完整的报文".to_owned(),
            parsed: String::new(),
        }
    }

    fn frame_row(&mut self, direction: Direction, raw: &[u8], time: &str) -> FrameRow {
        let sent = direction == Direction::Send;
        let mut row = self.new_frame(if sent { "send" } else { "receive" }, raw, time);
        let label = if sent { "发送" } else { "接收" };
        log::debug!("IEC104 {label}报文：{}", row.raw);
        if sent {
            self.snapshot.tx_bytes += raw.len() as u64;
        } else {
            self.snapshot.rx_bytes += raw.len() as u64;
            self.snapshot.last_receive = Some(time.to_owned());
        }
        let decoded = Decoder::default().push(raw);
        match decoded {
            Ok(frames) if frames.len() == 1 => {
                if sent {
                    self.snapshot.tx_frames += 1;
                } else {
                    self.snapshot.rx_frames += 1;
                }
                describe_frame(&mut row, &frames[0], sent);
            }
            Err(error) => row.parsed = error,
            _ => {}
        }
        row
    }

    fn update_points(&mut self, asdu: &Asdu, time: &str) {
        for object in &asdu.objects {
            let (kind, value) = point_value(&object.value);
            if kind == 100 {
                continue;
            }
            let id = format!("{}:{}:{kind}", asdu.common_address, object.address);
            let row = PointRow {
                id,
                time: time.to_owned(),
                received_frame_id: self.next_frame,
                common_address: asdu.common_address,
                address: object.address,
                type_id: asdu.type_id,
                value,
                quality: format!("0x{:02X}", object.quality),
                timestamp: object
                    .timestamp
                    .as_ref()
                    .map_or_else(String::new, |raw| hex(raw)),
            };
            if let Some(index) = self.snapshot.points.iter().position(|old| old.id == row.id) {
                self.snapshot.points.remove(index);
            }
            if matches!(kind, 1 | 3) && object.timestamp.is_some() {
                self.next_event += 1;
                let mut event = row.clone();
                event.id = format!("soe:{}", self.next_event);
                push_bounded(&mut self.snapshot.events, event, EVENT_LIMIT);
            }
            push_bounded(&mut self.snapshot.points, row, POINT_LIMIT);
        }
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, limit: usize) {
    if queue.len() == limit {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn hex(raw: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(raw.len() * 3);
    for (index, byte) in raw.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        let _ = write!(text, "{byte:02X}");
    }
    text
}

fn point_value(value: &PointValue) -> (u8, String) {
    match value {
        PointValue::Single(true) => (1, "合".to_owned()),
        PointValue::Single(false) => (1, "分".to_owned()),
        PointValue::Double(value) => (
            3,
            match value {
                1 => "分",
                2 => "合",
                _ => "不确定",
            }
            .to_owned(),
        ),
        PointValue::Normalized(value) => (9, value.to_string()),
        PointValue::Scaled(value) => (11, value.to_string()),
        PointValue::Float(value) => (13, value.to_string()),
        PointValue::Interrogation(value) => (100, value.to_string()),
    }
}

fn describe_frame(row: &mut FrameRow, frame: &Frame, sent: bool) {
    match frame {
        Frame::I { send, recv, asdu } => {
            row.kind = "I";
            row.summary = format!("I 帧 N(S)={send} N(R)={recv}");
            row.parsed = row.summary.clone();
            if sent {
                let Ok(asdu) = parse_asdu(asdu) else {
                    return;
                };
                add_asdu_details(row, &asdu);
            }
        }
        Frame::S { recv } => {
            row.kind = "S";
            row.summary = format!("S 帧 N(R)={recv}");
            row.parsed = row.summary.clone();
        }
        Frame::U(function) => {
            row.kind = "U";
            row.summary = match function {
                UFunction::StartAct => "STARTDT 激活",
                UFunction::StartCon => "STARTDT 确认",
                UFunction::StopAct => "STOPDT 激活",
                UFunction::StopCon => "STOPDT 确认",
                UFunction::TestAct => "TESTFR 激活",
                UFunction::TestCon => "TESTFR 确认",
            }
            .to_owned();
            row.parsed = row.summary.clone();
        }
    }
}

fn add_asdu_details(row: &mut FrameRow, asdu: &Asdu) {
    use std::fmt::Write;
    let _ = write!(
        row.parsed,
        "\n类型={} CA={} COT={} OA={} 测试={} 否定={}",
        asdu.type_id,
        asdu.common_address,
        asdu.cause,
        asdu.originator,
        if asdu.test { "是" } else { "否" },
        if asdu.negative { "是" } else { "否" },
    );
    for InformationObject {
        address,
        value,
        quality,
        timestamp,
    } in &asdu.objects
    {
        let (_, display) = point_value(value);
        let qualifier = if asdu.type_id == SHORT_FLOAT_SETPOINT_TYPE {
            "QOS"
        } else {
            "品质"
        };
        let _ = write!(
            row.parsed,
            "\nIOA={address} 值={display} {qualifier}=0x{quality:02X}"
        );
        if let Some(raw) = timestamp {
            let _ = write!(row.parsed, " 时标={}", hex(raw));
        }
    }
}

fn lock(runtime: &Mutex<Runtime>) -> Result<MutexGuard<'_, Runtime>, String> {
    runtime.lock().map_err(|_| "协议状态锁异常".to_owned())
}

#[tauri::command]
pub(crate) fn protocol_connect(
    config: ConnectionConfig,
    state: tauri::State<'_, ProtocolState>,
) -> Result<(), String> {
    config.validate()?;
    let mut runtime = lock(&state.runtime)?;
    if runtime.worker.is_some() {
        return Err("已有连接操作或活动会话".to_owned());
    }
    let (sender, receiver) = mpsc::channel();
    let common_address = config.common_address;
    let shared = Arc::clone(&state.runtime);
    let _worker = thread::Builder::new()
        .name("iec104-workbench".to_owned())
        .spawn(move || run_worker(&shared, &receiver, config))
        .map_err(|error| format!("启动协议线程失败：{error}"))?;
    runtime.snapshot = ProtocolSnapshot {
        state: LinkState::Connecting,
        ..Default::default()
    };
    runtime.common_address = common_address;
    runtime.setpoint_deadline = None;
    runtime.worker = Some(sender);
    Ok(())
}

#[tauri::command]
pub(crate) fn protocol_disconnect(state: tauri::State<'_, ProtocolState>) -> Result<(), String> {
    let mut runtime = lock(&state.runtime)?;
    if runtime.snapshot.state == LinkState::Disconnecting {
        return Ok(());
    }
    if let Some(worker) = &runtime.worker {
        worker
            .send(Action::Disconnect)
            .map_err(|_| "协议线程已退出".to_owned())?;
        runtime.snapshot.state = LinkState::Disconnecting;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn protocol_interrogate(state: tauri::State<'_, ProtocolState>) -> Result<(), String> {
    let mut runtime = lock(&state.runtime)?;
    if runtime.snapshot.state != LinkState::Started {
        return Err("数据传输启动后才可发送总召".to_owned());
    }
    if runtime.snapshot.interrogation_pending {
        return Err("已有总召正在等待响应".to_owned());
    }
    runtime
        .worker
        .as_ref()
        .ok_or_else(|| "没有活动协议线程".to_owned())?
        .send(Action::Interrogate)
        .map_err(|_| "协议线程已退出".to_owned())?;
    runtime.snapshot.interrogation = "总召已排队".to_owned();
    runtime.snapshot.interrogation_pending = true;
    Ok(())
}

#[tauri::command]
pub(crate) fn protocol_setpoint(
    address: u32,
    value: f32,
    state: tauri::State<'_, ProtocolState>,
) -> Result<(), String> {
    if address > 0xff_ffff || !value.is_finite() {
        return Err("遥调须使用三字节信息地址及有限的短浮点值".to_owned());
    }
    let mut runtime = lock(&state.runtime)?;
    if runtime.snapshot.state != LinkState::Started {
        return Err("数据传输启动后才可发送遥调".to_owned());
    }
    runtime.ensure_setpoint_available()?;
    runtime
        .worker
        .as_ref()
        .ok_or_else(|| "没有活动协议线程".to_owned())?
        .send(Action::Setpoint { address, value })
        .map_err(|_| "协议线程已退出".to_owned())?;
    runtime.queue_setpoint(address, value);
    Ok(())
}

#[tauri::command]
pub(crate) fn protocol_clear(state: tauri::State<'_, ProtocolState>) -> Result<(), String> {
    lock(&state.runtime)?.snapshot.clear();
    Ok(())
}

#[tauri::command]
pub(crate) fn protocol_snapshot(
    state: tauri::State<'_, ProtocolState>,
) -> Result<ProtocolSnapshot, String> {
    Ok(lock(&state.runtime)?.snapshot.clone())
}

fn run_worker(
    shared: &Mutex<Runtime>,
    receiver: &mpsc::Receiver<Action>,
    config: ConnectionConfig,
) {
    let common_address = config.common_address;
    let mut trailing_events = Vec::new();
    let result = (|| {
        if lock(shared)?.snapshot.state == LinkState::Disconnecting {
            return Ok(());
        }
        log::info!("开始建立 IEC104 TCP 连接，公共地址 {common_address}");
        let mut session = Session::connect(config, SessionOptions::default())?;
        let result = drive_session(shared, receiver, &mut session, common_address);
        session.disconnect();
        trailing_events = session.drain_events();
        result
    })();
    match &result {
        Ok(()) => log::info!("IEC104 会话已结束"),
        Err(error) => log::warn!("IEC104 会话结束：{error}"),
    }
    if let Ok(mut runtime) = lock(shared) {
        runtime.ingest(trailing_events, Instant::now());
        if runtime.snapshot.state != LinkState::Disconnecting {
            runtime.snapshot.error = result.err();
        }
        if runtime.snapshot.interrogation_pending {
            runtime.end_interrogation("会话已断开，总召结束");
        }
        if runtime
            .snapshot
            .setpoint
            .as_ref()
            .is_some_and(|result| result.status.pending())
        {
            runtime.setpoint_status(SetpointStatus::Failed, "会话已断开，遥调结果未确认");
        }
        runtime.snapshot.state = LinkState::Disconnected;
        runtime.worker = None;
    }
}

fn drive_session(
    shared: &Mutex<Runtime>,
    receiver: &mpsc::Receiver<Action>,
    session: &mut Session,
    common_address: u16,
) -> Result<(), String> {
    {
        let mut runtime = lock(shared)?;
        if runtime.snapshot.state == LinkState::Disconnecting {
            return Ok(());
        }
        runtime.snapshot.state = LinkState::Starting;
    }
    session.start()?;
    loop {
        match receiver.try_recv() {
            Ok(Action::Disconnect) | Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            Ok(Action::Interrogate) => {
                if lock(shared)?.snapshot.state == LinkState::Disconnecting {
                    return Ok(());
                }
                let result = session.interrogate();
                let now = Instant::now();
                let mut runtime = lock(shared)?;
                runtime.ingest(session.drain_events(), now);
                match result {
                    Ok(()) => runtime.begin_interrogation(common_address, now),
                    Err(error) if session.connection_state() == ConnectionState::Disconnected => {
                        return Err(error);
                    }
                    Err(error) => runtime.end_interrogation(&format!("总召未发送：{error}")),
                }
            }
            Ok(Action::Setpoint { address, value }) => {
                if lock(shared)?.snapshot.state == LinkState::Disconnecting {
                    return Ok(());
                }
                let result = session.setpoint(address, value);
                let now = Instant::now();
                let mut runtime = lock(shared)?;
                runtime.ingest(session.drain_events(), now);
                match result {
                    Ok(()) => {
                        if runtime
                            .snapshot
                            .setpoint
                            .as_ref()
                            .is_none_or(|result| result.status != SetpointStatus::Sent)
                        {
                            let error = "未找到本次遥调发送帧，请重新连接";
                            runtime.setpoint_status(SetpointStatus::Failed, error);
                            return Err(error.to_owned());
                        }
                        log::info!("已发送 IEC104 短浮点遥调：CA={common_address} IOA={address} 值={value}");
                    }
                    Err(error) => {
                        runtime.setpoint_status(
                            SetpointStatus::Failed,
                            &format!("遥调发送失败：{error}"),
                        );
                        if session.connection_state() == ConnectionState::Disconnected {
                            return Err(error);
                        }
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        let events = session.poll()?;
        let now = Instant::now();
        let mut runtime = lock(shared)?;
        runtime.ingest(events, now);
        if runtime.snapshot.state == LinkState::Disconnecting {
            return Ok(());
        }
        match session.transfer_state() {
            TransferState::Started => runtime.snapshot.state = LinkState::Started,
            TransferState::Stopped => return Err("对端停止数据传输".to_owned()),
            TransferState::Starting | TransferState::Stopping => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setpoint_frame(direction: Direction, asdu: Vec<u8>) -> Event {
        Event::Frame {
            direction,
            raw: Frame::I {
                send: 0,
                recv: 0,
                asdu,
            }
            .encode()
            .unwrap(),
        }
    }

    fn setpoint_reply(mut asdu: Vec<u8>, cause: u8) -> Vec<Event> {
        asdu[2] = cause;
        vec![
            setpoint_frame(Direction::Receive, asdu.clone()),
            Event::Asdu(parse_asdu(&asdu).unwrap()),
        ]
    }

    // 验证三帧证据绑定到实际报文，并在清空历史后保留。
    #[test]
    fn setpoint_keeps_independent_frame_evidence() {
        let now = Instant::now();
        let mut runtime = Runtime {
            common_address: 1,
            ..Runtime::default()
        };
        let command = short_float_setpoint(1, 7, 300.0).unwrap();
        runtime.queue_setpoint(7, 300.0);
        runtime.ingest(vec![setpoint_frame(Direction::Send, command.clone())], now);
        runtime.ingest(setpoint_reply(command.clone(), 10), now);
        runtime.ingest(setpoint_reply(command.clone(), 0x87), now);
        runtime.ingest(
            setpoint_reply(short_float_setpoint(1, 8, 300.0).unwrap(), 7),
            now,
        );
        let result = runtime.snapshot.setpoint.as_ref().unwrap();
        assert_eq!(result.status, SetpointStatus::Sent);
        assert!(result.confirmed_frame_id.is_none() && result.terminated_frame_id.is_none());

        runtime.ingest(setpoint_reply(command.clone(), 7), now);
        let result = runtime.snapshot.setpoint.as_ref().unwrap();
        assert_eq!(result.status, SetpointStatus::Confirmed);
        assert!(runtime.ensure_setpoint_available().is_err());
        runtime.ingest(setpoint_reply(command, 10), now);
        let frames = runtime.snapshot.frames.clone();
        runtime.snapshot.clear();
        assert!(runtime.snapshot.frames.is_empty());
        let result = runtime.snapshot.setpoint.as_ref().unwrap();
        assert_eq!(result.status, SetpointStatus::Terminated);
        for (id, time, raw, frame) in [
            (
                result.sent_frame_id,
                &result.sent_at,
                &result.sent_raw,
                &frames[0],
            ),
            (
                result.confirmed_frame_id,
                &result.confirmed_at,
                &result.confirmed_raw,
                &frames[4],
            ),
            (
                result.terminated_frame_id,
                &result.terminated_at,
                &result.terminated_raw,
                &frames[5],
            ),
        ] {
            assert_eq!(id, Some(frame.id));
            assert_eq!(time.as_deref(), Some(frame.time.as_str()));
            assert_eq!(raw.as_deref(), Some(frame.raw.as_str()));
        }
    }

    // 验证肯定确认开启独立终止计时，迟到终止不改变超时结果。
    #[test]
    fn setpoint_termination_timeout_requires_reconnect() {
        let now = Instant::now();
        let mut runtime = Runtime {
            common_address: 1,
            ..Runtime::default()
        };
        let command = short_float_setpoint(1, 7, 300.0).unwrap();
        runtime.queue_setpoint(7, 300.0);
        runtime.ingest(vec![setpoint_frame(Direction::Send, command.clone())], now);
        let confirmed_at = now + SETPOINT_TIMEOUT - Duration::from_secs(1);
        runtime.ingest(setpoint_reply(command.clone(), 7), confirmed_at);
        runtime.expire_setpoint(now + SETPOINT_TIMEOUT);
        assert_eq!(
            runtime.snapshot.setpoint.as_ref().unwrap().status,
            SetpointStatus::Confirmed
        );

        runtime.ingest(setpoint_reply(command, 10), confirmed_at + SETPOINT_TIMEOUT);
        let result = runtime.snapshot.setpoint.as_ref().unwrap();
        assert_eq!(result.status, SetpointStatus::Timeout);
        assert!(result.message.contains("终止"));
        assert!(result.sent_raw.is_some() && result.confirmed_raw.is_some());
        assert!(result.terminated_raw.is_none());
        assert!(runtime.ensure_setpoint_available().is_err());
    }

    // 验证发送结果未知时保留已有证据，并禁止在原连接再次发送。
    #[test]
    fn failed_setpoint_requires_reconnect() {
        let mut runtime = Runtime {
            common_address: 1,
            ..Runtime::default()
        };
        let command = short_float_setpoint(1, 7, 300.0).unwrap();
        runtime.queue_setpoint(7, 300.0);
        runtime.ingest(
            vec![setpoint_frame(Direction::Send, command)],
            Instant::now(),
        );
        runtime.setpoint_status(SetpointStatus::Failed, "会话已断开，遥调结果未确认");
        assert!(runtime.ensure_setpoint_available().is_err());
        assert!(runtime.snapshot.setpoint.unwrap().sent_raw.is_some());
    }

    #[test]
    fn short_float_setpoint_matches_only_the_active_command() {
        let now = Instant::now();
        let mut runtime = Runtime {
            common_address: 1,
            ..Runtime::default()
        };
        runtime.queue_setpoint(7, -12.5);
        runtime.setpoint_status(SetpointStatus::Sent, "遥调已发送");
        runtime.setpoint_deadline = Some(now + SETPOINT_TIMEOUT);
        for (ca, ioa, value, cause, oa, qos) in [
            (2, 7, -12.5, 7, 0, 0),
            (1, 8, -12.5, 7, 0, 0),
            (1, 7, -12.6, 7, 0, 0),
            (1, 7, -12.5, 0x87, 0, 0),
            (1, 7, -12.5, 7, 1, 0),
            (1, 7, -12.5, 7, 0, 0x80),
            (1, 7, -12.5, 10, 0, 0),
        ] {
            let mut bytes = short_float_setpoint(ca, ioa, value).unwrap();
            bytes[3] = oa;
            bytes[13] = qos;
            runtime.ingest(setpoint_reply(bytes, cause), now);
            assert_eq!(
                runtime.snapshot.setpoint.as_ref().unwrap().status,
                SetpointStatus::Sent
            );
        }
        let bytes = short_float_setpoint(1, 7, -12.5).unwrap();
        runtime.ingest(setpoint_reply(bytes.clone(), 7), now);
        assert_eq!(
            runtime.snapshot.setpoint.as_ref().unwrap().status,
            SetpointStatus::Confirmed
        );
        runtime.ingest(setpoint_reply(bytes, 10), now);
        assert_eq!(
            runtime.snapshot.setpoint.as_ref().unwrap().status,
            SetpointStatus::Terminated
        );
    }

    #[test]
    fn short_float_setpoint_rejection_and_timeout_are_not_confirmation() {
        let now = Instant::now();
        let mut runtime = Runtime {
            common_address: 1,
            ..Runtime::default()
        };
        let bytes = short_float_setpoint(1, 7, 8.0).unwrap();
        runtime.queue_setpoint(7, 8.0);
        runtime.setpoint_status(SetpointStatus::Sent, "遥调已发送");
        runtime.setpoint_deadline = Some(now + SETPOINT_TIMEOUT);
        runtime.ingest(setpoint_reply(bytes.clone(), 0x47), now);
        assert_eq!(
            runtime.snapshot.setpoint.as_ref().unwrap().status,
            SetpointStatus::Rejected
        );
        runtime.queue_setpoint(7, 8.0);
        runtime.setpoint_status(SetpointStatus::Sent, "遥调已发送");
        runtime.setpoint_deadline = Some(now + SETPOINT_TIMEOUT);
        runtime.ingest(setpoint_reply(bytes, 7), now + SETPOINT_TIMEOUT);
        let result = runtime.snapshot.setpoint.unwrap();
        assert_eq!(result.operation_id, 2);
        assert_eq!(result.status, SetpointStatus::Timeout);
    }

    fn response(cause: u8, address: u16, qoi: u8) -> Asdu {
        let [low, high] = address.to_le_bytes();
        parse_asdu(&[100, 1, cause, 0, low, high, 0, 0, 0, qoi]).unwrap()
    }

    #[test]
    fn interrogation_waits_for_matching_real_response_and_termination() {
        let now = Instant::now();
        let mut runtime = Runtime::default();
        runtime.begin_interrogation(1, now);
        runtime.update_interrogation(&response(10, 1, 20), now);
        assert_eq!(runtime.snapshot.interrogation, "总召已发送");
        assert!(runtime.snapshot.interrogation_pending);
        for asdu in [
            response(7, 2, 20),
            response(7, 1, 21),
            response(0x87, 1, 20),
        ] {
            runtime.update_interrogation(&asdu, now);
            assert_eq!(runtime.snapshot.interrogation, "总召已发送");
        }
        runtime.update_interrogation(&response(7, 1, 20), now);
        assert_eq!(runtime.snapshot.interrogation, "总召已确认，等待终止");
        assert!(runtime.snapshot.interrogation_pending);
        runtime.update_interrogation(&response(10, 1, 20), now);
        assert_eq!(runtime.snapshot.interrogation, "总召已终止");
        assert!(!runtime.snapshot.interrogation_pending);
    }

    #[test]
    fn interrogation_rejection_and_deadline_end_pending_request() {
        let now = Instant::now();
        let mut runtime = Runtime::default();
        runtime.begin_interrogation(1, now);
        runtime.update_interrogation(&response(0x47, 1, 20), now);
        assert_eq!(runtime.snapshot.interrogation, "总召被对端否定");
        assert!(!runtime.snapshot.interrogation_pending);
        runtime.begin_interrogation(1, now);
        runtime.update_interrogation(&response(10, 1, 20), now + INTERROGATION_TIMEOUT);
        assert_eq!(runtime.snapshot.interrogation, "总召等待超时");
        assert!(!runtime.snapshot.interrogation_pending);
    }

    #[test]
    fn clear_preserves_session_and_interrogation_state() {
        let now = Instant::now();
        let mut runtime = Runtime::default();
        runtime.snapshot.state = LinkState::Started;
        runtime.begin_interrogation(1, now);
        runtime.ingest(
            vec![Event::Frame {
                direction: Direction::Receive,
                raw: vec![0x68, 4, 0x43, 0, 0, 0],
            }],
            now,
        );
        assert_eq!(runtime.snapshot.rx_frames, 1);
        runtime.snapshot.clear();
        assert_eq!(runtime.snapshot.rx_frames, 0);
        assert!(runtime.snapshot.frames.is_empty());
        assert!(runtime.snapshot.interrogation_pending);
        assert_eq!(runtime.snapshot.state, LinkState::Started);
    }

    #[test]
    fn keeps_frame_evidence_without_counting_incomplete_receive_or_failed_send() {
        let mut runtime = Runtime::default();
        runtime.ingest(
            vec![
                Event::Frame {
                    direction: Direction::Receive,
                    raw: vec![0x68, 4, 0x43],
                },
                Event::SendFailure {
                    raw: vec![0x68, 4, 0x83, 0, 0, 0],
                    error: "发送失败".to_owned(),
                },
            ],
            Instant::now(),
        );
        assert_eq!(runtime.snapshot.rx_frames, 0);
        assert_eq!(runtime.snapshot.tx_frames, 0);
        assert_eq!(runtime.snapshot.rx_bytes, 3);
        assert_eq!(runtime.snapshot.tx_bytes, 0);
        assert_eq!(runtime.snapshot.frames[0].raw, "68 04 43");
        assert_eq!(runtime.snapshot.frames[1].direction, "sendFailed");
    }

    #[test]
    fn attaches_asdu_to_receive_row_and_updates_point() {
        let bytes = vec![1, 1, 3, 0, 1, 0, 7, 0, 0, 1];
        let asdu = parse_asdu(&bytes).unwrap();
        let raw = Frame::I {
            send: 0,
            recv: 0,
            asdu: bytes,
        }
        .encode()
        .unwrap();
        let mut runtime = Runtime::default();
        runtime.ingest(
            vec![
                Event::Frame {
                    direction: Direction::Receive,
                    raw,
                },
                Event::Asdu(asdu),
            ],
            Instant::now(),
        );
        assert_eq!(runtime.snapshot.rx_frames, 1);
        assert!(runtime.snapshot.frames[0].parsed.contains("IOA=7"));
        assert_eq!(runtime.snapshot.points[0].value, "合");
    }
}
