use std::sync::OnceLock;
use std::time::Duration;

fn build(timeout: Option<Duration>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Process-wide client with no explicit timeout.
pub fn shared() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build(None)).clone()
}

/// Process-wide client with a 30s timeout for ordinary API calls.
pub fn standard() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| build(Some(Duration::from_secs(30))))
        .clone()
}

/// Process-wide client with a 120s timeout for model inference calls.
pub fn long() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| build(Some(Duration::from_secs(120))))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_shared_returns_client() {
        let _client = shared();
    }

    #[test]
    fn test_standard_returns_client() {
        let _client = standard();
    }

    #[test]
    fn test_long_returns_client() {
        let _client = long();
    }

    #[test]
    fn test_thread_safe_initialization() {
        let handles: Vec<_> = (0..10)
            .map(|_| {
                thread::spawn(|| {
                    let _c1 = shared();
                    let _c2 = standard();
                    let _c3 = long();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
