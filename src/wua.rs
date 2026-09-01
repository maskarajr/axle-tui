use serde::{Deserialize, Deserializer, Serialize};
use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::Sender,
};

const DRIVER_TUI_TITLES_ENV: &str = "DRIVER_TUI_TITLES_JSON";
const SCAN_PS_SCRIPT: &str = include_str!("../scripts/scan.ps1");
const INSTALL_PS_SCRIPT: &str = include_str!("../scripts/install.ps1");

fn powershell_exe() -> PathBuf {
    std::env::var("SystemRoot")
        .map(|root| {
            PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .unwrap_or_else(|_| {
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        })
}

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
    #[serde(default)]
    pub max_download_size: Option<u64>,
    #[serde(default)]
    pub driver_class: Option<String>,
    #[serde(default)]
    pub driver_provider: Option<String>,
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

pub fn fetch_driver_state_blocking() -> DriverScanResult {
    let output = Command::new(powershell_exe())
        .args(["-NoProfile", "-NonInteractive", "-Command", SCAN_PS_SCRIPT])
        .output();

    if let Ok(res) = output {
        let stdout = String::from_utf8_lossy(&res.stdout).trim().to_string();
        if stdout.is_empty() || stdout == "null" {
            return DriverScanResult::default();
        }
        serde_json::from_str::<DriverScanResult>(&stdout).unwrap_or_default()
    } else {
        DriverScanResult::default()
    }
}

pub fn stream_install_selected(selected_titles: Vec<String>, tx: Sender<InstallEvent>) {
    if selected_titles.is_empty() {
        let _ = tx.send(InstallEvent::AllFinished {
            reboot_required: false,
        });
        return;
    }

    let titles_json = match serde_json::to_string(&selected_titles) {
        Ok(json) => json,
        Err(e) => {
            let _ = tx.send(InstallEvent::FatalError {
                error: format!("Failed to serialize driver titles: {e}"),
            });
            return;
        }
    };

    let mut child = match Command::new(powershell_exe())
        .args(["-NoProfile", "-NonInteractive", "-Command", INSTALL_PS_SCRIPT])
        .env(DRIVER_TUI_TITLES_ENV, titles_json)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(InstallEvent::FatalError {
                error: e.to_string(),
            });
            return;
        }
    };

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let trimmed = line.trim();
            if trimmed.starts_with('{') {
                if let Ok(event) = serde_json::from_str::<InstallEvent>(trimmed) {
                    let _ = tx.send(event);
                }
            }
        }
    }

    let _ = child.wait();
}
