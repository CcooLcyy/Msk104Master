use iec104_core::wire::{general_interrogation, parse_asdu, Decoder, Frame, PointValue, UFunction};

#[test]
fn fixed_control_frames_and_sequence_boundaries() {
    for (function, control) in [
        (UFunction::StartAct, 0x07),
        (UFunction::StartCon, 0x0b),
        (UFunction::StopAct, 0x13),
        (UFunction::StopCon, 0x23),
        (UFunction::TestAct, 0x43),
        (UFunction::TestCon, 0x83),
    ] {
        let expected = vec![0x68, 4, control, 0, 0, 0];
        assert_eq!(Frame::U(function).encode().unwrap(), expected);
        assert_eq!(
            Decoder::default().push(&expected).unwrap(),
            vec![Frame::U(function)]
        );
    }
    let supervisory = Frame::S { recv: 32767 };
    let expected = [0x68, 4, 1, 0, 0xfe, 0xff];
    assert_eq!(supervisory.encode().unwrap(), expected);
    assert_eq!(
        Decoder::default().push(&expected).unwrap(),
        vec![supervisory]
    );
    assert!(Frame::S { recv: 32768 }.encode().is_err());
    for (send, recv) in [(32768, 0), (0, 32768)] {
        assert!(Frame::I {
            send,
            recv,
            asdu: vec![1; 6],
        }
        .encode()
        .is_err());
    }
}

#[test]
fn general_interrogation_has_fixed_wire_bytes() {
    let asdu = general_interrogation(0x1234);
    assert_eq!(asdu, [100, 1, 6, 0, 0x34, 0x12, 0, 0, 0, 20]);
    let frame = Frame::I {
        send: 32767,
        recv: 1,
        asdu,
    };
    let expected = [
        0x68, 14, 0xfe, 0xff, 2, 0, 100, 1, 6, 0, 0x34, 0x12, 0, 0, 0, 20,
    ];
    assert_eq!(frame.encode().unwrap(), expected);
    assert_eq!(Decoder::default().push(&expected).unwrap(), vec![frame]);
    let parsed = parse_asdu(&expected[6..]).unwrap();
    assert_eq!(parsed.objects[0].value, PointValue::Interrogation(20));
}

#[test]
fn interrogation_requires_one_zero_address_object_without_sequence() {
    for bytes in [
        vec![100, 0x81, 7, 0, 1, 0, 0, 0, 0, 20],
        vec![100, 2, 7, 0, 1, 0, 0, 0, 0, 20, 0, 0, 0, 20],
        vec![100, 1, 7, 0, 1, 0, 1, 0, 0, 20],
    ] {
        assert!(parse_asdu(&bytes).is_err(), "应拒绝非法总召：{bytes:?}");
    }
    for cause in [6, 7, 10] {
        let mut bytes = general_interrogation(1);
        bytes[2] = cause;
        assert!(parse_asdu(&bytes).is_ok());
    }
}

#[test]
fn decoder_handles_every_split_and_concatenated_frames() {
    let bytes = [
        0x68, 4, 7, 0, 0, 0, 0x68, 4, 1, 0, 4, 0, 0x68, 10, 0, 0, 0, 0, 1, 0, 3, 0, 1, 0,
    ];
    let expected = vec![
        Frame::U(UFunction::StartAct),
        Frame::S { recv: 2 },
        Frame::I {
            send: 0,
            recv: 0,
            asdu: vec![1, 0, 3, 0, 1, 0],
        },
    ];
    for split in 0..=bytes.len() {
        let mut decoder = Decoder::default();
        let mut actual = decoder.push(&bytes[..split]).unwrap();
        actual.extend(decoder.push(&bytes[split..]).unwrap());
        assert_eq!(actual, expected, "拆包位置：{split}");
    }
    let mut decoder = Decoder::default();
    let mut actual = Vec::new();
    for byte in bytes {
        actual.extend(decoder.push(&[byte]).unwrap());
    }
    assert_eq!(actual, expected);
}

#[test]
fn decoder_rejects_malformed_headers_and_control_fields() {
    for bytes in [
        vec![0x67],
        vec![0x68, 3],
        vec![0x68, 254],
        vec![0x68, 4, 0, 0, 0, 0],
        vec![0x68, 4, 1, 1, 0, 0],
        vec![0x68, 4, 1, 0, 1, 0],
        vec![0x68, 4, 3, 0, 0, 0],
        vec![0x68, 4, 0x0f, 0, 0, 0],
        vec![0x68, 4, 7, 1, 0, 0],
        vec![0x68, 5, 7, 0, 0, 0, 0],
        vec![0x68, 10, 0, 0, 1, 0, 1, 0, 3, 0, 1, 0],
    ] {
        let mut decoder = Decoder::default();
        assert!(decoder.push(&bytes).is_err(), "应拒绝：{bytes:?}");
        assert!(decoder.push(&[0x68, 4, 7, 0, 0, 0]).is_err());
    }
}

