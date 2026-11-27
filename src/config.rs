use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct Config {
    pub input: PathBuf,
    pub output: PathBuf,
    pub profile: String,
    #[serde(default = "default_scan_secs")]
    pub scan_secs: u64,
    #[serde(default = "default_idle_secs")]
    pub idle_secs: u64,
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default = "default_output_name")]
    pub output_name: String,
    #[serde(default = "default_jobs")]
    pub jobs: usize,
    #[serde(default = "default_ffmpeg")]
    pub ffmpeg: PathBuf,
    #[serde(default = "default_ffprobe")]
    pub ffprobe: PathBuf,
    pub done_dir: Option<PathBuf>,
    pub failed_dir: Option<PathBuf>,
    #[serde(default)]
    pub delete_source: bool,
    pub log_file: Option<PathBuf>,
    #[serde(default)]
    pub folders: BTreeMap<String, String>,
    pub web: Option<Web>,
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Deserialize)]
pub struct Web {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub password: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Profile {
    pub ext: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_args: Vec<String>,
    #[serde(default = "default_true")]
    pub preserve_duration: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_codecs: Vec<String>,
    #[serde(default)]
    pub ignore_errors: bool,
}

#[derive(Clone)]
pub struct Watch {
    pub dir: PathBuf,
    pub profile: String,
    pub output: PathBuf,
    pub done: PathBuf,
    pub failed: PathBuf,
}

fn default_scan_secs() -> u64 {
    10
}

fn default_idle_secs() -> u64 {
    60
}

fn default_extensions() -> Vec<String> {
    ["mp4", "mkv", "mov", "avi", "m4v", "ts", "webm"].map(String::from).to_vec()
}

fn default_output_name() -> String {
    "{name}".into()
}

fn default_jobs() -> usize {
    1
}

fn default_ffmpeg() -> PathBuf {
    "ffmpeg".into()
}

fn default_ffprobe() -> PathBuf {
    "ffprobe".into()
}

fn default_true() -> bool {
    true
}

fn default_host() -> String {
    "127.0.0.1".into()
}

fn default_port() -> u16 {
    8686
}

impl Profile {
    pub fn encoder(&self) -> &str {
        self.args
            .windows(2)
            .find(|w| matches!(w[0].as_str(), "-c:v" | "-codec:v" | "-vcodec"))
            .map_or("ffmpeg default", |w| w[1].as_str())
    }
}

impl Config {
    pub fn load(path: &Path) -> crate::Result<Config> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("can't read config {}: {e}", path.display()))?;
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()).into())
    }

    pub fn parse(text: &str) -> crate::Result<Config> {
        let cfg: Config = toml::from_str(text)?;
        if cfg.scan_secs == 0 {
            return Err("scan_secs must be at least 1".into());
        }
        cfg.profile(&cfg.profile)?;
        for profile in cfg.folders.values() {
            cfg.profile(profile)?;
        }
        if !cfg.output_name.contains("{name}") {
            return Err("output_name must contain {name}, or every file would get the same name".into());
        }
        if cfg.jobs == 0 {
            return Err("jobs must be at least 1".into());
        }
        Ok(cfg)
    }

    pub fn watches(&self) -> Vec<Watch> {
        let root = std::iter::once((None, &self.profile));
        let subs = self.folders.iter().map(|(sub, p)| (Some(sub), p));
        root.chain(subs)
            .map(|(sub, profile)| {
                let dir = sub.map_or(self.input.clone(), |s| self.input.join(s));
                Watch {
                    output: sub.map_or(self.output.clone(), |s| self.output.join(s)),
                    done: self.done_dir.clone().unwrap_or_else(|| dir.join("done")),
                    failed: self.failed_dir.clone().unwrap_or_else(|| dir.join("failed")),
                    profile: profile.clone(),
                    dir,
                }
            })
            .collect()
    }

    pub fn web_addr(&self) -> Option<String> {
        self.web.as_ref().filter(|w| w.enabled).map(|w| format!("{}:{}", w.host, w.port))
    }

    pub fn profile(&self, name: &str) -> crate::Result<&Profile> {
        self.profiles.get(name).ok_or_else(|| {
            let known: Vec<_> = self.profiles.keys().map(String::as_str).collect();
            format!("no profile named '{name}' (have: {})", known.join(", ")).into()
        })
    }
}
