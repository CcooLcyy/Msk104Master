use iec104_core::ConnectionConfig;
use serde::Serialize;

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    protocol_ready: bool,
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    AppInfo {
        name: "Msk104Master",
        version: env!("CARGO_PKG_VERSION"),
        protocol_ready: false,
    }
}

#[tauri::command]
fn validate_connection(config: ConnectionConfig) -> Result<(), String> {
    let result = config.validate();
    match &result {
        Ok(()) => log::info!("连接参数校验通过；未发起 TCP 或 IEC104 连接"),
        Err(error) => log::warn!("连接参数校验失败：{error}"),
    }
    result
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .setup(|_| {
            log::info!("主站桌面应用已运行，当前为工程骨架，协议连接功能尚未实现");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_app_info, validate_connection])
        .run(tauri::generate_context!())
        .expect("运行主站桌面应用失败");
}