#[test]
fn frame_asdu_size_boundaries_are_enforced() {
    for size in [0, 5, 250] {
        assert!(Frame::I {
            send: 0,
            recv: 0,
            asdu: vec![0; size],
        }
        .encode()
        .is_err());
    }
    let frame = Frame::I {
        send: 0,
        recv: 0,
        asdu: vec![0; 249],
    };
    let bytes = frame.encode().unwrap();
    assert_eq!(bytes[1], 253);
    assert_eq!(Decoder::default().push(&bytes).unwrap(), vec![frame]);
}

#[test]
fn asdu_preserves_header_sparse_addresses_and_raw_quality() {
    let bytes = [1, 2, 0xc3, 9, 0x34, 0x12, 1, 2, 3, 0xf1, 9, 8, 7, 0x80];
    let parsed = parse_asdu(&bytes).unwrap();
    assert_eq!(parsed.type_id, 1);
    assert_eq!(parsed.cause, 3);
    assert!(parsed.negative);
    assert!(parsed.test);
    assert_eq!(parsed.originator, 9);
    assert_eq!(parsed.common_address, 0x1234);
    assert_eq!(parsed.raw, bytes);
    assert_eq!(parsed.objects.len(), 2);
    assert_eq!(parsed.objects[0].address, 0x030201);
    assert_eq!(parsed.objects[0].value, PointValue::Single(true));
    assert_eq!(parsed.objects[0].quality, 0xf1);
    assert_eq!(parsed.objects[1].address, 0x070809);
    assert_eq!(parsed.objects[1].value, PointValue::Single(false));
    assert_eq!(parsed.objects[1].timestamp, None);
}

#[test]
fn asdu_supports_all_value_types_and_cp56_timestamps() {
    let timestamp = [0x39, 0x30, 0x81, 0x92, 0x63, 0x0b, 0x1a];
    for (type_id, timed_id, payload, expected, quality) in [
        (1, 30, vec![0x91], PointValue::Single(true), 0x91),
        (3, 31, vec![0xa2], PointValue::Double(2), 0xa2),
        (
            9,
            34,
            vec![0, 0x80, 0x81],
            PointValue::Normalized(-32768),
            0x81,
        ),
        (
            11,
            35,
            vec![0xff, 0x7f, 0x51],
            PointValue::Scaled(32767),
            0x51,
        ),
        (
            13,
            36,
            vec![0, 0, 0x20, 0xc0, 0x10],
            PointValue::Float(-2.5),
            0x10,
        ),
    ] {
        for timed in [false, true] {
            let id = if timed { timed_id } else { type_id };
            for sequence in [false, true] {
                let vsq = if sequence { 0x82 } else { 2 };
                let mut bytes = vec![id, vsq, 20, 0, 1, 0];
                for index in 0..2 {
                    if !sequence || index == 0 {
                        bytes.extend_from_slice(&[0xfe + index, 0xff, 0]);
                    }
                    bytes.extend_from_slice(&payload);
                    if timed {
                        bytes.extend_from_slice(&timestamp);
                    }
                }
                let parsed = parse_asdu(&bytes).unwrap();
                for (index, object) in parsed.objects.iter().enumerate() {
                    assert_eq!(object.address, 0xfffe + index as u32);
                    assert_eq!(object.value, expected);
                    assert_eq!(object.quality, quality);
                    assert_eq!(object.timestamp, timed.then_some(timestamp));
                }
                for end in 0..bytes.len() {
                    assert!(
                        parse_asdu(&bytes[..end]).is_err(),
                        "类型：{id}，SQ：{sequence}，截断位置：{end}"
                    );
                }
            }
        }
    }
}

#[test]
fn asdu_rejects_truncation_trailing_data_unknown_types_and_ioa_overflow() {
    let bytes = [36, 1, 3, 0, 1, 0, 1, 0, 0, 0, 0, 0x80, 0x3f, 0, 1, 2, 3, 4, 5, 6, 7];
    assert!(parse_asdu(&bytes).is_ok());
    for end in 0..bytes.len() {
        assert!(parse_asdu(&bytes[..end]).is_err(), "截断位置：{end}");
    }
    let mut trailing = bytes.to_vec();
    trailing.push(0);
    assert!(parse_asdu(&trailing).is_err());
    assert!(parse_asdu(&[200, 1, 3, 0, 1, 0, 0, 0, 0, 0]).is_err());
    assert!(parse_asdu(&[1, 0x82, 3, 0, 1, 0, 0xff, 0xff, 0xff, 1, 0]).is_err());
    assert!(parse_asdu(&[1, 0, 3, 0, 1, 0]).is_err());
    assert!(parse_asdu(&[0; 250]).is_err());
}
