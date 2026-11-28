use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

#[derive(Debug)]
pub enum Error {
    Media(String),
    Retry(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Self::Media(msg) | Self::Retry(msg)) = self;
        f.write_str(msg)
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Retry(e.to_string())
    }
}

#[derive(Default)]
pub struct Progress {
    pub percent: f64,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub fps: String,
    pub speed: String,
    pub stage: &'static str,
    pub vmaf: Option<f64>,
}

fn follow(
    mut child: Child,
    duration_secs: f64,
    stage: &'static str,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<(ExitStatus, Diagnostics), Error> {
    let stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || Diagnostics::read(BufReader::new(stderr)));
    let mut p = Progress { duration_secs, stage, ..Default::default() };
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        let Some((key, val)) = line.split_once('=') else { continue };
        match key {
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = val.parse::<f64>() {
                    p.position_secs = us / 1e6;
                }
            }
            "fps" => p.fps = val.to_string(),
            "speed" => p.speed = val.trim().to_string(),
            "progress" => {
                if val == "end" {
                    p.percent = 100.0;
                } else if p.duration_secs > 0.0 {
                    p.percent = (p.position_secs / p.duration_secs * 100.0).clamp(0.0, 100.0);
                }
                on_progress(&p);
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    let diagnostics = err_thread.join().map_err(|_| Error::Retry("lost ffmpeg diagnostics".into()))??;
    Ok((status, diagnostics))
}

