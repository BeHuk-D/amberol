// SPDX-FileCopyrightText: 2026  Dmitriy
// SPDX-License-Identifier: GPL-3.0-or-later

mod manager;
mod ytdlp;

pub use manager::DownloadManager;
pub use ytdlp::normalize_url;
