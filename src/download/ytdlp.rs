// SPDX-FileCopyrightText: 2026  Dmitriy
// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

use async_channel::Sender;
use gtk::glib;

pub const PROGRESS_PREFIX: &str = "AMBEROL-PROGRESS:";
pub const TITLE_PREFIX: &str = "AMBEROL-TITLE:";
pub const PATH_PREFIX: &str = "AMBEROL-PATH:";

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    pub fraction: f64,
    pub speed: f64,
    pub eta: u64,
}

#[derive(Debug)]
pub enum DownloadEvent {
    Title(String),
    Progress(Progress),
    Processing,
    Path(PathBuf),
    Exited { code: Option<i32>, error: String },
}

pub fn find_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AMBEROL_YTDLP") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    glib::find_program_in_path("yt-dlp").or_else(|| {
        let path = PathBuf::from("/app/bin/yt-dlp");
        path.is_file().then_some(path)
    })
}

pub fn normalize_url(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return None;
    }

    let candidate = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };

    let uri = glib::Uri::parse(&candidate, glib::UriFlags::NONE).ok()?;
    let scheme = uri.scheme().to_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let host = uri.host()?;
    if host.is_empty() {
        return None;
    }

    Some(candidate)
}

pub fn build_args(url: &str, destination: &Path, temp: &Path) -> Vec<String> {
    let progress_template = format!(
        "download:{PROGRESS_PREFIX}%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s"
    );

    vec![
        "--no-playlist".to_string(),
        "--newline".to_string(),
        "--no-colors".to_string(),
        "--progress".to_string(),
        "--progress-template".to_string(),
        progress_template,
        "--print".to_string(),
        format!("before_dl:{TITLE_PREFIX}%(title)s"),
        "--print".to_string(),
        format!("after_move:{PATH_PREFIX}%(filepath)s"),
        "--paths".to_string(),
        format!("home:{}", destination.display()),
        "--paths".to_string(),
        format!("temp:{}", temp.display()),
        "-o".to_string(),
        "%(title)s [%(id)s].%(ext)s".to_string(),
        "-x".to_string(),
        "--audio-format".to_string(),
        "opus".to_string(),
        "--audio-quality".to_string(),
        "0".to_string(),
        "--embed-metadata".to_string(),
        "--embed-thumbnail".to_string(),
        "--convert-thumbnails".to_string(),
        "jpg".to_string(),
        "--no-mtime".to_string(),
        "--retries".to_string(),
        "3".to_string(),
        "--fragment-retries".to_string(),
        "3".to_string(),
        "--".to_string(),
        url.to_string(),
    ]
}

pub fn spawn(
    binary: &Path,
    url: &str,
    destination: &Path,
    temp: &Path,
    sender: Sender<DownloadEvent>,
) -> Result<Arc<Mutex<Option<Child>>>, String> {
    std::fs::create_dir_all(temp).map_err(|err| err.to_string())?;

    let mut command = Command::new(binary);
    command
        .args(build_args(url, destination, temp))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command.spawn().map_err(|err| err.to_string())?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Unable to read yt-dlp output".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Unable to read yt-dlp errors".to_string())?;

    let child_slot = Arc::new(Mutex::new(Some(child)));

    let stderr_thread = std::thread::spawn(move || read_stderr(stderr));

    let stdout_slot = child_slot.clone();
    std::thread::spawn(move || {
        read_stdout(stdout, &sender);

        let error = stderr_thread
            .join()
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default();

        let code = stdout_slot
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
            .and_then(|mut child| child.wait().ok())
            .and_then(|status| status.code());

        let _ = sender.send_blocking(DownloadEvent::Exited { code, error });
    });

    Ok(child_slot)
}

pub fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }

    let _ = child.kill();
}

fn read_stdout(stdout: ChildStdout, sender: &Sender<DownloadEvent>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else {
            continue;
        };

        if let Some(title) = line.strip_prefix(TITLE_PREFIX) {
            let _ = sender.send_blocking(DownloadEvent::Title(title.trim().to_string()));
        } else if let Some(progress) = parse_progress(&line) {
            let _ = sender.send_blocking(DownloadEvent::Progress(progress));
        } else if let Some(path) = line.strip_prefix(PATH_PREFIX) {
            let _ = sender.send_blocking(DownloadEvent::Path(PathBuf::from(path.trim())));
        } else if is_processing_line(&line) {
            let _ = sender.send_blocking(DownloadEvent::Processing);
        } else {
            log::debug!("yt-dlp: {line}");
        }
    }
}

