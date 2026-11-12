// SPDX-FileCopyrightText: 2026  Dmitriy
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::time::Duration;

use glib::{clone, subclass::Signal};
use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use once_cell::sync::Lazy;

use crate::{
    download::DownloadManager,
    i18n::{i18n, i18n_k},
    utils,
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/bassi/Amberol/download-bar.ui")]
    pub struct DownloadBar {
        #[template_child]
        pub revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub download_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub stats_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,

        pub indeterminate: Cell<bool>,
        pub pulse_id: RefCell<Option<glib::SourceId>>,
        pub hide_id: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DownloadBar {
        const NAME: &'static str = "AmberolDownloadBar";
        type Type = super::DownloadBar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("downloadbar");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DownloadBar {
        fn signals() -> &'static [Signal] {
            static SIGNALS: Lazy<Vec<Signal>> =
                Lazy::new(|| vec![Signal::builder("cancel-requested").build()]);
            SIGNALS.as_ref()
        }

        fn constructed(&self) {
            self.parent_constructed();

            let bar = self.obj();
            self.cancel_button.connect_clicked(clone!(
                #[weak]
                bar,
                move |_| {
                    bar.emit_by_name::<()>("cancel-requested", &[]);
                }
            ));
        }

        fn dispose(&self) {
            if let Some(id) = self.pulse_id.borrow_mut().take() {
                id.remove();
            }
            if let Some(id) = self.hide_id.borrow_mut().take() {
                id.remove();
            }

            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for DownloadBar {}
}

glib::wrapper! {
    pub struct DownloadBar(ObjectSubclass<imp::DownloadBar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DownloadBar {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DownloadBar {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&self, manager: &DownloadManager) {
        let imp = self.imp();
        let title = manager.title();
        let queued = manager.queued();

        match manager.status().as_str() {
            "resolving" => {
                self.reveal();
                self.set_error(false);
                self.set_indeterminate(true);
                self.set_dismiss_mode(false);
                imp.download_icon
                    .set_icon_name(Some("folder-download-symbolic"));
                imp.title_label.set_label(&i18n("Getting information…"));
                self.clear_stats();
            }
            "downloading" => {
                self.reveal();
                self.set_error(false);
                self.set_indeterminate(false);
                self.set_dismiss_mode(false);
                imp.download_icon
                    .set_icon_name(Some("folder-download-symbolic"));

                if title.is_empty() {
                    imp.title_label.set_label(&i18n("Downloading…"));
                } else {
                    imp.title_label.set_label(&title);
                }

                let stats =
                    format_stats(manager.progress(), manager.speed(), manager.eta(), queued);
                imp.stats_label.set_label(&stats);
                imp.stats_label.set_tooltip_text(None);
                imp.progress_bar.set_fraction(manager.progress());
            }
            "processing" => {
                self.reveal();
                self.set_error(false);
                self.set_indeterminate(true);
                self.set_dismiss_mode(false);
                imp.download_icon
                    .set_icon_name(Some("folder-download-symbolic"));
                imp.title_label.set_label(&i18n("Processing…"));
                self.clear_stats();
            }
            "success" => {
                self.reveal();
                self.set_error(false);
                self.set_indeterminate(false);
                self.set_dismiss_mode(true);
                imp.download_icon
                    .set_icon_name(Some("object-select-symbolic"));

                if title.is_empty() {
                    imp.title_label.set_label(&i18n("Download complete"));
                } else {
                    let label = i18n_k(
                        // Translators: `{title}` must be left untranslated;
                        // it will expand to the title of the downloaded song
                        "Added “{title}”",
                        &[("title", &title)],
                    );
                    imp.title_label.set_label(&label);
                }

                self.clear_stats();
                imp.progress_bar.set_fraction(1.0);
                self.schedule_hide();
            }
            "error" => {
                self.reveal();
                self.set_error(true);
                self.set_indeterminate(false);
                self.set_dismiss_mode(true);
                imp.download_icon
                    .set_icon_name(Some("dialog-warning-symbolic"));
                imp.title_label.set_label(&i18n("Download failed"));

                let message = manager.error_message();
                imp.stats_label.set_label(&message);
                imp.stats_label.set_tooltip_text(Some(&message));
                imp.progress_bar.set_fraction(0.0);
            }
            _ => self.conceal(),
        }
    }

    fn reveal(&self) {
        self.cancel_hide();
        self.imp().revealer.set_reveal_child(true);
    }

    fn conceal(&self) {
        self.cancel_hide();
        self.set_indeterminate(false);
        self.imp().revealer.set_reveal_child(false);
    }

    fn clear_stats(&self) {
        let stats = self.imp().stats_label.get();
        stats.set_label("");
        stats.set_tooltip_text(None);
    }

    fn set_error(&self, error: bool) {
        if error {
            self.add_css_class("error");
        } else {
            self.remove_css_class("error");
        }
    }

    fn set_indeterminate(&self, indeterminate: bool) {
        let imp = self.imp();
        imp.indeterminate.set(indeterminate);

        if indeterminate {
            imp.progress_bar.set_fraction(0.0);
            imp.progress_bar.set_pulse_step(0.08);
            self.start_pulse();
        } else if let Some(id) = imp.pulse_id.borrow_mut().take() {
            id.remove();
        }
    }

    fn start_pulse(&self) {
        if self.imp().pulse_id.borrow().is_some() {
            return;
        }

        let bar = self.downgrade();
        let id = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(bar) = bar.upgrade() else {
                return glib::ControlFlow::Break;
            };

            if bar.imp().indeterminate.get() {
                bar.imp().progress_bar.pulse();
                glib::ControlFlow::Continue
            } else {
                bar.imp().pulse_id.borrow_mut().take();
                glib::ControlFlow::Break
            }
        });
        self.imp().pulse_id.replace(Some(id));
    }

    fn schedule_hide(&self) {
        self.cancel_hide();

        let id = glib::timeout_add_seconds_local_once(
            2,
            clone!(
                #[weak(rename_to = bar)]
                self,
                move || {
                    bar.imp().hide_id.borrow_mut().take();
                    bar.conceal();
                }
            ),
        );
        self.imp().hide_id.replace(Some(id));
    }

    fn cancel_hide(&self) {
        if let Some(id) = self.imp().hide_id.borrow_mut().take() {
            id.remove();
        }
    }

    fn set_dismiss_mode(&self, dismiss: bool) {
        let label = if dismiss {
            i18n("Dismiss")
        } else {
            i18n("Cancel Download")
        };
        let button = self.imp().cancel_button.get();
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    }
}

fn format_stats(progress: f64, speed: f64, eta: u64, queued: u32) -> String {
    let mut stats = format!("{:.0}%", progress * 100.0);

    if speed > 0.0 {
        stats.push_str(" · ");
        stats.push_str(&format_speed(speed));
    }

    if eta > 0 {
        stats.push_str(" · ");
        stats.push_str(&utils::format_time(eta as i64));
    }

    if queued > 0 {
        stats.push_str(&format!(" · +{queued}"));
    }

    stats
}

fn format_speed(bytes_per_second: f64) -> String {
    let mut value = bytes_per_second;
    let mut unit = "B/s";

    for next in ["KB/s", "MB/s", "GB/s"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }

    format!("{value:.1} {unit}")
}
