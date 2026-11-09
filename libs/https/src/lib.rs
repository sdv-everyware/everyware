//! HTTPS for the service bundles that talk to the internet (the weather, the news): hyper over
//! tokio-openssl over BoringSSL, the TLS stack SDV itself uses for SDV-RPC (one TLS stack per
//! process: `bssl_sys` cannot be linked twice, so no rustls). The server's certificate is checked
//! against the system CA store and the host name. HTTPS only, HTTP/1.1, one connection per
//! request, redirects followed, the body limited in size and the whole request in time.

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1;
use hyper::Request;
use hyper_util::rt::TokioIo;
use log::warn;
use openssl::ssl::{SslConnector, SslMethod};
use openssl::x509::X509;
use std::error::Error;
use std::fs;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_openssl::SslStream;

/// The platform's CA certificates (one PEM file each).
const CA_DIR: &str = "/system/etc/security/cacerts";
/// A request with its redirects, at most.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Redirects followed, at most.
const MAX_REDIRECTS: usize = 5;
const USER_AGENT: &str = "sdv-everyware/1";

/// The error of a request.
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// A successful response (2xx).
#[derive(Clone, Debug)]
pub struct Response {
    /// The URL that answered (after the redirects).
    pub url: String,
    /// The Content-Type header, as sent.
    pub content_type: String,
    pub body: Vec<u8>,
}

/// An https URL taken apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    /// The path with the query, from the first '/'.
    pub path: String,
}

impl Url {
    /// Parses `https://host[:port][/path][?query]`; other schemes are refused.
    pub fn parse(url: &str) -> Result<Url> {
        let rest = url.strip_prefix("https://").ok_or_else(|| format!("not an https URL: {url}"))?;
        let rest = rest.split('#').next().unwrap_or("");
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let authority = authority.rsplit('@').next().unwrap_or("");
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse()?),
            _ => (authority, 443),
        };
        if host.is_empty() {
            return Err(format!("no host in {url}").into());
        }
        Ok(Url { host: host.to_ascii_lowercase(), port, path })
    }

    /// `location` (a redirect, a link in a page) resolved against this URL.
    pub fn join(&self, location: &str) -> Result<Url> {
        let location = location.trim();
        if location.starts_with("https://") {
            Url::parse(location)
        } else if let Some(rest) = location.strip_prefix("//") {
            Url::parse(&format!("https://{rest}"))
        } else if location.starts_with('/') {
            Ok(Url { path: location.to_string(), ..self.clone() })
        } else if location.contains("://") {
            Err(format!("not an https URL: {location}").into())
        } else {
            let dir = self.path.split('?').next().unwrap_or("/");
            let dir = &dir[..dir.rfind('/').map_or(1, |i| i + 1)];
            Ok(Url { path: format!("{dir}{location}"), ..self.clone() })
        }
    }

    /// The URL as text.
    pub fn to_url(&self) -> String {
        if self.port == 443 {
            format!("https://{}{}", self.host, self.path)
        } else {
            format!("https://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

/// GET `url`, following redirects; a body longer than `max_body` bytes is an error.
pub async fn get(url: &str, max_body: usize) -> Result<Response> {
    timeout(TIMEOUT, follow(Url::parse(url)?, max_body)).await.map_err(|_| "timeout")?
}

/// GET `url` as text (UTF-8, invalid sequences replaced).
pub async fn get_text(url: &str, max_body: usize) -> Result<String> {
    let response = get(url, max_body).await?;
    Ok(String::from_utf8_lossy(&response.body).into_owned())
}

async fn follow(mut url: Url, max_body: usize) -> Result<Response> {
    for _ in 0..=MAX_REDIRECTS {
        match once(&url, max_body).await? {
            Answer::Done(content_type, body) => return Ok(Response { url: url.to_url(), content_type, body }),
            Answer::Redirect(location) => url = url.join(&location)?,
        }
    }
    Err(format!("more than {MAX_REDIRECTS} redirects").into())
}

enum Answer {
    Done(String, Vec<u8>),
    Redirect(String),
}

/// One request on a new connection.
async fn once(url: &Url, max_body: usize) -> Result<Answer> {
    // The name goes through getaddrinfo(), so through dnsproxyd on a device without netd.
    let tcp = TcpStream::connect((url.host.as_str(), url.port)).await?;
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    if add_system_cas(builder.cert_store_mut()) == 0 {
        warn!("no CA certificates in {CA_DIR}");
    }
    // configure() turns on SNI and host name verification for the host.
    let ssl = builder.build().configure()?.into_ssl(&url.host)?;
    let mut tls = SslStream::new(ssl, tcp)?;
    Pin::new(&mut tls).connect().await?;

    let (mut sender, connection) = http1::handshake(TokioIo::new(tls)).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            log::debug!("connection: {err}");
        }
    });
    let request = Request::get(url.path.as_str())
        .header("host", url.host.as_str())
        .header("user-agent", USER_AGENT)
        .header("accept", "*/*")
        .body(Empty::<Bytes>::new())?;
    let response = sender.send_request(request).await?;
    let status = response.status();
    if status.is_redirection() {
        let location = response.headers().get("location").and_then(|v| v.to_str().ok()).ok_or_else(|| format!("HTTP {status} without a location"))?;
        return Ok(Answer::Redirect(location.to_string()));
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}").into());
    }
    let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            if bytes.len() + data.len() > max_body {
                return Err(format!("body over {max_body} bytes").into());
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(Answer::Done(content_type, bytes))
}

/// Adds the certificates of the Android CA store; returns how many.
fn add_system_cas(store: &mut openssl::x509::store::X509StoreBuilderRef) -> usize {
    let Ok(dir) = fs::read_dir(CA_DIR) else { return 0 };
    dir.filter_map(|entry| fs::read(entry.ok()?.path()).ok())
        .filter_map(|pem| X509::from_pem(&pem).ok())
        .filter(|cert| store.add_cert(cert.clone()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_taken_apart() {
        assert_eq!(Url::parse("https://news.ycombinator.com/item?id=1").unwrap(), Url { host: "news.ycombinator.com".into(), port: 443, path: "/item?id=1".into() });
        assert_eq!(Url::parse("https://Example.org:8443").unwrap(), Url { host: "example.org".into(), port: 8443, path: "/".into() });
        assert_eq!(Url::parse("https://a.b?x=1#frag").unwrap().path, "/?x=1");
        assert!(Url::parse("http://example.org/").is_err());
    }

    #[test]
    fn locations_are_resolved() {
        let base = Url::parse("https://example.org/blog/post.html?x=1").unwrap();
        assert_eq!(base.join("/img/a.png").unwrap().to_url(), "https://example.org/img/a.png");
        assert_eq!(base.join("a.png").unwrap().to_url(), "https://example.org/blog/a.png");
        assert_eq!(base.join("//cdn.example.org/a.png").unwrap().to_url(), "https://cdn.example.org/a.png");
        assert_eq!(base.join("https://other.org/").unwrap().host, "other.org");
        assert!(base.join("http://other.org/").is_err());
    }
}
