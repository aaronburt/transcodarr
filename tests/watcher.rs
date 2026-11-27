use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fs::create_dir(fixture.path("in")).unwrap();
        let example = include_str!("../transcodarr.example.toml")
            .replace("/media/in", "in")
            .replace("/media/out", "out")
            .replace("delete_source = false", "delete_source = true")
            .replace("enabled = true", "enabled = false")
            .replace("anime = \"av1\"", "")
            .replace("scan_secs = 10", "scan_secs = 1")
            .replace("idle_secs = 60", "idle_secs = 1");
        fs::write(fixture.path("config.toml"), example).unwrap();
        fixture
    }

    fn path(&self, path: &str) -> PathBuf {
        self.0.path().join(path)
    }

    fn config(&self, from: &str, to: &str) {
        let path = self.path("config.toml");
        let config = fs::read_to_string(&path).unwrap();
        assert!(config.contains(from));
        fs::write(path, config.replace(from, to)).unwrap();
    }

    fn clip(&self) {
        let output = Command::new("ffmpeg")
            .current_dir(self.0.path())
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x96:rate=30:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:duration=2",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                "mpeg4",
                "-c:a",
                "aac",
                "in/clip.mkv",
            ])
            .output()
            .unwrap();
        success(output);
    }

    fn cli(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_transcodarr"));
        cmd.current_dir(self.0.path()).args(args).args(["--config", "config.toml"]);
        cmd
    }

    fn watch(&self) -> Watcher {
        let log = self.path("watch.log");
        let child =
            self.cli(&["run"]).stdout(Stdio::null()).stderr(File::create(&log).unwrap()).spawn().unwrap();
        Watcher { child, log }
    }
}

struct Watcher {
    child: Child,
    log: PathBuf,
}

impl Watcher {
    fn until(&mut self, condition: impl Fn() -> bool) {
        let started = Instant::now();
        while !condition() {
            assert!(
                self.child.try_wait().unwrap().is_none() && started.elapsed() < Duration::from_secs(20),
                "watcher stopped or timed out: {}",
                fs::read_to_string(&self.log).unwrap()
            );
            sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        self.child.wait().unwrap();
    }
}

fn success(result: Output) -> Output {
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    result
}

fn audio_tracks(path: &Path) -> usize {
    let output = success(
        Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap(),
    );
    String::from_utf8(output.stdout).unwrap().lines().count()
}

#[test]
fn example_profiles_preserve_alternate_audio() {
    let f = Fixture::new();
    f.clip();
    for (profile, ext) in [("hevc", "mkv"), ("h264", "mp4")] {
        success(f.cli(&["encode", "in/clip.mkv", "-p", profile]).output().unwrap());
        assert_eq!(audio_tracks(&f.path(&format!("out/clip.{ext}"))), 2);
    }
}

#[test]
fn truncated_input_is_retained_and_partial_output_is_withheld() {
    let f = Fixture::new();
    f.clip();
    let input = f.path("in/clip.mkv");
    let mut bytes = fs::read(&input).unwrap();
    bytes.truncate(bytes.len() / 2);
    fs::write(input, &bytes).unwrap();
    let mut watcher = f.watch();
    watcher.until(|| f.path("in/failed/clip.mkv.log").is_file());
    assert_eq!(fs::read(f.path("in/failed/clip.mkv")).unwrap(), bytes);
    assert!(!f.path("out/clip.mkv").exists());
}

#[test]
fn ignore_errors_encodes_through_a_damaged_frame() {
    let f = Fixture::new();
    f.clip();
    let input = f.path("in/clip.mkv");
    let mut bytes = fs::read(&input).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle..middle + 400].fill(0x55);
    fs::write(&input, &bytes).unwrap();
    assert!(!f.cli(&["encode", "in/clip.mkv", "-p", "h264"]).output().unwrap().status.success());
    assert!(!f.path("out/clip.mp4").exists());
    f.config("ignore_errors = false", "ignore_errors = true");
    success(f.cli(&["encode", "in/clip.mkv", "-p", "h264"]).output().unwrap());
    assert!(f.path("out/clip.mp4").is_file());
}

