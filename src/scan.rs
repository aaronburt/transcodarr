use crate::config::{Config, Watch};
use crate::encode::{self, Destination, Error, Stamp};
use crate::status::{App, Current, Finished, Waiting, unix_now};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const PROCESSING: &str = ".transcodarr-processing";

#[derive(Clone, Copy)]
struct Observation {
    stamp: Stamp,
    since: Instant,
}

impl Observation {
    fn update(previous: Option<&Self>, stamp: Stamp, now: Instant) -> Self {
        previous.copied().filter(|p| p.stamp == stamp).unwrap_or(Self { stamp, since: now })
    }

    fn ready(&self, now: Instant, quiet: Duration) -> bool {
        now.duration_since(self.since) >= quiet
    }
}

pub fn run(app: &Arc<App>, wake: mpsc::Receiver<()>) -> crate::Result<()> {
    encode::check_tools(&app.config())?;
    let mut observations = HashMap::new();
    let mut watch_locks = HashMap::new();
    let retries: Arc<Mutex<HashMap<PathBuf, Instant>>> = Arc::default();
    let running: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    let cfg = app.config();
    let hardware = encode::detect_hardware(&cfg);
    log_settings(&cfg, &hardware);
    app.status.lock().unwrap().hardware = hardware.clone();
    let watches: Vec<_> = cfg
        .watches()
        .iter()
        .map(|w| json!({ "dir": w.dir, "profile": w.profile, "output": w.output, "done": w.done, "failed": w.failed }))
        .collect();
    app.log(
        "started",
        json!({
            "watches": watches,
            "jobs": cfg.jobs,
            "scan_secs": cfg.scan_secs,
            "idle_secs": cfg.idle_secs,
            "encoders": app.status.lock().unwrap().encoders,
            "hardware": hardware,
        }),
    );

    loop {
        app.reload_if_changed();
        let cfg = app.config();
        let quiet = Duration::from_secs(cfg.scan_secs);
        let retry_delay = Duration::from_secs(cfg.idle_secs.max(1));
        let mut seen = HashMap::new();
        let mut waiting = Vec::new();
        let mut ready = Vec::new();
        retries.lock().unwrap().retain(|_, since| since.elapsed() < retry_delay);

        for w in cfg.watches() {
            let staging = w.dir.join(PROCESSING);
            if !w.dir.is_dir() {
                eprintln!("can't read {}", w.dir.display());
                continue;
            }
            if !watch_locks.contains_key(&w.dir) {
                fs::create_dir_all(&staging)?;
                watch_locks.insert(w.dir.clone(), encode::lock(&staging.join(".watch.lock"))?);
            }
            let claimed = list_videos(&staging, None, &[]);
            let claimed_names: HashSet<_> =
                claimed.iter().filter_map(|(p, _)| p.file_name().map(ToOwned::to_owned)).collect();
            let inbox = list_videos(&w.dir, Some(&cfg.extensions), &cfg.ignore);
            for (path, stamp) in claimed.into_iter().chain(inbox) {
                let name = path.file_name().unwrap();
                let original = w.dir.join(name);
                let is_claimed = path != original;
                let now = Instant::now();
                let observation = Observation::update(observations.get(&path), stamp, now);
                seen.insert(path.clone(), observation);
                if is_claimed && running.lock().unwrap().contains(&original) {
                    continue;
                }
                let state = if !is_claimed && claimed_names.contains(name) {
                    "reserved"
                } else if !is_claimed && encode::output_path(&cfg, &original, &w.output, &w.profile).exists()
                {
                    "exists"
                } else if retries.lock().unwrap().contains_key(&original) {
                    "retry"
                } else if observation.ready(now, quiet) {
                    "ready"
                } else {
                    "copying"
                };
                if state == "ready" {
                    ready.push((w.clone(), path, original, stamp));
                } else {
                    waiting.push(Waiting { file: display_name(&cfg, &original), state });
                }
            }
        }
        observations = seen;

        let mut ready = ready.into_iter();
        while running.lock().unwrap().len() < cfg.jobs {
            let Some((w, path, original, stamp)) = ready.next() else { break };
            running.lock().unwrap().insert(original.clone());
            let (app, cfg, running, retries) = (app.clone(), cfg.clone(), running.clone(), retries.clone());
            std::thread::spawn(move || {
                if let Err(e) = process(&cfg, &w, &path, &original, stamp, &app) {
                    eprintln!("leaving {} queued: {e}", original.display());
                    app.log("queued_for_retry", json!({ "file": original, "error": e.to_string() }));
                    retries.lock().unwrap().insert(original.clone(), Instant::now());
                }
                let file = display_name(&cfg, &original);
                app.status.lock().unwrap().current.retain(|c| c.file != file);
                running.lock().unwrap().remove(&original);
                app.wake();
            });
        }
        waiting.extend(
            ready.map(|(_, _, original, _)| Waiting { file: display_name(&cfg, &original), state: "ready" }),
        );

        let active = waiting.iter().any(|w| matches!(w.state, "copying" | "retry"));
        app.status.lock().unwrap().waiting = waiting;
        let _ = wake.recv_timeout(if active { quiet.min(retry_delay) } else { retry_delay });
    }
}

