use anyhow::{Context, Result};
use futures::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest::{Client, StatusCode};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration, Instant};

#[derive(Default)]
struct RateState {
    blocked_until: Option<Instant>,
}

pub struct Downloader {
    client: Client,
    output_dir: PathBuf,
    concurrency: usize,
}

impl Downloader {
    pub fn new(output_dir: PathBuf, concurrency: usize) -> Result<Self> {
        let client = Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:108.0) Gecko/20100101 Firefox/108.0")
            .timeout(Duration::from_secs(60))
            .build()
            .context("Failed to build download client")?;

        Ok(Self { client, output_dir, concurrency })
    }

    pub async fn download_all(&self, urls: Vec<String>, failed_path: &std::path::Path) -> Result<DownloadStats> {
        tokio::fs::create_dir_all(&self.output_dir)
            .await
            .context("Failed to create output directory")?;

        clean_part_files(&self.output_dir).await;

        let total = urls.len() as u64;
        let multi = Arc::new(MultiProgress::new());

        // Overall progress bar
        let overall = multi.add(ProgressBar::new(total));
        overall.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{bar:55.cyan/blue}] {pos}/{len} ({percent}%) ETA {eta}")?
                .progress_chars("█▓░"),
        );

        // Single status line below the bar showing what's active
        let status = multi.add(ProgressBar::new_spinner());
        status.set_style(ProgressStyle::default_spinner().template("{spinner:.dim} {msg}")?);

        let rate_state = Arc::new(Mutex::new(RateState::default()));
        let failed_urls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // Track currently active filenames for the status line
        let active: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let mut stats = DownloadStats::default();
        let mut stream = futures::stream::iter(urls.into_iter().enumerate())
            .map(|(idx, url)| {
                let client = self.client.clone();
                let output_dir = self.output_dir.clone();
                let rate_state = rate_state.clone();
                let failed_urls = failed_urls.clone();
                let active = active.clone();
                let multi = multi.clone();
                let status = status.clone();
                async move {
                    let result = download_file(
                        client, url.clone(), idx, output_dir,
                        rate_state, active, multi, status,
                    ).await;
                    if result.is_err() {
                        failed_urls.lock().await.push(url);
                    }
                    result
                }
            })
            .buffer_unordered(self.concurrency);

        while let Some(result) = stream.next().await {
            match result {
                Ok(DownloadOutcome::Saved) => {
                    stats.succeeded += 1;
                    overall.inc(1);
                }
                Ok(DownloadOutcome::Skipped) => {
                    stats.skipped += 1;
                    overall.inc(1);
                }
                Err(_) => {
                    stats.failed += 1;
                    overall.inc(1);
                }
            }
        }

        status.finish_and_clear();
        overall.finish_with_message(format!(
            "Done — {} downloaded, {} skipped, {} failed",
            stats.succeeded, stats.skipped, stats.failed
        ));

        let failed = failed_urls.lock().await;
        if !failed.is_empty() {
            let mut f = tokio::fs::File::create(failed_path)
                .await
                .context("Create failed-urls file")?;
            for url in failed.iter() {
                f.write_all(url.as_bytes()).await?;
                f.write_all(b"\n").await?;
            }
            multi.println(format!(
                "  ⚠  {} failed URLs saved to '{}' — retry with: pr0dl download -i {}",
                failed.len(),
                failed_path.display(),
                failed_path.display(),
            ))?;
        } else if failed_path.exists() {
            let _ = tokio::fs::remove_file(failed_path).await;
        }

        Ok(stats)
    }
}

enum DownloadOutcome {
    Saved,
    Skipped,
}