fn vmaf(
    cfg: &Config,
    encoded: &Path,
    source_path: &Path,
    source: &Media,
    subsample: u32,
    on_progress: &mut impl FnMut(&Progress),
) -> Result<f64, Error> {
    let video = source.main_video().ok_or_else(|| Error::Media("no video stream found".into()))?;
    let mut distorted = Vec::new();
    if let (Some(w), Some(h)) = (video.width, video.height) {
        distorted.push(format!("scale={w}:{h}:flags=bicubic"));
    }
    if let Some(rate) = video.r_frame_rate.as_deref().filter(|r| !r.starts_with('0')) {
        distorted.push(format!("fps={rate}"));
    }
    distorted.push("format=yuv420p,setpts=PTS-STARTPTS".into());
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let graph = format!(
        "[0:v:0]{}[d];[1:v:0]format=yuv420p,setpts=PTS-STARTPTS[r];[d][r]libvmaf=n_subsample={subsample}:n_threads={threads}",
        distorted.join(",")
    );
    let child = Command::new(&cfg.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(encoded)
        .arg("-i")
        .arg(source_path)
        .args([
            "-lavfi",
            &graph,
            "-loglevel",
            "level+info",
            "-progress",
            "pipe:1",
            "-nostats",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Retry(format!("can't start {}: {e}", cfg.ffmpeg.display())))?;
    let (status, diagnostics) = follow(child, source.duration().unwrap_or_default(), "vmaf", on_progress)?;
    let score = diagnostics
        .tail
        .iter()
        .find_map(|line| line.split_once("VMAF score: ").and_then(|(_, v)| v.trim().parse::<f64>().ok()));
    match score {
        Some(score) if status.success() => Ok(score),
        _ if diagnostics.tail.iter().any(|l| l.contains("No such filter: 'libvmaf'")) => {
            Err(Error::Retry(format!("{} has no libvmaf filter, needed for min_vmaf", cfg.ffmpeg.display())))
        }
        _ => Err(diagnostics.failure(format!("VMAF check returned {status}"))),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    modified: Option<SystemTime>,
}

impl From<fs::Metadata> for Stamp {
    fn from(m: fs::Metadata) -> Self {
        Self { size: m.len(), modified: m.modified().ok() }
    }
}

pub fn output_path(cfg: &Config, input: &Path, out_dir: &Path, profile: &str) -> PathBuf {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let name = cfg.output_name.replace("{name}", &stem).replace("{profile}", profile);
    out_dir.join(format!("{}.{}", safe_file_name(&name), cfg.profiles[profile].ext))
}

fn safe_file_name(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.');
    match s.strip_prefix('-') {
        Some(rest) => format!("_{rest}"),
        None if s.is_empty() => "_".to_string(),
        None => s.to_string(),
    }
}

struct TempOutput(PathBuf);

impl TempOutput {
    fn new(dir: &Path, ext: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(dir.join(format!("{}-{n}.{ext}", std::process::id())))
    }

    fn publish(self, dest: &Path) -> io::Result<()> {
        match fs::hard_link(&self.0, dest) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(e),
            Err(_) if dest.try_exists()? => Err(io::ErrorKind::AlreadyExists.into()),
            Err(_) => fs::rename(&self.0, dest),
        }
    }
}

impl Drop for TempOutput {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn check_tools(cfg: &Config) -> Result<(), Error> {
    for tool in [&cfg.ffmpeg, &cfg.ffprobe] {
        let status = Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| Error::Retry(format!("can't start {}: {e}", tool.display())))?;
        if !status.success() {
            return Err(Error::Retry(format!("{} -version returned {status}", tool.display())));
        }
    }
    Ok(())
}

#[derive(Serialize, Default, Clone)]
pub struct Hardware {
    pub gpus: Vec<String>,
    pub encoders: Vec<String>,
}

pub fn detect_hardware(cfg: &Config) -> Hardware {
    let output = |cmd: &mut Command| {
        cmd.stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let gpus =
        output(Command::new("nvidia-smi").args(["--query-gpu=name,driver_version", "--format=csv,noheader"]))
            .lines()
            .filter_map(|line| line.split_once(", "))
            .map(|(name, driver)| format!("{} (driver {})", name.trim(), driver.trim()))
            .collect();
    let encoders = output(Command::new(&cfg.ffmpeg).args(["-hide_banner", "-encoders"]))
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|name| {
            ["_nvenc", "_qsv", "_vaapi", "_amf", "_videotoolbox", "_mf"].iter().any(|s| name.ends_with(s))
        })
        .map(str::to_owned)
        .collect();
    Hardware { gpus, encoders }
}

pub fn lock(path: &Path) -> Result<File, Error> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| Error::Retry(format!("can't open {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => Err(Error::Retry(format!("already in use: {}", path.display()))),
        Err(fs::TryLockError::Error(e)) => Err(Error::Retry(format!("can't lock {}: {e}", path.display()))),
    }
}

pub struct Destination {
    pub path: PathBuf,
    work_dir: PathBuf,
    _lock: File,
}

impl Destination {
    pub fn reserve(cfg: &Config, input: &Path, dir: &Path, profile: &str) -> Result<Self, Error> {
        let path = output_path(cfg, input, dir, profile);
        let work_dir = dir.join(".transcodarr");
        fs::create_dir_all(&work_dir)
            .map_err(|e| Error::Retry(format!("can't create {}: {e}", work_dir.display())))?;
        let mut name = path.file_name().unwrap_or_default().to_owned();
        name.push(".lock");
        let lock = lock(&work_dir.join(name))?;
        Ok(Self { path, work_dir, _lock: lock })
    }

    pub fn check_available(&self) -> Result<(), Error> {
        if self.path.try_exists()? {
            return Err(Error::Retry(format!("{} already exists", self.path.display())));
        }
        Ok(())
    }

    pub fn encode(
        &self,
        cfg: &Config,
        input: &Path,
        profile_name: &str,
        source: &Media,
        mut on_progress: impl FnMut(&Progress),
    ) -> Result<Option<&'static str>, Error> {
        let profile = &cfg.profiles[profile_name];
        self.check_available()?;
        let stamp = Stamp::from(fs::metadata(input)?);
        let tmp = TempOutput::new(&self.work_dir, &profile.ext);
        let child = Command::new(&cfg.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-y"])
            .args((!profile.ignore_errors).then_some("-xerror"))
            .args(&profile.input_args)
            .arg("-i")
            .arg(input)
            .args(&profile.args)
            .args(["-loglevel", "level+error", "-progress", "pipe:1", "-nostats"])
            .arg(&tmp.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::Retry(format!("can't start {}: {e}", cfg.ffmpeg.display())))?;

        let duration = source.duration().unwrap_or_default();
        let (status, diagnostics) = follow(child, duration, "encoding", &mut on_progress)?;
        if !status.success() || !profile.ignore_errors && (diagnostics.had_error || diagnostics.bad_input) {
            return Err(diagnostics.failure(format!("ffmpeg returned {status}")));
        }
        let encoded = probe(&cfg.ffprobe, &tmp.0, false)
            .map_err(|e| Error::Retry(format!("can't verify output: {e}")))?;
        let keep_source = source.verify(&encoded, profile.preserve_duration)?;
        if let Some(min) = profile.min_vmaf {
            let score = vmaf(cfg, &tmp.0, input, source, profile.vmaf_subsample, &mut on_progress)?;
            on_progress(&Progress { vmaf: Some(score), ..Default::default() });
            if score < min {
                return Err(Error::Media(format!("VMAF {score:.2} is below min_vmaf {min}")));
            }
        }
        if Stamp::from(fs::metadata(input)?) != stamp {
            return Err(Error::Retry(format!("{} changed during the encode", input.display())));
        }
        tmp.publish(&self.path)
            .map_err(|e| Error::Retry(format!("can't publish {}: {e}", self.path.display())))?;
        Ok(keep_source)
    }
}

#[derive(Deserialize, Serialize)]
pub struct Media {
    streams: Vec<Stream>,
    format: Format,
}

#[derive(Deserialize, Serialize)]
struct Stream {
    codec_name: Option<String>,
    codec_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    r_frame_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: DurationTag,
    disposition: Disposition,
}

#[derive(Default, Deserialize, Serialize)]
struct DurationTag {
    #[serde(rename = "DURATION")]
    duration: Option<String>,
}

impl Stream {
    fn duration(&self) -> Option<f64> {
        if let Some(value) = self.duration.as_deref().and_then(seconds) {
            return Some(value);
        }
        let mut parts = self.tags.duration.as_deref()?.splitn(3, ':');
        let hours = parts.next()?.parse::<u32>().ok()?;
        let minutes = parts.next()?.parse::<u32>().ok()?;
        let value =
            f64::from(hours) * 3600.0 + f64::from(minutes) * 60.0 + parts.next()?.parse::<f64>().ok()?;
        (value.is_finite() && value > 0.0).then_some(value)
    }
}

fn seconds(value: &str) -> Option<f64> {
    value.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0)
}

#[derive(Deserialize, Serialize)]
struct Disposition {
    attached_pic: u8,
}

#[derive(Deserialize, Serialize)]
struct Format {
    duration: Option<String>,
}

impl Media {
    fn main_video(&self) -> Option<&Stream> {
        self.streams.iter().find(|s| s.codec_type == "video" && s.disposition.attached_pic == 0)
    }

    pub fn video_codec(&self) -> Option<&str> {
        self.main_video()?.codec_name.as_deref()
    }

    fn duration(&self) -> Option<f64> {
        self.format.duration.as_deref().and_then(seconds)
    }

    fn track_durations(&self, kind: &str) -> Option<Vec<f64>> {
        let mut lengths: Vec<_> = self
            .streams
            .iter()
            .filter(|s| s.codec_type == kind && s.disposition.attached_pic == 0)
            .map(|s| s.duration().or_else(|| if self.streams.len() == 1 { self.duration() } else { None }))
            .collect::<Option<_>>()?;
        lengths.sort_unstable_by(f64::total_cmp);
        Some(lengths)
    }

    fn stream_counts(&self) -> HashMap<(&str, u8), usize> {
        let mut counts = HashMap::new();
        for stream in &self.streams {
            *counts.entry((stream.codec_type.as_str(), stream.disposition.attached_pic)).or_default() += 1;
        }
        counts
    }

    fn verify(&self, output: &Media, preserve_duration: bool) -> Result<Option<&'static str>, Error> {
        if !preserve_duration {
            return Ok(Some("original kept for a duration-changing profile"));
        }
        let mut lengths = Vec::new();
        let mut keep = None;
        if let (Some(expected), Some(actual)) = (self.duration(), output.duration()) {
            lengths.push(("container", expected, actual));
        } else {
            keep = Some("original kept because duration could not be verified");
        }
        for kind in ["video", "audio"] {
            match (self.track_durations(kind), output.track_durations(kind)) {
                (Some(before), Some(after)) if before.len() == after.len() => {
                    lengths.extend(before.into_iter().zip(after).map(|(a, b)| (kind, a, b)));
                }
                _ => keep = Some("original kept because track durations could not be matched"),
            }
        }
        for (kind, expected, actual) in lengths {
            let shortfall = expected - actual;
            if shortfall > (expected * 0.05).min(0.25) {
                return Err(Error::Media(format!(
                    "output is too short ({kind}): expected {expected:.3}s, got {actual:.3}s"
                )));
            }
            if shortfall > 0.001 {
                keep = Some("original kept because the output duration is slightly shorter");
            }
        }
        let counts = output.stream_counts();
        if self
            .stream_counts()
            .into_iter()
            .any(|(kind, n)| counts.get(&kind).copied().unwrap_or_default() < n)
        {
            return Ok(Some("original kept because the output contains fewer streams"));
        }
        Ok(keep)
    }
}

