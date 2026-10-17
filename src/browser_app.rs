use crate::{
    app::{App, AppDocs, AppId, AppToolExecutionContext, AppToolExecutionResult, AppToolSpec},
    model_protocol::AgentToolCall,
    schema_utils::model_schema_for,
};
use async_trait::async_trait;
use miette::{Result, miette};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
pub use desktop::*;

/// Browser navigation never confers local filesystem or privileged protocol
/// access, and does not consume credentials from a URL.
pub fn validate_browser_url(value: &str) -> Result<()> {
    if value.len() > 8192 {
        return Err(miette!("browser URL exceeds the size limit"));
    }
    let url = url::Url::parse(value)
        .map_err(|_| miette!("browser URL must be an absolute http or https URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(miette!("browser URL must use http or https"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(miette!("browser URLs cannot contain credentials"));
    }
    Ok(())
}

/// Read-only browsing uses HTTP GET with no browser engine, cookies, scripts,
/// service workers, ambient proxy credentials, or automatic redirects.
pub struct ReadOnlyBrowserApp {
    pages: HashMap<String, ReadOnlyPage>,
}

#[daat_locus_macros::model_schema]
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadUrlArgs {
    url: String,
    refresh: bool,
}

#[derive(Clone, Serialize)]
struct ReadOnlyPage {
    requested_url: String,
    url: String,
    status: u16,
    media_type: String,
    fetched_at_ms: u64,
    body: String,
}

const MAX_READ_URL_BYTES: usize = 1024 * 1024;
const MAX_READ_URL_REDIRECTS: usize = 5;
const MAX_READ_URL_CACHE: usize = 8;

impl Default for ReadOnlyBrowserApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadOnlyBrowserApp {
    pub fn new() -> Self {
        Self {
            pages: HashMap::new(),
        }
    }

    async fn read_url(&mut self, args: &ReadUrlArgs) -> Result<ReadOnlyPage> {
        validate_public_url(&args.url)?;
        if !args.refresh
            && let Some(page) = self.pages.get(&args.url)
        {
            return Ok(page.clone());
        }
        let page = tokio::time::timeout(Duration::from_secs(30), Self::fetch_public_url(args))
            .await
            .map_err(|_| miette!("HTTP GET deadline exceeded"))??;
        if self.pages.len() >= MAX_READ_URL_CACHE
            && !self.pages.contains_key(&args.url)
            && let Some(key) = self.pages.keys().next().cloned()
        {
            self.pages.remove(&key);
        }
        self.pages.insert(args.url.clone(), page.clone());
        Ok(page)
    }

    async fn fetch_public_url(args: &ReadUrlArgs) -> Result<ReadOnlyPage> {
        let mut current = validate_public_url(&args.url)?;
        for hop in 0..=MAX_READ_URL_REDIRECTS {
            let addresses = resolve_public_addresses(&current).await?;
            let host = current
                .host_str()
                .ok_or_else(|| miette!("HTTP host is missing"))?;
            // Each hop has a fresh resolver containing only addresses already
            // validated as public. DNS is never repeated during the connection.
            // No cookie provider is enabled; no ambient proxy is consulted.
            let client = crate::http_client::builder()
                .no_proxy()
                .http1_only()
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(host, &addresses)
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .build()
                .map_err(|_| miette!("read-only HTTP client is unavailable"))?;
            let mut response = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| miette!("HTTP GET failed"))?;
            if response.status().is_redirection() {
                if hop == MAX_READ_URL_REDIRECTS {
                    return Err(miette!("HTTP redirect limit exceeded"));
                }
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| miette!("HTTP redirect has no valid location"))?;
                current = safe_redirect_url(&current, location)?;
                continue;
            }
            let status = response.status().as_u16();
            let media_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("text/plain")
                .to_owned();
            if response
                .content_length()
                .is_some_and(|length| length > MAX_READ_URL_BYTES as u64)
            {
                return Err(miette!("HTTP body exceeds the size limit"));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| miette!("unable to read HTTP body"))?
            {
                if bytes.len().saturating_add(chunk.len()) > MAX_READ_URL_BYTES {
                    return Err(miette!("HTTP body exceeds the size limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
            let body =
                String::from_utf8(bytes).map_err(|_| miette!("HTTP body is not UTF-8 text"))?;
            let page = ReadOnlyPage {
                requested_url: args.url.clone(),
                url: current.into(),
                status,
                media_type,
                fetched_at_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                body,
            };
            return Ok(page);
        }
        Err(miette!("HTTP redirect limit exceeded"))
    }
}

fn safe_redirect_url(current: &url::Url, location: &str) -> Result<url::Url> {
    let target = current
        .join(location)
        .map_err(|_| miette!("invalid HTTP redirect URL"))?;
    validate_public_url(target.as_str())
}

fn validate_public_url(value: &str) -> Result<url::Url> {
    validate_browser_url(value)?;
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
                return Err(miette!(
                    "read-only Browser permits public Internet hosts only"
                ));
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
            "read-only Browser cannot access nonpublic network addresses"
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

#[async_trait]
impl App for ReadOnlyBrowserApp {
    fn can_passivate(&self) -> bool {
        // This is only a GET cache; tool observations already live in history.
        true
    }
    fn id(&self) -> AppId {
        AppId::browser()
    }
    fn docs(&self) -> AppDocs {
        AppDocs::from_markdown(include_str!("app_instructions/browser_read_only.md"))
    }
    fn tool_specs(&self) -> Vec<AppToolSpec> {
        vec![AppToolSpec { name: "read_url".into(), description: "Read URL source with HTTP GET, returning the final URL, status, media type, and original body.".into(), input_schema: model_schema_for::<ReadUrlArgs>() }]
    }
    async fn execute_tool(
        &mut self,
        call: &AgentToolCall,
        _: &AppToolExecutionContext,
    ) -> Result<AppToolExecutionResult> {
        if call.name != "read_url" {
            return Err(miette!("unknown read-only Browser tool"));
        }
        let args: ReadUrlArgs = serde_json::from_value(call.arguments.clone())
            .map_err(|_| miette!("invalid read_url arguments"))?;
        let page = self.read_url(&args).await?;
        let model_content = serde_json::to_string(&page)
            .map_err(|_| miette!("HTTP result serialization failed"))?;
        Ok(AppToolExecutionResult::new(
            format!("read {}", page.url),
            serde_json::to_value(page).map_err(|_| miette!("HTTP result serialization failed"))?,
            Some(model_content),
        ))
    }
    async fn shutdown(&mut self) -> Result<()> {
        self.pages.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_rejects_privileged_schemes_and_embedded_credentials() {
        for url in [
            "file:///C:/secret.txt",
            "data:text/plain,secret",
            "javascript:alert(1)",
            "chrome://settings",
            "https://user:password@example.com",
            "//example.com",
        ] {
            assert!(validate_browser_url(url).is_err(), "{url}");
        }
        assert!(validate_browser_url("https://example.com/document").is_ok());
    }

    #[tokio::test]
    async fn readonly_http_rejects_privileged_targets_before_dns_lookup() {
        let mut browser = ReadOnlyBrowserApp::new();
        let args = ReadUrlArgs {
            url: "file:///C:/secret.txt".into(),
            refresh: true,
        };
        assert!(browser.read_url(&args).await.is_err());
        assert!(browser.pages.is_empty());
        assert_eq!(browser.tool_specs()[0].name, "read_url");
        crate::schema_utils::validate_model_facing_schema(&browser.tool_specs()[0].input_schema)
            .unwrap();
    }

    #[test]
    fn readonly_redirects_validate_every_target() {
        let current = url::Url::parse("https://example.com/a/b").unwrap();
        assert_eq!(
            safe_redirect_url(&current, "../c").unwrap().as_str(),
            "https://example.com/c"
        );
        for location in [
            "file:///secret",
            "data:text/plain,secret",
            "https://name:secret@example.com",
            "http://127.1/private",
            "http://169.254.169.254/metadata",
            "http://[::ffff:192.168.1.1]/",
            "http://router.local/path",
        ] {
            assert!(safe_redirect_url(&current, location).is_err());
        }
    }

    #[tokio::test]
    async fn readonly_cache_is_local_and_preserves_source() {
        let mut browser = ReadOnlyBrowserApp::new();
        let args = ReadUrlArgs {
            url: "https://example.com/a".into(),
            refresh: false,
        };
        browser.pages.insert(
            args.url.clone(),
            ReadOnlyPage {
                requested_url: args.url.clone(),
                url: "https://example.com/b".into(),
                status: 200,
                media_type: "text/html".into(),
                fetched_at_ms: 1,
                body: "<script>write()</script>original".into(),
            },
        );
        let page = browser.read_url(&args).await.unwrap();
        assert_eq!(page.url, "https://example.com/b");
        assert_eq!(page.body, "<script>write()</script>original");
        assert!(ReadOnlyBrowserApp::new().pages.is_empty());
    }

    #[test]
    fn readonly_public_targets_reject_private_reserved_and_mapped_addresses() {
        for target in [
            "http://127.0.0.1",
            "http://127.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://0177.0.0.1",
            "http://0.1.2.3",
            "http://10.0.0.1",
            "http://100.64.0.1",
            "http://169.254.169.254",
            "http://172.31.0.1",
            "http://192.168.1.1",
            "http://192.0.0.1",
            "http://192.0.2.1",
            "http://192.88.99.1",
            "http://198.18.0.1",
            "http://198.51.100.1",
            "http://203.0.113.1",
            "http://224.0.0.1",
            "http://240.0.0.1",
            "http://255.255.255.255",
            "http://[::]",
            "http://[::1]",
            "http://[::ffff:127.0.0.1]",
            "http://[::ffff:c0a8:1]",
            "http://[fe80::1]",
            "http://[fc00::1]",
            "http://[ff02::1]",
            "http://[2001:db8::1]",
            "http://[2001::1]",
            "http://[2002:a00:1::]",
            "http://[64:ff9b::a00:1]",
            "http://[3fff::1]",
            "http://localhost",
            "http://LOCALHOST.",
            "http://a.localhost",
            "http://router.local",
            "http://corp.internal",
            "http://host.home.arpa",
        ] {
            assert!(validate_public_url(target).is_err(), "{target}");
        }
        for target in [
            "https://example.com",
            "https://8.8.8.8",
            "https://[2606:4700:4700::1111]",
            "https://[::ffff:8.8.8.8]",
        ] {
            assert!(validate_public_url(target).is_ok(), "{target}");
        }
    }

    #[test]
    fn readonly_dns_address_set_rejects_any_private_answer_and_bounds_count() {
        let public = "8.8.8.8:443".parse::<SocketAddr>().unwrap();
        let private = "127.0.0.1:443".parse::<SocketAddr>().unwrap();
        assert!(validate_public_addresses(vec![public, private]).is_err());
        assert!(validate_public_addresses(vec![]).is_err());
        assert!(validate_public_addresses(vec![public; 33]).is_err());
        assert_eq!(
            validate_public_addresses(vec![public, public]).unwrap(),
            [public]
        );
    }
}
