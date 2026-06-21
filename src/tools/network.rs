use std::net::IpAddr;

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            octets[0] == 127
                || octets[0] == 10
                || (octets[0] == 172 && (16..=31).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 168)
                || (octets[0] == 169 && octets[1] == 254)
        }
        IpAddr::V6(ipv6) => ipv6.is_loopback() || ((ipv6.segments()[0] & 0xfe00) == 0xfc00),
    }
}

fn host_matches_allowlist(host: &str, allowed_domains: &[String]) -> bool {
    allowed_domains.iter().any(|domain| {
        host.eq_ignore_ascii_case(domain)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", domain.to_ascii_lowercase()))
    })
}

pub async fn validate_public_http_url(
    url: &str,
    allowed_domains: &[String],
) -> anyhow::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).map_err(|e| anyhow::anyhow!("Invalid URL: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => anyhow::bail!("Unsupported URL scheme: {}", other),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("URL is missing a host"))?;

    if host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("0.0.0.0")
        || host.ends_with(".localhost")
    {
        anyhow::bail!("Requests to local hosts are blocked");
    }

    if !allowed_domains.is_empty() && !host_matches_allowlist(host, allowed_domains) {
        anyhow::bail!("Domain '{}' is not in the allowed list", host);
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            anyhow::bail!("Requests to private IP addresses are blocked");
        }
        return Ok(parsed);
    }

    let port = parsed.port_or_known_default().unwrap_or(80);
    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve host '{}': {}", host, e))?;

    let mut saw_address = false;
    for addr in resolved {
        saw_address = true;
        if is_private_ip(addr.ip()) {
            anyhow::bail!("Host '{}' resolves to a private address", host);
        }
    }

    if !saw_address {
        anyhow::bail!("Host '{}' did not resolve to any addresses", host);
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_localhost() {
        let err = validate_public_http_url("http://localhost:8080", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("local hosts"));
    }

    #[tokio::test]
    async fn rejects_private_ip() {
        let err = validate_public_http_url("https://127.0.0.1", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("private IP"));
    }

    #[tokio::test]
    async fn rejects_bad_scheme() {
        let err = validate_public_http_url("file:///etc/passwd", &[])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unsupported URL scheme"));
    }
}
