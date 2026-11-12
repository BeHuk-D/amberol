// SPDX-FileCopyrightText: 2026  Dmitriy
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_channel::{Receiver, Sender};
use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::{
    download::ytdlp::{self, DownloadEvent},
    i18n::i18n,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Idle,
    Resolving,
    Downloading,
    Processing,
    Success,
    Error,
    Cancelled,
}

impl DownloadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Resolving => "resolving",
            Self::Downloading => "downloading",
            Self::Processing => "processing",
            Self::Success => "success",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

pub struct ActiveJob {
    temp_dir: PathBuf,
    child: Arc<Mutex<Option<std::process::Child>>>,
    final_path: Option<PathBuf>,
    cancelled: Cell<bool>,
}

mod imp {
    use glib::{
        subclass::Signal, ParamSpec, ParamSpecBoolean, ParamSpecDouble, ParamSpecString,
        ParamSpecUInt, ParamSpecUInt64,
    };
    use once_cell::sync::Lazy;

    use super::*;

    pub struct DownloadManager {
        pub binary: RefCell<Option<PathBuf>>,
        pub destination: PathBuf,
        pub jobs: RefCell<VecDeque<String>>,
        pub active: RefCell<Option<ActiveJob>>,
        pub sender: RefCell<Option<Sender<DownloadEvent>>>,
        pub receiver: RefCell<Option<Receiver<DownloadEvent>>>,
        pub status: Cell<DownloadStatus>,
        pub title: RefCell<String>,
        pub progress: Cell<f64>,
        pub speed: Cell<f64>,
        pub eta: Cell<u64>,
        pub error_message: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DownloadManager {
        const NAME: &'static str = "AmberolDownloadManager";
        type Type = super::DownloadManager;

        fn new() -> Self {
            let (sender, receiver) = async_channel::unbounded();
            let music_dir = glib::user_special_dir(glib::UserDirectory::Music)
                .unwrap_or_else(|| glib::home_dir().join("Music"));

            Self {
                binary: RefCell::new(ytdlp::find_binary()),
                destination: music_dir.join("Amberol"),
                jobs: RefCell::new(VecDeque::new()),
                active: RefCell::new(None),
                sender: RefCell::new(Some(sender)),
                receiver: RefCell::new(Some(receiver)),
                status: Cell::new(DownloadStatus::Idle),
                title: RefCell::new(String::new()),
                progress: Cell::new(0.0),
                speed: Cell::new(0.0),
                eta: Cell::new(0),
                error_message: RefCell::new(String::new()),
            }
        }
    }

    impl ObjectImpl for DownloadManager {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("busy").read_only().build(),
                    ParamSpecString::builder("status").read_only().build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecDouble::builder("progress")
                        .minimum(0.0)
                        .maximum(1.0)
                        .read_only()
                        .build(),
                    ParamSpecDouble::builder("speed").read_only().build(),
                    ParamSpecUInt64::builder("eta").read_only().build(),
                    ParamSpecUInt::builder("queued").read_only().build(),
                    ParamSpecString::builder("error-message")
                        .read_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "busy" => obj.busy().to_value(),
                "status" => obj.status().to_value(),
                "title" => obj.title().to_value(),
                "progress" => obj.progress().to_value(),
                "speed" => obj.speed().to_value(),
                "eta" => obj.eta().to_value(),
                "queued" => obj.queued().to_value(),
                "error-message" => obj.error_message().to_value(),
                _ => unimplemented!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
                vec![
                    Signal::builder("state-changed").build(),
                    Signal::builder("finished")
                        .param_types([String::static_type()])
                        .build(),
                    Signal::builder("failed")
                        .param_types([String::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }
    }
}

glib::wrapper! {
    pub struct DownloadManager(ObjectSubclass<imp::DownloadManager>);
}

impl DownloadManager {
    pub fn new() -> Self {
        let manager = glib::Object::new::<Self>();
        manager.setup_channel();
        manager
    }

    pub fn is_available(&self) -> bool {
        self.binary().is_some()
    }

    pub fn busy(&self) -> bool {
        self.imp().active.borrow().is_some()
    }

    pub fn status(&self) -> String {
        self.imp().status.get().as_str().to_string()
    }

    pub fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }

    pub fn progress(&self) -> f64 {
        self.imp().progress.get()
    }

    pub fn speed(&self) -> f64 {
        self.imp().speed.get()
    }

    pub fn eta(&self) -> u64 {
        self.imp().eta.get()
    }

    pub fn queued(&self) -> u32 {
        self.imp().jobs.borrow().len() as u32
    }

    pub fn error_message(&self) -> String {
        self.imp().error_message.borrow().clone()
    }

    pub fn enqueue(&self, url: &str) -> Result<(), String> {
        if !self.is_available() {
            return Err(i18n("yt-dlp is not installed"));
        }

        let url = ytdlp::normalize_url(url).ok_or_else(|| i18n("Invalid link"))?;
        self.imp().jobs.borrow_mut().push_back(url);

        if self.busy() {
            self.emit_changed();
        } else {
            self.start_next();
        }

        Ok(())
    }

    pub fn cancel_active(&self) {
        let active = self.imp().active.borrow();
        let Some(job) = active.as_ref() else {
            return;
        };

        job.cancelled.set(true);
        if let Ok(mut child) = job.child.lock() {
            if let Some(child) = child.as_mut() {
                ytdlp::terminate(child);
            }
        }

        self.imp().jobs.borrow_mut().clear();
        self.imp().speed.set(0.0);
        self.imp().eta.set(0);
        self.set_status(DownloadStatus::Cancelled);
    }

