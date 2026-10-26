use crate::session::{DownloadSettlement, PlannedPackage, WorkQueues, install_rank};
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashSet,
    sync::mpsc::{self, Sender},
    thread,
};
use windows::{
    Win32::{
        Foundation::{DECIMAL, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS},
        System::{
            Com::{
                CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
            },
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
            },
            UpdateAgent::{
                IUpdate, IUpdate2, IUpdateCollection, IUpdateHistoryEntry, IUpdateHistoryEntry2,
                IUpdateSearcher, IUpdateSession, IWindowsDriverUpdate, UpdateCollection,
                UpdateSession, ssWindowsUpdate,
            },
        },
    },
    core::{BSTR, Interface, PWSTR, w},
};

const DRIVER_SEARCH: &str = "IsInstalled=0 and Type='Driver'";
const DRIVER_CATEGORY_ID: &str = "ebfc9e0a-0498-422a-a9e9-7c40d25514f7";
const CATALOG_ID_MISMATCH: &str = "Update package not found in catalog (ID mismatch)";

const _: () = assert!(ssWindowsUpdate.0 == 2);

// Deserializer helper to accept both a single JSON object or a JSON array
fn deserialize_vec_or_single<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Array(arr) => {
            let mut items = Vec::new();
            for item in arr {
                items.push(T::deserialize(item).map_err(serde::de::Error::custom)?);
            }
            Ok(items)
        }
        serde_json::Value::Object(_) => {
            let item = T::deserialize(value).map_err(serde::de::Error::custom)?;
            Ok(vec![item])
        }
        serde_json::Value::Null => Ok(Vec::new()),
        _ => Ok(Vec::new()),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WuaUpdateEntry {
    pub title: String,
    pub update_id: String,
    #[serde(default)]
    pub max_download_size: Option<u64>,
    #[serde(default)]
    pub driver_class: Option<String>,
    #[serde(default)]
    pub driver_provider: Option<String>,
    #[serde(default)]
    pub superseded_update_ids: Vec<String>,
}

