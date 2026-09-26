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
    fn test_clients_initialization() {
        let _ = shared();
        let _ = standard();
        let _ = long();
    }

    #[test]
    fn test_clients_thread_safety() {
        let mut handles = vec![];
        for _ in 0..10 {
            handles.push(thread::spawn(|| {
                let _ = shared();
                let _ = standard();
                let _ = long();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
