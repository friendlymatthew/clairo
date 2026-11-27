use std::{
    borrow::Cow,
    hash::{Hash, Hasher},
    io::{ErrorKind, Read, Write},
    net::{IpAddr, TcpStream},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use url::{Host, Position, Url};

use compression::GZipReader;
use utils::impl_read_slice;

fn tls_config() -> Arc<ClientConfig> {
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

pub fn get(url: &str) -> Result<Http1Response> {
    let parsed = Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https",
        "only https:// urls are supported"
    );
    let response = get_response(&parsed, None, None)?;
    ensure!(
        response.status == HttpStatus::Ok,
        "HTTP {} for {url}",
        response.status.as_u16()
    );
    Ok(response)
}

pub fn get_following_redirects(url: &str) -> Result<(Url, Http1Response)> {
    let mut url = Url::parse(url)?;
    for _ in 0..=10 {
        ensure!(url.scheme() == "https", "only https:// urls are supported");
        let response = get_response(&url, None, None)?;
        match response.status.as_u16() {
            200 => return Ok((url, response)),
            301 | 302 | 303 | 307 | 308 => {
                let location = response
                    .header("location")
                    .context("redirect has no location")?;
                let mut next = url.join(std::str::from_utf8(location)?)?;
                if next.fragment().is_none() {
                    next.set_fragment(url.fragment());
                }
                url = next;
            }
            status => bail!("HTTP {status} for {url}"),
        }
    }
    bail!("too many redirects for {url}")
}

pub(crate) fn get_response(
    url: &Url,
    etag: Option<&str>,
    modified: Option<&str>,
) -> Result<Http1Response> {
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "unsupported URL scheme"
    );

    let mut connection = Err(std::io::Error::new(
        ErrorKind::NotFound,
        "no addresses for host",
    ));
    for address in url.socket_addrs(|| None)? {
        connection = TcpStream::connect_timeout(&address, std::time::Duration::from_secs(5));
        if connection.is_ok() {
            break;
        }
    }
    let tcp = connection?;
    let timeout = Some(std::time::Duration::from_secs(15));
    tcp.set_read_timeout(timeout)?;
    tcp.set_write_timeout(timeout)?;
    if url.scheme() == "http" {
        return exchange(tcp, url, etag, modified);
    }
    let server_name = match url.host().ok_or_else(|| anyhow!("url has no host"))? {
        Host::Domain(domain) => ServerName::try_from(domain.to_owned())?,
        Host::Ipv4(ip) => ServerName::from(IpAddr::V4(ip)),
        Host::Ipv6(ip) => ServerName::from(IpAddr::V6(ip)),
    };
    let conn = rustls::ClientConnection::new(tls_config(), server_name)?;
    exchange(rustls::StreamOwned::new(conn, tcp), url, etag, modified)
}

fn exchange(
    mut stream: impl Read + Write,
    url: &Url,
    etag: Option<&str>,
    modified: Option<&str>,
) -> Result<Http1Response> {
    let mut request = request_head(url)?;
    request.truncate(request.len() - 2);
    for (name, value) in [("If-None-Match", etag), ("If-Modified-Since", modified)] {
        if let Some(value) = value {
            ensure!(
                value.bytes().all(|byte| (32..=126).contains(&byte)),
                "invalid validator bytes"
            );
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes())?;
    stream.flush()?;

    let mut buf = Vec::new();
    const RESPONSE_LIMIT: u64 = 32 * 1024 * 1024;
    match stream.take(RESPONSE_LIMIT + 1).read_to_end(&mut buf) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(e.into()),
    }
    ensure!(
        buf.len() as u64 <= RESPONSE_LIMIT,
        "HTTP response exceeds 32 MiB"
    );
    let mut reader = Http1MessageReader::new(&buf);
    loop {
        let response = reader.read()?;
        if !response.status.is_informational() || response.status.as_u16() == 101 {
            return Ok(response);
        }
    }
}

fn request_head(url: &Url) -> Result<String> {
    let host = url.host_str().ok_or_else(|| anyhow!("url has no host"))?;

    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };

    let target = &url[Position::BeforePath..Position::AfterQuery];

    Ok(format!(
        "GET {target} HTTP/1.1\r\n\
         Host: {host}\r\n\
         User-Agent: desole/0.1\r\n\
         Accept-Encoding: gzip\r\n\
         Connection: close\r\n\
         \r\n"
    ))
}