    pub fn dismiss(&self) {
        if self.busy() || !self.imp().jobs.borrow().is_empty() {
            return;
        }

        self.imp().title.replace(String::new());
        self.imp().progress.set(0.0);
        self.imp().speed.set(0.0);
        self.imp().eta.set(0);
        self.imp().error_message.replace(String::new());
        self.set_status(DownloadStatus::Idle);
    }

    fn binary(&self) -> Option<PathBuf> {
        let mut binary = self.imp().binary.borrow_mut();
        if binary.is_none() {
            *binary = ytdlp::find_binary();
        }
        binary.clone()
    }

    fn setup_channel(&self) {
        let receiver = self.imp().receiver.borrow_mut().take().unwrap();
        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = manager)]
            self,
            async move {
                use futures::prelude::*;

                let mut receiver = std::pin::pin!(receiver);
                while let Some(event) = receiver.next().await {
                    manager.process_event(event);
                }
            }
        ));
    }

    fn process_event(&self, event: DownloadEvent) {
        match event {
            DownloadEvent::Title(title) => {
                self.imp().title.replace(title);
                self.emit_changed();
            }
            DownloadEvent::Progress(progress) => {
                self.imp().progress.set(progress.fraction);
                self.imp().speed.set(progress.speed);
                self.imp().eta.set(progress.eta);
                if self.imp().status.get() == DownloadStatus::Resolving {
                    self.imp().status.set(DownloadStatus::Downloading);
                }
                self.emit_changed();
            }
            DownloadEvent::Processing => {
                self.imp().status.set(DownloadStatus::Processing);
                self.emit_changed();
            }
            DownloadEvent::Path(path) => {
                {
                    let mut active = self.imp().active.borrow_mut();
                    let Some(job) = active.as_mut() else {
                        return;
                    };
                    job.final_path = Some(path);
                }
                self.imp().progress.set(1.0);
                self.emit_changed();
            }
            DownloadEvent::Exited { code, error } => self.finish(code, error),
        }
    }

    fn finish(&self, code: Option<i32>, error: String) {
        let Some(mut job) = self.imp().active.borrow_mut().take() else {
            return;
        };

        let _ = std::fs::remove_dir_all(&job.temp_dir);

        if job.cancelled.get() {
            self.imp().speed.set(0.0);
            self.imp().eta.set(0);
            self.set_status(DownloadStatus::Cancelled);
            self.start_next();
            return;
        }

        if code == Some(0) {
            if let Some(path) = job.final_path.take() {
                let path = path.to_string_lossy().to_string();
                self.imp().progress.set(1.0);
                self.imp().speed.set(0.0);
                self.imp().eta.set(0);
                self.set_status(DownloadStatus::Success);
                self.emit_by_name::<()>("finished", &[&path]);
                self.start_next();
                return;
            }
        }

        let message = if error.trim().is_empty() {
            i18n("Unable to download the song")
        } else {
            error.trim().to_string()
        };

        self.imp().error_message.replace(message.clone());
        self.imp().speed.set(0.0);
        self.imp().eta.set(0);
        self.set_status(DownloadStatus::Error);
        self.emit_by_name::<()>("failed", &[&message]);
        self.start_next();
    }

    fn start_next(&self) {
        let Some(url) = self.imp().jobs.borrow_mut().pop_front() else {
            self.emit_changed();
            return;
        };

        let Some(binary) = self.binary() else {
            let message = i18n("yt-dlp is not installed");
            self.imp().error_message.replace(message.clone());
            self.set_status(DownloadStatus::Error);
            self.emit_by_name::<()>("failed", &[&message]);
            self.start_next();
            return;
        };

        let destination = self.imp().destination.clone();
        if let Err(err) = std::fs::create_dir_all(&destination) {
            let message = err.to_string();
            self.imp().error_message.replace(message.clone());
            self.set_status(DownloadStatus::Error);
            self.emit_by_name::<()>("failed", &[&message]);
            self.start_next();
            return;
        }

        let temp_dir = glib::user_cache_dir()
            .join("amberol")
            .join("downloads")
            .join(glib::uuid_string_random().as_str());
        let sender = self.imp().sender.borrow().clone().unwrap();

        match ytdlp::spawn(&binary, &url, &destination, &temp_dir, sender) {
            Ok(child) => {
                self.imp().active.replace(Some(ActiveJob {
                    temp_dir,
                    child,
                    final_path: None,
                    cancelled: Cell::new(false),
                }));
                self.imp().title.replace(String::new());
                self.imp().progress.set(0.0);
                self.imp().speed.set(0.0);
                self.imp().eta.set(0);
                self.imp().error_message.replace(String::new());
                self.set_status(DownloadStatus::Resolving);
            }
            Err(err) => {
                let _ = std::fs::remove_dir_all(&temp_dir);
                self.imp().error_message.replace(err.clone());
                self.set_status(DownloadStatus::Error);
                self.emit_by_name::<()>("failed", &[&err]);
                self.start_next();
            }
        }
    }

    fn set_status(&self, status: DownloadStatus) {
        self.imp().status.set(status);
        self.emit_changed();
    }

    fn emit_changed(&self) {
        for property in [
            "busy",
            "status",
            "title",
            "progress",
            "speed",
            "eta",
            "queued",
            "error-message",
        ] {
            self.notify(property);
        }
        self.emit_by_name::<()>("state-changed", &[]);
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}