#[test]
fn log_file_records_each_step_as_json() {
    let f = Fixture::new();
    f.clip();
    let mut watcher = f.watch();
    watcher
        .until(|| f.path("out/clip.mkv").exists() && !f.path("in/.transcodarr-processing/clip.mkv").exists());
    let log = f.path("transcodarr.jsonl");
    watcher.until(|| fs::read_to_string(&log).unwrap_or_default().contains("\"finished\""));
    let events: Vec<serde_json::Value> =
        fs::read_to_string(log).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let names: Vec<_> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(names, ["started", "encode_started", "finished"]);
    assert_eq!(events[1]["source"]["streams"].as_array().unwrap().len(), 3);
    assert_eq!(events[2]["result"], "done");
}

#[test]
fn missing_encoder_leaves_inbox_untouched() {
    let f = Fixture::new();
    f.config("ffmpeg = \"ffmpeg\"", "ffmpeg = \"unavailable-test-ffmpeg\"");
    fs::write(f.path("in/clip.mkv"), b"queued").unwrap();
    let result = f.cli(&["run"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unavailable-test-ffmpeg"));
    assert_eq!(fs::read(f.path("in/clip.mkv")).unwrap(), b"queued");
    assert!(!f.path("in/failed").exists());
}

#[test]
fn inaccessible_output_retries_without_quarantining_the_source() {
    let f = Fixture::new();
    f.clip();
    fs::write(f.path("out"), b"occupied").unwrap();
    let mut watcher = f.watch();
    watcher.until(|| fs::read_to_string(f.path("watch.log")).unwrap().contains("queued:"));
    assert!(f.path("in/clip.mkv").exists());
    assert!(!f.path("in/failed").exists());
    fs::remove_file(f.path("out")).unwrap();
    watcher
        .until(|| f.path("out/clip.mkv").exists() && !f.path("in/.transcodarr-processing/clip.mkv").exists());
    assert!(!f.path("in/done/clip.mkv").exists());
    assert_eq!(audio_tracks(&f.path("out/clip.mkv")), 2);
}

#[test]
fn watcher_only_deletes_the_claimed_file_and_reserves_its_output() {
    let f = Fixture::new();
    f.clip();
    f.config("\"-c:v\", \"libx265\"", "\"-vf\", \"realtime\", \"-c:v\", \"libx265\"");
    let mut watcher = f.watch();
    let claimed = f.path("in/.transcodarr-processing/clip.mkv");
    watcher.until(|| claimed.exists());
    fs::write(f.path("in/clip.mkv"), b"replacement").unwrap();
    let other = f.cli(&["encode", "in/clip.mkv"]).output().unwrap();
    assert!(!other.status.success());
    assert!(String::from_utf8_lossy(&other.stderr).contains("already in use"));
    watcher.until(|| f.path("out/clip.mkv").exists() && !claimed.exists());
    assert_eq!(fs::read(f.path("in/clip.mkv")).unwrap(), b"replacement");
    assert_eq!(audio_tracks(&f.path("out/clip.mkv")), 2);
}

#[test]
fn interrupted_job_keeps_its_original_when_the_output_already_exists() {
    let f = Fixture::new();
    let stage = f.path("in/.transcodarr-processing");
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("clip.mkv"), b"claimed").unwrap();
    fs::write(f.path("in/clip.mkv"), b"replacement").unwrap();
    fs::create_dir(f.path("out")).unwrap();
    fs::write(f.path("out/clip.mkv"), b"published").unwrap();
    let mut watcher = f.watch();
    watcher.until(|| f.path("in/done/clip.mkv").is_file());
    assert_eq!(fs::read(f.path("in/done/clip.mkv")).unwrap(), b"claimed");
    assert_eq!(fs::read(f.path("in/clip.mkv")).unwrap(), b"replacement");
    assert_eq!(fs::read(f.path("out/clip.mkv")).unwrap(), b"published");
}

#[test]
fn clean_but_short_output_requires_a_duration_changing_profile() {
    let f = Fixture::new();
    f.clip();
    f.config("\"-c:v\", \"libx265\"", "\"-t\", \"0.4\", \"-c:v\", \"libx265\"");
    let result = f.cli(&["encode", "in/clip.mkv"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("output is too short"));
    assert!(!f.path("out/clip.mkv").exists());
    f.config("[profiles.hevc]", "[profiles.hevc]\npreserve_duration = false");
    let mut watcher = f.watch();
    watcher.until(|| f.path("in/done/clip.mkv").is_file());
    assert!(f.path("out/clip.mkv").is_file());
}

#[test]
fn full_length_audio_cannot_hide_a_short_video_track() {
    let f = Fixture::new();
    f.clip();
    f.config("\"-c:v\", \"libx265\"", "\"-vf\", \"trim=duration=0.4\", \"-c:v\", \"libx265\"");
    let result = f.cli(&["encode", "in/clip.mkv"]).output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("output is too short (video)"));
    assert!(!f.path("out/clip.mkv").exists());
}

#[test]
fn encoder_initialization_failure_survives_restart() {
    let f = Fixture::new();
    f.clip();
    f.config("libx265", "unavailable-test-encoder");
    let original = fs::read(f.path("in/clip.mkv")).unwrap();
    let claimed = f.path("in/.transcodarr-processing/clip.mkv");
    {
        let mut watcher = f.watch();
        watcher.until(|| fs::read_to_string(f.path("watch.log")).unwrap().contains("queued:"));
        assert_eq!(fs::read(&claimed).unwrap(), original);
        assert!(!f.path("in/failed").exists());
    }
    f.config("unavailable-test-encoder", "libx265");
    let mut watcher = f.watch();
    watcher.until(|| f.path("out/clip.mkv").exists() && !claimed.exists());
    assert_eq!(audio_tracks(&f.path("out/clip.mkv")), 2);
}

#[test]
fn profile_that_omits_a_track_keeps_its_original() {
    let f = Fixture::new();
    f.clip();
    f.config("0:a?", "0:a:0?");
    let mut watcher = f.watch();
    watcher.until(|| f.path("in/done/clip.mkv").exists());
    assert_eq!(audio_tracks(&f.path("in/done/clip.mkv")), 2);
    assert_eq!(audio_tracks(&f.path("out/clip.mkv")), 1);
}

#[test]
fn unknown_source_duration_keeps_the_original() {
    let f = Fixture::new();
    success(
        Command::new("ffmpeg")
            .current_dir(f.0.path())
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x96:rate=25:duration=1",
                "-c:v",
                "libx264",
                "-f",
                "h264",
                "in/clip.mkv",
            ])
            .output()
            .unwrap(),
    );
    let original = fs::read(f.path("in/clip.mkv")).unwrap();
    let mut watcher = f.watch();
    watcher.until(|| f.path("in/done/clip.mkv").exists());
    assert!(f.path("out/clip.mkv").exists());
    assert_eq!(fs::read(f.path("in/done/clip.mkv")).unwrap(), original);
}

#[test]
fn an_output_created_during_encoding_is_never_overwritten() {
    let f = Fixture::new();
    f.clip();
    f.config("\"-c:v\", \"libx265\"", "\"-vf\", \"realtime\", \"-c:v\", \"libx265\"");
    let mut watcher = f.watch();
    watcher.until(|| {
        fs::read_dir(f.path("out/.transcodarr")).is_ok_and(|entries| {
            entries.flatten().any(|e| e.path().extension().is_some_and(|ext| ext == "mkv"))
        })
    });
    fs::write(f.path("out/clip.mkv"), b"other producer").unwrap();
    watcher.until(|| f.path("in/done/clip.mkv").exists());
    assert_eq!(fs::read(f.path("out/clip.mkv")).unwrap(), b"other producer");
    assert_eq!(audio_tracks(&f.path("in/done/clip.mkv")), 2);
}
