use iec104_core::ConnectionConfig;
use serde::Serialize;

mod updater;

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: String,
    protocol_ready: bool,
}

#[tauri::command]
fn get_app_info(app: tauri::AppHandle) -> AppInfo {
    AppInfo {
        name: "Msk104Master",
        version: app.package_info().version.to_string(),
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
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::UpdateState::default())
        .setup(|_| {
            log::info!("主站桌面应用已运行，当前为工程骨架，协议连接功能尚未实现");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            validate_connection,
            updater::check_update,
            updater::install_update
        ])
        .run(tauri::generate_context!())
        .expect("运行主站桌面应用失败");
}
