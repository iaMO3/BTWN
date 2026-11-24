use crate::control::{self, Command, Control, Pause};
use crate::storage::*;
use anyhow::{Context, Result, bail, ensure};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

pub const PC: &str = "DESKTOP";
pub const LAPTOP: &str = "LAPTOP";
const MAX_META: u64 = 16 * 1024 * 1024;
const MAX_FILE: u64 = 80 * 1024 * 1024 * 1024;
pub const MAX_DEVICE_NAME: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Desktop,
    Laptop,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Laptop => "Laptop",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceIcon {
    #[default]
    Auto,
    Monitor,
    Notebook,
    Tower,
    Mini,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct DeviceStyle {
    pub kind: Option<DeviceKind>,
    pub icon: DeviceIcon,
}

impl DeviceStyle {
    pub fn label(self) -> &'static str {
        self.kind.map_or("PC", DeviceKind::label)
    }

    pub fn icon(self) -> DeviceIcon {
        match (self.icon, self.kind) {
            (DeviceIcon::Auto, Some(DeviceKind::Laptop)) => DeviceIcon::Notebook,
            (DeviceIcon::Auto, _) => DeviceIcon::Monitor,
            (icon, _) => icon,
        }
    }
}

fn valid_device_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_DEVICE_NAME
        && !name.chars().any(char::is_control)
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Pair {
    pub address: Ipv4Addr,
    pub port: u16,
    pub fingerprint: String,
    pub token: String,
}

pub(crate) fn private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        _ => false,
    }
}