pub fn probe(tool: &Path, input: &Path, ignore_errors: bool) -> Result<Media, Error> {
    File::open(input).map_err(|e| Error::Retry(format!("can't open {}: {e}", input.display())))?;
    let result = Command::new(tool)
        .args([
            "-v",
            "level+error",
            "-show_entries",
            "format=duration:stream=codec_name,codec_type,width,height,r_frame_rate,duration:stream_disposition=attached_pic:stream_tags=DURATION",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .map_err(|e| Error::Retry(format!("can't start {}: {e}", tool.display())))?;
    let mut diagnostics = Diagnostics::read(result.stderr.as_slice())?;
    if !result.status.success() || !ignore_errors && (diagnostics.had_error || diagnostics.bad_input) {
        diagnostics.bad_input |= diagnostics
            .tail
            .iter()
            .any(|line| line.to_ascii_lowercase().ends_with("invalid data found when processing input"));
        return Err(diagnostics.failure(format!("ffprobe returned {}", result.status)));
    }
    let media: Media = serde_json::from_slice(&result.stdout)
        .map_err(|e| Error::Retry(format!("invalid ffprobe response: {e}")))?;
    if media.video_codec().is_none() {
        return Err(Error::Media("no video stream found".into()));
    }
    Ok(media)
}

#[derive(Default)]
struct Diagnostics {
    tail: VecDeque<String>,
    had_error: bool,
    bad_input: bool,
}

impl Diagnostics {
    fn read(mut reader: impl BufRead) -> io::Result<Self> {
        let mut result = Self::default();
        let mut line = Vec::new();
        while reader.read_until(b'\n', &mut line)? != 0 {
            let text = String::from_utf8_lossy(&line);
            let lower = text.to_ascii_lowercase();
            result.had_error |= ["[error]", "[fatal]", "[panic]"].iter().any(|tag| lower.contains(tag));
            result.bad_input |= [
                "file ended prematurely",
                "moov atom not found",
                "error submitting packet to decoder",
                "corrupt decoded frame",
                "packet corrupt",
                "invalid nal unit",
            ]
            .iter()
            .any(|message| lower.contains(message));
            if result.tail.len() == 40 {
                result.tail.pop_front();
            }
            result.tail.push_back(text.trim_end().to_owned());
            line.clear();
        }
        Ok(result)
    }

    fn failure(self, summary: String) -> Error {
        let no_gpu = self.tail.iter().any(|line| {
            let lower = line.to_ascii_lowercase();
            ["libcuda.so", "nvcuda.dll", "no capable devices found", "no nvenc capable devices"]
                .iter()
                .any(|m| lower.contains(m))
        });
        let mut message =
            if self.tail.is_empty() { summary } else { self.tail.into_iter().collect::<Vec<_>>().join("\n") };
        if no_gpu {
            message.push_str(
                "\nffmpeg can't reach an NVIDIA GPU. Use a CPU profile, or give the container the GPU (gpus: all).",
            );
        }
        if self.bad_input { Error::Media(message) } else { Error::Retry(message) }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn safe_file_name() {
        assert_eq!(super::safe_file_name("a/b\\c:d*e?"), "a_b_c_d_e_");
        assert_eq!(super::safe_file_name("-i evil"), "_i evil");
        assert_eq!(super::safe_file_name("name. "), "name");
        assert_eq!(super::safe_file_name(".."), "_");
        assert_eq!(super::safe_file_name("Show S01E01 [av1]"), "Show S01E01 [av1]");
    }
}
