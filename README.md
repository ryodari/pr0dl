# pr0dl

A command-line downloader for pr0gramm.com. Fetches media URLs from the API and downloads them in parallel with resume support.

## Requirements

- Rust toolchain (https://rustup.rs)

## Installation

```bash
git clone <repo>
cd pr0dl
cargo install --path .
```

## Authentication

NSFW, NSFL and partner content requires a valid session. Log in to pr0gramm.com in your browser, then copy the `pp` and `me` cookie values from DevTools (Application → Cookies).

Pass them via flags or environment variables:

```bash
export PR0DL_PP="your_pp_cookie"
export PR0DL_ME="your_me_cookie"
```

## Content Flags

Flags control which content categories are fetched. Add the values together to combine categories.

| Flag | Category |
|------|----------|
| 1    | SFW      |
| 2    | NSFP     |
| 4    | NSFW     |
| 8    | NSFL     |
| 16   | POL      |

**31** (= 1+2+4+8+16) fetches everything. This matches what the pr0gramm web app uses.

## Commands

### `run` — Fetch and download in one step

```bash
pr0dl run --user USERNAME --flags 31 --output ./downloads
```

```
Options:
  --user <USER>         Download posts by this user
  --tags <TAGS>         Download posts matching these tags
  --favourites          Download the user's favourites instead of their posts
  --flags <FLAGS>       Content flags (default: 1)
  --output <DIR>        Download directory (default: ./downloads)
  -j, --jobs <N>        Parallel download workers (default: 3)
  --pp <VALUE>          Session cookie (env: PR0DL_PP)
  --me <VALUE>          Session cookie (env: PR0DL_ME)
  --state <FILE>        Fetch progress file for resume (default: .fetch_state.json)
  --failed <FILE>       File to write failed URLs into (default: failed.txt)
```

### `fetch` — Collect URLs only

Writes one URL per line to a file or stdout. Useful if you want to inspect or filter URLs before downloading.

```bash
pr0dl fetch --user USERNAME --flags 31 --output urls.txt
```

Same options as `run`, minus `--jobs` and `--failed`.

### `download` — Download from a URL list

Downloads from a file previously created by `fetch`.

```bash
pr0dl download --input urls.txt --output ./downloads
```

```
Options:
  -i, --input <FILE>    File with one URL per line
  -o, --output <DIR>    Download directory (default: ./downloads)
  -j, --jobs <N>        Parallel download workers (default: 3)
  --failed <FILE>       File to write failed URLs into (default: failed.txt)
```

## Examples

Download all posts by a user (all content categories):

```bash
pr0dl run --user Ryodari --flags 31 --output ./downloads
```

Download posts matching a tag search:

```bash
pr0dl run --tags "landscape photography" --flags 1 --output ./downloads
```

Download a user's favourites:

```bash
pr0dl run --user Ryodari --favourites --flags 31 --output ./downloads
```

Fetch URLs first, then download separately:

```bash
pr0dl fetch --user Ryodari --flags 31 --output urls.txt
pr0dl download --input urls.txt --output ./downloads -j 5
```

Retry failed downloads:

```bash
pr0dl download --input failed.txt --output ./downloads
```

## Resume on Interruption

`fetch` and `run` save progress to `.fetch_state.json` after every page. If interrupted, re-running the same command resumes from where it left off. The state file is deleted automatically on successful completion.

## Parallel Workers

The `-j` flag controls how many files are downloaded simultaneously. The default of 3 is conservative to avoid rate limiting. Values between 3 and 8 are reasonable depending on your connection. Higher values offer no benefit when the server or your bandwidth is the bottleneck, and increase the risk of receiving 429 responses.