impl Pair {
    pub fn from_file(file: &Path) -> Result<Self> {
        let pair: Self = read(file)?;
        ensure!(
            is_hash(&pair.fingerprint)
                && is_hash(&pair.token)
                && private(pair.address.into())
                && pair.port != 0,
            "Invalid Between pairing file"
        );
        Ok(pair)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Offer {
    pub source: String,
    pub snapshot: String,
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pointer {
    pub snapshot: String,
    pub manifest_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Ready,
    Waiting,
    Receiving,
    Applying,
    Done,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub chat_conflicts: usize,
    pub project_conflicts: usize,
    pub sidebar_verified: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub snapshot: String,
    pub phase: Phase,
    pub detail: String,
    pub counts: Option<(u64, u64)>,
    pub at: u64,
    #[serde(default)]
    pub job: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

impl Status {
    pub fn new(snapshot: impl Into<String>, phase: Phase, detail: &str) -> Self {
        Self {
            snapshot: snapshot.into(),
            phase,
            detail: detail.chars().take(800).collect(),
            counts: None,
            at: Self::now(),
            job: 0,
            outcome: None,
        }
    }

    pub fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    pub fn with_counts(mut self, done: u64, total: u64) -> Self {
        self.counts = Some((done, total));
        self
    }

    pub fn for_job(mut self, job: u64) -> Self {
        self.job = job;
        self
    }
}

pub fn status_path(store: &Path, source: &str, receipt: bool) -> PathBuf {
    let side = if receipt { "receipt" } else { "offer" };
    store
        .join("deliveries")
        .join(format!("{source}-{side}.json"))
}

fn valid_snapshot(snapshot: &str) -> bool {
    let t = snapshot.as_bytes();
    t.len() == 24
        && t[8] == b'-'
        && t[15] == b'-'
        && t[..8].iter().chain(&t[9..15]).all(u8::is_ascii_digit)
        && t[16..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

impl Offer {
    pub fn validate(&self, source: &str) -> Result<()> {
        ensure!(
            [PC, LAPTOP].contains(&source) && self.source == source,
            "Backup was sent by a different computer"
        );
        ensure!(valid_snapshot(&self.snapshot), "Invalid backup date");
        ensure!(self.files.len() <= 10_000, "Backup has too many files");
        let data = format!("Snapshots/{}/", self.snapshot);
        let projects = format!("ProjectArchives/{}/", self.snapshot);
        let mut seen = HashSet::new();
        for row in &self.files {
            let permitted = row.path.starts_with(&data)
                || row
                    .path
                    .strip_prefix(&projects)
                    .and_then(|p| p.strip_suffix(".zip"))
                    .is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
            ensure!(
                permitted
                    && is_hash(&row.sha256)
                    && row.bytes <= MAX_FILE
                    && seen.insert(row.path.to_ascii_lowercase()),
                "Invalid file in backup index"
            );
            scoped(Path::new(""), &row.path)?;
        }
        self.pointer()?;
        Ok(())
    }

    pub fn pointer(&self) -> Result<Pointer> {
        let index = format!("Snapshots/{}/TRANSFER.json", self.snapshot);
        let item = self
            .files
            .iter()
            .find(|row| row.path == index)
            .context("Backup index is missing")?;
        Ok(Pointer {
            snapshot: self.snapshot.clone(),
            manifest_sha256: item.sha256.clone(),
        })
    }
}

pub fn object(root: &Path, hash: &str) -> Result<PathBuf> {
    ensure!(is_hash(hash), "Invalid object checksum");
    Ok(root.join("objects").join(&hash[..2]).join(hash))
}

#[derive(Debug)]
struct Pin {
    digest: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if checksum(cert.as_ref()) == self.digest {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "The hosting PC’s certificate does not match".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            msg,
            cert,
            sig,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            msg,
            cert,
            sig,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub struct Client {
    pair: Pair,
    config: Arc<ClientConfig>,
    pause: Arc<Pause>,
}

fn header(reader: &mut impl BufRead) -> Result<(String, HashMap<String, String>)> {
    let mut total = 0;
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let count = reader.take(8193).read_line(&mut line)?;
        total += count;
        ensure!(
            count > 0 && total <= 8192 && line.ends_with('\n'),
            "Invalid or interrupted HTTPS header"
        );
        if line.trim_end().is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut lines = lines.into_iter();
    let start = lines.next().context("Missing HTTPS response")?;
    let mut fields = HashMap::new();
    for field in lines {
        let (key, value) = field
            .split_once(':')
            .context("Invalid HTTPS header field")?;
        ensure!(
            fields
                .insert(key.to_ascii_lowercase(), value.trim().to_owned())
                .is_none(),
            "Repeated HTTPS header field"
        );
    }
    ensure!(
        !fields.contains_key("transfer-encoding"),
        "Unsupported transfer framing"
    );
    Ok((start, fields))
}

type Response = (
    u16,
    u64,
    BufReader<StreamOwned<ClientConnection, TcpStream>>,
);

impl Client {
    pub fn new(pair: Pair) -> Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pin {
                digest: pair.fingerprint.clone(),
                provider,
            }))
            .with_no_client_auth();
        Ok(Self {
            pair,
            config: Arc::new(config),
            pause: Arc::new(Pause::default()),
        })
    }

    pub fn with_pause(mut self, pause: Arc<Pause>) -> Self {
        self.pause = pause;
        self
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<(&mut dyn Read, u64)>,
    ) -> Result<Response> {
        let address = SocketAddr::new(self.pair.address.into(), self.pair.port);
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(4))
            .context("Could not connect securely to the hosting PC. Check its private-network firewall rule for BTWN.")?;
        let timeout =
            if path.starts_with("/sync-progress/") || ["/ping", "/control"].contains(&path) {
                6
            } else {
                180
            };
        socket.set_read_timeout(Some(Duration::from_secs(timeout)))?;
        socket.set_write_timeout(Some(Duration::from_secs(timeout)))?;
        let mut conn = ClientConnection::new(
            self.config.clone(),
            ServerName::IpAddress(address.ip().into()),
        )?;
        let mut socket = socket;
        while conn.is_handshaking() {
            conn.complete_io(&mut socket)
                .context("Verify the hosting PC")?;
        }
        let mut tls = StreamOwned::new(conn, socket);
        write!(
            tls,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
            address, self.pair.token
        )?;
        if let Some((body, size)) = body {
            write!(tls, "Content-Length: {size}\r\n\r\n")?;
            ensure!(
                std::io::copy(&mut body.take(size), &mut tls)? == size,
                "A local file changed during transfer"
            );
        } else {
            write!(tls, "\r\n")?;
        }
        tls.flush()?;
        let mut response = BufReader::new(tls);
        let (start, fields) = header(&mut response)?;
        let status: u16 = start
            .split_whitespace()
            .nth(1)
            .context("Invalid HTTPS status")?
            .parse()?;
        let size: u64 = fields
            .get("content-length")
            .context("Missing HTTPS response length")?
            .parse()?;
        if ![200, 201, 404].contains(&status) {
            let mut error = String::new();
            response
                .by_ref()
                .take(size.min(2048))
                .read_to_string(&mut error)?;
            bail!("Paired computer refused the request ({status}) {error}");
        }
        Ok((status, size, response))
    }

    pub fn ping(&self) -> Result<()> {
        let (status, size, response) = self.request("GET", "/ping", None)?;
        let data: serde_json::Value = serde_json::from_reader(response.take(size.min(MAX_META)))?;
        ensure!(
            status == 200 && data["protocol"] == "between-lan-1",
            "Unexpected response from the hosting PC"
        );
        Ok(())
    }

    pub fn supports_progress(&self) -> Result<bool> {
        let (code, size, response) = self.request("GET", "/ping", None)?;
        ensure!(code == 200 && size <= 8192, "Invalid Wi-Fi response");
        let value: serde_json::Value = serde_json::from_reader(response.take(size))?;
        Ok(value["controls"] == true)
    }

    pub fn control(&self) -> Result<Control> {
        let (code, size, response) = self.request("GET", "/control", None)?;
        ensure!(
            code == 200 && size <= 8192,
            "Update BTWN on the hosting PC to use sync controls."
        );
        Ok(serde_json::from_reader(response.take(size))?)
    }

    pub fn command(&self, command: Command) -> Result<Control> {
        let body = serde_json::to_vec(&command)?;
        let (code, size, response) = self.request(
            "PUT",
            "/control",
            Some((&mut body.as_slice(), body.len() as u64)),
        )?;
        ensure!(
            code == 200 && size <= 8192,
            "Update BTWN on the hosting PC to use sync controls."
        );
        Ok(serde_json::from_reader(response.take(size))?)
    }

    pub fn progress(&self, source: &str, receipt: bool) -> Result<Option<Status>> {
        ensure!([PC, LAPTOP].contains(&source), "Unknown computer");
        let side = if receipt { "receipt" } else { "offer" };
        let (code, size, response) =
            self.request("GET", &format!("/sync-progress/{source}/{side}"), None)?;
        if code == 404 {
            return Ok(None);
        }
        ensure!(code == 200 && size <= 8192, "Invalid transfer progress");
        Ok(Some(serde_json::from_reader(response.take(size))?))
    }

    pub fn device_name(&self, source: &str) -> Result<Option<String>> {
        ensure!([PC, LAPTOP].contains(&source), "Unknown computer");
        let (code, size, response) = self.request("GET", &format!("/devices/{source}"), None)?;
        if code == 404 {
            return Ok(None);
        }
        ensure!(code == 200 && size <= 512, "Invalid device response");
        let name: String = serde_json::from_reader(response.take(size))?;
        ensure!(valid_device_name(&name), "Invalid device name");
        Ok(Some(name))
    }

    pub fn exchange_style(
        &self,
        source: &str,
        peer: &str,
        style: DeviceStyle,
    ) -> Result<DeviceStyle> {
        ensure!(
            [PC, LAPTOP].contains(&source) && [PC, LAPTOP].contains(&peer),
            "Unknown computer"
        );
        let bytes = serde_json::to_vec(&style)?;
        let (code, _, _) = self.request(
            "PUT",
            &format!("/device-styles/{source}"),
            Some((&mut bytes.as_slice(), bytes.len() as u64)),
        )?;
        if code == 404 {
            return Ok(DeviceStyle::default());
        }
        ensure!(code == 201, "Could not share device appearance");
        let (code, len, body) = self.request("GET", &format!("/device-styles/{peer}"), None)?;
        if code == 404 {
            return Ok(DeviceStyle::default());
        }
        ensure!(code == 200 && len <= 512, "Invalid device appearance");
        Ok(serde_json::from_reader(body.take(len))?)
    }

    pub fn set_device_name(&self, source: &str, name: &str) -> Result<()> {
        ensure!(
            [PC, LAPTOP].contains(&source) && valid_device_name(name),
            "Invalid device name"
        );
        let body = serde_json::to_vec(name)?;
        let (code, _, _) = self.request(
            "PUT",
            &format!("/devices/{source}"),
            Some((&mut body.as_slice(), body.len() as u64)),
        )?;
        ensure!(code == 201 || code == 404, "Could not update device name");
        Ok(())
    }

    pub fn report(&self, source: &str, receipt: bool, status: &Status) -> Result<()> {
        let data = serde_json::to_vec(status)?;
        let side = if receipt { "receipt" } else { "offer" };
        let (code, _, _) = self.request(
            "PUT",
            &format!("/sync-progress/{source}/{side}"),
            Some((&mut data.as_slice(), data.len() as u64)),
        )?;
        ensure!(
            code == 201,
            "Open the updated BTWN on the hosting PC to see transfer progress."
        );
        Ok(())
    }

    pub fn offer(&self, source: &str) -> Result<Option<Offer>> {
        let (status, size, response) = self.request("GET", &format!("/offers/{source}"), None)?;
        if status == 404 {
            return Ok(None);
        }
        ensure!(size <= MAX_META, "Backup index is too large");
        let offer: Offer = serde_json::from_reader(response.take(size))?;
        offer.validate(source)?;
        Ok(Some(offer))
    }

    pub fn verify_small_object(&self, offer: &Offer) -> Result<u64> {
        let file = offer
            .files
            .iter()
            .filter(|row| row.bytes > 0)
            .min_by_key(|row| row.bytes)
            .context("No backup file is available")?;
        ensure!(file.bytes <= MAX_META, "No small backup file is available");
        let (status, size, response) =
            self.request("GET", &format!("/objects/{}", file.sha256), None)?;
        ensure!(
            status == 200 && size == file.bytes,
            "Backup file is incomplete"
        );
        let mut bytes = Vec::new();
        response.take(size).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 == size && checksum(&bytes) == file.sha256,
            "Backup file checksum differs"
        );
        Ok(size)
    }

    pub fn download(
        &self,
        offer: &Offer,
        cache: &Path,
        output: &Path,
        local_profile: Option<&Path>,
        progress: impl Fn(u64, u64),
    ) -> Result<()> {
        offer.validate(&offer.source)?;
        let assets = format!("Snapshots/{}/AssetObjects/", offer.snapshot);
        let chat_data = format!("Snapshots/{}/VerifiedChatData/", offer.snapshot);
        let rows = offer
            .files
            .iter()
            .filter(|row| {
                !row.path.starts_with("ProjectArchives/") && !row.path.starts_with(&assets)
            })
            .collect::<Vec<_>>();
        let total = rows.iter().map(|r| r.bytes).sum();
        let mut done = 0;
        progress(done, total);
        for row in rows {
            self.pause.wait();
            if let (Some(profile), Some(relative)) =
                (local_profile, row.path.strip_prefix(&chat_data))
            {
                let blob = object(cache, &row.sha256)?;
                if !blob.exists() {
                    let local = scoped(profile, relative)?;
                    if local.is_file()
                        && local.metadata()?.len() == row.bytes
                        && hash(&local)? == row.sha256
                    {
                        let destination = scoped(output, &row.path)?;
                        if destination.is_file()
                            && destination.metadata()?.len() == row.bytes
                            && hash(&destination)? == row.sha256
                        {
                            done += row.bytes;
                            progress(done, total);
                            continue;
                        }
                        verified_stream(
                            &mut fs::File::open(local)?,
                            &destination,
                            row.bytes,
                            &row.sha256,
                        )?;
                        done += row.bytes;
                        progress(done, total);
                        continue;
                    }
                }
            }
            self.receive_counted(row, cache, &scoped(output, &row.path)?, &|count| {
                progress(done + count, total)
            })?;
            done += row.bytes;
            progress(done, total);
        }
        write(&output.join("CURRENT.json"), &offer.pointer()?)
    }

    pub fn receive_counted(
        &self,
        row: &Entry,
        cache: &Path,
        destination: &Path,
        progress: &impl Fn(u64),
    ) -> Result<()> {
        loop {
            self.pause.wait();
            match self.receive_once(row, cache, destination, progress) {
                Err(error) if control::interrupted(&error) => continue,
                result => return result,
            }
        }
    }

    pub fn receive_project(&self, row: &Entry, cache: &Path, destination: &Path) -> Result<()> {
        let blob = object(cache, &row.sha256)?;
        loop {
            self.pause.wait();
            let result = (|| -> Result<()> {
                if blob.is_file()
                    && blob.metadata()?.len() == row.bytes
                    && hash(&blob)? == row.sha256
                {
                    return verified_stream(
                        &mut fs::File::open(&blob)?,
                        destination,
                        row.bytes,
                        &row.sha256,
                    );
                }
                let (status, size, mut response) =
                    self.request("GET", &format!("/objects/{}", row.sha256), None)?;
                ensure!(
                    status == 200 && size == row.bytes,
                    "A project file is missing or incomplete"
                );
                let mut reader = read_with_progress(&mut response, &self.pause, |_| {});
                verified_stream(&mut reader, destination, size, &row.sha256)
            })();
            match result {
                Err(error) if control::interrupted(&error) => continue,
                result => return result,
            }
        }
    }

    fn receive_once(
        &self,
        row: &Entry,
        cache: &Path,
        destination: &Path,
        progress: &impl Fn(u64),
    ) -> Result<()> {
        let blob = object(cache, &row.sha256)?;
        if !blob.is_file() {
            let (status, size, mut response) =
                self.request("GET", &format!("/objects/{}", row.sha256), None)?;
            ensure!(
                status == 200 && size == row.bytes,
                "A file in the backup is missing or incomplete"
            );
            let mut done = 0;
            let mut reader = read_with_progress(&mut response, &self.pause, |n| {
                done += n;
                progress(done);
            });
            verified_stream(&mut reader, &blob, size, &row.sha256)?;
        }
        ensure!(
            blob.metadata()?.len() == row.bytes,
            "A cached file has an unexpected size"
        );
        copy_link(&blob, destination)
    }

    pub fn upload_object(&self, row: &Entry, path: &Path) -> Result<()> {
        loop {
            self.pause.wait();
            match self.upload_once(row, path) {
                Err(error) if control::interrupted(&error) => continue,
                result => return result,
            }
        }
    }

    fn upload_once(&self, row: &Entry, path: &Path) -> Result<()> {
        let address = format!("/objects/{}", row.sha256);
        let (status, size, _) = self.request("HEAD", &address, None)?;
        if status == 404 {
            let mut file = fs::File::open(path)?;
            let mut reader = read_with_progress(&mut file, &self.pause, |_| {});
            self.request("PUT", &address, Some((&mut reader, row.bytes)))?;
        } else {
            ensure!(
                size == row.bytes,
                "A file on the hosting PC has an unexpected size"
            );
        }
        Ok(())
    }

    pub fn upload(&self, offer: &Offer, folder: &Path, progress: impl Fn(&str)) -> Result<()> {
        offer.validate(LAPTOP)?;
        for (index, row) in offer.files.iter().enumerate() {
            self.upload_object(row, &scoped(folder, &row.path)?)?;
            if index % 20 == 0 {
                progress(&format!(
                    "Checking files to send · {}/{}",
                    index + 1,
                    offer.files.len()
                ));
            }
        }
        let json = serde_json::to_vec(offer)?;
        self.request(
            "PUT",
            &format!("/offers/{LAPTOP}"),
            Some((&mut json.as_slice(), json.len() as u64)),
        )?;
        Ok(())
    }
}

fn read_with_progress<'a>(
    input: &'a mut impl Read,
    pause: &'a Pause,
    tick: impl FnMut(u64) + 'a,
) -> impl Read + 'a {
    struct Counted<'a, R, F>(R, &'a Pause, F);
    impl<R: Read, F: FnMut(u64)> Read for Counted<'_, R, F> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.1.check_io()?;
            let read = self.0.read(buffer)?;
            (self.2)(read as u64);
            Ok(read)
        }
    }
    Counted(input, pause, tick)
}

