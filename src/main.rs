mod config;
mod encode;
mod scan;
mod status;
mod web;
mod ws;

use config::Config;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const VERSION: &str = match option_env!("TRANSCODARR_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};

const USAGE: &str = "\
transcodarr: watch a folder and transcode new videos with ffmpeg

usage:
  transcodarr run      [--config FILE] [--web ADDR]
  transcodarr encode   FILE [--config FILE] [--profile NAME]
  transcodarr profiles [--config FILE]
  transcodarr --version

  --config         defaults to transcodarr.toml
  --default-config copies this file to --config first if that doesn't exist
  --web            serves a status page, e.g. --web 127.0.0.1:8686
                   (or set it in the config's [web] section)";

fn main() {
    if let Err(e) = cli(std::env::args().skip(1).collect()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn cli(args: Vec<String>) -> Result<()> {
    let mut config = PathBuf::from("transcodarr.toml");
    let mut default_config: Option<PathBuf> = None;
    let mut web = None;
    let mut profile = None;
    let mut positional = Vec::new();

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" | "-c" => config = value()?.into(),
            "--default-config" => default_config = Some(value()?.into()),
            "--web" => web = Some(value()?),
            "--profile" | "-p" => profile = Some(value()?),
            "-V" | "--version" => {
                println!("transcodarr {VERSION}");
                return Ok(());
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a if a.starts_with('-') => return Err(format!("unknown option {a}\n\n{USAGE}").into()),
            _ => positional.push(arg),
        }
    }

    if let Some(default) = default_config
        && !config.exists()
    {
        std::fs::copy(&default, &config)
            .map_err(|e| format!("can't copy {} to {}: {e}", default.display(), config.display()))?;
        eprintln!("wrote {} from {}", config.display(), default.display());
    }

    match positional.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["run"] => {
            let cfg = Config::load(&config)?;
            let addr = web.or_else(|| cfg.web_addr());
            let (wake_tx, wake_rx) = std::sync::mpsc::channel();
            let app = status::App::new(&config, cfg, wake_tx);
            if let Some(addr) = addr {
                web::serve(&addr, app.clone())?;
            }
            scan::run(&app, wake_rx)
        }
        ["encode", file] => {
            let cfg = Config::load(&config)?;
            let name = profile.unwrap_or_else(|| cfg.profile.clone());
            encode_one(Path::new(file), &cfg, &name)
        }
        ["profiles"] => {
            let cfg = Config::load(&config)?;
            for (name, p) in &cfg.profiles {
                let mark = if *name == cfg.profile { " (default)" } else { "" };
                println!("{name}{mark} -> .{}\n  {}", p.ext, p.args.join(" "));
            }
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn encode_one(file: &Path, cfg: &Config, profile_name: &str) -> Result<()> {
    let profile = cfg.profile(profile_name)?;
    let dest = encode::Destination::reserve(cfg, file, &cfg.output, profile_name)?;
    dest.check_available()?;
    let source = encode::probe(&cfg.ffprobe, file, profile.ignore_errors)?;
    dest.encode(cfg, file, profile_name, &source, |p| match p.vmaf {
        Some(score) => eprint!("\nVMAF {score:.2} "),
        None => {
            eprint!("\r{} {:5.1}%  {}x  {} fps   ", p.stage, p.percent, p.speed.trim_end_matches('x'), p.fps)
        }
    })?;
    eprintln!("\nwrote {}", dest.path.display());
    Ok(())
}
