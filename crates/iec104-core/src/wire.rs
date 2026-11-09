//! IEC104 纯报文编解码，固定使用 COT2、CA2、IOA3。

const MAX_SEQUENCE: u16 = 0x7fff;
const MAX_ASDU_LENGTH: usize = 249;
pub const SHORT_FLOAT_SETPOINT_TYPE: u8 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UFunction {
    StartAct,
    StartCon,
    StopAct,
    StopCon,
    TestAct,
    TestCon,
}

impl UFunction {
    fn control(self) -> u8 {
        match self {
            Self::StartAct => 0x07,
            Self::StartCon => 0x0b,
            Self::StopAct => 0x13,
            Self::StopCon => 0x23,
            Self::TestAct => 0x43,
            Self::TestCon => 0x83,
        }
    }

    fn from_control(control: u8) -> Result<Self, String> {
        match control {
            0x07 => Ok(Self::StartAct),
            0x0b => Ok(Self::StartCon),
            0x13 => Ok(Self::StopAct),
            0x23 => Ok(Self::StopCon),
            0x43 => Ok(Self::TestAct),
            0x83 => Ok(Self::TestCon),
            _ => Err(format!("无效的 U 帧功能控制字：0x{control:02X}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    I { send: u16, recv: u16, asdu: Vec<u8> },
    S { recv: u16 },
    U(UFunction),
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut bytes = vec![0x68, 4];
        match self {
            Self::I { send, recv, asdu } => {
                check_sequence(*send)?;
                check_sequence(*recv)?;
                if !(6..=MAX_ASDU_LENGTH).contains(&asdu.len()) {
                    return Err("I 帧 ASDU 长度必须在 6～249 字节之间".to_owned());
                }
                bytes[1] = (4 + asdu.len()) as u8;
                bytes.extend_from_slice(&(send << 1).to_le_bytes());
                bytes.extend_from_slice(&(recv << 1).to_le_bytes());
                bytes.extend_from_slice(asdu);
            }
            Self::S { recv } => {
                check_sequence(*recv)?;
                bytes.extend_from_slice(&[1, 0]);
                bytes.extend_from_slice(&(recv << 1).to_le_bytes());
            }
            Self::U(function) => {
                bytes.extend_from_slice(&[function.control(), 0, 0, 0]);
            }
        }
        Ok(bytes)
    }
}

fn check_sequence(sequence: u16) -> Result<(), String> {
    if sequence > MAX_SEQUENCE {
        return Err("帧序号必须在 0～32767 之间".to_owned());
    }
    Ok(())
}

/// 增量 TCP 分帧器；协议错误后拒绝继续使用，调用者应关闭连接并新建分帧器。
/// 内部最多缓存一帧，不会把任意长度的 TCP 输入整体复制到缓存。
#[derive(Debug, Default)]
pub struct Decoder {
    pending: Vec<u8>,
    failed: bool,
}

impl Decoder {
    pub fn push(&mut self, mut data: &[u8]) -> Result<Vec<Frame>, String> {
        if self.failed {
            return Err("分帧器已因协议错误停止，请关闭连接并重新创建".to_owned());
        }
        let mut frames = Vec::new();
        while !data.is_empty() {
            let target = match self.pending.len() {
                0 => 1,
                1 => 2,
                _ => usize::from(self.pending[1]) + 2,
            };
            let count = (target - self.pending.len()).min(data.len());
            self.pending.extend_from_slice(&data[..count]);
            data = &data[count..];
            if self.pending[0] != 0x68 {
                return self.fail("APDU 起始字节必须为 0x68".to_owned());
            }
            if self.pending.len() < 2 {
                continue;
            }
            let length = usize::from(self.pending[1]);
            if !(4..=253).contains(&length) {
                return self.fail("APDU 长度字段必须在 4～253 之间".to_owned());
            }
            if self.pending.len() == length + 2 {
                match decode_complete(&self.pending) {
                    Ok(frame) => frames.push(frame),
                    Err(error) => return self.fail(error),
                }
                self.pending.clear();
            }
        }
        Ok(frames)
    }

    fn fail(&mut self, error: String) -> Result<Vec<Frame>, String> {
        self.failed = true;
        self.pending.clear();
        Err(error)
    }
}

fn decode_complete(bytes: &[u8]) -> Result<Frame, String> {
    let control = &bytes[2..6];
    if control[0] & 1 == 0 {
        if bytes.len() < 12 {
            return Err("I 帧缺少完整 ASDU 头部".to_owned());
        }
        if control[2] & 1 != 0 {
            return Err("I 帧接收序号最低位必须为零".to_owned());
        }
        Ok(Frame::I {
            send: u16::from_le_bytes([control[0], control[1]]) >> 1,
            recv: u16::from_le_bytes([control[2], control[3]]) >> 1,
            asdu: bytes[6..].to_vec(),
        })
    } else if control[0] & 3 == 1 {
        if bytes.len() != 6 || control[0] != 1 || control[1] != 0 || control[2] & 1 != 0 {
            return Err("S 帧长度或保留控制位无效".to_owned());
        }
        Ok(Frame::S {
            recv: u16::from_le_bytes([control[2], control[3]]) >> 1,
        })
    } else {
        if bytes.len() != 6 || control[1..] != [0, 0, 0] {
            return Err("U 帧长度或保留控制位无效".to_owned());
        }
        Ok(Frame::U(UFunction::from_control(control[0])?))
    }
}

/// 总召激活：站召唤限定词 QOI=20，信息地址为零。
pub fn general_interrogation(common_address: u16) -> Vec<u8> {
    let [low, high] = common_address.to_le_bytes();
    vec![100, 1, 6, 0, low, high, 0, 0, 0, 20]
}

/// 单个 C_SE_NC_1 短浮点直接执行对象，QOS 取零。
pub fn short_float_setpoint(
    common_address: u16,
    address: u32,
    value: f32,
) -> Result<Vec<u8>, String> {
    if address > 0xff_ffff || !value.is_finite() {
        return Err("遥调须使用三字节信息地址及有限的短浮点值".to_owned());
    }
    let [ca_low, ca_high] = common_address.to_le_bytes();
    let [ioa_low, ioa_middle, ioa_high, _] = address.to_le_bytes();
    let mut asdu = vec![
        SHORT_FLOAT_SETPOINT_TYPE,
        1,
        6,
        0,
        ca_low,
        ca_high,
        ioa_low,
        ioa_middle,
        ioa_high,
    ];
    asdu.extend_from_slice(&value.to_le_bytes());
    asdu.push(0);
    Ok(asdu)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Asdu {
    pub type_id: u8,
    pub cause: u8,
    pub negative: bool,
    pub test: bool,
    pub originator: u8,
    pub common_address: u16,
    pub objects: Vec<InformationObject>,
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InformationObject {
    pub address: u32,
    pub value: PointValue,
    /// 原始 SIQ/DIQ（含状态位）、QDS 或遥调 QOS；总召为零。
    pub quality: u8,
    /// 原始 CP56Time2a；此层不校验日历或解释时区。
    pub timestamp: Option<[u8; 7]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PointValue {
    Single(bool),
    Double(u8),
    Normalized(i16),
    Scaled(i16),
    Float(f32),
    Interrogation(u8),
}

pub fn parse_asdu(bytes: &[u8]) -> Result<Asdu, String> {
    if !(6..=MAX_ASDU_LENGTH).contains(&bytes.len()) {
        return Err("ASDU 长度必须在 6～249 字节之间".to_owned());
    }
    let type_id = bytes[0];
    let (base_type, timed) = match type_id {
        1 | 3 | 9 | 11 | 13 | 100 => (type_id, false),
        SHORT_FLOAT_SETPOINT_TYPE if bytes[1] == 1 => (13, false),
        SHORT_FLOAT_SETPOINT_TYPE => {
            return Err("短浮点遥调 ASDU 必须为 SQ=0 且只有一个对象".to_owned())
        }
        30 => (1, true),
        31 => (3, true),
        34 => (9, true),
        35 => (11, true),
        36 => (13, true),
        _ => return Err(format!("暂不支持 ASDU 类型：{type_id}")),
    };
    let count = usize::from(bytes[1] & 0x7f);
    if count == 0 {
        return Err("ASDU 信息对象数量不能为零".to_owned());
    }
    let sequence = bytes[1] & 0x80 != 0;
    let value_length = match base_type {
        1 | 3 | 100 => 1,
        9 | 11 => 3,
        13 => 5,
        _ => unreachable!("类型已在前面校验"),
    };
    let object_length = value_length + if timed { 7 } else { 0 };
    let expected_length = 6 + if sequence {
        3 + count * object_length
    } else {
        count * (3 + object_length)
    };
    if bytes.len() != expected_length {
        return Err(format!(
            "ASDU 信息对象长度不符：应为 {expected_length} 字节，实际为 {} 字节",
            bytes.len()
        ));
    }
    if type_id == 100 && (bytes[1] != 1 || bytes[6..9] != [0, 0, 0]) {
        return Err("总召 ASDU 必须为 SQ=0、单个对象且信息地址为零".to_owned());
    }
    let mut objects = Vec::with_capacity(count);
    let mut offset = 6;
    let mut sequence_address = 0;
    for index in 0..count {
        let address = if !sequence || index == 0 {
            let address =
                u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], 0]);
            offset += 3;
            sequence_address = address;
            address
        } else {
            sequence_address + index as u32
        };
        if address > 0xff_ffff {
            return Err("SQ 连续信息地址超出三字节范围".to_owned());
        }
        let payload = &bytes[offset..offset + object_length];
        let (value, quality) = match base_type {
            1 => (PointValue::Single(payload[0] & 1 != 0), payload[0]),
            3 => (PointValue::Double(payload[0] & 3), payload[0]),
            9 => (
                PointValue::Normalized(i16::from_le_bytes([payload[0], payload[1]])),
                payload[2],
            ),
            11 => (
                PointValue::Scaled(i16::from_le_bytes([payload[0], payload[1]])),
                payload[2],
            ),
            13 => (
                PointValue::Float(f32::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ])),
                payload[4],
            ),
            100 => (PointValue::Interrogation(payload[0]), 0),
            _ => unreachable!("类型已在前面校验"),
        };
        let timestamp = if timed {
            let mut timestamp = [0; 7];
            timestamp.copy_from_slice(&payload[value_length..]);
            Some(timestamp)
        } else {
            None
        };
        objects.push(InformationObject {
            address,
            value,
            quality,
            timestamp,
        });
        offset += object_length;
    }
    Ok(Asdu {
        type_id,
        cause: bytes[2] & 0x3f,
        negative: bytes[2] & 0x40 != 0,
        test: bytes[2] & 0x80 != 0,
        originator: bytes[3],
        common_address: u16::from_le_bytes([bytes[4], bytes[5]]),
        objects,
        raw: bytes.to_vec(),
    })
}