fn log_settings(cfg: &Config, hardware: &encode::Hardware) {
    for w in cfg.watches() {
        eprintln!(
            "watching {} with profile {} ({})",
            w.dir.display(),
            w.profile,
            cfg.profiles[&w.profile].encoder()
        );
    }
    match hardware.gpus.as_slice() {
        [] => eprintln!("GPU: none detected"),
        gpus => eprintln!("GPU: {}", gpus.join(", ")),
    }
    if !hardware.encoders.is_empty() {
        eprintln!("ffmpeg hardware encoders: {}", hardware.encoders.join(", "));
    }
    eprintln!(
        "files must be unchanged for {}s; rescanning every {}s when idle; {} job(s) at once",
        cfg.scan_secs, cfg.idle_secs, cfg.jobs
    );
}

enum Completed {
    Skipped(String),
    Encoded(Option<&'static str>),
}

fn process(
    cfg: &Config,
    w: &Watch,
    path: &Path,
    original: &Path,
    stamp: Stamp,
    app: &App,
) -> Result<(), Error> {
    let status = &app.status;
    let profile = &cfg.profiles[&w.profile];
    let dest = Destination::reserve(cfg, original, &w.output, &w.profile)?;
    let claimed = w.dir.join(PROCESSING).join(original.file_name().unwrap());
    let started = Instant::now();
    let file = display_name(cfg, original);
    let mut finished = Finished {
        file: file.clone(),
        profile: w.profile.clone(),
        size_in: stamp.size,
        result: "done",
        size_out: None,
        note: None,
        log: None,
        took_secs: 0,
        finished_at: 0,
        id: 0,
        failed_path: None,
        source_path: None,
        vmaf: None,
    };
    if path != claimed {
        dest.check_available()?;
        if claimed.try_exists()? {
            return Err(Error::Retry("an earlier input with this name is still queued".into()));
        }
        fs::rename(path, &claimed).map_err(|e| Error::Retry(format!("can't claim {file}: {e}")))?;
    }
    if Stamp::from(fs::metadata(&claimed)?) != stamp {
        return Err(Error::Retry("input changed before it could be claimed".into()));
    }

    let mut ffmpeg_output = None;
    let mut vmaf = None;
    let outcome = if dest.path.try_exists()? {
        Ok(Completed::Skipped("output already exists, kept the interrupted job's original".into()))
    } else {
        encode::probe(&cfg.ffprobe, &claimed, profile.ignore_errors).and_then(|source| {
            if let Some(codec) = source
                .video_codec()
                .filter(|name| profile.skip_codecs.iter().any(|c| c.eq_ignore_ascii_case(name)))
            {
                return Ok(Completed::Skipped(format!("already {codec}")));
            }
            eprintln!("encode {file} with {}", w.profile);
            app.log(
                "encode_started",
                json!({
                    "file": file,
                    "input": original,
                    "output": dest.path,
                    "profile": w.profile,
                    "input_args": profile.input_args,
                    "args": profile.args.iter().cloned().chain(source.audio_args(profile)).collect::<Vec<_>>(),
                    "source": source,
                }),
            );
            status.lock().unwrap().current.push(Current {
                file: file.clone(),
                profile: w.profile.clone(),
                from_ext: ext(original),
                to_ext: profile.ext.clone(),
                size_in: stamp.size,
                elapsed_secs: 0,
                percent: 0.0,
                position_secs: 0.0,
                duration_secs: 0.0,
                fps: String::new(),
                speed: String::new(),
                stage: "encoding",
            });
            dest.encode(cfg, &claimed, &w.profile, &source, |p| {
                if p.vmaf.is_some() {
                    vmaf = p.vmaf;
                } else if let Some(c) = status.lock().unwrap().current.iter_mut().find(|c| c.file == file) {
                    c.elapsed_secs = started.elapsed().as_secs();
                    c.percent = p.percent;
                    c.position_secs = p.position_secs;
                    c.duration_secs = p.duration_secs;
                    c.fps.clone_from(&p.fps);
                    c.speed.clone_from(&p.speed);
                    c.stage = p.stage;
                }
            })
            .map(Completed::Encoded)
        })
    };

    if Stamp::from(fs::metadata(&claimed)?) != stamp {
        return Err(Error::Retry("input changed during processing".into()));
    }
    finished.vmaf = vmaf;
    match outcome {
        Ok(Completed::Skipped(reason)) => {
            move_to(&claimed, &w.done)?;
            finished.result = "skipped";
            finished.note = Some(reason);
        }
        Ok(Completed::Encoded(keep_reason)) => {
            finished.size_out = Some(fs::metadata(&dest.path)?.len());
            if cfg.delete_source && keep_reason.is_none() {
                fs::remove_file(&claimed)?;
            } else {
                move_to(&claimed, &w.done)?;
            }
            finished.note = keep_reason.map(str::to_owned);
        }
        Err(Error::Media(message)) => {
            let moved = move_to(&claimed, &w.failed)?;
            let moved_path = moved.clone();
            let mut log = moved.into_os_string();
            log.push(".log");
            let log = PathBuf::from(log);
            finished.result = "failed";
            finished.failed_path = Some(moved_path.clone());
            finished.source_path = Some(original.to_path_buf());
            finished.note = message.lines().last().map(str::to_owned);
            ffmpeg_output = Some(message.clone());
            match fs::write(&log, &message) {
                Ok(()) => finished.log = Some(log.display().to_string()),
                Err(e) => eprintln!("can't write {}: {e}\n{message}", log.display()),
            }
        }
        Err(e) => return Err(e),
    }
    finished.took_secs = started.elapsed().as_secs();
    finished.finished_at = unix_now();
    eprintln!("{} {file}: {}", finished.result, finished.note.as_deref().unwrap_or("encode complete"));
    let mut event = json!(finished);
    event["input"] = json!(original);
    event["output"] = json!(dest.path);
    event["ffmpeg_output"] = json!(ffmpeg_output);
    app.log("finished", event);
    status.lock().unwrap().push_finished(finished);
    Ok(())
}

fn move_to(path: &Path, dir: &Path) -> Result<PathBuf, Error> {
    fs::create_dir_all(dir)?;
    let _lock = encode::lock(&dir.join(".transcodarr-archive.lock"))?;
    let name = path.file_name().unwrap();
    let mut dest = dir.join(name);
    let mut n = 1;
    while dest.try_exists()? {
        let mut numbered = std::ffi::OsString::from(format!("{n}."));
        numbered.push(name);
        dest = dir.join(numbered);
        n += 1;
    }
    fs::rename(path, &dest)
        .map_err(|e| Error::Retry(format!("can't move {} to {}: {e}", path.display(), dest.display())))?;
    Ok(dest)
}

fn display_name(cfg: &Config, path: &Path) -> String {
    path.strip_prefix(&cfg.input).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

fn list_videos(dir: &Path, extensions: Option<&[String]>, ignore: &[String]) -> Vec<(PathBuf, Stamp)> {
    let Ok(entries) = fs::read_dir(dir) else {
        eprintln!("can't read {}", dir.display());
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| {
            extensions.is_none_or(|allowed| {
                let extension = ext(&e.path());
                allowed.iter().any(|extn| extn.eq_ignore_ascii_case(&extension))
            })
        })
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            !ignore.iter().any(|pat| glob(&pat.to_lowercase(), &name))
        })
        .filter_map(|e| e.metadata().ok().filter(|m| m.is_file()).map(|m| (e.path(), Stamp::from(m))))
        .collect();
    files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    files
}

