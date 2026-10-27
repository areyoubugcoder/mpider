//! GitHub 签名更新：检查、预下载、跨重启缓存及用户确认后的安装；安装前重新验签。
use std::{fs, path::Path, sync::Mutex, time::Duration};

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

pub const RELEASES_URL: &str = "https://github.com/areyoubugcoder/mpider/releases";

#[derive(Clone, Serialize)]
pub struct UpdateStatus {
    current_version: String,
    configured: bool,
    supported: bool,
    phase: String,
    version: Option<String>,
    notes: Option<String>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
    releases_url: &'static str,
}

#[derive(Default)]
struct Inner {
    status: Option<UpdateStatus>,
    update: Option<Update>,
    initialized: bool,
}

#[derive(Default)]
pub struct UpdateState {
    // 独立操作锁防止重复下载、检查覆盖待安装对象或安装期间删除缓存。
    operation: tokio::sync::Mutex<()>,
    inner: Mutex<Inner>,
}

impl UpdateState {
    fn edit<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Result<T, String> {
        self.inner
            .lock()
            .map(|mut inner| f(&mut inner))
            .map_err(|_| "更新状态不可用，请重启应用".into())
    }

    fn status(&self, app: &AppHandle) -> Result<UpdateStatus, String> {
        self.edit(|inner| inner.status.clone().unwrap_or_else(|| initial_status(app)))
    }

    fn failed(&self, error: String, phase: &str) -> Result<(), String> {
        self.edit(|inner| {
            if let Some(status) = &mut inner.status {
                status.phase = phase.into();
                status.error = Some(error);
            }
        })
    }
}

fn initial_status(app: &AppHandle) -> UpdateStatus {
    let configured = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .is_some_and(|v| !v.trim().is_empty());
    UpdateStatus {
        current_version: app.package_info().version.to_string(),
        configured,
        supported: cfg!(all(target_os = "windows", target_arch = "x86_64")),
        phase: "idle".into(),
        version: None,
        notes: None,
        downloaded_bytes: 0,
        total_bytes: None,
        error: None,
        releases_url: RELEASES_URL,
    }
}

fn updater(app: &AppHandle) -> Result<Updater, String> {
    if !initial_status(app).supported {
        return Err("目前仅提供 Windows x64 的版本更新".into());
    }
    if !initial_status(app).configured {
        return Err("此构建未配置更新签名，请从 GitHub Releases 安装正式版本".into());
    }
    app.updater_builder()
        .timeout(Duration::from_secs(30))
        // 正式通道不安装预发布、同版本或旧版本，版本号取 Tauri 项目配置。
        .version_comparator(|current, release| {
            release.version > current && release.version.pre.is_empty()
        })
        .build()
        .map_err(|e| format!("更新配置不可用：{e}"))
}

fn cache_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|p| p.join("updates"))
        .map_err(|e| format!("无法打开更新缓存目录：{e}"))
}

fn restore_cache(updater: &Updater, root: &Path) -> Result<Option<(Update, u64)>, String> {
    let manifest = root.join("release.json");
    if !manifest.exists() {
        return Ok(None);
    }
    let raw = fs::read(&manifest).map_err(|e| format!("无法读取更新清单：{e}"))?;
    let update = updater
        .restore_update(serde_json::from_slice(&raw).map_err(|e| format!("更新清单损坏：{e}"))?)
        .map_err(|e| format!("更新清单无效：{e}"))?;
    let Some(update) = update else {
        fs::remove_dir_all(root).map_err(|e| format!("无法清理旧更新：{e}"))?;
        return Ok(None);
    };
    let bytes =
        fs::read(root.join("package.bin")).map_err(|e| format!("预下载包缺失或不可读：{e}"))?;
    update
        .verify(&bytes)
        .map_err(|e| format!("预下载包签名校验失败，请重新下载：{e}"))?;
    Ok(Some((update, bytes.len() as u64)))
}