async fn download_file(
    client: Client,
    url: String,
    idx: usize,
    output_dir: PathBuf,
    rate_state: Arc<Mutex<RateState>>,
    active: Arc<Mutex<Vec<String>>>,
    multi: Arc<MultiProgress>,
    status: ProgressBar,
) -> Result<DownloadOutcome> {
    let filename = url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("file_{idx}"));

    let dest = output_dir.join(&filename);
    let part = output_dir.join(format!("{filename}.part"));

    if dest.exists() {
        return Ok(DownloadOutcome::Skipped);
    }

    // Register as active
    {
        let mut a = active.lock().await;
        a.push(filename.clone());
        status.set_message(format_active(&a));
    }

    const MAX_RETRIES: u32 = 8;
    let mut delay_secs = 5u64;
    let mut outcome = Err(anyhow::anyhow!("exhausted retries"));

    'retry: for attempt in 0..MAX_RETRIES {
        // Honour global rate-limit pause
        let wait_until = rate_state.lock().await.blocked_until;
        if let Some(until) = wait_until {
            let now = Instant::now();
            if until > now {
                let secs = (until - now).as_secs() + 1;
                status.set_message(format!("⏸  rate limited — waiting {secs}s…"));
                sleep(until - now).await;
            }
        }

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("  ✗ network error on {filename} (attempt {}): {e}", attempt + 1);
                multi.println(&msg).ok();
                sleep(Duration::from_secs(delay_secs)).await;
                delay_secs = (delay_secs * 2).min(300);
                continue;
            }
        };

        let status_code = resp.status();

        if status_code == StatusCode::TOO_MANY_REQUESTS || status_code.is_server_error() {
            // Try to read exact wait time from Retry-After header.
            let has_retry_after = resp.headers().contains_key(reqwest::header::RETRY_AFTER);
            let wait_secs = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(delay_secs);

            // Block all workers globally for at least this long.
            {
                let mut state = rate_state.lock().await;
                let proposed = Instant::now() + Duration::from_secs(wait_secs);
                if state.blocked_until.map_or(true, |cur| proposed > cur) {
                    state.blocked_until = Some(proposed);
                }
            }

            let source = if has_retry_after { "server" } else { "estimated" };
            multi.println(format!(
                "  ⏸  {status_code} on {filename} — pausing {wait_secs}s ({source}, attempt {}/{})",
                attempt + 1, MAX_RETRIES
            )).ok();
            sleep(Duration::from_secs(wait_secs)).await;
            // If server gave us a time, trust it + small buffer; otherwise exponential backoff.
            delay_secs = if has_retry_after {
                (wait_secs + 2).min(300)
            } else {
                (delay_secs * 2).min(300)
            };
            continue;
        }

        if !status_code.is_success() {
            outcome = Err(anyhow::anyhow!("HTTP {} for {url}", status_code));
            break;
        }

        // Stream to .part file
        let mut file = match tokio::fs::File::create(&part).await {
            Ok(f) => f,
            Err(e) => { outcome = Err(e.into()); break; }
        };

        let mut stream = resp.bytes_stream();
        let mut write_ok = true;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(data) => {
                    if file.write_all(&data).await.is_err() {
                        write_ok = false;
                        break;
                    }
                }
                Err(_) => { write_ok = false; break; }
            }
        }

        if write_ok {
            file.flush().await.ok();
            drop(file);
            if tokio::fs::rename(&part, &dest).await.is_ok() {
                outcome = Ok(DownloadOutcome::Saved);
                break 'retry;
            }
        }

        let _ = tokio::fs::remove_file(&part).await;
        sleep(Duration::from_secs(delay_secs)).await;
        delay_secs = (delay_secs * 2).min(300);
    }

    // Unregister from active list
    {
        let mut a = active.lock().await;
        a.retain(|n| n != &filename);
        status.set_message(format_active(&a));
    }

    if outcome.is_err() {
        multi.println(format!("  ✗ gave up on {filename} after {MAX_RETRIES} attempts")).ok();
    }

    outcome
}

fn format_active(names: &[String]) -> String {
    match names.len() {
        0 => String::new(),
        1 => format!("↓ {}", names[0]),
        2 => format!("↓ {}  ↓ {}", names[0], names[1]),
        n => format!("↓ {}  ↓ {}  (+{})", names[0], names[1], n - 2),
    }
}

async fn clean_part_files(dir: &PathBuf) {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("part") {
            eprintln!("  Removing incomplete: {}", path.display());
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
}

#[derive(Default)]
pub struct DownloadStats {
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
}
