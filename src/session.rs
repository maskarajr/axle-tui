use crate::wua::{DriverInstallStatus, InstallEvent, StagedDriverEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub update_id: String,
    pub title: String,
    pub max_download_size: Option<u64>,
    pub driver_class: Option<String>,
    pub superseded_update_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub update_id: String,
    pub title: String,
    pub status: DriverInstallStatus,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finish {
    pub reboot_required: bool,
    pub clean: bool,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub queue: Vec<QueueItem>,
    pub staged: Vec<StagedDriverEntry>,
    pub status_text: String,
    pub finish: Option<Finish>,
}

pub fn plan(selected: &[Job]) -> Vec<Job> {
    selected
        .iter()
        .enumerate()
        .filter(|(index, job)| {
            !selected.iter().enumerate().any(|(other, candidate)| {
                other != *index
                    && candidate
                        .superseded_update_ids
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(&job.update_id))
            })
        })
        .map(|(_, job)| job.clone())
        .collect()
}

pub fn apply(session: &Session, event: &InstallEvent) -> Session {
    let mut queue = session.queue.clone();
    let mut staged = session.staged.clone();
    let mut status_text = session.status_text.clone();
    let mut finish = session.finish.clone();

    match event {
        InstallEvent::ItemDownloading { index, .. } => {
            if let Some(item) = queue.get_mut(*index) {
                item.status = DriverInstallStatus::Downloading;
            }
        }
        InstallEvent::ItemInstalling { index, .. } => {
            if let Some(item) = queue.get_mut(*index) {
                item.status = DriverInstallStatus::Installing;
            }
        }
        InstallEvent::ItemFinished {
            index,
            success,
            reboot_required,
            message,
            ..
        } => {
            if let Some(item) = queue.get_mut(*index) {
                item.status = if *success {
                    DriverInstallStatus::Success
                } else {
                    DriverInstallStatus::Failed(message.clone())
                };
                item.message = message.clone();
            }

            if *success && *reboot_required {
                if let Some(title) = queue.get(*index).map(|item| item.title.clone()) {
                    if !staged.iter().any(|entry| entry.title == title) {
                        staged.push(StagedDriverEntry { title });
                    }
                }
            }
        }
        InstallEvent::AllFinished { reboot_required } => {
            let missing_result = queue.iter().any(|item| status_still_open(&item.status));
            if missing_result {
                for item in &mut queue {
                    if status_still_open(&item.status) {
                        let message = String::from("No item result arrived");
                        item.status = DriverInstallStatus::Failed(message.clone());
                        item.message = message;
                    }
                }
            }

            finish = Some(Finish {
                reboot_required: *reboot_required,
                clean: !missing_result,
            });
            status_text = if missing_result {
                String::from("Install did not finish cleanly.")
            } else if *reboot_required {
                String::from("Installation complete. System reboot is recommended.")
            } else {
                String::from("All updates processed successfully.")
            };
        }
        InstallEvent::FatalError { error } => {
            finish = Some(Finish {
                reboot_required: false,
                clean: true,
            });
            status_text = format!("Installation aborted: {error}");
        }
    }

    Session {
        queue,
        staged,
        status_text,
        finish,
    }
}

