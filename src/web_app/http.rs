use miette::{Result, miette};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};
const MAX_READ_URL_REDIRECTS: usize = 5;
pub(super) struct Page {
    pub url: String,
    pub media_type: String,
    pub body: String,
}
pub(super) async fn fetch(value: &str) -> Result<Page> {
    tokio::time::timeout(Duration::from_secs(30), fetch_inner(value))
        .await
        .map_err(|_| miette!("Web HTTP deadline exceeded"))?
}
async fn fetch_inner(value: &str) -> Result<Page> {
    let mut current = validate_public_url(value)?;
    for hop in 0..=MAX_READ_URL_REDIRECTS {
        let addresses = resolve_public_addresses(&current).await?;
        let host = current
            .host_str()
            .ok_or_else(|| miette!("HTTP host missing"))?;
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| miette!("Web HTTP client unavailable"))?;
        let mut response = client
            .get(current.clone())
            .header(
                reqwest::header::ACCEPT,
                "text/html,application/xhtml+xml,text/plain,application/json;q=0.8",
            )
            .send()
            .await
            .map_err(|_| miette!("Web HTTP GET failed"))?;
        if response.status().is_redirection() {
            if hop == MAX_READ_URL_REDIRECTS {
                return Err(miette!("Web redirect limit exceeded"));
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| miette!("Web redirect has no location"))?;
            current = safe_redirect_url(&current, location)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(miette!(
                "Web returned HTTP {} for {}",
                response.status().as_u16(),
                current
            ));
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .to_owned();
        let mime = media_type.split(';').next().unwrap_or("").trim();
        if !mime.starts_with("text/")
            && !matches!(
                mime,
                "application/xhtml+xml" | "application/json" | "application/xml"
            )
        {
            return Err(miette!(
                "Web cannot extract {mime}; use a resource viewer instead"
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| miette!("Web body read failed"))?
        {
            if bytes.len() + chunk.len() > daat_locus_client_protocol::web::MAX_CAPTURE_BYTES {
                return Err(miette!(
                    "Web document exceeds the capture budget; no partial body was substituted"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = decode(&media_type, &bytes)?;
        return Ok(Page {
            url: current.into(),
            media_type,
            body,
        });
    }
    Err(miette!("Web redirect limit exceeded"))
}
fn charset(content_type: &str) -> Option<&'static encoding_rs::Encoding> {
    content_type
        .split(';')
        .find_map(|part| {
            part.split_once('=')
                .filter(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        })
        .and_then(|(_, label)| {
            encoding_rs::Encoding::for_label(label.trim().trim_matches(['\"', '\'']).as_bytes())
        })
}
fn decode(content_type: &str, bytes: &[u8]) -> Result<String> {
    let encoding = encoding_rs::Encoding::for_bom(bytes)
        .map(|(encoding, _)| encoding)
        .or_else(|| charset(content_type))
        .or_else(|| {
            if !content_type.to_ascii_lowercase().contains("html") {
                return None;
            }
            let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]);
            let head = scraper::Html::parse_document(&prefix);
            let selector = scraper::Selector::parse("meta").expect("static selector");
            head.select(&selector).find_map(|meta| {
                meta.attr("charset")
                    .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
                    .or_else(|| {
                        if meta
                            .attr("http-equiv")
                            .is_some_and(|value| value.eq_ignore_ascii_case("content-type"))
                        {
                            meta.attr("content").and_then(charset)
                        } else {
                            None
                        }
                    })
            })
        })
        .unwrap_or(encoding_rs::UTF_8);
    let (body, _, errors) = encoding.decode(bytes);
    if errors {
        return Err(miette!(
            "Web document encoding could not be decoded without data loss"
        ));
    }
    Ok(body.into_owned())
}
fn safe_redirect_url(current: &url::Url, location: &str) -> Result<url::Url> {
    let target = current
        .join(location)
        .map_err(|_| miette!("invalid HTTP redirect URL"))?;
    validate_public_url(target.as_str())
}

fn validate_public_url(value: &str) -> Result<url::Url> {
    super::validate_url(value)?;
    let target = url::Url::parse(value).map_err(|_| miette!("invalid HTTP URL"))?;
    match target
        .host()
        .ok_or_else(|| miette!("HTTP host is missing"))?
    {
        url::Host::Domain(host) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            if !name.contains('.')
                || [
                    "localhost",
                    "local",
                    "localdomain",
                    "internal",
                    "home.arpa",
                    "test",
                    "invalid",
                    "example",
                    "onion",
                ]
                .iter()
                .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
            {
                return Err(miette!("HTTP Web permits public Internet hosts only"));
            }
        }
        url::Host::Ipv4(ip) => ensure_public_ip(IpAddr::V4(ip))?,
        url::Host::Ipv6(ip) => ensure_public_ip(IpAddr::V6(ip))?,
    }
    Ok(target)
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127)
        && a < 224
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && ((b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99) || b == 168))
        && !(a == 198 && (matches!(b, 18 | 19) || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}

fn public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return public_ipv4(ipv4);
    }
    let segments = ip.segments();
    // Conservatively allow ordinary global unicast. Transition/NAT64 ranges
    // and special allocations can encode another address or route internally.
    segments[0] & 0xe000 == 0x2000
        && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
        && !matches!(segments[0], 0x2002 | 0x3ffe | 0x3fff)
}

fn ensure_public_ip(ip: IpAddr) -> Result<()> {
    if match ip {
        IpAddr::V4(ip) => public_ipv4(ip),
        IpAddr::V6(ip) => public_ipv6(ip),
    } {
        Ok(())
    } else {
        Err(miette!(
            "HTTP Web cannot access nonpublic network addresses"
        ))
    }
}

fn validate_public_addresses(mut addresses: Vec<SocketAddr>) -> Result<Vec<SocketAddr>> {
    if addresses.is_empty() || addresses.len() > 32 {
        return Err(miette!("HTTP host has no usable bounded address set"));
    }
    for address in &addresses {
        ensure_public_ip(address.ip())?;
    }
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

async fn resolve_public_addresses(target: &url::Url) -> Result<Vec<SocketAddr>> {
    validate_public_url(target.as_str())?;
    let port = target
        .port_or_known_default()
        .ok_or_else(|| miette!("HTTP port is missing"))?;
    let addresses = match target
        .host()
        .ok_or_else(|| miette!("HTTP host is missing"))?
    {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        url::Host::Domain(host) => tokio::time::timeout(
            Duration::from_secs(5),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| miette!("HTTP DNS lookup timed out"))?
        .map_err(|_| miette!("HTTP DNS lookup failed"))?
        .take(33)
        .collect(),
    };
    validate_public_addresses(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn http_decoding_honors_header_and_html_charset_without_replacing_bad_bytes() {
        let (bytes, _, _) = encoding_rs::GBK.encode("<meta charset='gb2312'><main>中文网页</main>");
        assert!(decode("text/html", &bytes).unwrap().contains("中文网页"));
        assert_eq!(
            decode("text/plain; Charset=\"windows-1252\"", b"caf\xe9").unwrap(),
            "café"
        );
        assert!(decode("text/plain; charset=utf-8", b"\xff").is_err());
    }
    #[tokio::test]
    async fn public_http_rejects_privileged_private_and_credential_targets_before_networking() {
        for url in [
            "file:///secret",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://169.254.169.254/metadata",
            "http://10.0.0.1",
            "http://[::ffff:192.168.1.1]",
            "http://router.local",
            "http://host.home.arpa",
            "https://name:secret@example.com",
        ] {
            assert!(fetch(url).await.is_err(), "{url}");
        }
    }
    #[test]
    fn redirects_and_dns_answers_cannot_cross_the_public_network_boundary() {
        let current = url::Url::parse("https://example.com/a/b").unwrap();
        assert_eq!(
            safe_redirect_url(&current, "../c").unwrap().as_str(),
            "https://example.com/c"
        );
        for value in [
            "http://localhost",
            "http://[::1]",
            "data:text/plain,test",
            "http://192.168.1.1",
        ] {
            assert!(safe_redirect_url(&current, value).is_err());
        }
        let public = "8.8.8.8:443".parse().unwrap();
        let private = "127.0.0.1:443".parse().unwrap();
        assert!(validate_public_addresses(vec![public, private]).is_err());
        assert!(validate_public_addresses(vec![]).is_err());
        assert!(validate_public_addresses(vec![public; 33]).is_err());
        assert_eq!(
            validate_public_addresses(vec![public, public]).unwrap(),
            [public]
        );
    }
}
