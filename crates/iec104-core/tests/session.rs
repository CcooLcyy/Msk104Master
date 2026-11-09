use iec104_core::session::{
    ConnectionState, Direction, Event, Session, SessionOptions, TransferState,
};
use iec104_core::wire::{Frame, PointValue, UFunction};
use iec104_core::ConnectionConfig;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

fn receive(socket: &mut TcpStream) -> Vec<u8> {
    let mut header = [0; 2];
    socket.read_exact(&mut header).unwrap();
    let mut raw = vec![0; header[1] as usize + 2];
    raw[..2].copy_from_slice(&header);
    socket.read_exact(&mut raw[2..]).unwrap();
    raw
}

fn options() -> SessionOptions {
    SessionOptions {
        read_timeout: Duration::from_secs(3),
        ..Default::default()
    }
}

fn fixture() -> (TcpListener, ConnectionConfig) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = ConnectionConfig {
        port: listener.local_addr().unwrap().port(),
        ..Default::default()
    };
    (listener, config)
}

#[test]
fn start_interrogate_acknowledge_and_stop_over_tcp() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(receive(&mut socket), [0x68, 4, 7, 0, 0, 0]);
        socket.write_all(&[0x68, 4, 11, 0, 0, 0]).unwrap();
        let interrogation = receive(&mut socket);
        assert_eq!(&interrogation[2..6], &[0, 0, 0, 0]);
        assert_eq!(&interrogation[6..], &[100, 1, 6, 0, 1, 0, 0, 0, 0, 20]);
        // 手写单点报文，通过 N(R)=1 确认总召 I 帧并发送 IOA=7 的合位。
        socket
            .write_all(&[0x68, 14, 0, 0, 2, 0, 1, 1, 3, 0, 1, 0, 7, 0, 0, 1])
            .unwrap();
        assert_eq!(receive(&mut socket), [0x68, 4, 1, 0, 2, 0]);
        assert_eq!(receive(&mut socket), [0x68, 4, 19, 0, 0, 0]);
        socket.write_all(&[0x68, 4, 35, 0, 0, 0]).unwrap();
        // 保持连接直到主站主动关闭，避免 EOF 与确认竞态。
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    let options = SessionOptions { w: 1, ..options() };
    let mut session = Session::connect(config, options).unwrap();
    assert_eq!(session.transfer_state(), TransferState::Stopped);
    assert!(session.interrogate().is_err());
    session.start().unwrap();
    while session.transfer_state() != TransferState::Started {
        session.poll().unwrap();
    }
    session.interrogate().unwrap();
    let mut events = Vec::new();
    while !events.iter().any(|event| matches!(event, Event::Asdu(_))) {
        events.extend(session.poll().unwrap());
    }
    let point = events
        .iter()
        .find_map(|event| match event {
            Event::Asdu(asdu) => Some(asdu),
            _ => None,
        })
        .unwrap();
    assert_eq!(point.common_address, 1);
    assert_eq!(point.cause, 3);
    assert_eq!(point.objects.len(), 1);
    assert_eq!(point.objects[0].address, 7);
    assert_eq!(point.objects[0].value, PointValue::Single(true));
    assert!(events.iter().any(|event| matches!(event,
        Event::Frame { direction: Direction::Receive, raw }
            if raw == &[0x68, 14, 0, 0, 2, 0, 1, 1, 3, 0, 1, 0, 7, 0, 0, 1]
    )));
    session.stop().unwrap();
    while session.transfer_state() != TransferState::Stopped {
        session.poll().unwrap();
    }
    session.disconnect();
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    server.join().unwrap();
}