fn glob(pat: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pat.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            (pi, ni) = (pi + 1, ni + 1);
        } else if pi < p.len() && p[pi] == '*' {
            (star, mark, pi) = (Some(pi), ni, pi + 1);
        } else if let Some(s) = star {
            (pi, mark, ni) = (s + 1, mark + 1, mark + 1);
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

fn ext(p: &Path) -> String {
    p.extension().unwrap_or_default().to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_rescans_cannot_shorten_the_quiet_interval() {
        let file = tempfile::tempfile().unwrap();
        let stamp = Stamp::from(file.metadata().unwrap());
        let start = Instant::now();
        let quiet = Duration::from_secs(10);
        let first = Observation::update(None, stamp, start);
        let early = start + Duration::from_millis(10);
        let second = Observation::update(Some(&first), stamp, early);
        assert!(!second.ready(early, quiet));
        assert!(second.ready(start + quiet, quiet));
        file.set_len(10).unwrap();
        let changed = Observation::update(Some(&second), Stamp::from(file.metadata().unwrap()), early);
        assert!(!changed.ready(start + quiet, quiet));
        assert!(changed.ready(early + quiet, quiet));
    }

    #[test]
    fn glob_patterns() {
        assert!(glob("*.part", "movie.mkv.part"));
        assert!(glob("sample*", "sample-1080p.mkv"));
        assert!(glob("*s01e0?*", "show.s01e05.mkv"));
        assert!(!glob("sample*", "my sample.mkv"));
        assert!(glob("*", ""));
    }
}