fn reply(writer: &mut impl Write, code: u16, body: &[u8], head: bool) -> Result<()> {
    reply_header(writer, code, body.len() as u64)?;
    if !head {
        writer.write_all(body)?;
    }
    writer.flush()?;
    Ok(())
}

fn reply_header(writer: &mut impl Write, code: u16, length: u64) -> Result<()> {
    write!(
        writer,
        "HTTP/1.1 {code} Between\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )?;
    Ok(())
}

fn serve(socket: TcpStream, config: Arc<ServerConfig>, pair: &Pair, store: &Path) -> Result<()> {
    let caller = socket.peer_addr()?.ip();
    ensure!(private(caller), "Connection is outside the local network");
    socket.set_read_timeout(Some(Duration::from_secs(180)))?;
    socket.set_write_timeout(Some(Duration::from_secs(180)))?;
    let mut stream = BufReader::new(StreamOwned::new(ServerConnection::new(config)?, socket));
    let (line, fields) = header(&mut stream)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().context("Missing method")?;
    let path = parts.next().context("Missing path")?;
    let received = fields
        .get("authorization")
        .map(String::as_bytes)
        .unwrap_or(b"");
    let expected = format!("Bearer {}", pair.token);
    let valid = received.len() == expected.len()
        && received
            .iter()
            .zip(expected.bytes())
            .fold(0u8, |mask, (a, b)| mask | (*a ^ b))
            == 0;
    if !valid {
        return reply(
            stream.get_mut(),
            401,
            b"Pairing is required",
            method == "HEAD",
        );
    }
    if method == "GET" && path == "/ping" {
        return reply(
            stream.get_mut(),
            200,
            br#"{"protocol":"between-lan-1","progress":true,"deliveries":true,"controls":true}"#,
            false,
        );
    }
    if path == "/control" && ["GET", "PUT"].contains(&method) {
        static CONTROL_LOCK: Mutex<()> = Mutex::new(());
        let result = (|| -> Result<Vec<u8>> {
            let _lock = CONTROL_LOCK
                .lock()
                .map_err(|_| anyhow::anyhow!("Sync controls could not be saved"))?;
            let file = store.join("sync-control.json");
            let mut control = if file.is_file() {
                read(&file)?
            } else {
                Control::default()
            };
            if method == "PUT" {
                let size: u64 = fields
                    .get("content-length")
                    .context("Missing sync command length")?
                    .parse()?;
                ensure!(size <= 8192, "Sync command is too large");
                let command = serde_json::from_reader(stream.by_ref().take(size))?;
                control.change(command);
                write(&file, &control)?;
            }
            Ok(serde_json::to_vec(&control)?)
        })();
        return match result {
            Ok(body) => reply(stream.get_mut(), 200, &body, false),
            Err(error) => reply(stream.get_mut(), 400, error.to_string().as_bytes(), false),
        };
    }
    let style_request = path.starts_with("/device-styles/");
    if let Some(source) = path
        .strip_prefix("/devices/")
        .or_else(|| path.strip_prefix("/device-styles/"))
        .filter(|source| [PC, LAPTOP].contains(source))
    {
        let file = store
            .join(if style_request {
                "device-styles"
            } else {
                "devices"
            })
            .join(format!("{source}.json"));
        if method == "GET" {
            return if file.is_file() {
                reply(stream.get_mut(), 200, &fs::read(&file)?, false)
            } else {
                reply(stream.get_mut(), 404, b"", false)
            };
        }
        if method == "PUT" && (source == LAPTOP || caller.is_loopback()) {
            let accepted = (|| -> Result<()> {
                let length: u64 = fields
                    .get("content-length")
                    .context("Missing device name length")?
                    .parse()?;
                ensure!(length <= 512, "Device name is too large");
                if style_request {
                    let style: DeviceStyle = serde_json::from_reader(stream.by_ref().take(length))?;
                    if !file.is_file() || read::<DeviceStyle>(&file)? != style {
                        write(&file, &style)?;
                    }
                } else {
                    let name: String = serde_json::from_reader(stream.by_ref().take(length))?;
                    ensure!(valid_device_name(&name), "Invalid device name");
                    if !file.is_file() || read::<String>(&file)? != name {
                        write(&file, &name)?;
                    }
                }
                Ok(())
            })();
            return match accepted {
                Ok(()) => reply(stream.get_mut(), 201, b"", false),
                Err(error) => reply(stream.get_mut(), 400, error.to_string().as_bytes(), false),
            };
        }
    }
    if let Some((source, side)) = path
        .strip_prefix("/sync-progress/")
        .and_then(|p| p.split_once('/'))
        .filter(|(source, side)| {
            [PC, LAPTOP].contains(source) && ["offer", "receipt"].contains(side)
        })
    {
        let receipt = side == "receipt";
        let file = status_path(store, source, receipt);
        if method == "GET" {
            return if file.is_file() {
                reply(stream.get_mut(), 200, &fs::read(&file)?, false)
            } else {
                reply(stream.get_mut(), 404, b"", false)
            };
        }
        if method == "PUT" && (source == PC && receipt || source == LAPTOP && !receipt) {
            let accepted = (|| -> Result<()> {
                let length: u64 = fields
                    .get("content-length")
                    .context("Missing progress size")?
                    .parse()?;
                ensure!(length <= 8192, "Progress message is too large");
                let mut status: Status = serde_json::from_reader(stream.by_ref().take(length))?;
                ensure!(
                    status.detail.chars().count() <= 800
                        && if receipt {
                            valid_snapshot(&status.snapshot)
                                && matches!(
                                    status.phase,
                                    Phase::Waiting
                                        | Phase::Receiving
                                        | Phase::Applying
                                        | Phase::Done
                                        | Phase::Failed
                                )
                        } else if status.phase == Phase::Ready {
                            valid_snapshot(&status.snapshot)
                        } else {
                            status.snapshot.is_empty()
                                && matches!(status.phase, Phase::Preparing | Phase::Failed)
                        },
                    "Invalid progress message"
                );
                status.at = Status::now();
                write(&file, &status)
            })();
            return match accepted {
                Ok(()) => reply(stream.get_mut(), 201, b"", false),
                Err(error) => reply(stream.get_mut(), 400, error.to_string().as_bytes(), false),
            };
        }
    }
    if let Some(digest) = path.strip_prefix("/objects/").filter(|hash| is_hash(hash)) {
        let file = object(store, digest)?;
        if method == "GET" || method == "HEAD" {
            let file = if file.is_file() {
                Some(file)
            } else {
                crate::backup::project_source(store, digest, None)?
            };
            let Some(file) = file else {
                return reply(stream.get_mut(), 404, b"", method == "HEAD");
            };
            let mut input = fs::File::open(file)?;
            reply_header(stream.get_mut(), 200, input.metadata()?.len())?;
            if method == "GET" {
                std::io::copy(&mut input, stream.get_mut())?;
            }
            stream.get_mut().flush()?;
            return Ok(());
        }
        if method == "PUT" {
            let size: u64 = fields
                .get("content-length")
                .context("Missing upload size")?
                .parse()?;
            if size > MAX_FILE {
                return reply(stream.get_mut(), 413, b"File is too large", false);
            }
            return match verified_stream(&mut stream, &file, size, digest) {
                Ok(()) => reply(stream.get_mut(), 201, b"", false),
                Err(error) => reply(stream.get_mut(), 400, error.to_string().as_bytes(), false),
            };
        }
    }
    if let Some(source) = path
        .strip_prefix("/offers/")
        .filter(|source| [PC, LAPTOP].contains(source))
    {
        let index = store.join("offers").join(format!("{source}.json"));
        if method == "GET" {
            return if index.is_file() {
                reply(stream.get_mut(), 200, &fs::read(&index)?, false)
            } else {
                reply(stream.get_mut(), 404, b"", false)
            };
        }
        if method == "PUT" && source == LAPTOP {
            let accepted = (|| -> Result<()> {
                let length: u64 = fields
                    .get("content-length")
                    .context("Missing index size")?
                    .parse()?;
                ensure!(length <= MAX_META, "Backup index is too large");
                let offer: Offer = serde_json::from_reader(stream.by_ref().take(length))?;
                offer.validate(source)?;
                for row in &offer.files {
                    ensure!(
                        object(store, &row.sha256)?.metadata()?.len() == row.bytes,
                        "Upload is incomplete"
                    );
                }
                write(&index, &offer)
            })();
            return match accepted {
                Ok(()) => reply(stream.get_mut(), 201, b"", false),
                Err(e) => reply(stream.get_mut(), 400, e.to_string().as_bytes(), false),
            };
        }
    }
    reply(stream.get_mut(), 404, b"", method == "HEAD")
}

pub fn start(pair: &Pair, identity: &Path, store: &Path) -> Result<()> {
    let certificate = fs::File::open(identity.join("certificate.pem"))
        .context("Host pairing certificate is missing")?;
    let key =
        fs::File::open(identity.join("private.pem")).context("Host pairing key is missing")?;
    let certs = rustls_pemfile::certs(&mut BufReader::new(certificate))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        certs
            .first()
            .is_some_and(|cert| checksum(cert.as_ref()) == pair.fingerprint),
        "Host pairing certificate does not match"
    );
    let key = rustls_pemfile::private_key(&mut BufReader::new(key))?
        .context("Host pairing key is empty")?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    let listener = TcpListener::bind((pair.address, pair.port))
        .context("Could not start Between's Wi-Fi listener")?;
    let config = Arc::new(config);
    let pair = pair.clone();
    let store = store.to_path_buf();
    thread::spawn(move || {
        for socket in listener.incoming().flatten() {
            let (config, pair, store) = (config.clone(), pair.clone(), store.clone());
            thread::spawn(move || {
                let _ = serve(socket, config, &pair, &store);
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_https_checks_pair_and_round_trips_backup() -> Result<()> {
        let profile = tempfile::tempdir()?;
        let identity = profile.path().join("Identity");
        crate::pairing::create(&identity)?;
        let mut pair = Pair::from_file(&identity.join("pair.json"))?;
        pair.address = Ipv4Addr::LOCALHOST;
        let reserve = TcpListener::bind((pair.address, 0))?;
        pair.port = reserve.local_addr()?.port();
        drop(reserve);
        let temp = tempfile::tempdir()?;
        let store = temp.path().join("server");
        start(&pair, &identity, &store)?;
        let pause = Arc::new(Pause::default());
        let client = Client::new(pair.clone())?.with_pause(pause.clone());
        let desktop_client = Client::new(pair.clone())?;
        client.ping()?;
        assert!(client.supports_progress()?);
        assert!(client.control()?.mode.is_none());
        for mode in [
            control::Mode::DesktopToLaptop,
            control::Mode::LaptopToDesktop,
            control::Mode::Automatic,
        ] {
            let chosen = client.command(Command::Start {
                mode,
                projects: true,
            })?;
            assert_eq!(desktop_client.control()?.mode, Some(mode));
            assert_eq!(mode.sends(PC), mode != control::Mode::LaptopToDesktop);
            assert_eq!(mode.sends(LAPTOP), mode != control::Mode::DesktopToLaptop);
            assert!(desktop_client.command(Command::SetPaused(true))?.paused);
            let paused = client.control()?;
            assert_eq!(paused.job, chosen.job);
            assert!(paused.paused && paused.projects);
            let resumed = client.command(Command::SetPaused(false))?;
            assert!(!desktop_client.control()?.paused);
            assert_eq!(resumed.mode, Some(mode));
            assert_eq!(resumed.job, chosen.job);
        }
        let mut bad = pair.clone();
        bad.fingerprint = "0".repeat(64);
        assert!(Client::new(bad)?.ping().is_err());
        let mut bad = pair;
        bad.token = "0".repeat(64);
        assert!(Client::new(bad)?.ping().is_err());
        let tag = "20261123-111111-112233aa";
        let contents = b"isolated backup fixture";
        let relative = format!("Snapshots/{tag}/TRANSFER.json");
        let digest = checksum(contents);
        let entry = Entry {
            path: relative.clone(),
            sha256: digest.clone(),
            bytes: contents.len() as u64,
        };
        let outgoing = temp.path().join("outgoing");
        verified_stream(
            &mut contents.as_slice(),
            &scoped(&outgoing, &relative)?,
            entry.bytes,
            &digest,
        )?;
        let offer = Offer {
            source: LAPTOP.into(),
            snapshot: tag.into(),
            files: vec![entry.clone()],
        };
        client.upload(&offer, &outgoing, |_| {})?;
        let offer = client
            .offer(LAPTOP)?
            .context("Laptop offer was not published")?;
        let output = temp.path().join("received");
        let cache = temp.path().join("cache");
        let counts = std::cell::Cell::new((0, 0));
        client.download(&offer, &cache, &output, None, |done, total| {
            counts.set((done, total))
        })?;
        assert_eq!(counts.get(), (entry.bytes, entry.bytes));
        assert_eq!(fs::read(scoped(&output, &relative)?)?, contents);
        let desktop = Offer {
            source: PC.into(),
            snapshot: tag.into(),
            files: vec![entry.clone()],
        };
        write(&store.join("offers").join(format!("{PC}.json")), &desktop)?;
        let published = client
            .offer(PC)?
            .context("Desktop offer was not published")?;
        assert_eq!(client.verify_small_object(&published)?, entry.bytes);
        write(
            &status_path(&store, PC, false),
            &Status::new(tag, Phase::Ready, "Laptop can start"),
        )?;
        assert_eq!(client.progress(PC, false)?.unwrap().phase, Phase::Ready);
        let copying = Status::new(tag, Phase::Receiving, "Receiving").with_counts(2, 4);
        client.report(PC, true, &copying)?;
        let seen = desktop_client.progress(PC, true)?.unwrap();
        assert_eq!(seen.snapshot, tag);
        assert_eq!(seen.counts, Some((2, 4)));
        assert_eq!(seen.phase, Phase::Receiving);
        client.report(PC, true, &Status::new(tag, Phase::Done, "Applied"))?;
        assert_eq!(
            desktop_client.progress(PC, true)?.unwrap().phase,
            Phase::Done
        );
        client.report(
            LAPTOP,
            false,
            &Status::new(tag, Phase::Ready, "Desktop can start"),
        )?;
        assert_eq!(
            desktop_client.progress(LAPTOP, false)?.unwrap().phase,
            Phase::Ready
        );
        write(
            &status_path(&store, LAPTOP, true),
            &Status::new(tag, Phase::Done, "Desktop applied"),
        )?;
        assert_eq!(client.progress(LAPTOP, true)?.unwrap().phase, Phase::Done);
        assert_eq!(client.progress(PC, true)?.unwrap().detail, "Applied");
        assert_eq!(client.device_name(LAPTOP)?, None);
        let custom = DeviceStyle {
            kind: Some(DeviceKind::Desktop),
            icon: DeviceIcon::Mini,
        };
        client.exchange_style(LAPTOP, PC, custom)?;
        assert_eq!(
            desktop_client.exchange_style(PC, LAPTOP, DeviceStyle::default())?,
            custom
        );
        client.set_device_name(LAPTOP, "Work laptop")?;
        desktop_client.set_device_name(PC, "Studio PC")?;
        assert_eq!(
            desktop_client.device_name(LAPTOP)?.as_deref(),
            Some("Work laptop")
        );
        assert_eq!(client.device_name(PC)?.as_deref(), Some("Studio PC"));
        assert!(
            client
                .report(
                    PC,
                    false,
                    &Status::new(tag, Phase::Ready, "Not this endpoint")
                )
                .is_err()
        );
        let contents = vec![b'X'; 2 * 1024 * 1024];
        let large = Entry {
            path: "large".into(),
            bytes: contents.len() as u64,
            sha256: checksum(&contents),
        };
        let source = object(&store, &large.sha256)?;
        mkdir_parent(&source)?;
        fs::write(source, &contents)?;
        let paused_once = std::cell::Cell::new(false);
        let observed = std::cell::Cell::new(0);
        let restarted = std::cell::Cell::new(false);
        let output = temp.path().join("large");
        client.receive_counted(&large, &cache, &output, &|bytes| {
            if bytes <= observed.replace(bytes) {
                restarted.set(true);
            }
            if bytes > 0 && bytes < large.bytes && !paused_once.replace(true) {
                pause.set(true);
                let pause = pause.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(150));
                    pause.set(false);
                });
            }
        })?;
        assert!(paused_once.get() && restarted.get());
        assert_eq!(hash(&output)?, large.sha256);
        let direct_cache = temp.path().join("direct-cache");
        let direct_output = temp.path().join("direct-project");
        client.receive_project(&large, &direct_cache, &direct_output)?;
        assert_eq!(hash(&direct_output)?, large.sha256);
        assert!(!object(&direct_cache, &large.sha256)?.exists());
        fs::write(&direct_output, b"local change")?;
        assert!(
            client
                .receive_project(&large, &direct_cache, &direct_output)
                .is_err()
        );
        assert_eq!(fs::read(&direct_output)?, b"local change");
        let wrong = temp.path().join("invalid");
        assert!(
            verified_stream(
                &mut b"isolated backup fixture".as_slice(),
                &wrong,
                entry.bytes,
                &"0".repeat(64)
            )
            .is_err()
        );
        assert!(!wrong.exists());
        Ok(())
    }
}