fn save_cache(root: &Path, update: &Update, bytes: &[u8]) -> Result<(), String> {
    update
        .verify(bytes)
        .map_err(|e| format!("安装包签名校验失败：{e}"))?;
    fs::create_dir_all(root).map_err(|e| format!("无法创建更新目录：{e}"))?;
    // 清单最后落盘，未完成的下载不会被当成可安装包。
    let manifest = root.join("release.json");
    if manifest.exists() {
        fs::remove_file(&manifest).map_err(|e| e.to_string())?;
    }
    fs::write(root.join("package.bin"), bytes).map_err(|e| format!("无法保存安装包：{e}"))?;
    fs::write(
        manifest,
        serde_json::to_vec(&update.raw_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("无法保存更新清单：{e}"))
}

fn set_release(status: &mut UpdateStatus, update: &Update, phase: &str) {
    status.phase = phase.into();
    status.version = Some(update.version.clone());
    status.notes = update.body.clone();
    status.error = None;
}

async fn initialize(app: &AppHandle, state: &UpdateState) -> Result<(), String> {
    if state.edit(|inner| inner.initialized)? {
        return Ok(());
    }
    state.edit(|inner| {
        inner.status = Some(initial_status(app));
        inner.initialized = true;
    })?;
    if !initial_status(app).configured || !initial_status(app).supported {
        return Ok(());
    }
    let updater = updater(app)?;
    let root = cache_dir(app)?;
    let cached = tauri::async_runtime::spawn_blocking(move || restore_cache(&updater, &root))
        .await
        .map_err(|e| e.to_string())?;
    match cached {
        Ok(Some((update, size))) => state.edit(|inner| {
            let status = inner.status.as_mut().unwrap();
            set_release(status, &update, "ready");
            status.downloaded_bytes = size;
            status.total_bytes = Some(size);
            inner.update = Some(update);
        })?,
        Ok(None) => (),
        Err(error) => state.failed(error, "error")?,
    }
    Ok(())
}

#[tauri::command]
pub async fn update_status(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<UpdateStatus, String> {
    if let Ok(_operation) = state.operation.try_lock() {
        initialize(&app, &state).await?;
    }
    state.status(&app)
}

#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<UpdateStatus, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后再试")?;
    initialize(&app, &state).await?;
    // 已预下载时保留该版本，用户可安装或清除后重新检查，避免新清单覆盖离线安装对象。
    if state.status(&app)?.phase == "ready" {
        return state.status(&app);
    }
    state.edit(|inner| {
        let status = inner.status.as_mut().unwrap();
        status.phase = "checking".into();
        status.error = None;
    })?;
    let result = async {
        updater(&app)?
            .check()
            .await
            .map_err(|e| format!("检查更新失败，请检查网络或稍后重试：{e}"))
    }
    .await;
    match result {
        Ok(update) => state.edit(|inner| {
            let status = inner.status.as_mut().unwrap();
            status.downloaded_bytes = 0;
            status.total_bytes = None;
            if let Some(update) = &update {
                set_release(status, update, "available");
            } else {
                status.phase = "up_to_date".into();
                status.version = None;
                status.notes = None;
            }
            inner.update = update;
        })?,
        Err(error) => state.failed(error, "error")?,
    }
    state.status(&app)
}

#[tauri::command]
pub async fn update_download(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<UpdateStatus, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后再试")?;
    initialize(&app, &state).await?;
    if state.status(&app)?.phase == "ready" {
        return state.status(&app);
    }
    let mut update = state
        .edit(|inner| inner.update.clone())?
        .ok_or("请先检查新版本")?;
    // 安装包可大于百 MB，下载使用独立的较长超时。
    update.timeout = Some(Duration::from_secs(30 * 60));
    state.edit(|inner| {
        let status = inner.status.as_mut().unwrap();
        status.phase = "downloading".into();
        status.error = None;
        status.downloaded_bytes = 0;
        status.total_bytes = None;
    })?;
    let result = async {
        let bytes = update
            .download(
                |chunk, total| {
                    let _ = state.edit(|inner| {
                        let status = inner.status.as_mut().unwrap();
                        status.downloaded_bytes += chunk as u64;
                        status.total_bytes = total;
                    });
                },
                || {},
            )
            .await
            .map_err(|e| format!("下载安装包失败，请重试：{e}"))?;
        let root = cache_dir(&app)?;
        let cached_update = update.clone();
        let size = bytes.len() as u64;
        tauri::async_runtime::spawn_blocking(move || save_cache(&root, &cached_update, &bytes))
            .await
            .map_err(|e| e.to_string())??;
        Ok::<_, String>(size)
    }
    .await;
    match result {
        Ok(size) => state.edit(|inner| {
            let status = inner.status.as_mut().unwrap();
            set_release(status, &update, "ready");
            status.downloaded_bytes = size;
            status.total_bytes = Some(size);
        })?,
        Err(error) => state.failed(error, "available")?,
    }
    state.status(&app)
}

#[tauri::command]
pub async fn update_clear(
    app: AppHandle,
    state: State<'_, UpdateState>,
) -> Result<UpdateStatus, String> {
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后再试")?;
    let root = cache_dir(&app)?;
    if root.exists() {
        fs::remove_dir_all(root).map_err(|e| format!("无法清理更新缓存：{e}"))?;
    }
    state.edit(|inner| {
        inner.update = None;
        inner.status = Some(initial_status(&app));
        inner.initialized = true;
    })?;
    state.status(&app)
}

#[tauri::command]
pub async fn update_install(app: AppHandle, state: State<'_, UpdateState>) -> Result<(), String> {
    if tauri::is_dev() {
        return Err("开发模式仅支持检查与预下载，请使用正式安装包启动更新".into());
    }
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "更新操作正在进行，请稍后再试")?;
    initialize(&app, &state).await?;
    if state.status(&app)?.phase != "ready" {
        return Err("请先完成安装包预下载".into());
    }
    state.edit(|inner| {
        let status = inner.status.as_mut().unwrap();
        status.phase = "installing".into();
        status.error = None;
    })?;
    let result = async {
        let installer = updater(&app)?;
        let root = cache_dir(&app)?;
        tauri::async_runtime::spawn_blocking(move || {
            // 从磁盘重建并重新验签，不信任下载后可能被替换的缓存。
            let (update, _) =
                restore_cache(&installer, &root)?.ok_or("缓存版本已不适用，请重新检查更新")?;
            let bytes = fs::read(root.join("package.bin")).map_err(|e| e.to_string())?;
            // Windows 安装器会直接退出进程，必须提前复位旧代理。
            mpider_core::sysproxy::force_restore_all();
            update
                .install(bytes)
                .map_err(|e| format!("安装失败，请重试或从 GitHub 下载安装：{e}"))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    .await;
    if let Err(error) = result {
        state.failed(error.clone(), "ready")?;
        return Err(error);
    }
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const PAYLOAD: &[u8] = include_bytes!("../fixtures/updater/package.bin");
    const PUBLIC_KEY: &str = include_str!("../fixtures/updater/public.key");
    const SIGNATURE: &str = include_str!("../fixtures/updater/package.bin.sig");

    fn test_updater(endpoint: &str) -> Updater {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            json!({
                "pubkey":PUBLIC_KEY.trim(), "requireSignedVersion":true,
                "dangerousInsecureTransportProtocol":true
            }),
        );
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        app.updater_builder()
            .target("test-target")
            .version_comparator(|current, release| {
                release.version > current && release.version.pre.is_empty()
            })
            .timeout(Duration::from_secs(3))
            .no_proxy()
            .endpoints(vec![endpoint.parse().unwrap()])
            .unwrap()
            .build()
            .unwrap()
    }

    fn manifest(version: &str) -> serde_json::Value {
        json!({"version":version,"notes":"离线更新测试","platforms":{"test-target":{
            "url":"https://example.invalid/package", "signature":SIGNATURE.trim()
        }}})
    }

    #[test]
    fn signed_cache_survives_restart_and_rejects_tampering() {
        let first = test_updater("https://example.invalid/latest.json");
        let update = first.restore_update(manifest("9.9.9")).unwrap().unwrap();
        let temp = tempfile::tempdir().unwrap();
        save_cache(temp.path(), &update, PAYLOAD).unwrap();
        let restarted = test_updater("https://example.invalid/latest.json");
        let (restored, size) = restore_cache(&restarted, temp.path()).unwrap().unwrap();
        assert_eq!(restored.version, "9.9.9");
        assert_eq!(size, PAYLOAD.len() as u64);
        fs::write(temp.path().join("package.bin"), b"tampered").unwrap();
        assert!(restore_cache(&restarted, temp.path())
            .err()
            .unwrap()
            .contains("签名校验失败"));
    }

    #[test]
    fn cached_metadata_cannot_relabel_old_signed_package() {
        let updater = test_updater("https://example.invalid/latest.json");
        let forged = updater.restore_update(manifest("99.0.0")).unwrap().unwrap();
        let temp = tempfile::tempdir().unwrap();
        assert!(save_cache(temp.path(), &forged, PAYLOAD).is_err());
        assert!(!temp.path().join("release.json").exists());
    }

    #[test]
    fn stable_channel_rejects_same_old_and_prerelease_versions() {
        let updater = test_updater("https://example.invalid/latest.json");
        for version in ["0.0.0", "0.1.0", "99.0.0-beta.1"] {
            assert!(
                updater.restore_update(manifest(version)).unwrap().is_none(),
                "{version}"
            );
        }
    }

    #[test]
    fn incomplete_download_is_not_installable() {
        let updater = test_updater("https://example.invalid/latest.json");
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("package.bin"), PAYLOAD).unwrap();
        assert!(restore_cache(&updater, temp.path()).unwrap().is_none());
        fs::write(temp.path().join("release.json"), b"broken json").unwrap();
        assert!(restore_cache(&updater, temp.path()).is_err());
    }

    #[tokio::test]
    #[ignore = "需先运行 Postman 更新模拟服务，设置 MPIDER_UPDATER_TEST_ENDPOINT"]
    async fn network_simulation() {
        let endpoint = std::env::var("MPIDER_UPDATER_TEST_ENDPOINT").expect("缺少模拟服务地址");
        let updater = test_updater(&format!("{endpoint}/latest.json"));
        let update = updater.check().await.unwrap().unwrap();
        let mut received = 0;
        let bytes = update
            .download(|chunk, _| received += chunk, || {})
            .await
            .unwrap();
        assert_eq!(bytes, PAYLOAD);
        assert_eq!(received, PAYLOAD.len());
        let temp = tempfile::tempdir().unwrap();
        save_cache(temp.path(), &update, &bytes).unwrap();
        let offline = test_updater("https://example.invalid/no-network");
        assert!(restore_cache(&offline, temp.path()).unwrap().is_some());
        let corrupt = test_updater(&format!("{endpoint}/corrupt.json"))
            .check()
            .await
            .unwrap()
            .unwrap();
        assert!(corrupt.download(|_, _| {}, || {}).await.is_err());
        for path in ["unavailable.json", "missing.json", "invalid.json"] {
            assert!(
                test_updater(&format!("{endpoint}/{path}"))
                    .check()
                    .await
                    .is_err(),
                "{path}"
            );
        }
        assert!(test_updater(&format!("{endpoint}/latest.json"))
            .check()
            .await
            .unwrap()
            .is_some());
    }
}
