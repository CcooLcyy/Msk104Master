//! 显式驱动的同步 TCP 会话。调用者须持续调用 `poll` 才会推进定时器。
use crate::wire::{general_interrogation, parse_asdu, Asdu, Decoder, Frame, UFunction};
use crate::ConnectionConfig;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

const MASK: u16 = 0x7fff;

/// 协议与套接字超时；所有时长必须非零，且 t2 < t1、1 <= w <= k <= 32767。
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub t1: Duration,
    pub t2: Duration,
    pub t3: Duration,
    pub k: u16,
    pub w: u16,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            t1: Duration::from_secs(15),
            t2: Duration::from_secs(10),
            t3: Duration::from_secs(20),
            k: 12,
            w: 8,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_millis(100),
        }
    }
}

impl SessionOptions {
    pub fn validate(&self) -> Result<(), String> {
        if [
            self.t1,
            self.t2,
            self.t3,
            self.connect_timeout,
            self.read_timeout,
        ]
        .iter()
        .any(Duration::is_zero)
        {
            return Err("所有超时时长必须大于零".to_owned());
        }
        if self.t2 >= self.t1 {
            return Err("t2 必须小于 t1".to_owned());
        }
        if self.w == 0 || self.k == 0 || self.w > self.k || self.k > MASK {
            return Err("窗口必须满足 1 <= w <= k <= 32767".to_owned());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    Stopped,
    Starting,
    Started,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Send,
    Receive,
}

/// 原始报文与解析结果分开记录；失败时通过 `drain_events` 获取未取走的证据。
#[derive(Debug)]
pub enum Event {
    Frame {
        direction: Direction,
        raw: Vec<u8>,
    },
    /// write_all 失败时的完整发送尝试；对端可能只收到前缀，不能视为发送成功。
    SendFailure {
        raw: Vec<u8>,
        error: String,
    },
    Asdu(Asdu),
    ParseError(String),
    Error(String),
}

#[derive(Default)]
struct Sequence {
    send: u16,
    receive: u16,
    acknowledged: u16,
    pending: VecDeque<Instant>,
    unacknowledged_receive: u16,
    receive_since: Option<Instant>,
}

impl Sequence {
    fn acknowledge(&mut self, receive: u16) -> Result<(), String> {
        if receive > MASK {
            return Err("接收确认序号超出 15 位范围".to_owned());
        }
        let count = receive.wrapping_sub(self.acknowledged) & MASK;
        if usize::from(count) > self.pending.len() {
            return Err("对端确认序号超出未确认发送窗口".to_owned());
        }
        for _ in 0..count {
            self.pending.pop_front();
        }
        self.acknowledged = receive;
        Ok(())
    }

    fn received(&mut self, send: u16, now: Instant) -> Result<(), String> {
        if send != self.receive {
            return Err(format!(
                "对端发送序号错误：期待 {}，收到 {send}",
                self.receive
            ));
        }
        self.receive = (self.receive + 1) & MASK;
        self.unacknowledged_receive += 1;
        self.receive_since.get_or_insert(now);
        Ok(())
    }

    fn receive_acknowledged(&mut self) {
        self.unacknowledged_receive = 0;
        self.receive_since = None;
    }
}

/// 无后台线程、无自动重连。`connect` 只建立 TCP；`start` 等待 `poll` 收到确认。
/// 操作参数/状态拒绝不会关闭连接；网络、协议、确认超时故障一律关闭连接。
/// `poll` 失败时事件仍留在队列中；调用 `drain_events` 可获取故障前的完整原始帧。
pub struct Session {
    socket: Option<TcpStream>,
    config: ConnectionConfig,
    options: SessionOptions,
    transfer: TransferState,
    sequence: Sequence,
    decoder: Decoder,
    receiving_raw: Vec<u8>,
    events: VecDeque<Event>,
    control_pending: Option<(UFunction, Instant)>,
    test_pending: Option<Instant>,
    last_activity: Instant,
}

impl Session {
    /// 同步解析 DNS 并尝试地址；connect_timeout 限制每次 TCP 尝试，不限制系统 DNS。
    pub fn connect(config: ConnectionConfig, options: SessionOptions) -> Result<Self, String> {
        config.validate()?;
        options.validate()?;
        let addresses = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|error| format!("主机解析失败：{error}"))?;
        let mut last_error = "没有可用的主机地址".to_owned();
        for address in addresses {
            match TcpStream::connect_timeout(&address, options.connect_timeout) {
                Ok(socket) => {
                    if let Err(error) = socket.set_read_timeout(Some(options.read_timeout)) {
                        let _ = socket.shutdown(Shutdown::Both);
                        return Err(format!("设置接收超时失败：{error}"));
                    }
                    if let Err(error) = socket.set_write_timeout(Some(options.t1)) {
                        let _ = socket.shutdown(Shutdown::Both);
                        return Err(format!("设置发送超时失败：{error}"));
                    }
                    return Ok(Self {
                        socket: Some(socket),
                        config,
                        options,
                        transfer: TransferState::Stopped,
                        sequence: Sequence::default(),
                        decoder: Decoder::default(),
                        receiving_raw: Vec::new(),
                        events: VecDeque::new(),
                        control_pending: None,
                        test_pending: None,
                        last_activity: Instant::now(),
                    });
                }
                Err(error) => last_error = format!("TCP 连接失败：{error}"),
            }
        }
        Err(last_error)
    }

    pub fn connection_state(&self) -> ConnectionState {
        if self.socket.is_some() {
            ConnectionState::Connected
        } else {
            ConnectionState::Disconnected
        }
    }

    pub fn transfer_state(&self) -> TransferState {
        self.transfer
    }

    /// 仅发送 STARTDT 激活；收到匹配确认后传输状态才成为 Started。
    pub fn start(&mut self) -> Result<(), String> {
        self.tick(Instant::now())?;
        if self.transfer != TransferState::Stopped || self.control_pending.is_some() {
            return Err("当前状态不能启动数据传输".to_owned());
        }
        self.send_frame(Frame::U(UFunction::StartAct))?;
        self.transfer = TransferState::Starting;
        self.control_pending = Some((UFunction::StartCon, Instant::now()));
        Ok(())
    }

    /// 停止前确认已收到的数据；STOPDT 确认前不再允许发起总召。
    pub fn stop(&mut self) -> Result<(), String> {
        self.tick(Instant::now())?;
        if self.transfer != TransferState::Started || self.control_pending.is_some() {
            return Err("当前状态不能停止数据传输".to_owned());
        }
        self.send_acknowledgement()?;
        self.send_frame(Frame::U(UFunction::StopAct))?;
        self.transfer = TransferState::Stopping;
        self.control_pending = Some((UFunction::StopCon, Instant::now()));
        Ok(())
    }

    /// 发送 C_IC_NA_1 总召激活；发送成功不等于收到总召确认或终止。
    pub fn interrogate(&mut self) -> Result<(), String> {
        self.tick(Instant::now())?;
        if self.transfer != TransferState::Started {
            return Err("只有 STARTDT 确认后才能发送总召".to_owned());
        }
        if self.sequence.pending.len() >= usize::from(self.options.k) {
            return Err("发送窗口已满，须等待对端确认".to_owned());
        }
        self.send_frame(Frame::I {
            send: self.sequence.send,
            recv: self.sequence.receive,
            asdu: general_interrogation(self.config.common_address),
        })?;
        self.sequence.send = (self.sequence.send + 1) & MASK;
        self.sequence.pending.push_back(Instant::now());
        self.sequence.receive_acknowledged();
        Ok(())
    }

    /// 单次读取最多等待 read_timeout，并缩短到最近协议定时器期限。
    /// 自动回复的 write_all 还可能等待 t1；整个方法不承诺 read_timeout 上界。
    /// 无接收数据时仍可能返回定时器触发的发送事件。
    /// 收到无法解析的 ASDU 产生 ParseError，保留 raw，连接仍可继续。
    pub fn poll(&mut self) -> Result<Vec<Event>, String> {
        self.tick(Instant::now())?;
        let timeout = self.next_wait(Instant::now());
        if let Err(error) = self
            .socket
            .as_ref()
            .unwrap()
            .set_read_timeout(Some(timeout))
        {
            return self.fail(format!("设置接收超时失败：{error}"));
        }
        let mut bytes = [0; 4096];
        match self.socket.as_mut().unwrap().read(&mut bytes) {
            Ok(0) => return self.fail("对端已关闭 TCP 连接".to_owned()),
            Ok(count) => {
                // 逐字节送解码器，完整/畸形原始帧均在失败前保留。
                for byte in &bytes[..count] {
                    self.receiving_raw.push(*byte);
                    match self.decoder.push(&[*byte]) {
                        Ok(frames) => {
                            for frame in frames {
                                self.events.push_back(Event::Frame {
                                    direction: Direction::Receive,
                                    raw: std::mem::take(&mut self.receiving_raw),
                                });
                                self.handle_frame(frame, Instant::now())?;
                            }
                        }
                        Err(error) => {
                            self.events.push_back(Event::Frame {
                                direction: Direction::Receive,
                                raw: std::mem::take(&mut self.receiving_raw),
                            });
                            return self.fail(format!("报文解码失败：{error}"));
                        }
                    }
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return self.fail(format!("TCP 接收失败：{error}")),
        }
        self.tick(Instant::now())?;
        Ok(self.drain_events())
    }

    /// 取走发送、接收、解析及故障事件，包括上次 poll 返回错误时保留的事件。
    pub fn drain_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    /// 立即关闭 TCP，清除传输状态；不自动发送 STOPDT。
    pub fn disconnect(&mut self) {
        if let Some(socket) = self.socket.take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if !self.receiving_raw.is_empty() {
            self.events.push_back(Event::Frame {
                direction: Direction::Receive,
                raw: std::mem::take(&mut self.receiving_raw),
            });
        }
        self.transfer = TransferState::Stopped;
        self.control_pending = None;
        self.test_pending = None;
    }

    fn fail<T>(&mut self, message: String) -> Result<T, String> {
        self.events.push_back(Event::Error(message.clone()));
        self.disconnect();
        Err(message)
    }

    fn send_frame(&mut self, frame: Frame) -> Result<(), String> {
        let raw = match frame.encode() {
            Ok(raw) => raw,
            Err(error) => return self.fail(format!("报文编码失败：{error}")),
        };
        let Some(socket) = self.socket.as_mut() else {
            return Err("TCP 会话已断开".to_owned());
        };
        if let Err(error) = socket.write_all(&raw) {
            let message = format!("TCP 发送失败：{error}");
            self.events.push_back(Event::SendFailure {
                raw,
                error: message.clone(),
            });
            return self.fail(message);
        }
        self.last_activity = Instant::now();
        self.events.push_back(Event::Frame {
            direction: Direction::Send,
            raw,
        });
        Ok(())
    }

    fn send_acknowledgement(&mut self) -> Result<(), String> {
        if self.sequence.unacknowledged_receive != 0 {
            self.send_frame(Frame::S {
                recv: self.sequence.receive,
            })?;
            self.sequence.receive_acknowledged();
        }
        Ok(())
    }

    fn handle_frame(&mut self, frame: Frame, now: Instant) -> Result<(), String> {
        // 不能通过迟到确认挽救已经到期的会话。
        self.check_expired(now)?;
        self.last_activity = now;
        match frame {
            Frame::I { send, recv, asdu } => {
                if !matches!(
                    self.transfer,
                    TransferState::Started | TransferState::Stopping
                ) {
                    return self.fail("数据传输未开启时收到 I 帧".to_owned());
                }
                if let Err(error) = self.sequence.acknowledge(recv) {
                    return self.fail(error);
                }
                if let Err(error) = self.sequence.received(send, now) {
                    return self.fail(error);
                }
                match parse_asdu(&asdu) {
                    Ok(asdu) => self.events.push_back(Event::Asdu(asdu)),
                    Err(error) => self.events.push_back(Event::ParseError(error)),
                }
                if self.sequence.unacknowledged_receive >= self.options.w {
                    self.send_acknowledgement()?;
                }
            }
            Frame::S { recv } => {
                if let Err(error) = self.sequence.acknowledge(recv) {
                    return self.fail(error);
                }
            }
            Frame::U(function) => match function {
                UFunction::StartCon | UFunction::StopCon => {
                    if !self
                        .control_pending
                        .as_ref()
                        .is_some_and(|(expected, _)| *expected == function)
                    {
                        return self.fail("收到未请求或不匹配的 U 帧确认".to_owned());
                    }
                    if function == UFunction::StopCon {
                        self.send_acknowledgement()?;
                    }
                    self.control_pending = None;
                    self.transfer = if function == UFunction::StartCon {
                        TransferState::Started
                    } else {
                        TransferState::Stopped
                    };
                }
                UFunction::TestCon => {
                    if self.test_pending.take().is_none() {
                        return self.fail("收到未请求的 TESTFR 确认".to_owned());
                    }
                }
                UFunction::TestAct => self.send_frame(Frame::U(UFunction::TestCon))?,
                UFunction::StopAct => {
                    if self.transfer == TransferState::Starting {
                        return self.fail("等待 STARTDT 确认时收到 STOPDT 激活".to_owned());
                    }
                    self.send_acknowledgement()?;
                    self.send_frame(Frame::U(UFunction::StopCon))?;
                    self.transfer = TransferState::Stopped;
                    // 交叉 STOPDT 仍须等待本地主动请求的确认。
                }
                UFunction::StartAct => {
                    return self.fail("主站不接受对端 STARTDT 激活".to_owned());
                }
            },
        }
        Ok(())
    }

    fn check_expired(&mut self, now: Instant) -> Result<(), String> {
        let expired = self
            .control_pending
            .as_ref()
            .map(|(_, time)| *time)
            .into_iter()
            .chain(self.test_pending)
            .chain(self.sequence.pending.front().copied())
            .any(|time| now.saturating_duration_since(time) >= self.options.t1);
        if expired {
            return self.fail("t1 确认等待超时，已关闭会话".to_owned());
        }
        Ok(())
    }

    fn tick(&mut self, now: Instant) -> Result<(), String> {
        if self.socket.is_none() {
            return Err("TCP 会话已断开".to_owned());
        }
        self.check_expired(now)?;
        if self
            .sequence
            .receive_since
            .is_some_and(|time| now.saturating_duration_since(time) >= self.options.t2)
        {
            self.send_acknowledgement()?;
        }
        if self.test_pending.is_none()
            && now.saturating_duration_since(self.last_activity) >= self.options.t3
        {
            self.send_frame(Frame::U(UFunction::TestAct))?;
            self.test_pending = Some(now);
        }
        Ok(())
    }

    fn next_wait(&self, now: Instant) -> Duration {
        let mut wait = self.options.read_timeout;
        let mut limit = |since: Instant, timeout: Duration| {
            wait = wait.min(timeout.saturating_sub(now.saturating_duration_since(since)));
        };
        if let Some((_, since)) = self.control_pending {
            limit(since, self.options.t1);
        }
        if let Some(since) = self.test_pending {
            limit(since, self.options.t1);
        }
        if let Some(since) = self.sequence.pending.front() {
            limit(*since, self.options.t1);
        }
        if let Some(since) = self.sequence.receive_since {
            limit(since, self.options.t2);
        }
        if self.test_pending.is_none() {
            limit(self.last_activity, self.options.t3);
        }
        wait.max(Duration::from_millis(1))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn connected() -> (Session, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let config = ConnectionConfig {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let session = Session::connect(config, SessionOptions::default()).unwrap();
        let (peer, _) = listener.accept().unwrap();
        (session, peer)
    }

    #[test]
    fn rejects_invalid_options() {
        let mut options = SessionOptions {
            k: 32768,
            ..Default::default()
        };
        assert!(options.validate().is_err());
        options.k = 12;
        options.w = 0;
        assert!(options.validate().is_err());
        options.w = 13;
        assert!(options.validate().is_err());
        options.w = 8;
        options.t2 = options.t1;
        assert!(options.validate().is_err());
        options.t2 = Duration::ZERO;
        assert!(options.validate().is_err());
    }

    #[test]
    fn sequence_wraps_and_rejects_future_or_old_acknowledgement() {
        let now = Instant::now();
        let mut sequence = Sequence {
            send: 1,
            receive: MASK,
            acknowledged: MASK,
            pending: VecDeque::from([now, now]),
            ..Default::default()
        };
        sequence.received(MASK, now).unwrap();
        assert_eq!(sequence.receive, 0);
        assert!(sequence.received(MASK, now).is_err());
        assert!(sequence.acknowledge(2).is_err());
        sequence.acknowledge(0).unwrap();
        assert_eq!(sequence.pending.len(), 1);
        assert!(sequence.acknowledge(MASK).is_err());
        sequence.acknowledge(1).unwrap();
        assert!(sequence.pending.is_empty());
        sequence.acknowledge(1).unwrap();
    }

    #[test]
    fn oldest_unconfirmed_i_frame_controls_t1() {
        let (mut session, _peer) = connected();
        let now = Instant::now();
        session.sequence.pending = VecDeque::from([now, now + Duration::from_secs(5)]);
        session.last_activity = now + Duration::from_secs(14);
        assert!(session.check_expired(now + session.options.t1).is_err());
        assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn acknowledged_oldest_frame_moves_deadline_to_next_frame() {
        let (mut session, _peer) = connected();
        let now = Instant::now();
        session.sequence.pending = VecDeque::from([now, now + Duration::from_secs(5)]);
        session.sequence.acknowledge(1).unwrap();
        assert!(session.check_expired(now + Duration::from_secs(15)).is_ok());
        assert!(session
            .check_expired(now + Duration::from_secs(20))
            .is_err());
    }

    #[test]
    fn u_confirmation_timeout_closes_and_resets_transfer() {
        let (mut session, _peer) = connected();
        session.start().unwrap();
        let since = session.control_pending.unwrap().1;
        assert!(session.tick(since + session.options.t1).is_err());
        assert_eq!(session.transfer_state(), TransferState::Stopped);
        assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn idle_test_remains_pending_despite_other_activity() {
        let (mut session, _peer) = connected();
        let now = Instant::now();
        session.last_activity = now;
        session.tick(now + session.options.t3).unwrap();
        let since = session.test_pending.unwrap();
        session
            .handle_frame(Frame::U(UFunction::TestAct), since + Duration::from_secs(1))
            .unwrap();
        assert!(session.tick(since + session.options.t1).is_err());
        assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn delayed_receive_acknowledgement_uses_first_receive_time() {
        let (mut session, _peer) = connected();
        let now = Instant::now();
        session.sequence.received(0, now).unwrap();
        session
            .sequence
            .received(1, now + Duration::from_secs(2))
            .unwrap();
        session.tick(now + session.options.t2).unwrap();
        assert_eq!(session.sequence.unacknowledged_receive, 0);
        assert!(session.drain_events().iter().any(|event| matches!(event,
            Event::Frame { direction: Direction::Send, raw } if raw == &[0x68, 4, 1, 0, 4, 0]
        )));
    }

    #[test]
    fn full_send_window_refuses_without_closing_connection() {
        let (mut session, _peer) = connected();
        session.transfer = TransferState::Started;
        session.options.k = 1;
        session.interrogate().unwrap();
        assert!(session.interrogate().is_err());
        assert_eq!(session.connection_state(), ConnectionState::Connected);
        session.sequence.acknowledge(1).unwrap();
        session.interrogate().unwrap();
    }

    #[test]
    fn invalid_sequences_close_session() {
        for frame in [
            Frame::S { recv: 1 },
            Frame::I {
                send: 1,
                recv: 0,
                asdu: vec![],
            },
        ] {
            let (mut session, _peer) = connected();
            session.transfer = TransferState::Started;
            assert!(session.handle_frame(frame, Instant::now()).is_err());
            assert_eq!(session.connection_state(), ConnectionState::Disconnected);
        }
    }

    #[test]
    fn unknown_asdu_preserves_error_without_point_event() {
        let (mut session, _peer) = connected();
        session.transfer = TransferState::Started;
        session
            .handle_frame(
                Frame::I {
                    send: 0,
                    recv: 0,
                    asdu: vec![255, 1, 3, 0, 1, 0],
                },
                Instant::now(),
            )
            .unwrap();
        let events = session.drain_events();
        assert!(events
            .iter()
            .any(|event| matches!(event, Event::ParseError(_))));
        assert!(!events.iter().any(|event| matches!(event, Event::Asdu(_))));
        assert_eq!(session.connection_state(), ConnectionState::Connected);
    }

    #[test]
    fn matching_test_confirmation_clears_pending_deadline() {
        let (mut session, _peer) = connected();
        let now = Instant::now();
        session.last_activity = now;
        session.tick(now + session.options.t3).unwrap();
        let since = session.test_pending.unwrap();
        session
            .handle_frame(Frame::U(UFunction::TestCon), since + Duration::from_secs(1))
            .unwrap();
        assert!(session.test_pending.is_none());
        assert!(session.check_expired(since + session.options.t1).is_ok());
    }

    #[test]
    fn outgoing_i_piggybacks_receive_acknowledgement_and_clears_t2() {
        let (mut session, _peer) = connected();
        session.transfer = TransferState::Started;
        session.sequence.received(0, Instant::now()).unwrap();
        session.interrogate().unwrap();
        assert_eq!(session.sequence.unacknowledged_receive, 0);
        assert!(session.sequence.receive_since.is_none());
        let raw = session
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                Event::Frame {
                    direction: Direction::Send,
                    raw,
                } => Some(raw),
                _ => None,
            })
            .unwrap();
        assert_eq!(&raw[2..6], &[0, 0, 2, 0]);
    }

    #[test]
    fn failed_send_keeps_attempt_without_successful_send_event() {
        let (mut session, _peer) = connected();
        session
            .socket
            .as_ref()
            .unwrap()
            .shutdown(Shutdown::Write)
            .unwrap();
        assert!(session.start().is_err());
        let events = session.drain_events();
        assert!(events.iter().any(|event| matches!(event,
            Event::SendFailure { raw, .. } if raw == &[0x68, 4, 7, 0, 0, 0]
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::Frame {
                direction: Direction::Send,
                ..
            }
        )));
        assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    }
}
