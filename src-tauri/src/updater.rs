use std::time::Duration;

use serde::Serialize;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::{Mutex, MutexGuard};

pub(crate) struct UpdateSession<T> {
    candidate: Mutex<Option<T>>,
}

impl<T> Default for UpdateSession<T> {
    fn default() -> Self {
        Self {
            candidate: Mutex::new(None),
        }
    }
}

impl<T> UpdateSession<T> {
    fn begin_check(&self) -> Result<MutexGuard<'_, Option<T>>, String> {
        let mut candidate = self.lock()?;
        *candidate = None;
        Ok(candidate)
    }

    fn begin_install(&self) -> Result<MutexGuard<'_, Option<T>>, String> {
        let candidate = self.lock()?;
        if candidate.is_none() {
            return Err(update_error("请先检查更新并确认有可安装的新版本"));
        }
        Ok(candidate)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<T>>, String> {
        self.candidate
            .try_lock()
            .map_err(|_| update_error("已有更新操作正在执行，请稍后重试"))
    }
}

pub(crate) type UpdateState = UpdateSession<Update>;

#[derive(Serialize)]
pub(crate) struct UpdateInfo {
    version: String,
    body: Option<String>,
}

fn update_error(message: impl Into<String>) -> String {
    let message = message.into();
    log::warn!("主站更新：{message}");
    message
}

#[tauri::command]
pub(crate) async fn check_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, UpdateState>,
) -> Result<Option<UpdateInfo>, String> {
    let mut candidate = state.begin_check()?;
    log::info!("开始检查主站 R2 更新清单");
    let updater = app
        .updater_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| update_error(format!("初始化更新检查失败：{error}")))?;
    let update = updater
        .check()
        .await
        .map_err(|error| update_error(format!("检查更新失败，请检查网络后重试：{error}")))?;
    let Some(mut update) = update else {
        log::info!("主站更新检查完成，当前已是最新版本");
        return Ok(None);
    };
    update.timeout = Some(Duration::from_secs(600));
    let info = UpdateInfo {
        version: update.version.clone(),
        body: update.body.clone(),
    };
    log::info!("发现主站新版本：{}，准备自动下载安装", info.version);
    *candidate = Some(update);
    Ok(Some(info))
}

#[tauri::command]
pub(crate) async fn install_update(state: tauri::State<'_, UpdateState>) -> Result<(), String> {
    let mut candidate = state.begin_install()?;
    let update = candidate
        .as_ref()
        .ok_or_else(|| update_error("请先检查更新并确认有可安装的新版本"))?;
    log::info!("开始下载并校验主站更新：{}", update.version);
    update
        .download_and_install(
            |_, _| {},
            || log::info!("主站更新下载完成，安装前将校验更新签名"),
        )
        .await
        .map_err(|error| update_error(format!("下载或安装更新失败，可重试：{error}")))?;
    *candidate = None;
    log::info!("主站更新安装流程已完成");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::UpdateSession;

    // 未检查到更新时拒绝安装，不能接收界面传入的下载地址。
    #[test]
    fn install_requires_checked_candidate() {
        let session = UpdateSession::<&str>::default();
        assert!(session.begin_install().is_err());
    }

    // 检查和安装共用同一互斥边界，拒绝重入而不是排队执行旧操作。
    #[test]
    fn check_and_install_are_exclusive() {
        let session = UpdateSession::default();
        let mut checking = session.begin_check().unwrap();
        *checking = Some("候选版本");
        assert!(session.begin_check().is_err());
        assert!(session.begin_install().is_err());
        drop(checking);
        let installing = session.begin_install().unwrap();
        assert!(session.begin_check().is_err());
        assert!(session.begin_install().is_err());
        drop(installing);
    }

    // 新检查开始即失效旧候选，即使后续网络检查失败也不能安装旧候选。
    #[test]
    fn failed_recheck_discards_previous_candidate() {
        let session = UpdateSession::default();
        *session.begin_check().unwrap() = Some("旧候选");
        drop(session.begin_check().unwrap());
        assert!(session.begin_install().is_err());
    }

    // 安装失败释放互斥锁并保留已检查对象，允许重试同一签名版本。
    #[test]
    fn failed_install_keeps_candidate_for_retry() {
        let session = UpdateSession::default();
        *session.begin_check().unwrap() = Some("候选版本");
        drop(session.begin_install().unwrap());
        assert_eq!(*session.begin_install().unwrap(), Some("候选版本"));
    }
}