pub const DOWNLOAD_SLOT_LIMIT: usize = 3;
pub const INSTALL_STOPPED_FOR_REBOOT: &str = "Install stopped because a reboot is required";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstallRank {
    Base,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPackage {
    pub max_download_size: u64,
    pub rank: InstallRank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootHalt {
    pub message: &'static str,
    pub remaining: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadSettlement {
    Ready,
    Failed,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkQueues {
    packages: Vec<PlannedPackage>,
    download_waiting: Vec<usize>,
    download_active: Vec<usize>,
    install_ready: Vec<usize>,
    settled: Vec<bool>,
    install_busy: bool,
    halt_installs: bool,
}

pub fn install_rank(title: &str, driver_class: Option<&str>) -> InstallRank {
    let class = driver_class.unwrap_or("");
    if title.to_ascii_lowercase().contains("extension")
        || class.to_ascii_lowercase().contains("extension")
    {
        InstallRank::Extension
    } else {
        InstallRank::Base
    }
}

impl WorkQueues {
    pub fn new(packages: Vec<PlannedPackage>) -> Self {
        let mut download_waiting: Vec<usize> = (0..packages.len()).collect();
        download_waiting.sort_by_key(|index| (packages[*index].max_download_size, *index));
        Self {
            settled: vec![false; packages.len()],
            packages,
            download_waiting,
            download_active: Vec::new(),
            install_ready: Vec::new(),
            install_busy: false,
            halt_installs: false,
        }
    }

    pub fn take_downloads(&mut self) -> Vec<usize> {
        if self.halt_installs {
            return Vec::new();
        }
        let mut started = Vec::new();
        while self.download_active.len() < DOWNLOAD_SLOT_LIMIT {
            let Some(index) = self.download_waiting.first().copied() else {
                break;
            };
            self.download_waiting.remove(0);
            self.download_active.push(index);
            started.push(index);
        }
        started
    }

    pub fn complete_download(&mut self, index: usize, success: bool) -> DownloadSettlement {
        let Some(position) = self.download_active.iter().position(|item| *item == index) else {
            return DownloadSettlement::Ignored;
        };
        self.download_active.remove(position);
        if self.settled.get(index).copied().unwrap_or(true) {
            return DownloadSettlement::Ignored;
        }
        if !success {
            self.settled[index] = true;
            return DownloadSettlement::Failed;
        }
        self.install_ready.push(index);
        DownloadSettlement::Ready
    }

    pub fn take_install(&mut self) -> Option<usize> {
        if self.halt_installs || self.install_busy {
            return None;
        }
        let choice = self
            .install_ready
            .iter()
            .copied()
            .min_by_key(|index| (self.packages[*index].rank, *index))?;
        self.install_ready.retain(|index| *index != choice);
        self.install_busy = true;
        Some(choice)
    }

    pub fn complete_install(
        &mut self,
        index: usize,
        success: bool,
        reboot_required: bool,
    ) -> Option<RebootHalt> {
        self.install_busy = false;
        if index < self.settled.len() {
            self.settled[index] = true;
        }
        if !(success && reboot_required) {
            return None;
        }
        self.halt_installs = true;
        self.download_waiting.clear();
        self.install_ready.clear();
        let remaining: Vec<usize> = self
            .settled
            .iter()
            .enumerate()
            .filter_map(|(index, settled)| if *settled { None } else { Some(index) })
            .collect();
        for index in &remaining {
            self.settled[*index] = true;
        }
        Some(RebootHalt {
            message: INSTALL_STOPPED_FOR_REBOOT,
            remaining,
        })
    }

    pub fn can_dispatch(&self) -> bool {
        if self.halt_installs {
            return false;
        }
        let download_room = self.download_active.len() < DOWNLOAD_SLOT_LIMIT
            && !self.download_waiting.is_empty();
        let install_room = !self.install_busy && !self.install_ready.is_empty();
        download_room || install_room
    }

    pub fn work_pending(&self) -> bool {
        !self.download_active.is_empty() || self.install_busy || self.can_dispatch()
    }

    pub fn reboot_required(&self) -> bool {
        self.halt_installs
    }
}

fn status_still_open(status: &DriverInstallStatus) -> bool {
    match status {
        DriverInstallStatus::Queued
        | DriverInstallStatus::Downloading
        | DriverInstallStatus::Installing => true,
        DriverInstallStatus::Success | DriverInstallStatus::Failed(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DownloadSettlement, Finish, InstallRank, Job, PlannedPackage, QueueItem, Session,
        WorkQueues, apply, install_rank, plan,
    };
    use crate::wua::{DriverInstallStatus, InstallEvent};

    const INTEL_TITLES: [&str; 3] = [
        "Intel Corporation - Display - 32.0.101.6790",
        "Intel Corporation Display Driver Update (32.0.101.7088)",
        "Intel Corporation Extension Driver Update (32.0.101.7088)",
    ];

    fn queued_item(title: &str) -> QueueItem {
        QueueItem {
            update_id: format!("id:{title}"),
            title: title.to_string(),
            status: DriverInstallStatus::Queued,
            message: String::new(),
        }
    }

    fn apply_events(queue: Vec<QueueItem>, events: &[InstallEvent]) -> Session {
        let mut session = Session {
            queue,
            staged: Vec::new(),
            status_text: String::from("installing"),
            finish: None,
        };
        for event in events {
            session = apply(&session, event);
        }
        session
    }

    fn reports_clean_success(session: &Session) -> bool {
        session.finish
            == Some(Finish {
                reboot_required: false,
                clean: true,
            })
            && session.status_text == "All updates processed successfully."
    }

    #[test]
    fn three_selected_drivers_stay_three_jobs_in_selection_order() {
        let selected = vec![
            Job {
                update_id: String::from("id-6790"),
                title: INTEL_TITLES[0].to_string(),
                max_download_size: None,
                driver_class: None,
                superseded_update_ids: Vec::new(),
            },
            Job {
                update_id: String::from("id-7088-display"),
                title: INTEL_TITLES[1].to_string(),
                max_download_size: None,
                driver_class: None,
                superseded_update_ids: Vec::new(),
            },
            Job {
                update_id: String::from("id-7088-extension"),
                title: INTEL_TITLES[2].to_string(),
                max_download_size: None,
                driver_class: None,
                superseded_update_ids: Vec::new(),
            },
        ];

        let jobs = plan(&selected);

        assert_eq!(jobs, selected);
    }

    fn intel_selection(superseded_on_7088: &[&str]) -> Vec<Job> {
        vec![
            Job {
                update_id: String::from("6790"),
                title: INTEL_TITLES[0].to_string(),
                max_download_size: Some(700_000_000),
                driver_class: Some(String::from("Display")),
                superseded_update_ids: Vec::new(),
            },
            Job {
                update_id: String::from("7088"),
                title: INTEL_TITLES[1].to_string(),
                max_download_size: Some(20_000),
                driver_class: Some(String::from("Display")),
                superseded_update_ids: superseded_on_7088
                    .iter()
                    .map(|id| (*id).to_string())
                    .collect(),
            },
            Job {
                update_id: String::from("ext"),
                title: INTEL_TITLES[2].to_string(),
                max_download_size: Some(10_000),
                driver_class: Some(String::from("Extension")),
                superseded_update_ids: Vec::new(),
            },
        ]
    }

    #[test]
    fn plan_drops_a_driver_superseded_by_another_selected_driver() {
        let selected = intel_selection(&["6790"]);

        let jobs = plan(&selected);

        assert_eq!(jobs, vec![selected[1].clone(), selected[2].clone()]);
    }

    #[test]
    fn plan_keeps_every_selected_driver_when_superseded_lists_are_empty() {
        let selected = intel_selection(&[]);

        let jobs = plan(&selected);

        assert_eq!(jobs, selected);
    }

    #[test]
    fn all_finished_without_item_results_is_not_a_clean_success() {
        let session = apply_events(
            INTEL_TITLES.iter().copied().map(queued_item).collect(),
            &[InstallEvent::AllFinished {
                reboot_required: false,
            }],
        );

        let any_queued = session
            .queue
            .iter()
            .any(|item| item.status == DriverInstallStatus::Queued);
        assert!(
            !(reports_clean_success(&session) && any_queued),
            "screenshot invariant: clean success while a row is still Queued"
        );
        assert!(session.queue.iter().all(|item| {
            matches!(
                &item.status,
                DriverInstallStatus::Failed(message) if message == "No item result arrived"
            ) && item.message == "No item result arrived"
        }));
        assert_eq!(session.status_text, "Install did not finish cleanly.");
        assert!(!reports_clean_success(&session));
        assert_eq!(
            session.finish,
            Some(Finish {
                reboot_required: false,
                clean: false,
            })
        );
        assert!(session.staged.is_empty());
    }

    fn finished_ok(index: usize, title: &str, reboot_required: bool) -> Vec<InstallEvent> {
        vec![
            InstallEvent::ItemDownloading {
                index,
                title: title.to_string(),
            },
            InstallEvent::ItemInstalling {
                index,
                title: title.to_string(),
            },
            InstallEvent::ItemFinished {
                index,
                title: title.to_string(),
                success: true,
                reboot_required,
                message: "Installed successfully".to_string(),
            },
        ]
    }

    #[test]
    fn tiny_and_large_payload_updates_both_succeed() {
        let tiny = "Lenovo System Driver Update (2.1.2602.8)";
        let large = "Intel Corporation - Display - 32.0.101.6790 payload 713213 KB";
        let mut events = finished_ok(0, tiny, false);
        events.extend(finished_ok(1, large, false));
        events.push(InstallEvent::AllFinished {
            reboot_required: false,
        });
        let session = apply_events(vec![queued_item(tiny), queued_item(large)], &events);

        assert!(
            session
                .queue
                .iter()
                .all(|item| item.status == DriverInstallStatus::Success)
        );
        assert!(
            session
                .queue
                .iter()
                .all(|item| item.status != DriverInstallStatus::Queued)
        );
        assert_eq!(session.status_text, "All updates processed successfully.");
        assert!(reports_clean_success(&session));
        assert!(session.staged.is_empty());
    }

    #[test]
    fn reboot_required_stages_successful_title() {
        let title = "Intel Corporation - Display - 32.0.101.6790";
        let mut events = finished_ok(0, title, true);
        events.push(InstallEvent::AllFinished {
            reboot_required: true,
        });
        let session = apply_events(vec![queued_item(title)], &events);

        assert!(matches!(
            session.queue[0].status,
            DriverInstallStatus::Success
        ));
        assert_eq!(
            session.status_text,
            "Installation complete. System reboot is recommended."
        );
        assert_eq!(
            session.finish,
            Some(Finish {
                reboot_required: true,
                clean: true,
            })
        );
        assert_eq!(session.staged.len(), 1);
        assert_eq!(session.staged[0].title, title);
    }

    fn ready_queues(items: &[(&str, Option<&str>)]) -> WorkQueues {
        let packages = items
            .iter()
            .map(|(title, class)| PlannedPackage {
                max_download_size: 1,
                rank: install_rank(title, *class),
            })
            .collect();
        let mut queues = WorkQueues::new(packages);
        let started = queues.take_downloads();
        for index in started {
            assert_eq!(
                queues.complete_download(index, true),
                DownloadSettlement::Ready
            );
        }
        queues
    }

    #[test]
    fn five_packages_start_the_three_smallest_downloads() {
        let sizes = [700_000_000_u64, 20_000, 30_000, 500_000_000, 10_000];
        let packages = sizes
            .into_iter()
            .map(|max_download_size| PlannedPackage {
                max_download_size,
                rank: InstallRank::Base,
            })
            .collect();
        let mut queues = WorkQueues::new(packages);
        let started = queues.take_downloads();
        assert_eq!(started, vec![4, 1, 2]);
        assert!(queues.take_downloads().is_empty());
        assert_eq!(queues.complete_download(4, true), DownloadSettlement::Ready);
        assert_eq!(queues.take_downloads(), vec![3]);
        assert_eq!(queues.complete_download(1, false), DownloadSettlement::Failed);
        assert_eq!(queues.take_downloads(), vec![0]);
        assert_eq!(queues.take_install(), Some(4));
    }

    #[test]
    fn equal_download_sizes_keep_plan_order() {
        let packages = [5_u64, 5, 1]
            .into_iter()
            .map(|max_download_size| PlannedPackage {
                max_download_size,
                rank: InstallRank::Base,
            })
            .collect();
        let mut queues = WorkQueues::new(packages);
        assert_eq!(queues.take_downloads(), vec![2, 0, 1]);
    }

    #[test]
    fn ready_extension_waits_behind_ready_display() {
        let display = "Intel Corporation Display Driver Update (32.0.101.7088)";
        let extension = "Intel Corporation Extension Driver Update (32.0.101.7088)";
        let mut queues = ready_queues(&[
            (extension, Some("Extension")),
            (display, Some("Display")),
        ]);
        assert_eq!(queues.take_install(), Some(1));

        let mut class_ranked = ready_queues(&[
            (display, Some("Display EXTENSION")),
            (display, Some("Display")),
        ]);
        assert_eq!(class_ranked.take_install(), Some(1));

        let mut same_rank = ready_queues(&[(display, Some("Display")), (display, Some("Net"))]);
        assert_eq!(same_rank.take_install(), Some(0));
    }

    #[test]
    fn reboot_stops_the_next_ready_install() {
        let mut queues = ready_queues(&[
            ("Intel Corporation Display Driver Update (32.0.101.7088)", Some("Display")),
            ("Intel Corporation Display Driver Update (32.0.101.6790)", Some("Display")),
        ]);
        assert_eq!(queues.take_install(), Some(0));
        let halt = queues
            .complete_install(0, true, true)
            .expect("reboot stops the queue");
        assert_eq!(halt.message, "Install stopped because a reboot is required");
        assert_eq!(halt.remaining, vec![1]);
        assert_eq!(queues.take_install(), None);
        assert!(queues.reboot_required());
    }

    #[test]
    fn failed_download_does_not_enter_the_install_queue() {
        let mut queues = WorkQueues::new(vec![
            PlannedPackage {
                max_download_size: 10,
                rank: InstallRank::Base,
            },
            PlannedPackage {
                max_download_size: 20,
                rank: InstallRank::Base,
            },
        ]);
        let started = queues.take_downloads();
        assert_eq!(
            queues.complete_download(started[0], false),
            DownloadSettlement::Failed
        );
        assert_eq!(
            queues.complete_download(started[1], true),
            DownloadSettlement::Ready
        );
        assert_eq!(queues.take_install(), Some(started[1]));
    }
}