// 验证真实 TCP 会话中的短浮点执行帧及肯定、否定和错误应答证据。
#[test]
fn short_float_setpoint_preserves_replies_over_tcp() {
    const REQUEST: [u8; 20] = [
        0x68, 18, 0, 0, 0, 0, 50, 1, 6, 0, 1, 0, 7, 0, 0, 0, 0, 0x96, 0x43, 0,
    ];
    for causes in [vec![7u8, 10], vec![0x47], vec![0x6f]] {
        let (listener, config) = fixture();
        let replies = causes.clone();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            assert_eq!(receive(&mut socket), [0x68, 4, 7, 0, 0, 0]);
            socket.write_all(&[0x68, 4, 11, 0, 0, 0]).unwrap();
            assert_eq!(receive(&mut socket), REQUEST);
            for (index, cause) in replies.into_iter().enumerate() {
                let raw = [
                    0x68,
                    18,
                    (index as u8) * 2,
                    0,
                    2,
                    0,
                    50,
                    1,
                    cause,
                    0,
                    1,
                    0,
                    7,
                    0,
                    0,
                    0,
                    0,
                    0x96,
                    0x43,
                    0,
                ];
                socket.write_all(&raw).unwrap();
                assert_eq!(
                    receive(&mut socket),
                    [0x68, 4, 1, 0, (index as u8 + 1) * 2, 0]
                );
            }
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).unwrap(), 0);
        });
        let options = SessionOptions {
            k: 1,
            w: 1,
            ..options()
        };
        let mut session = Session::connect(config, options).unwrap();
        assert!(session.setpoint(7, 300.0).is_err());
        session.start().unwrap();
        assert!(session.setpoint(7, 300.0).is_err());
        while session.transfer_state() != TransferState::Started {
            session.poll().unwrap();
        }
        assert!(session.setpoint(7, f32::NAN).is_err());
        session.setpoint(7, 300.0).unwrap();
        // 遥调占用和总召相同的发送窗口，接收确认前不能再发 I 帧。
        assert!(session.interrogate().is_err());
        assert!(session.drain_events().iter().any(|event| matches!(event,
            Event::Frame { direction: Direction::Send, raw } if raw == &REQUEST
        )));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = Vec::new();
        let mut decoded = Vec::new();
        while decoded.len() < causes.len() && Instant::now() < deadline {
            for event in session.poll().unwrap() {
                match event {
                    Event::Frame {
                        direction: Direction::Receive,
                        raw,
                    } => received.push(raw),
                    Event::Asdu(asdu) => decoded.push(asdu),
                    _ => {}
                }
            }
        }
        assert_eq!(decoded.len(), causes.len());
        assert_eq!(received.len(), causes.len());
        for ((asdu, raw), cause) in decoded.iter().zip(&received).zip(causes) {
            assert_eq!(asdu.raw, &raw[6..]);
            assert_eq!(asdu.type_id, 50);
            assert_eq!(asdu.cause, cause & 0x3f);
            assert_eq!(asdu.negative, cause & 0x40 != 0);
            assert_eq!(asdu.common_address, 1);
            assert_eq!(asdu.objects.len(), 1);
            assert_eq!(asdu.objects[0].address, 7);
            assert_eq!(asdu.objects[0].value, PointValue::Float(300.0));
            assert_eq!(asdu.objects[0].quality, 0);
            assert_eq!(asdu.objects[0].timestamp, None);
        }
        session.disconnect();
        server.join().unwrap();
    }
}

#[test]
fn malformed_receive_keeps_evidence_and_closes() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.write_all(&[0x69]).unwrap();
    });
    let mut session = Session::connect(config, options()).unwrap();
    assert!(session.poll().is_err());
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    assert!(session.drain_events().iter().any(|event| matches!(event,
        Event::Frame { direction: Direction::Receive, raw } if raw == &[0x69]
    )));
    server.join().unwrap();
}

#[test]
fn unsolicited_confirmation_closes_session() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .write_all(&Frame::U(UFunction::StartCon).encode().unwrap())
            .unwrap();
    });
    let mut session = Session::connect(config, options()).unwrap();
    assert!(session.poll().is_err());
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    server.join().unwrap();
}

#[test]
fn peer_eof_closes_session() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket.shutdown(std::net::Shutdown::Both).unwrap();
    });
    let mut session = Session::connect(config, options()).unwrap();
    assert!(session.poll().is_err());
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    server.join().unwrap();
}

#[test]
fn peer_test_activation_is_confirmed_without_starting_data_transfer() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket.write_all(&[0x68, 4, 0x43, 0, 0, 0]).unwrap();
        assert_eq!(receive(&mut socket), [0x68, 4, 0x83, 0, 0, 0]);
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    let mut session = Session::connect(config, options()).unwrap();
    let events = session.poll().unwrap();
    assert!(events.iter().any(|event| matches!(event,
        Event::Frame { direction: Direction::Send, raw } if raw == &[0x68, 4, 0x83, 0, 0, 0]
    )));
    assert_eq!(session.transfer_state(), TransferState::Stopped);
    session.disconnect();
    server.join().unwrap();
}

#[test]
fn unknown_asdu_keeps_full_raw_frame_and_parse_error() {
    let (listener, config) = fixture();
    let raw = vec![0x68, 10, 0, 0, 0, 0, 255, 1, 3, 0, 1, 0];
    let expected = raw.clone();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        assert_eq!(receive(&mut socket), [0x68, 4, 7, 0, 0, 0]);
        let mut response = vec![0x68, 4, 11, 0, 0, 0];
        response.extend(raw);
        socket.write_all(&response).unwrap();
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).unwrap(), 0);
    });
    let mut session = Session::connect(config, options()).unwrap();
    session.start().unwrap();
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|event| matches!(event, Event::ParseError(_)))
    {
        events.extend(session.poll().unwrap());
    }
    assert!(events.iter().any(|event| matches!(event,
        Event::Frame { direction: Direction::Receive, raw } if raw == &expected
    )));
    assert!(!events.iter().any(|event| matches!(event, Event::Asdu(_))));
    assert_eq!(session.connection_state(), ConnectionState::Connected);
    session.disconnect();
    server.join().unwrap();
}

#[test]
fn eof_retains_partial_received_frame() {
    let (listener, config) = fixture();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.write_all(&[0x68, 4, 0x0b]).unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
    });
    let mut session = Session::connect(config, options()).unwrap();
    // 第一轮可能仅收到半帧；EOF 到达后仍须保留已经接收的字节。
    while session.poll().is_ok() {}
    assert_eq!(session.connection_state(), ConnectionState::Disconnected);
    assert!(session.drain_events().iter().any(|event| matches!(event,
        Event::Frame { direction: Direction::Receive, raw } if raw == &[0x68, 4, 0x0b]
    )));
    server.join().unwrap();
}