#[derive(Debug)]
pub struct Http1MessageReader<'a> {
    cursor: usize,
    data: &'a [u8],
}

impl<'a> Http1MessageReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { cursor: 0, data }
    }

    pub fn read(&mut self) -> Result<Http1Response> {
        self.read_slice_expect(b"HTTP/1.1 ")?;

        let status = self.read_status_code()?;

        self.read_slice_expect(b" ")?;
        let _reason = self.read_slice_until(b'\r')?;
        self.read_slice_expect(b"\n")?;

        let mut res = Http1Response {
            status,
            headers: self
                .read_headers()?
                .into_iter()
                .map(|(name, value)| (HeaderName(name.into()), value.to_vec()))
                .collect(),
            body: Vec::new(),
        };

        res.body = self.read_body(&res)?;

        Ok(res)
    }

    fn read_status_code(&mut self) -> Result<HttpStatus> {
        let digits = self.read_array::<3>()?;
        ensure!(digits.iter().all(u8::is_ascii_digit), "bad status code");

        let c = digits.iter().fold(0u16, |n, &d| n * 10 + (d - b'0') as u16);

        Ok(HttpStatus::from(c))
    }

    fn read_body(&mut self, res: &Http1Response) -> Result<Vec<u8>> {
        if res.status.forbids_body() {
            return Ok(Vec::new());
        }

        if let Some(encoding) = res.header("transfer-encoding") {
            ensure!(
                encoding.eq_ignore_ascii_case(b"chunked"),
                "unsupported transfer-encoding {}",
                encoding.escape_ascii()
            );
            return self.read_chunked();
        }

        if let Some(len) = res.header("content-length") {
            let len = parse_number(len.trim_ascii(), 10)?;

            return Ok(self.read_slice(len)?.to_vec());
        }

        Ok(self.read_rest().to_vec())
    }

    fn read_chunked(&mut self) -> Result<Vec<u8>> {
        let mut body = Vec::new();

        loop {
            let line = self.read_slice_until(b'\r')?;
            self.read_slice_expect(b"\n")?;

            let size = line.split(|&b| b == b';').next().unwrap_or_default();
            let size = parse_number(size.trim_ascii(), 16)?;

            if size == 0 {
                break;
            }

            body.extend_from_slice(self.read_slice(size)?);
            self.read_slice_expect(b"\r\n")?;
        }

        while self.read_header()?.is_some() {}

        Ok(body)
    }

    fn read_rest(&mut self) -> &'a [u8] {
        let rest = &self.data[self.cursor..];
        self.cursor = self.data.len();

        rest
    }

    fn read_headers(&mut self) -> Result<Vec<(&'a [u8], &'a [u8])>> {
        let mut out = Vec::new();

        while let Some((name, value)) = self.read_header()? {
            out.push((name, value));
        }

        Ok(out)
    }

    fn read_header(&mut self) -> Result<Option<(&'a [u8], &'a [u8])>> {
        let line = self.read_slice_until(b'\r')?;
        self.read_slice_expect(b"\n")?;

        if line.is_empty() {
            return Ok(None);
        }

        let colon = line
            .iter()
            .position(|&b| b == b':')
            .ok_or_else(|| anyhow!("header line without ':'"))?;
        let (name, value) = (&line[..colon], &line[colon + 1..]);

        ensure!(
            !name.is_empty()
                && name.iter().all(|&byte| {
                    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^`_|~".contains(&byte)
                }),
            "bad field name"
        );

        Ok(Some((name, value.trim_ascii())))
    }

    impl_read_slice!();
}

#[derive(Debug)]
pub struct HeaderName(Box<[u8]>);

impl HeaderName {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn matches(&self, name: &str) -> bool {
        self.0.eq_ignore_ascii_case(name.as_bytes())
    }
}

impl PartialEq for HeaderName {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Eq for HeaderName {}

impl Hash for HeaderName {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.len().hash(state);
        for byte in &self.0 {
            byte.to_ascii_lowercase().hash(state);
        }
    }
}

#[derive(Debug)]
pub struct Http1Response {
    pub status: HttpStatus,
    pub headers: Vec<(HeaderName, Vec<u8>)>,
    pub body: Vec<u8>,
}

impl Http1Response {
    pub fn header(&self, name: &str) -> Option<&[u8]> {
        self.headers
            .iter()
            .find(|(field, _)| field.matches(name))
            .map(|(_, value)| value.as_slice())
    }

    pub fn decoded_body(&self) -> Result<Cow<'_, [u8]>> {
        let mut body = Cow::Borrowed(&self.body[..]);

        let Some(codings) = self.header("content-encoding") else {
            return Ok(body);
        };

        // codings are listed in the order they were applied, so undo them in reverse
        for coding in codings.rsplit(|&b| b == b',').map(<[u8]>::trim_ascii) {
            if coding.eq_ignore_ascii_case(b"gzip") || coding.eq_ignore_ascii_case(b"x-gzip") {
                body = Cow::Owned(GZipReader::new(body.as_ref()).read()?);
            } else if !coding.is_empty() && !coding.eq_ignore_ascii_case(b"identity") {
                bail!("unsupported content-encoding {}", coding.escape_ascii());
            }
        }

        Ok(body)
    }
}

fn parse_number(digits: &[u8], radix: u32) -> Result<usize> {
    ensure!(!digits.is_empty(), "empty number");

    digits.iter().try_fold(0usize, |n, &d| {
        let v = (d as char)
            .to_digit(radix)
            .ok_or_else(|| anyhow!("bad digit {:?}", d as char))?;

        n.checked_mul(radix as usize)
            .and_then(|n| n.checked_add(v as usize))
            .ok_or_else(|| anyhow!("number overflow"))
    })
}

macro_rules! http_status_codes {
    ($($code:literal => $status:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum HttpStatus {
            $($status,)+
            Unknown(u16),
        }

        impl From<u16> for HttpStatus {
            fn from(code: u16) -> Self {
                match code {
                    $($code => Self::$status,)+
                    code => Self::Unknown(code),
                }
            }
        }

        impl HttpStatus {
            pub const fn as_u16(self) -> u16 {
                match self {
                    $(Self::$status => $code,)+
                    Self::Unknown(code) => code,
                }
            }
        }
    };
}

// https://www.iana.org/assignments/http-status-codes/
http_status_codes! {
    100 => Continue,
    101 => SwitchingProtocols,
    102 => Processing,
    103 => EarlyHints,
    104 => UploadResumptionSupported,

    200 => Ok,
    201 => Created,
    202 => Accepted,
    203 => NonAuthoritativeInformation,
    204 => NoContent,
    205 => ResetContent,
    206 => PartialContent,
    207 => MultiStatus,
    208 => AlreadyReported,
    226 => ImUsed,

    300 => MultipleChoices,
    301 => MovedPermanently,
    302 => Found,
    303 => SeeOther,
    304 => NotModified,
    305 => UseProxy,
    306 => Unused306,
    307 => TemporaryRedirect,
    308 => PermanentRedirect,

    400 => BadRequest,
    401 => Unauthorized,
    402 => PaymentRequired,
    403 => Forbidden,
    404 => NotFound,
    405 => MethodNotAllowed,
    406 => NotAcceptable,
    407 => ProxyAuthenticationRequired,
    408 => RequestTimeout,
    409 => Conflict,
    410 => Gone,
    411 => LengthRequired,
    412 => PreconditionFailed,
    413 => ContentTooLarge,
    414 => UriTooLong,
    415 => UnsupportedMediaType,
    416 => RangeNotSatisfiable,
    417 => ExpectationFailed,
    418 => Unused418,
    421 => MisdirectedRequest,
    422 => UnprocessableContent,
    423 => Locked,
    424 => FailedDependency,
    425 => TooEarly,
    426 => UpgradeRequired,
    428 => PreconditionRequired,
    429 => TooManyRequests,
    431 => RequestHeaderFieldsTooLarge,
    451 => UnavailableForLegalReasons,

    500 => InternalServerError,
    501 => NotImplemented,
    502 => BadGateway,
    503 => ServiceUnavailable,
    504 => GatewayTimeout,
    505 => HttpVersionNotSupported,
    506 => VariantAlsoNegotiates,
    507 => InsufficientStorage,
    508 => LoopDetected,
    510 => NotExtended,
    511 => NetworkAuthenticationRequired,
}

impl HttpStatus {
    pub const fn is_informational(self) -> bool {
        matches!(self.as_u16(), 100..=199)
    }

    /// status codes that end an http1.1 response at the header section
    /// regardless of its framing fields
    pub const fn forbids_body(self) -> bool {
        self.is_informational() || matches!(self, Self::NoContent | Self::NotModified)
    }
}
