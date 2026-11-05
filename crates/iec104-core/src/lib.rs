//! IEC104 主站核心。当前仅定义配置边界，不建立协议连接。

use serde::{Deserialize, Serialize};
use std::net::IpAddr;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub common_address: u16,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 2404,
            common_address: 1,
        }
    }
}

impl ConnectionConfig {
    /// 仅校验参数，不解析 DNS、不发起 TCP 或 IEC104 通信。
    pub fn validate(&self) -> Result<(), String> {
        let host = self.host.as_str();
        let valid_hostname = !host.is_empty()
            && host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|value| value.is_ascii_alphanumeric() || value == b'-')
            });
        if host.parse::<IpAddr>().is_err() && !valid_hostname {
            return Err("主机必须是有效的 IP 地址或主机名，不含端口、空格或协议前缀".to_owned());
        }
        if self.port == 0 {
            return Err("端口必须在 1～65535 之间".to_owned());
        }
        if self.common_address == 0 || self.common_address == u16::MAX {
            return Err("连接公共地址必须在 1～65534 之间，不能使用广播地址".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 默认配置可用于后续手动连接，但校验本身没有网络副作用。
    #[test]
    fn default_config_is_valid() {
        assert!(ConnectionConfig::default().validate().is_ok());
    }

    // 接受 IPv4、IPv6 和常见主机名。
    #[test]
    fn accepts_network_hosts() {
        for host in ["192.168.1.219", "::1", "device-01.local", "localhost"] {
            let config = ConnectionConfig {
                host: host.to_owned(),
                ..Default::default()
            };
            assert!(config.validate().is_ok(), "主机应有效：{host}");
        }
    }

    // 拒绝地址为空、带协议、带端口及存在空白或非法标签的情况。
    #[test]
    fn rejects_invalid_hosts() {
        for host in [
            "",
            " host",
            "host ",
            "http://host",
            "host:2404",
            "a..b",
            "-host",
        ] {
            let config = ConnectionConfig {
                host: host.to_owned(),
                ..Default::default()
            };
            assert!(config.validate().is_err(), "主机应无效：{host}");
        }
    }

    // 普通连接拒绝无效端口、零公共地址与广播公共地址。
    #[test]
    fn rejects_invalid_connection_numbers() {
        for (port, common_address) in [(0, 1), (2404, 0), (2404, 65535)] {
            let config = ConnectionConfig {
                port,
                common_address,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
    }

    // 校验边界允许最大端口和最大非广播公共地址。
    #[test]
    fn accepts_upper_boundaries() {
        let config = ConnectionConfig {
            port: 65535,
            common_address: 65534,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }
}