#[derive(Clone)]
pub struct SelectableDriver {
    pub entry: WuaUpdateEntry,
    pub selected: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StagedDriverEntry {
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct DriverScanResult {
    #[serde(default, deserialize_with = "deserialize_vec_or_single")]
    pub pending: Vec<WuaUpdateEntry>,
    #[serde(default, deserialize_with = "deserialize_vec_or_single")]
    pub staged: Vec<StagedDriverEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallTarget {
    pub update_id: String,
    pub title: String,
    pub max_download_size: Option<u64>,
    pub driver_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriverInstallStatus {
    Queued,
    Downloading,
    Installing,
    Success,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum InstallEvent {
    ItemDownloading {
        index: usize,
        title: String,
    },
    ItemInstalling {
        index: usize,
        title: String,
    },
    ItemFinished {
        index: usize,
        title: String,
        success: bool,
        reboot_required: bool,
        message: String,
    },
    AllFinished {
        reboot_required: bool,
    },
    FatalError {
        error: String,
    },
}

struct CatalogUpdate {
    title: String,
    update_id: String,
    max_download_size: u64,
    driver_class: Option<String>,
    driver_provider: Option<String>,
    reboot_required: bool,
    superseded_update_ids: Vec<String>,
}

struct ComApartment;

impl ComApartment {
    fn new() -> windows::core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct OpenKey(HKEY);

impl Drop for OpenKey {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn decimal_to_u64(value: DECIMAL) -> u64 {
    let scale = unsafe { value.Anonymous1.Anonymous.scale };
    let sign = unsafe { value.Anonymous1.Anonymous.sign };
    if sign != 0 {
        return 0;
    }
    let mut magnitude = u128::from(unsafe { value.Anonymous2.Lo64 });
    magnitude += u128::from(value.Hi32) << 64;
    for _ in 0..scale {
        magnitude /= 10;
    }
    u64::try_from(magnitude).unwrap_or(u64::MAX)
}

fn title_has_driver_marker(title: &str) -> bool {
    let title = title.to_ascii_lowercase();
    [
        "driver",
        "extension",
        "intel",
        "lenovo",
        "realtek",
        "nvidia",
        "amd",
        "qualcomm",
        "audio",
        "bluetooth",
        "firmware",
        "sensor",
    ]
    .into_iter()
    .any(|marker| title.contains(marker))
}

fn looks_like_driver(categories: &[(String, String)], title: &str) -> bool {
    let category_hit = categories.iter().any(|(name, id)| {
        let name = name.to_ascii_lowercase();
        name.contains("driver")
            || name.contains("device")
            || id.eq_ignore_ascii_case(DRIVER_CATEGORY_ID)
    });
    category_hit || title_has_driver_marker(title)
}

fn absorb_catalog_update(
    result: &mut DriverScanResult,
    seen: &mut HashSet<String>,
    reboot_ids: &HashSet<String>,
    update: CatalogUpdate,
) {
    let id_lower = update.update_id.to_ascii_lowercase();
    if !seen.insert(id_lower.clone()) {
        return;
    }
    if update.reboot_required || reboot_ids.contains(&id_lower) {
        result.staged.push(StagedDriverEntry {
            title: update.title,
        });
        return;
    }
    result.pending.push(WuaUpdateEntry {
        title: update.title,
        update_id: update.update_id,
        max_download_size: Some(update.max_download_size),
        driver_class: update.driver_class,
        driver_provider: update.driver_provider,
        superseded_update_ids: update.superseded_update_ids,
    });
}

fn stage_history_item(
    result: &mut DriverScanResult,
    seen: &mut HashSet<String>,
    reboot_ids: &HashSet<String>,
    update_id: &str,
    categories: &[(String, String)],
    title: &str,
) -> bool {
    let id_lower = update_id.to_ascii_lowercase();
    if !reboot_ids.contains(&id_lower) || seen.contains(&id_lower) {
        return false;
    }
    if !looks_like_driver(categories, title) {
        return false;
    }
    seen.insert(id_lower);
    result.staged.push(StagedDriverEntry {
        title: title.to_string(),
    });
    true
}

fn reboot_update_ids() -> HashSet<String> {
    let mut ids = HashSet::new();
    unsafe {
        let mut raw = HKEY::default();
        let status = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!(
                "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\WindowsUpdate\\Auto Update\\RebootRequired"
            ),
            Some(0),
            KEY_READ,
            &mut raw,
        );
        if status != ERROR_SUCCESS {
            return ids;
        }
        let _key = OpenKey(raw);
        let mut index = 0u32;
        loop {
            let mut name = vec![0u16; 256];
            let mut len = name.len() as u32;
            let status = RegEnumValueW(
                raw,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            );
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status == ERROR_MORE_DATA {
                name.resize(len as usize + 1, 0);
                len = name.len() as u32;
                let status = RegEnumValueW(
                    raw,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut len,
                    None,
                    None,
                    None,
                    None,
                );
                if status == ERROR_SUCCESS {
                    push_reboot_id(&mut ids, &name, len);
                }
                index += 1;
                continue;
            }
            if status != ERROR_SUCCESS {
                break;
            }
            push_reboot_id(&mut ids, &name, len);
            index += 1;
        }
    }
    ids
}

fn push_reboot_id(ids: &mut HashSet<String>, name: &[u16], len: u32) {
    let text = String::from_utf16_lossy(&name[..len as usize]);
    if !text.is_empty() {
        ids.insert(text.to_ascii_lowercase());
    }
}

fn read_superseded_update_ids(update: &IUpdate) -> Vec<String> {
    let Ok(ids) = (unsafe { update.SupersededUpdateIDs() }) else {
        return Vec::new();
    };
    let Ok(count) = (unsafe { ids.Count() }) else {
        return Vec::new();
    };
    let mut superseded = Vec::new();
    for index in 0..count {
        let Ok(id) = (unsafe { ids.get_Item(index) }) else {
            continue;
        };
        let text = id.to_string();
        if !text.is_empty() {
            superseded.push(text);
        }
    }
    superseded
}

fn read_catalog_update(update: &IUpdate) -> Option<CatalogUpdate> {
    let title = unsafe { update.Title() }.ok()?.to_string();
    let update_id = unsafe { update.Identity() }
        .ok()
        .and_then(|identity| unsafe { identity.UpdateID() }.ok())?
        .to_string();
    let reboot_required = update
        .cast::<IUpdate2>()
        .ok()
        .and_then(|update| unsafe { update.RebootRequired() }.ok())
        .is_some_and(|flag| flag.0 != 0);
    let max_download_size = unsafe { update.MaxDownloadSize() }
        .map(decimal_to_u64)
        .unwrap_or(0);
    let (driver_class, driver_provider) = update
        .cast::<IWindowsDriverUpdate>()
        .ok()
        .map(|driver| {
            (
                unsafe { driver.DriverClass() }
                    .ok()
                    .map(|value| value.to_string()),
                unsafe { driver.DriverProvider() }
                    .ok()
                    .map(|value| value.to_string()),
            )
        })
        .unwrap_or((None, None));
    let superseded_update_ids = read_superseded_update_ids(update);
    Some(CatalogUpdate {
        title,
        update_id,
        max_download_size,
        driver_class,
        driver_provider,
        reboot_required,
        superseded_update_ids,
    })
}

fn history_categories(item: &IUpdateHistoryEntry) -> Vec<(String, String)> {
    let Ok(entry) = item.cast::<IUpdateHistoryEntry2>() else {
        return Vec::new();
    };
    let Ok(categories) = (unsafe { entry.Categories() }) else {
        return Vec::new();
    };
    let Ok(count) = (unsafe { categories.Count() }) else {
        return Vec::new();
    };
    let mut pairs = Vec::new();
    for index in 0..count {
        let Ok(category) = (unsafe { categories.get_Item(index) }) else {
            continue;
        };
        let name = unsafe { category.Name() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        let id = unsafe { category.CategoryID() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        pairs.push((name, id));
    }
    pairs
}

fn append_reboot_history(
    searcher: &IUpdateSearcher,
    result: &mut DriverScanResult,
    seen: &mut HashSet<String>,
    reboot_ids: &HashSet<String>,
) {
    if !reboot_ids.iter().any(|id| !seen.contains(id)) {
        return;
    }
    let history_count = match unsafe { searcher.GetTotalHistoryCount() } {
        Ok(count) if count > 0 => count,
        Ok(_) => return,
        Err(err) => {
            eprintln!("axle: reboot history scan failed: {err}");
            return;
        }
    };
    let take = history_count.min(40);
    let history = match unsafe { searcher.QueryHistory(0, take) } {
        Ok(history) => history,
        Err(err) => {
            eprintln!("axle: reboot history scan failed: {err}");
            return;
        }
    };
    let count = match unsafe { history.Count() } {
        Ok(count) => count,
        Err(err) => {
            eprintln!("axle: reboot history scan failed: {err}");
            return;
        }
    };
    for index in 0..count {
        let Ok(item) = (unsafe { history.get_Item(index) }) else {
            continue;
        };
        let Ok(identity) = (unsafe { item.UpdateIdentity() }) else {
            continue;
        };
        let Ok(update_id) = (unsafe { identity.UpdateID() }) else {
            continue;
        };
        let title = unsafe { item.Title() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        let categories = history_categories(&item);
        stage_history_item(
            result,
            seen,
            reboot_ids,
            &update_id.to_string(),
            &categories,
            &title,
        );
    }
}

pub(crate) fn fetch_driver_state_com() -> windows::core::Result<DriverScanResult> {
    let _apartment = ComApartment::new()?;
    unsafe {
        let session: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_ALL)?;
        let app_id = BSTR::from("axle-tui");
        session.SetClientApplicationID(&app_id)?;
        let searcher = session.CreateUpdateSearcher()?;
        searcher.SetServerSelection(ssWindowsUpdate)?;
        let criteria = BSTR::from(DRIVER_SEARCH);
        let found = searcher.Search(&criteria)?;
        let updates = found.Updates()?;
        let count = updates.Count()?;
        let reboot_ids = reboot_update_ids();
        let mut result = DriverScanResult::default();
        let mut seen = HashSet::new();
        for index in 0..count {
            let Ok(update) = updates.get_Item(index) else {
                continue;
            };
            let Some(parsed) = read_catalog_update(&update) else {
                continue;
            };
            absorb_catalog_update(&mut result, &mut seen, &reboot_ids, parsed);
        }
        append_reboot_history(&searcher, &mut result, &mut seen, &reboot_ids);
        Ok(result)
    }
}

pub fn fetch_driver_state_blocking() -> DriverScanResult {
    match fetch_driver_state_com() {
        Ok(result) => result,
        Err(err) => {
            eprintln!("axle: catalog scan failed: {err}");
            DriverScanResult::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallFinish {
    success: bool,
    reboot_required: bool,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogMatch {
    Missing,
    Found { catalog_index: usize },
}

fn operation_succeeded(result_code: i32) -> bool {
    result_code == 2 || result_code == 3
}

fn download_failure_message(result_code: i32) -> Option<String> {
    if operation_succeeded(result_code) {
        None
    } else {
        Some(format!(
            "Download operation failed with result code {result_code}"
        ))
    }
}

fn install_finish(result_code: i32, reboot_required: bool) -> InstallFinish {
    let success = operation_succeeded(result_code);
    let message = match result_code {
        2 if reboot_required => "Installed successfully (reboot pending)",
        2 => "Installed successfully",
        3 if reboot_required => "Installed with warnings (reboot pending)",
        3 => "Installed with warnings",
        4 => "Installation failed by system",
        5 => "Installation canceled",
        other => return InstallFinish {
            success,
            reboot_required,
            message: format!("Completed with code {other}"),
        },
    };
    InstallFinish {
        success,
        reboot_required,
        message: message.to_string(),
    }
}

fn batch_reboot_required(finishes: &[InstallFinish]) -> bool {
    finishes
        .iter()
        .any(|finish| finish.success && finish.reboot_required)
}

fn match_update_ids(target_ids: &[impl AsRef<str>], catalog_ids: &[impl AsRef<str>]) -> Vec<CatalogMatch> {
    target_ids
        .iter()
        .map(|target_id| {
            let needle = target_id.as_ref();
            catalog_ids
                .iter()
                .position(|catalog_id| catalog_id.as_ref().eq_ignore_ascii_case(needle))
                .map(|catalog_index| CatalogMatch::Found { catalog_index })
                .unwrap_or(CatalogMatch::Missing)
        })
        .collect()
}

fn download_one(session: &IUpdateSession, update: &IUpdate) -> windows::core::Result<Option<String>> {
    unsafe {
        let collection: IUpdateCollection = CoCreateInstance(&UpdateCollection, None, CLSCTX_ALL)?;
        collection.Add(update)?;
        let downloader = session.CreateUpdateDownloader()?;
        downloader.SetUpdates(&collection)?;
        let result = downloader.Download()?;
        let code = result.ResultCode()?;
        Ok(download_failure_message(code.0))
    }
}

fn install_one(session: &IUpdateSession, update: &IUpdate) -> windows::core::Result<InstallFinish> {
    unsafe {
        let collection: IUpdateCollection = CoCreateInstance(&UpdateCollection, None, CLSCTX_ALL)?;
        collection.Add(update)?;
        let installer = session.CreateUpdateInstaller()?;
        installer.SetUpdates(&collection)?;
        let result = installer.Install()?;
        let code = result.ResultCode()?;
        let reboot = result.RebootRequired()?;
        Ok(install_finish(code.0, reboot.0 != 0))
    }
}

fn emit_finished(
    tx: &Sender<InstallEvent>,
    index: usize,
    title: &str,
    success: bool,
    reboot_required: bool,
    message: String,
) {
    let _ = tx.send(InstallEvent::ItemFinished {
        index,
        title: title.to_string(),
        success,
        reboot_required,
        message,
    });
}

enum OwnSession<T> {
    Missing,
    Ready(T),
}

enum DownloadReport {
    Missing,
    Failed(String),
    Ready,
}

enum InstallReport {
    Missing,
    Error(String),
    Finish(InstallFinish),
}

enum WorkerEvent {
    Download { index: usize, report: DownloadReport },
    Install { index: usize, report: InstallReport },
}

fn on_own_session<T>(
    update_id: &str,
    action: impl FnOnce(&IUpdateSession, &IUpdate) -> windows::core::Result<T>,
) -> Result<OwnSession<T>, String> {
    let _apartment = ComApartment::new().map_err(|err| err.to_string())?;
    unsafe {
        let session: IUpdateSession =
            CoCreateInstance(&UpdateSession, None, CLSCTX_ALL).map_err(|err| err.to_string())?;
        let app_id = BSTR::from("axle-tui");
        session
            .SetClientApplicationID(&app_id)
            .map_err(|err| err.to_string())?;
        let searcher = session
            .CreateUpdateSearcher()
            .map_err(|err| err.to_string())?;
        searcher
            .SetServerSelection(ssWindowsUpdate)
            .map_err(|err| err.to_string())?;
        let criteria = BSTR::from(DRIVER_SEARCH);
        let found = searcher.Search(&criteria).map_err(|err| err.to_string())?;
        let updates = found.Updates().map_err(|err| err.to_string())?;
        let count = updates.Count().map_err(|err| err.to_string())?;
        let mut catalog_ids = Vec::new();
        let mut catalog_updates = Vec::new();
        for index in 0..count {
            let Ok(update) = updates.get_Item(index) else {
                continue;
            };
            let Ok(identity) = update.Identity() else {
                continue;
            };
            let Ok(id) = identity.UpdateID() else {
                continue;
            };
            catalog_ids.push(id.to_string());
            catalog_updates.push(update);
        }
        let matched = match_update_ids(&[update_id], &catalog_ids);
        let CatalogMatch::Found { catalog_index } = matched[0] else {
            return Ok(OwnSession::Missing);
        };
        action(&session, &catalog_updates[catalog_index])
            .map(OwnSession::Ready)
            .map_err(|err| err.to_string())
    }
}

fn run_download(target: &InstallTarget) -> DownloadReport {
    match on_own_session(&target.update_id, download_one) {
        Ok(OwnSession::Missing) => DownloadReport::Missing,
        Ok(OwnSession::Ready(None)) => DownloadReport::Ready,
        Ok(OwnSession::Ready(Some(message))) => DownloadReport::Failed(message),
        Err(err) => {
            eprintln!("axle: download error for {} : {err}", target.title);
            DownloadReport::Failed(format!("Download failed: {err}"))
        }
    }
}

fn run_install(target: &InstallTarget) -> InstallReport {
    match on_own_session(&target.update_id, install_one) {
        Ok(OwnSession::Missing) => InstallReport::Missing,
        Ok(OwnSession::Ready(finish)) => InstallReport::Finish(finish),
        Err(err) => {
            eprintln!("axle: install error for {} : {err}", target.title);
            InstallReport::Error(err)
        }
    }
}

pub fn stream_install_selected(targets: Vec<InstallTarget>, tx: Sender<InstallEvent>) {
    if targets.is_empty() {
        let _ = tx.send(InstallEvent::AllFinished {
            reboot_required: false,
        });
        return;
    }

    if let Err(err) = install_selected_com(&targets, &tx) {
        let _ = tx.send(InstallEvent::FatalError {
            error: err,
        });
    }
}

fn install_selected_com(targets: &[InstallTarget], tx: &Sender<InstallEvent>) -> Result<(), String> {
    let packages = targets
        .iter()
        .map(|target| PlannedPackage {
            max_download_size: target.max_download_size.unwrap_or(0),
            rank: install_rank(&target.title, target.driver_class.as_deref()),
        })
        .collect();
    let mut queues = WorkQueues::new(packages);
    let (event_tx, event_rx) = mpsc::channel();
    let (install_tx, install_rx) = mpsc::channel::<usize>();
    let install_targets = targets.to_vec();
    let install_events = event_tx.clone();
    let install_worker = thread::spawn(move || {
        while let Ok(index) = install_rx.recv() {
            let report = run_install(&install_targets[index]);
            if install_events
                .send(WorkerEvent::Install { index, report })
                .is_err()
            {
                break;
            }
        }
    });
    let mut download_workers = Vec::new();

    loop {
        for index in queues.take_downloads() {
            let _ = tx.send(InstallEvent::ItemDownloading {
                index,
                title: targets[index].title.clone(),
            });
            let target = targets[index].clone();
            let events = event_tx.clone();
            download_workers.push(thread::spawn(move || {
                let report = run_download(&target);
                let _ = events.send(WorkerEvent::Download { index, report });
            }));
        }

        if let Some(index) = queues.take_install() {
            let _ = tx.send(InstallEvent::ItemInstalling {
                index,
                title: targets[index].title.clone(),
            });
            install_tx
                .send(index)
                .map_err(|_| String::from("install worker stopped"))?;
        }

        if !queues.work_pending() {
            break;
        }
        if queues.can_dispatch() {
            continue;
        }

        match event_rx.recv().map_err(|_| String::from("install workers stopped"))? {
            WorkerEvent::Download { index, report } => {
                let (success, message) = match report {
                    DownloadReport::Ready => (true, None),
                    DownloadReport::Missing => (false, Some(CATALOG_ID_MISMATCH.to_string())),
                    DownloadReport::Failed(message) => (false, Some(message)),
                };
                if queues.complete_download(index, success) == DownloadSettlement::Failed {
                    emit_finished(
                        tx,
                        index,
                        &targets[index].title,
                        false,
                        false,
                        message.unwrap_or_default(),
                    );
                }
            }
            WorkerEvent::Install { index, report } => {
                let (success, reboot_required, message) = match report {
                    InstallReport::Finish(finish) => {
                        (finish.success, finish.reboot_required, finish.message)
                    }
                    InstallReport::Missing => (false, false, CATALOG_ID_MISMATCH.to_string()),
                    InstallReport::Error(err) => (false, false, format!("Install error: {err}")),
                };
                emit_finished(
                    tx,
                    index,
                    &targets[index].title,
                    success,
                    reboot_required,
                    message,
                );
                if let Some(halt) = queues.complete_install(index, success, reboot_required) {
                    for remaining in halt.remaining {
                        emit_finished(
                            tx,
                            remaining,
                            &targets[remaining].title,
                            false,
                            false,
                            halt.message.to_string(),
                        );
                    }
                }
            }
        }
    }

    let _ = tx.send(InstallEvent::AllFinished {
        reboot_required: queues.reboot_required(),
    });
    drop(event_tx);
    drop(install_tx);
    for worker in download_workers {
        let _ = worker.join();
    }
    let _ = install_worker.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::mpsc;

    #[test]
    fn install_code_2_without_reboot_is_success() {
        let finish = install_finish(2, false);
        assert!(finish.success);
        assert!(!finish.reboot_required);
        assert_eq!(finish.message, "Installed successfully");
        assert!(!batch_reboot_required(&[finish]));
    }

    #[test]
    fn install_code_2_with_reboot_sets_batch_reboot() {
        let finish = install_finish(2, true);
        assert!(finish.success);
        assert!(finish.reboot_required);
        assert_eq!(finish.message, "Installed successfully (reboot pending)");
        assert!(batch_reboot_required(&[finish]));
    }

    #[test]
    fn install_code_4_is_not_success() {
        let finish = install_finish(4, true);
        assert!(!finish.success);
        assert_eq!(finish.message, "Installation failed by system");
        assert!(!batch_reboot_required(&[finish]));
    }

    #[test]
    fn install_code_3_with_reboot_warns() {
        let finish = install_finish(3, true);
        assert!(finish.success);
        assert_eq!(finish.message, "Installed with warnings (reboot pending)");
        assert!(batch_reboot_required(&[finish]));
    }

    #[test]
    fn missing_update_id_leaves_other_ids_matched() {
        let matched = match_update_ids(&["missing-id", "Present-ID", "also-missing"], &["present-id"]);
        assert_eq!(
            matched,
            vec![
                CatalogMatch::Missing,
                CatalogMatch::Found { catalog_index: 0 },
                CatalogMatch::Missing,
            ]
        );
    }

    #[test]
    fn empty_install_list_finishes_without_reboot() {
        let (tx, rx) = mpsc::channel();
        stream_install_selected(Vec::new(), tx);
        let events: Vec<InstallEvent> = rx.into_iter().collect();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            InstallEvent::AllFinished {
                reboot_required: false
            }
        ));
    }

    #[test]
    fn catalog_reboot_and_history_follow_the_scan_script() {
        let mut result = DriverScanResult::default();
        let mut seen = HashSet::new();
        let reboot_ids = HashSet::from(["abc".to_string()]);
        absorb_catalog_update(
            &mut result,
            &mut seen,
            &reboot_ids,
            CatalogUpdate {
                title: "Needs reboot".to_string(),
                update_id: "ABC".to_string(),
                max_download_size: 10,
                driver_class: Some("Net".to_string()),
                driver_provider: Some("Intel".to_string()),
                reboot_required: false,
                superseded_update_ids: Vec::new(),
            },
        );
        absorb_catalog_update(
            &mut result,
            &mut seen,
            &reboot_ids,
            CatalogUpdate {
                title: "Duplicate".to_string(),
                update_id: "abc".to_string(),
                max_download_size: 11,
                driver_class: None,
                driver_provider: None,
                reboot_required: false,
                superseded_update_ids: Vec::new(),
            },
        );
        assert_eq!(result.staged.len(), 1);
        assert_eq!(result.staged[0].title, "Needs reboot");
        assert!(result.pending.is_empty());

        let mut result = DriverScanResult::default();
        let mut seen = HashSet::new();
        absorb_catalog_update(
            &mut result,
            &mut seen,
            &HashSet::new(),
            CatalogUpdate {
                title: "Ready".to_string(),
                update_id: "id-1".to_string(),
                max_download_size: 42,
                driver_class: Some("Display".to_string()),
                driver_provider: Some("NVIDIA".to_string()),
                reboot_required: true,
                superseded_update_ids: Vec::new(),
            },
        );
        assert!(result.pending.is_empty());
        assert_eq!(result.staged.len(), 1);

        let mut result = DriverScanResult::default();
        let mut seen = HashSet::new();
        absorb_catalog_update(
            &mut result,
            &mut seen,
            &HashSet::new(),
            CatalogUpdate {
                title: "Ready".to_string(),
                update_id: "id-1".to_string(),
                max_download_size: 42,
                driver_class: Some("Display".to_string()),
                driver_provider: Some("NVIDIA".to_string()),
                reboot_required: false,
                superseded_update_ids: vec!["old-id".to_string()],
            },
        );
        assert_eq!(result.pending.len(), 1);
        assert_eq!(result.pending[0].update_id, "id-1");
        assert_eq!(
            result.pending[0].superseded_update_ids,
            vec!["old-id".to_string()]
        );
        assert_eq!(result.pending[0].max_download_size, Some(42));
        assert_eq!(result.pending[0].driver_class.as_deref(), Some("Display"));
        assert_eq!(result.pending[0].driver_provider.as_deref(), Some("NVIDIA"));

        let reboot_ids = HashSet::from(["hist".to_string()]);
        let mut seen = HashSet::new();
        assert!(!stage_history_item(
            &mut result,
            &mut seen,
            &reboot_ids,
            "hist",
            &[],
            "Security Update",
        ));
        assert!(stage_history_item(
            &mut result,
            &mut seen,
            &reboot_ids,
            "HIST",
            &[],
            "Intel driver",
        ));
        assert!(!stage_history_item(
            &mut result,
            &mut seen,
            &reboot_ids,
            "hist",
            &[],
            "Intel driver",
        ));
        let mut seen = HashSet::new();
        assert!(stage_history_item(
            &mut result,
            &mut seen,
            &reboot_ids,
            "hist",
            &[("Devices".to_string(), "other".to_string())],
            "plain",
        ));
        let mut seen = HashSet::new();
        assert!(stage_history_item(
            &mut result,
            &mut seen,
            &reboot_ids,
            "hist",
            &[(
                "Other".to_string(),
                "EBfc9e0a-0498-422a-a9e9-7c40d25514f7".to_string(),
            )],
            "plain",
        ));
        assert!(!stage_history_item(
            &mut result,
            &mut seen,
            &HashSet::new(),
            "other",
            &[],
            "NVIDIA firmware",
        ));
    }
}