fn read_stderr(stderr: ChildStderr) -> Vec<String> {
    BufReader::new(stderr)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .collect()
}

pub fn parse_progress(line: &str) -> Option<Progress> {
    let mut fields = line.strip_prefix(PROGRESS_PREFIX)?.split('|');

    let downloaded = parse_u64(fields.next()?)?;
    let total = parse_u64(fields.next()?).filter(|value| *value > 0);
    let estimate = parse_u64(fields.next()?).filter(|value| *value > 0);
    let speed = fields.next().and_then(parse_f64).unwrap_or(0.0);
    let eta = fields.next().and_then(parse_u64).unwrap_or(0);

    let total = total.or(estimate)?;
    let fraction = (downloaded as f64 / total as f64).clamp(0.0, 1.0);

    Some(Progress {
        downloaded,
        total,
        fraction,
        speed,
        eta,
    })
}

fn parse_u64(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || value == "NA" {
        None
    } else {
        value.parse().ok()
    }
}

fn parse_f64(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() || value == "NA" {
        None
    } else {
        value.parse().ok()
    }
}

pub fn is_processing_line(line: &str) -> bool {
    [
        "[ExtractAudio]",
        "[Metadata]",
        "[EmbedThumbnail]",
        "[ThumbnailsConvertor]",
        "[MoveFiles]",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_links() {
        assert_eq!(
            normalize_url("  https://www.youtube.com/watch?v=abc  ").as_deref(),
            Some("https://www.youtube.com/watch?v=abc")
        );
        assert_eq!(
            normalize_url("youtube.com/watch?v=abc").as_deref(),
            Some("https://youtube.com/watch?v=abc")
        );
        assert_eq!(normalize_url("ftp://example.com/song.mp3"), None);
        assert_eq!(normalize_url("javascript:alert(1)"), None);
        assert_eq!(normalize_url("not a link"), None);
        assert_eq!(normalize_url(""), None);
    }

    #[test]
    fn parses_progress() {
        let progress = parse_progress("AMBEROL-PROGRESS:500|1000|NA|2048.5|10").unwrap();
        assert_eq!(progress.downloaded, 500);
        assert_eq!(progress.total, 1000);
        assert_eq!(progress.fraction, 0.5);
        assert_eq!(progress.speed, 2048.5);
        assert_eq!(progress.eta, 10);

        let progress = parse_progress("AMBEROL-PROGRESS:250|NA|1000|NA|NA").unwrap();
        assert_eq!(progress.total, 1000);
        assert_eq!(progress.fraction, 0.25);
        assert_eq!(progress.speed, 0.0);
        assert_eq!(progress.eta, 0);

        assert_eq!(parse_progress("AMBEROL-PROGRESS:100|NA|NA|NA|NA"), None);
        assert_eq!(parse_progress("[download] 50%"), None);
    }

    #[test]
    fn builds_arguments() {
        let args = build_args(
            "https://example.com/video",
            Path::new("/music/Amberol"),
            Path::new("/cache/job"),
        );

        assert!(args.contains(&"--no-playlist".to_string()));
        assert!(args.contains(&"-x".to_string()));
        assert!(args.contains(&"--embed-metadata".to_string()));
        assert!(args.contains(&"--embed-thumbnail".to_string()));
        assert!(args.contains(&"--audio-format".to_string()));
        assert!(args.contains(&"home:/music/Amberol".to_string()));
        assert!(args.contains(&"temp:/cache/job".to_string()));
        assert_eq!(args.last().unwrap(), "https://example.com/video");
        assert_eq!(args[args.len() - 2], "--");
    }

    #[test]
    fn detects_processing() {
        assert!(is_processing_line("[ExtractAudio] Destination: song.opus"));
        assert!(is_processing_line("[Metadata] Adding metadata"));
        assert!(!is_processing_line("[download] 42%"));
    }
}
