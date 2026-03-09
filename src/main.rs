mod api;
mod downloader;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use api::Pr0grammClient;
use downloader::Downloader;

/// pr0dl — a fast CLI downloader for pr0gramm.com
#[derive(Parser)]
#[command(name = "pr0dl", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch media URLs from pr0gramm and save them to a file (or stdout)
    Fetch {
        #[arg(long)] user: Option<String>,
        #[arg(long)] tags: Option<String>,
        #[arg(long, default_value_t = false)] favourites: bool,
        /// Content flags: 1=SFW, 2=NSFP, 4=NSFW, 8=NSFL (add to combine)
        #[arg(long, default_value_t = 1)] flags: u8,
        #[arg(long, env = "PR0DL_PP")] pp: Option<String>,
        #[arg(long, env = "PR0DL_ME")] me: Option<String>,
        /// Write URLs to this file instead of stdout
        #[arg(long, short)] output: Option<PathBuf>,
        /// Where to store fetch progress (resumes on crash)
        #[arg(long, default_value = ".fetch_state.json")] state: PathBuf,
    },

    /// Download files from a URL list (one URL per line)
    Download {
        #[arg(long, short)] input: PathBuf,
        #[arg(long, short, default_value = "./downloads")] output: PathBuf,
        /// Parallel download workers (keep low to avoid rate limits)
        #[arg(long, short = 'j', default_value_t = 3)] jobs: usize,
        /// File to write failed URLs into for later retry
        #[arg(long, default_value = "failed.txt")] failed: PathBuf,
    },

    /// Fetch URLs then immediately download (combines fetch + download)
    Run {
        #[arg(long)] user: Option<String>,
        #[arg(long)] tags: Option<String>,
        #[arg(long, default_value_t = false)] favourites: bool,
        /// Content flags: 1=SFW, 2=NSFP, 4=NSFW, 8=NSFL (add to combine)
        #[arg(long, default_value_t = 1)] flags: u8,
        #[arg(long, env = "PR0DL_PP")] pp: Option<String>,
        #[arg(long, env = "PR0DL_ME")] me: Option<String>,
        #[arg(long, short, default_value = "./downloads")] output: PathBuf,
        /// Parallel download workers (keep low to avoid rate limits)
        #[arg(long, short = 'j', default_value_t = 3)] jobs: usize,
        /// Where to store fetch progress (resumes on crash)
        #[arg(long, default_value = ".fetch_state.json")] state: PathBuf,
        /// File to write failed URLs into for later retry
        #[arg(long, default_value = "failed.txt")] failed: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Fetch { user, tags, favourites, flags, pp, me, output, state } => {
            let urls = fetch_urls(user, tags, favourites, flags, pp, me, &state).await?;
            write_urls(&urls, output.as_deref()).await?;
            eprintln!("Fetched {} URLs.", urls.len());
        }

        Command::Download { input, output, jobs, failed } => {
            let urls = read_url_file(&input).await?;
            eprintln!("Loaded {} URLs from '{}'.", urls.len(), input.display());
            run_downloads(urls, output, jobs, &failed).await?;
        }

        Command::Run { user, tags, favourites, flags, pp, me, output, jobs, state, failed } => {
            let urls = fetch_urls(user, tags, favourites, flags, pp, me, &state).await?;
            eprintln!("Fetched {} URLs total.", urls.len());
            run_downloads(urls, output, jobs, &failed).await?;
        }
    }

    Ok(())
}

fn build_arg(user: Option<String>, tags: Option<String>, favourites: bool) -> String {
    if let Some(u) = user {
        if favourites {
            format!("&user={u}&collection=favoriten&self=true")
        } else {
            format!("&user={u}")
        }
    } else if let Some(t) = tags {
        format!("&tags={}", t.replace(' ', "+"))
    } else {
        String::new()
    }
}

async fn fetch_urls(
    user: Option<String>,
    tags: Option<String>,
    favourites: bool,
    flags: u8,
    pp: Option<String>,
    me: Option<String>,
    state_path: &std::path::Path,
) -> Result<Vec<String>> {
    let arg = build_arg(user, tags, favourites);
    let pp = pp.unwrap_or_default();
    let me = me.unwrap_or_default();
    let client = Pr0grammClient::new(flags, arg, pp, me).context("Failed to create API client")?;
    eprintln!("Fetching URL list from pr0gramm API...");
    client.fetch_all_urls(state_path).await
}

async fn write_urls(urls: &[String], path: Option<&std::path::Path>) -> Result<()> {
    match path {
        Some(p) => {
            let mut file = tokio::fs::File::create(p)
                .await
                .with_context(|| format!("Create '{}'", p.display()))?;
            for url in urls {
                file.write_all(url.as_bytes()).await?;
                file.write_all(b"\n").await?;
            }
        }
        None => {
            for url in urls {
                println!("{url}");
            }
        }
    }
    Ok(())
}

async fn read_url_file(path: &std::path::Path) -> Result<Vec<String>> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("Open '{}'", path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let mut urls = Vec::new();
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim().to_owned();
        if !trimmed.is_empty() {
            urls.push(trimmed);
        }
    }
    Ok(urls)
}

async fn run_downloads(urls: Vec<String>, output: PathBuf, jobs: usize, failed: &std::path::Path) -> Result<()> {
    let downloader = Downloader::new(output.clone(), jobs).context("Failed to create downloader")?;
    eprintln!("Downloading to '{}' with {} parallel jobs...", output.display(), jobs);
    let stats = downloader.download_all(urls, failed).await?;
    eprintln!(
        "\nFinished: {} downloaded, {} skipped, {} failed.",
        stats.succeeded, stats.skipped, stats.failed
    );
    Ok(())
}
