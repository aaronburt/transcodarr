use crate::config::{Config, Profile};
use crate::encode::Hardware;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{SystemTime, UNIX_EPOCH};

const RECENT: usize = 20;

pub struct App {
    pub config_path: PathBuf,
    config: RwLock<Arc<Config>>,
    loaded_mtime: Mutex<Option<SystemTime>>,
    pub status: Mutex<Status>,
    wake: mpsc::Sender<()>,
    log: Mutex<()>,
}

impl App {
    pub fn new(config_path: &Path, cfg: Config, wake: mpsc::Sender<()>) -> Arc<App> {
        let mut status = Status::default();
        status.apply(&cfg, config_path);
        Arc::new(App {
            config_path: config_path.to_path_buf(),
            loaded_mtime: Mutex::new(mtime(config_path)),
            config: RwLock::new(Arc::new(cfg)),
            status: Mutex::new(status),
            wake,
            log: Mutex::new(()),
        })
    }

    pub fn log(&self, event: &str, fields: Value) {
        let Some(path) = self.config().log_file.clone() else { return };
        let mut line = json!({ "time": unix_now(), "event": event });
        if let (Some(line), Value::Object(fields)) = (line.as_object_mut(), fields) {
            line.extend(fields);
        }
        let _guard = self.log.lock().unwrap();
        let written =
            OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = written {
            eprintln!("can't write {}: {e}", path.display());
        }
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.read().unwrap().clone()
    }

    pub fn wake(&self) {
        let _ = self.wake.send(());
    }

    pub fn reload_if_changed(&self) {
        if mtime(&self.config_path) != *self.loaded_mtime.lock().unwrap()
            && let Err(e) = self.reload()
        {
            eprintln!("config not reloaded, keeping the old one: {e}");
        }
    }

    pub fn reload(&self) -> Result<(), String> {
        *self.loaded_mtime.lock().unwrap() = mtime(&self.config_path);
        let cfg = Config::load(&self.config_path).map_err(|e| {
            self.status.lock().unwrap().config_error = Some(e.to_string());
            self.log("config_error", json!({ "error": e.to_string() }));
            e.to_string()
        })?;
        self.status.lock().unwrap().apply(&cfg, &self.config_path);
        *self.config.write().unwrap() = Arc::new(cfg);
        eprintln!("config reloaded");
        self.log("config_reloaded", json!({}));
        self.wake();
        Ok(())
    }

    pub fn save(&self, text: &str) -> Result<(), String> {
        Config::parse(text).map_err(|e| e.to_string())?;
        let tmp = self.config_path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, &self.config_path))
            .map_err(|e| format!("can't write {}: {e}", self.config_path.display()))?;
        self.reload()
    }

    pub fn retry(&self, id: u64) -> Result<(), String> {
        let mut s = self.status.lock().unwrap();
        let f = s.recent.iter_mut().find(|f| f.id == id && f.result == "failed").ok_or("nothing to retry")?;
        let (Some(from), Some(to)) = (&f.failed_path, &f.source_path) else {
            return Err("nothing to retry".into());
        };
        if to.exists() {
            return Err(format!("{} is already back", to.display()));
        }
        std::fs::rename(from, to).map_err(|e| format!("can't move it back: {e}"))?;
        if let Some(log) = &f.log {
            let _ = std::fs::remove_file(log);
        }
        f.result = "retried";
        f.note = Some("moved back to be encoded again".into());
        f.log = None;
        let file = f.file.clone();
        drop(s);
        self.log("retry_requested", json!({ "file": file }));
        self.wake();
        Ok(())
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[derive(Serialize, Default)]
pub struct Status {
    pub config_path: String,
    pub config_error: Option<String>,
    pub input: String,
    pub scan_secs: u64,
    pub idle_secs: u64,
    pub jobs: usize,
    pub profile: String,
    pub folders: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, Profile>,
    pub encoders: BTreeMap<String, String>,
    pub hardware: Hardware,
    pub current: Vec<Current>,
    pub waiting: Vec<Waiting>,
    pub recent: VecDeque<Finished>,
}

#[derive(Serialize)]
pub struct Current {
    pub file: String,
    pub profile: String,
    pub from_ext: String,
    pub to_ext: String,
    pub size_in: u64,
    pub elapsed_secs: u64,
    pub percent: f64,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub fps: String,
    pub speed: String,
}

#[derive(Serialize)]
pub struct Waiting {
    pub file: String,
    pub state: &'static str,
}

#[derive(Serialize)]
pub struct Finished {
    pub id: u64,
    pub result: &'static str,
    pub file: String,
    pub profile: String,
    pub size_in: u64,
    pub size_out: Option<u64>,
    pub took_secs: u64,
    pub finished_at: u64,
    pub note: Option<String>,
    pub log: Option<String>,
    #[serde(skip)]
    pub failed_path: Option<PathBuf>,
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

impl Status {
    fn apply(&mut self, cfg: &Config, path: &Path) {
        self.config_path = path.display().to_string();
        self.config_error = None;
        self.input = cfg.input.display().to_string();
        self.scan_secs = cfg.scan_secs;
        self.idle_secs = cfg.idle_secs;
        self.jobs = cfg.jobs;
        self.profile = cfg.profile.clone();
        self.folders = cfg.folders.clone();
        self.profiles = cfg.profiles.clone();
        self.encoders = cfg.profiles.iter().map(|(name, p)| (name.clone(), p.encoder().to_owned())).collect();
    }

    pub fn push_finished(&mut self, mut f: Finished) {
        f.id = self.recent.front().map_or(1, |r| r.id + 1);
        self.recent.push_front(f);
        self.recent.truncate(RECENT);
    }
}
