use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::time::{sleep, Duration};

const BASE_URL: &str = "https://pr0gramm.com/api/items/get";
const IMG_CDN: &str = "https://img.pr0gramm.com/";
const VID_CDN: &str = "https://vid.pr0gramm.com/";

/// Persisted after every page so we can resume if interrupted.
#[derive(Serialize, Deserialize, Default)]
pub struct FetchState {
    pub older_id: Option<u64>,
    pub urls: Vec<String>,
}

impl FetchState {
    pub async fn load(path: &Path) -> Self {
        tokio::fs::read_to_string(path)
            .await
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub async fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string(self).context("Serialize state")?;
        tokio::fs::write(path, json)
            .await
            .context("Write state file")?;
        Ok(())
    }
}

#[derive(Deserialize, Debug)]
pub struct ApiResponse {
    pub items: Vec<Item>,
}

#[derive(Deserialize, Debug)]
pub struct Item {
    pub id: u64,
    pub image: String,
}

pub struct Pr0grammClient {
    client: Client,
    flags: String,
    arg: String,
}

impl Pr0grammClient {
    pub fn new(flags: u8, arg: String, pp_cookie: String, me_cookie: String) -> Result<Self> {
        let cookie_header = format!("pp={}; me={}", pp_cookie, me_cookie);

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (X11; Linux x86_64; rv:108.0) Gecko/20100101 Firefox/108.0"
                .parse()
                .unwrap(),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            "application/json, text/javascript, */*; q=0.01"
                .parse()
                .unwrap(),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("x-requested-with"),
            "XMLHttpRequest".parse().unwrap(),
        );
        headers.insert(
            reqwest::header::REFERER,
            "https://pr0gramm.com/".parse().unwrap(),
        );
        headers.insert(
            reqwest::header::COOKIE,
            cookie_header.parse().context("Invalid cookie value")?,
        );

        let client = Client::builder()
            .default_headers(headers)
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            flags: flags.to_string(),
            arg,
        })
    }

    /// Fetch all media URLs, checkpointing after each page.
    /// Pass `state_path` to enable resume-on-crash.
    pub async fn fetch_all_urls(&self, state_path: &Path) -> Result<Vec<String>> {
        let mut state = FetchState::load(state_path).await;

        if !state.urls.is_empty() {
            eprintln!(
                "Resuming fetch from item id {:?} ({} URLs already collected).",
                state.older_id,
                state.urls.len()
            );
        }

        loop {
            let response = self.fetch_page_with_retry(state.older_id).await?;
            for item in &response.items {
                state.urls.push(media_url(&item.image));
            }

            // Checkpoint after every page.
            state.older_id = response.items.last().map(|i| i.id);
            state.save(state_path).await?;

            eprintln!(
                "  Fetched page ending at id {:?} — {} URLs total.",
                state.older_id,
                state.urls.len()
            );

            if response.items.is_empty() {
                break;
            }
        }

        // Remove the state file once we've fully completed.
        let _ = tokio::fs::remove_file(state_path).await;

        Ok(state.urls)
    }

    /// Fetch one API page with exponential backoff on 429 / 5xx.
    async fn fetch_page_with_retry(&self, older_id: Option<u64>) -> Result<ApiResponse> {
        const MAX_RETRIES: u32 = 6;
        let mut delay_secs = 2u64;

        for attempt in 0..MAX_RETRIES {
            let mut query = format!("flags={}{}", self.flags, self.arg);
            if let Some(id) = older_id {
                query = format!("older={}&{}", id, query);
            }
            let url = format!("{}?{}", BASE_URL, query);

            let resp = self
                .client
                .get(&url)
                .send()
                .await
                .context("HTTP request failed")?;

            let status = resp.status();

            if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                eprintln!(
                    "  Rate limited / server error ({}). Waiting {}s before retry {}/{}...",
                    status,
                    delay_secs,
                    attempt + 1,
                    MAX_RETRIES
                );
                sleep(Duration::from_secs(delay_secs)).await;
                delay_secs = (delay_secs * 2).min(120);
                continue;
            }

            if !status.is_success() {
                anyhow::bail!("API returned status {}", status);
            }

            return resp
                .json::<ApiResponse>()
                .await
                .context("Failed to parse API response as JSON");
        }

        anyhow::bail!("Exceeded {} retries for API page older={:?}", MAX_RETRIES, older_id);
    }
}

fn media_url(image_path: &str) -> String {
    if image_path.ends_with(".mp4") || image_path.ends_with(".webm") {
        format!("{}{}", VID_CDN, image_path)
    } else {
        format!("{}{}", IMG_CDN, image_path)
    }
}
