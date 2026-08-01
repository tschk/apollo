//! Loading attachment bytes for channels that upload rather than link.
//!
//! Telegram takes a URL and fetches it itself. Discord and Slack do not — they
//! want the bytes. Rather than let each channel hand-roll "download it, guess a
//! filename, upload it", that lives here once.

use super::traits::MediaSource;

/// Cap on what apollo will pull into memory to re-upload. Both Discord's and
/// Slack's own limits sit below this for non-boosted accounts, so hitting it
/// means the upload was going to be rejected anyway — better to say so before
/// buffering an arbitrary remote body.
const MAX_ATTACHMENT_BYTES: usize = 100 * 1024 * 1024;

/// Attachment bytes plus a filename the platform can display.
pub async fn load(source: &MediaSource) -> anyhow::Result<(Vec<u8>, String)> {
    match source {
        MediaSource::Path(path) => {
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|e| anyhow::anyhow!("cannot read attachment {}: {e}", path.display()))?;
            check_size(bytes.len())?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "upload".to_string());
            Ok((bytes, name))
        }
        MediaSource::Url(url) => {
            let resp = crate::http::shared().get(url).send().await?;
            if !resp.status().is_success() {
                anyhow::bail!("cannot fetch attachment {url}: HTTP {}", resp.status());
            }
            // Trust the declared length enough to refuse early, then re-check
            // what actually arrived — a lying Content-Length must not get us to
            // buffer more than the cap.
            if let Some(len) = resp.content_length() {
                check_size(len as usize)?;
            }
            let bytes = resp.bytes().await?.to_vec();
            check_size(bytes.len())?;
            Ok((bytes, filename_from_url(url)))
        }
    }
}

fn check_size(len: usize) -> anyhow::Result<()> {
    if len > MAX_ATTACHMENT_BYTES {
        anyhow::bail!(
            "attachment is {len} bytes, over the {MAX_ATTACHMENT_BYTES} byte limit apollo will buffer"
        );
    }
    Ok(())
}

/// The last path segment of a URL, ignoring query and fragment.
///
/// Deliberately conservative: anything without a usable segment becomes
/// `upload`, and separators are stripped so a crafted URL cannot smuggle a
/// path into a multipart filename.
fn filename_from_url(url: &str) -> String {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/');
    let candidate = path.rsplit('/').next().unwrap_or("");
    let cleaned: String = candidate
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\0'))
        .collect();
    let cleaned = cleaned.trim_start_matches('.').to_string();
    if cleaned.is_empty() {
        "upload".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_come_from_the_last_segment() {
        assert_eq!(filename_from_url("https://x/y/cat.png"), "cat.png");
        assert_eq!(filename_from_url("https://x/y/cat.png?v=2"), "cat.png");
        assert_eq!(filename_from_url("https://x/y/cat.png#frag"), "cat.png");
        assert_eq!(filename_from_url("https://x/y/"), "y");
    }

    #[test]
    fn hostile_urls_cannot_smuggle_a_path() {
        // No segment at all, and no way to walk out of an upload directory.
        assert_eq!(filename_from_url("https://x"), "x");
        assert_eq!(filename_from_url("https://x/"), "x");
        assert_eq!(filename_from_url("https://x/%2e%2e/etc/passwd"), "passwd");
        assert_eq!(filename_from_url("https://x/..."), "upload");
        for name in [
            filename_from_url("https://x/a/../../etc/passwd"),
            filename_from_url("https://x/y/..%2f..%2fpasswd"),
        ] {
            assert!(!name.contains('/'), "{name}");
            assert!(!name.contains('\\'), "{name}");
        }
    }

    #[tokio::test]
    async fn missing_local_file_names_the_path() {
        let err = load(&MediaSource::Path("/nonexistent/apollo/x.png".into()))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/apollo/x.png"), "{err}");
    }
}
