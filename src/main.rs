#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod backup;
mod brand;
mod codex;
mod control;
mod discovery;
mod lan;
mod pairing;
mod sidebar;
mod storage;
mod ui;
#[cfg(windows)]
mod window_appearance;

use anyhow::{Context, Result, ensure};
use control::{Command, Control, Mode, Pause};
use eframe::egui;
use lan::{Client, LAPTOP, Offer, PC, Pair, Phase, Status};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

const INTERVAL: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_secs(3);

fn default_pc_name(joined_pair: bool) -> &'static str {
    if joined_pair { "PC 2" } else { "PC 1" }
}

#[derive(Clone)]
struct Paths {
    home: PathBuf,
    identity: PathBuf,
    store: PathBuf,
    source: &'static str,
    seen: Arc<AtomicU64>,
    listener: Arc<Mutex<()>>,
    discovery_timeout_ms: u64,
}

impl Paths {
    fn new() -> Result<Self> {
        let local = PathBuf::from(
            std::env::var_os("LOCALAPPDATA").context("Windows local storage is unavailable")?,
        );
        let home = local.join("BTWN");
        let identity = home.join("Identity");
        let source = if identity.join("private.pem").is_file() {
            PC
        } else {
            LAPTOP
        };
        let store = home.join("Transfer");
        fs::create_dir_all(&store).context("Open this user's BTWN transfer folder")?;
        Ok(Self {
            home,
            identity,
            store,
            source,
            seen: Arc::new(AtomicU64::new(0)),
            listener: Arc::new(Mutex::new(())),
            discovery_timeout_ms: discovery::DEFAULT_TIMEOUT_MS,
        })
    }

    fn pair(&self) -> Result<Pair> {
        Pair::from_file(&self.identity.join("pair.json"))
            .context("Open the pairing file from the hosting PC")
    }

    fn peer(&self) -> &'static str {
        if self.source == PC { LAPTOP } else { PC }
    }

    fn client(&self) -> Result<Client> {
        let mut pair = self.pair()?;
        pair.port = discovery::PORT;
        if self.source == PC {
            let _start = self
                .listener
                .lock()
                .map_err(|_| anyhow::anyhow!("BTWN could not start its network listener"))?;
            pair.address = Ipv4Addr::LOCALHOST;
            let client = Client::new(pair.clone())?;
            if client.ping().is_err() {
                ensure!(
                    self.store.is_dir(),
                    "The hosting PC’s transfer folder is unavailable."
                );
                let mut listener = pair;
                listener.address = Ipv4Addr::UNSPECIFIED;
                lan::start(&listener, &self.identity, &self.store)?;
                discovery::start(listener, self.seen.clone())?;
            }
            return Ok(client);
        }
        pair.address = discovery::find(&pair, self.discovery_timeout_ms)?;
        Ok(Client::new(pair)?)
    }

    fn report(&self, client: &Client, source: &str, receipt: bool, status: &Status) -> Result<()> {
        if self.source == PC {
            storage::write(&lan::status_path(&self.store, source, receipt), status)
        } else {
            client.report(source, receipt, status)
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Settings {
    projects: bool,
    #[serde(default)]
    theme: ui::Theme,
    #[serde(default)]
    reduce_motion: bool,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    device_style: lan::DeviceStyle,
    #[serde(default)]
    projects_folder: Option<PathBuf>,
    #[serde(default)]
    discovery_timeout_ms: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            projects: true,
            theme: ui::Theme::default(),
            reduce_motion: false,
            device_name: String::new(),
            device_style: lan::DeviceStyle::default(),
            projects_folder: None,
            discovery_timeout_ms: None,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Cycle {
    published: Option<String>,
    #[serde(default)]
    project_digest: Option<String>,
    received: Option<String>,
}

fn local_offer(paths: &Paths, offer: &Offer, folder: &Path, pause: &Pause) -> Result<()> {
    for row in &offer.files {
        pause.wait();
        let from = storage::scoped(folder, &row.path)?;
        let dest = lan::object(&paths.store, &row.sha256)?;
        storage::copy_link(&from, &dest)?;
        ensure!(
            dest.metadata()?.len() == row.bytes,
            "A stored file has an unexpected size"
        );
    }
    storage::write(
        &paths.store.join("offers").join(format!("{PC}.json")),
        offer,
    )
}

fn preparing(detail: &str) -> Status {
    Status::new("", Phase::Preparing, detail)
}

fn announce(paths: &Paths, client: &Client, offer: &Offer, job: u64) -> Result<()> {
    if client
        .progress(paths.source, false)?
        .is_some_and(|s| s.phase == Phase::Ready && s.job == job && s.snapshot == offer.snapshot)
    {
        return Ok(());
    }
    paths.report(
        client,
        paths.source,
        false,
        &Status::new(
            &offer.snapshot,
            Phase::Ready,
            "Waiting for the receiving PC.",
        )
        .for_job(job),
    )
}

fn receive_offer(
    paths: &Paths,
    client: &Client,
    offer: &Offer,
    home: &Path,
    job: u64,
    options: backup::RestoreOptions<'_>,
    pause: &Pause,
    send: &(impl Fn(Status) + Sync),
) -> Result<String> {
    let receive = paths.home.join("Incoming").join(&offer.snapshot);
    let cache = paths.home.join("Cache");
    let last = Mutex::new((Phase::Ready, Instant::now()));
    let update = |state: Status| {
        let state = state.for_job(job);
        let Ok(mut last) = last.lock() else {
            return;
        };
        let (phase, at) = *last;
        if phase != state.phase || at.elapsed() >= Duration::from_millis(750) {
            *last = (state.phase, Instant::now());
            let _ = paths.report(client, &offer.source, true, &state);
            send(state);
        }
    };
    let report = (|| -> Result<(String, lan::Outcome)> {
        if let Some(summary) =
            backup::confirmed(&receive, home, &paths.home, &offer.snapshot, options)?
        {
            let result = backup::outcome(&receive, home, &paths.home, &offer.snapshot)?;
            return Ok((summary, result));
        }
        client.download(offer, &cache, &receive, Some(home), |done, total| {
            update(
                Status::new(
                    &offer.snapshot,
                    Phase::Receiving,
                    "Receiving and checking chat files",
                )
                .with_counts(done, total),
            );
        })?;
        pause.wait();
        let result = backup::apply(
            &receive,
            home,
            &paths.home,
            &offer.source,
            options,
            &|destination, object| {
                if let Some(row) = object {
                    return client.receive_project(row, &cache, destination);
                }
                let relative = destination
                    .strip_prefix(&receive)?
                    .to_string_lossy()
                    .replace('\\', "/");
                let row = offer
                    .files
                    .iter()
                    .find(|row| row.path == relative)
                    .context("A backup file is missing from the transfer index")?;
                client.receive_counted(row, &cache, destination, &|done| {
                    update(
                        Status::new(
                            &offer.snapshot,
                            Phase::Receiving,
                            "Receiving a project file or attachment",
                        )
                        .with_counts(done, row.bytes),
                    );
                })
            },
            &|detail| {
                let mut state = Status::new(&offer.snapshot, Phase::Applying, detail);
                if let Some((done, total)) = detail
                    .rsplit_once(" · ")
                    .and_then(|(_, text)| text.split_once('/'))
                {
                    if let (Ok(done), Ok(total)) = (done.parse(), total.parse()) {
                        state = state.with_counts(done, total);
                    }
                }
                update(state);
                pause.wait();
            },
        )?;
        Ok((
            result,
            backup::outcome(&receive, home, &paths.home, &offer.snapshot)?,
        ))
    })();
    let state = match &report {
        Ok((detail, outcome)) => {
            let mut status = Status::new(&offer.snapshot, Phase::Done, detail);
            status.outcome = Some(outcome.clone());
            status
        }
        Err(error) => Status::new(&offer.snapshot, Phase::Failed, &format!("{error:#}")),
    }
    .for_job(job);
    paths
        .report(client, &offer.source, true, &state)
        .context("Could not report the final sync status")?;
    send(state);
    report.map(|(summary, _)| summary)
}

fn sync(
    paths: &Paths,
    receive_only: bool,
    job: Control,
    pause: Arc<Pause>,
    projects_folder: Option<&Path>,
    send: &(impl Fn(Status) + Sync),
) -> Result<String> {
    let _transfer_lock = storage::lock(&paths.home.join("sync.lock"))?;
    send(preparing("Finding the paired computer on your network"));
    let client = paths.client()?.with_pause(pause.clone());
    ensure!(
        client.supports_progress()?,
        "Open the current BTWN release on the hosting PC so both PCs can show progress."
    );
    pause.wait();
    ensure!(client.control()?.job == job.job, "A new sync was selected.");
    ensure!(
        !codex::is_open()?,
        "Close ChatGPT on this computer to sync."
    );
    let codex_home = codex::home()?;
    let _chat_lock = storage::lock(
        &codex_home
            .parent()
            .context("No ChatGPT profile folder")?
            .join("PC-Chat-Repair.lock"),
    )?;
    let cycle_path = paths.home.join("cycle.json");
    let mut cycle = if cycle_path.is_file() {
        storage::read::<Cycle>(&cycle_path)?
    } else {
        Cycle::default()
    };
    let our_revision = backup::revision(&codex_home)?;
    if receive_only {
        let peer = paths.peer();
        let offer = client
            .offer(peer)?
            .context("The sender has not shared a backup yet.")?;
        let status = client.progress(peer, false)?;
        ensure!(
            status.is_some_and(|s| s.phase == Phase::Ready
                && s.job == job.job
                && s.snapshot == offer.snapshot),
            "The other computer is still preparing its backup."
        );
        let options = backup::RestoreOptions {
            match_sender: matches!(
                job.mode,
                Some(Mode::DesktopToLaptop | Mode::LaptopToDesktop)
            ),
            projects_folder,
        };
        let summary = receive_offer(
            paths,
            &client,
            &offer,
            &codex_home,
            job.job,
            options,
            &pause,
            send,
        )?;
        if job.mode == Some(Mode::Automatic) && cycle.published.as_ref() == Some(&our_revision) {
            // A received snapshot does not need to be sent back unchanged.
            cycle.published = Some(backup::revision(&codex_home)?);
        }
        cycle.received = Some(offer.snapshot);
        storage::write(&cycle_path, &cycle)?;
        return Ok(summary);
    }
    let prepare = |detail: &str| {
        send(preparing(detail).for_job(job.job));
        pause.wait();
    };
    let project_files = if job.projects {
        backup::scan_projects(&codex_home, &paths.store, &prepare)?
    } else {
        Vec::new()
    };
    let project_digest = job
        .projects
        .then(|| serde_json::to_vec(&project_files).map(|rows| storage::checksum(&rows)))
        .transpose()?;
    let previous = client.offer(paths.source)?;
    if previous.as_ref().is_none_or(|offer| {
        !offer
            .files
            .iter()
            .any(|file| file.path.rsplit('/').next() == Some(sidebar::MANIFEST))
    }) || cycle.published.as_ref() != Some(&our_revision)
        || project_digest != cycle.project_digest
    {
        ensure!(
            !codex::is_open()?,
            "Close ChatGPT on this computer to share the latest chats."
        );
        let outgoing = if paths.source == PC {
            paths
                .store
                .parent()
                .context("Missing transfer folder on the hosting PC")?
                .join("BetweenRustBackups")
        } else {
            paths.store.clone()
        };
        fs::create_dir_all(&outgoing)?;
        let staging = tempfile::Builder::new()
            .prefix("pending-")
            .tempdir_in(outgoing)?;
        let last_shared = Cell::new(None::<Instant>);
        let prepare = |detail: &str| {
            let state = preparing(detail).for_job(job.job);
            if last_shared
                .get()
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
            {
                last_shared.set(Some(Instant::now()));
                let _ = paths.report(&client, paths.source, false, &state);
            }
            send(state);
            pause.wait();
        };
        if paths.source == LAPTOP {
            prepare("Sending new or changed project files");
            for row in &project_files {
                let source = backup::project_source(&paths.store, &row.sha256, Some(row.bytes))?
                    .context("A project file changed before it could be sent. Start sync again.")?;
                client.upload_object(row, &source)?;
            }
        }
        let offer = backup::publish(
            &codex_home,
            staging.path(),
            (paths.source == PC).then_some(paths.store.as_path()),
            paths.source,
            project_files,
            &prepare,
        )?;
        if paths.source == PC {
            prepare("Sharing the latest chats with the paired PC");
            local_offer(paths, &offer, staging.path(), &pause)?;
        } else {
            client.upload(&offer, staging.path(), &prepare)?;
        }
        pause.wait();
        ensure!(client.control()?.job == job.job, "A new sync was selected.");
        announce(paths, &client, &offer, job.job)?;
        cycle.published = Some(our_revision);
        cycle.project_digest = project_digest;
        storage::write(&cycle_path, &cycle)?;
        return Ok("Changes ready. Waiting for the receiving computer.".into());
    }
    if let Some(offer) = previous {
        pause.wait();
        ensure!(client.control()?.job == job.job, "A new sync was selected.");
        announce(paths, &client, &offer, job.job)?;
    }
    Ok("Changes ready. Waiting for the receiving computer.".into())
}

enum Message {
    Connection(Result<()>),
    Progress(Status),
    Done(Result<String>),
}

#[derive(Default)]
struct Delivery {
    incoming: (Option<Status>, Option<Status>),
    outgoing: (Option<Status>, Option<Status>),
    peer_found: bool,
    peer_name: Option<String>,
    peer_style: lan::DeviceStyle,
    control: Control,
}

fn matching_delivery<'a>(desktop: &'a Status, laptop: &'a Option<Status>) -> Option<&'a Status> {
    laptop
        .as_ref()
        .filter(|state| state.job == desktop.job && state.snapshot == desktop.snapshot)
}

fn already_reported(phase: Phase) -> bool {
    matches!(phase, Phase::Done | Phase::Failed)
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn receiver_progress_matches_even_when_computer_clocks_differ() {
        let mut offer = Status::new("snapshot", Phase::Ready, "Ready").for_job(42);
        offer.at = 200;
        let mut receiver = Status::new("snapshot", Phase::Receiving, "Receiving")
            .with_counts(4, 10)
            .for_job(42);
        receiver.at = 100;
        assert_eq!(
            matching_delivery(&offer, &Some(receiver.clone()))
                .unwrap()
                .counts,
            Some((4, 10))
        );
        receiver.job = 43;
        assert!(matching_delivery(&offer, &Some(receiver)).is_none());
    }
}

struct Between {
    paths: Result<Paths>,
    settings: Settings,
    settings_open: bool,
    name_draft: String,
    style_draft: lan::DeviceStyle,
    settings_tab: ui::SettingsTab,
    applied_theme: Option<ui::Theme>,
    #[cfg(windows)]
    window_appearance: window_appearance::WindowAppearance,
    worker: Option<Receiver<Message>>,
    status: String,
    progress: Option<Status>,
    error: bool,
    next: Instant,
    paired: bool,
    next_pair_check: Instant,
    connected: bool,
    link: Option<Receiver<Result<Delivery>>>,
    delivery: Delivery,
    next_poll: Instant,
    link_error: Option<String>,
    attempted: Option<(String, u64)>,
    mode: Mode,
    chosen: bool,
    pause: Arc<Pause>,
    command: Option<Receiver<Result<Control>>>,
    sending_job: Option<u64>,
}

impl Between {
    fn device_name(&self, laptop: bool) -> &str {
        if self.settings.device_name.is_empty() {
            default_pc_name(laptop)
        } else {
            &self.settings.device_name
        }
    }

    fn new() -> Self {
        Self::from_paths(Paths::new())
    }

    fn from_paths(mut paths: Result<Paths>) -> Self {
        let settings: Settings = paths
            .as_ref()
            .ok()
            .and_then(|p| storage::read(&p.home.join("settings.json")).ok())
            .unwrap_or_default();
        if let Ok(paths) = &mut paths {
            paths.discovery_timeout_ms = settings
                .discovery_timeout_ms
                .unwrap_or(discovery::DEFAULT_TIMEOUT_MS);
        }
        let paired = paths.as_ref().is_ok_and(|paths| paths.pair().is_ok());
        let mut app = Self {
            paths,
            settings,
            settings_open: false,
            name_draft: String::new(),
            style_draft: lan::DeviceStyle::default(),
            settings_tab: ui::SettingsTab::Device,
            applied_theme: None,
            #[cfg(windows)]
            window_appearance: Default::default(),
            worker: None,
            status: String::new(),
            progress: None,
            error: false,
            next: Instant::now(),
            paired,
            next_pair_check: Instant::now() + POLL,
            connected: false,
            link: None,
            delivery: Delivery::default(),
            next_poll: Instant::now(),
            link_error: None,
            attempted: None,
            mode: Mode::default(),
            chosen: false,
            pause: Arc::new(Pause::default()),
            command: None,
            sending_job: None,
        };
        app.status = match &app.paths {
            Ok(_) if paired => "Ready to sync over Wi-Fi.".into(),
            Ok(_) => "Create a pairing file on one PC. Open that file on the other PC.".into(),
            Err(error) => error.to_string(),
        };
        app
    }

    fn connect(&mut self, context: &egui::Context) {
        let Ok(paths) = self.paths.as_ref().cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.status = "Checking the Wi-Fi connection".into();
        let context = context.clone();
        std::thread::spawn(move || {
            let result = paths.client().and_then(|client| {
                ensure!(
                    client.supports_progress()?,
                    "The hosting PC needs the current BTWN release."
                );
                Ok(())
            });
            let _ = tx.send(Message::Connection(result));
            context.request_repaint();
        });
    }

    fn poll(&mut self, context: &egui::Context) {
        let Ok(paths) = self.paths.as_ref().cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let context = context.clone();
        let receiving = self.worker.is_some();
        let local_name = self.device_name(paths.source == LAPTOP).to_owned();
        let local_style = self.settings.device_style;
        self.link = Some(rx);
        std::thread::spawn(move || {
            let result = (|| -> Result<Delivery> {
                let client = paths.client()?;
                let control = client.control()?;
                let peer = paths.peer();
                // Device names are optional when one computer is still on an older release.
                let _ = client.set_device_name(paths.source, &local_name);
                let peer_name = client.device_name(peer).ok().flatten();
                let peer_style = client
                    .exchange_style(paths.source, peer, local_style)
                    .unwrap_or_default();
                let current = |state: &Status| control.mode.is_some() && state.job == control.job;
                let incoming = client.progress(peer, false)?.filter(current);
                let mut receipt = client.progress(peer, true)?.filter(current);
                if !receiving && !control.paused {
                    if let Some(ready) = incoming
                        .as_ref()
                        .filter(|status| status.phase == Phase::Ready)
                    {
                        let reported = matching_delivery(ready, &receipt);
                        if reported.is_none_or(|s| !already_reported(s.phase)) && codex::is_open()?
                        {
                            let state = Status::new(
                                &ready.snapshot,
                                Phase::Waiting,
                                &format!(
                                    "Close Codex or ChatGPT on {local_name} to receive changes."
                                ),
                            )
                            .for_job(control.job);
                            paths.report(&client, peer, true, &state)?;
                            receipt = Some(state);
                        }
                    }
                }
                Ok(Delivery {
                    incoming: (incoming, receipt),
                    outgoing: (
                        client.progress(paths.source, false)?.filter(current),
                        client.progress(paths.source, true)?.filter(current),
                    ),
                    peer_found: paths.source == LAPTOP
                        || Status::now().saturating_sub(paths.seen.load(Ordering::Relaxed)) < 12,
                    peer_name,
                    peer_style,
                    control,
                })
            })();
            let _ = tx.send(result);
            context.request_repaint();
        });
    }

    fn save(&mut self) {
        if let Ok(paths) = &mut self.paths {
            paths.discovery_timeout_ms = self
                .settings
                .discovery_timeout_ms
                .unwrap_or(discovery::DEFAULT_TIMEOUT_MS);
            if let Err(error) = storage::write(&paths.home.join("settings.json"), &self.settings) {
                self.status = format!("Could not save settings. {error}");
                self.error = true;
            }
        }
    }

    fn set_control(&mut self, context: &egui::Context, command: Command) {
        let Ok(paths) = self.paths.as_ref().cloned() else {
            return;
        };
        if matches!(command, Command::SetPaused(true)) {
            self.pause.set(true);
        }
        let (tx, rx) = mpsc::channel();
        let context = context.clone();
        self.command = Some(rx);
        std::thread::spawn(move || {
            let result = paths.client().and_then(|client| client.command(command));
            let _ = tx.send(result);
            context.request_repaint();
        });
    }

    fn update_control(&mut self, control: Control) {
        if control.job != self.delivery.control.job {
            self.next = Instant::now();
            self.attempted = None;
            self.sending_job = None;
            self.error = false;
            self.status = "Waiting for the sending computer.".into();
            if !self.chosen {
                if let Some(mode) = control.mode {
                    self.mode = mode;
                }
                if control.mode.is_some() {
                    self.settings.projects = control.projects;
                }
            }
        }
        self.pause.set(control.paused);
        self.delivery.control = control;
    }

    fn run(&mut self, context: &egui::Context, receive_only: bool) {
        let Ok(paths) = self.paths.as_ref().cloned() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.status = "Checking the Wi-Fi connection".into();
        self.progress = Some(preparing(&self.status));
        self.error = false;
        let context = context.clone();
        let job = self.delivery.control.clone();
        let pause = self.pause.clone();
        let project_folder = self.settings.projects_folder.clone();
        std::thread::spawn(move || {
            let post = |tx: &Sender<Message>, message| {
                let _ = tx.send(message);
                context.request_repaint();
            };
            let result = sync(
                &paths,
                receive_only,
                job.clone(),
                pause,
                project_folder.as_deref(),
                &|status| post(&tx, Message::Progress(status)),
            );
            if !receive_only && let Err(error) = &result {
                if let Ok(client) = paths.client() {
                    if client.control().is_ok_and(|state| state.job == job.job) {
                        let _ = paths.report(
                            &client,
                            paths.source,
                            false,
                            &Status::new("", Phase::Failed, &format!("{error:#}")).for_job(job.job),
                        );
                    }
                }
            }
            post(&tx, Message::Done(result));
        });
    }

    fn export_pair(&mut self) -> Result<()> {
        let paths = self
            .paths
            .as_mut()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let Some(file) = rfd::FileDialog::new()
            .set_title("Save a private pairing file for your other PC")
            .set_file_name("BTWN-pair.json")
            .add_filter("BTWN pairing", &["json"])
            .save_file()
        else {
            return Ok(());
        };
        let pair = if paths.identity.exists() {
            ensure!(
                paths.identity.join("private.pem").is_file(),
                "Only the hosting PC can export a pairing file."
            );
            paths.pair()?
        } else {
            pairing::create(&paths.identity)?
        };
        paths.source = PC;
        self.paired = true;
        self.next_poll = Instant::now();
        pairing::export(&pair, &file)?;
        self.error = false;
        self.status = "Pairing file saved. Open it in BTWN on your other PC.".into();
        Ok(())
    }

    fn pair(&mut self) -> Result<()> {
        let paths = self
            .paths
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let Some(file) = rfd::FileDialog::new()
            .add_filter("Between pairing", &["json"])
            .pick_file()
        else {
            return Ok(());
        };
        pairing::import(&paths.identity, &file)?;
        self.error = false;
        self.paired = true;
        self.next_pair_check = Instant::now() + POLL;
        self.connected = false;
        self.delivery = Delivery::default();
        self.attempted = None;
        self.next_poll = Instant::now();
        Ok(())
    }
}

impl eframe::App for Between {
    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(worker) = &self.worker {
            while let Ok(message) = worker.try_recv() {
                match message {
                    Message::Connection(result) => {
                        self.status = "Ready to sync over Wi-Fi.".into();
                        self.link_error = result.err().map(|error| format!("{error:#}"));
                        self.worker = None;
                        break;
                    }
                    Message::Progress(status) => {
                        self.status.clone_from(&status.detail);
                        self.progress = Some(status);
                    }
                    Message::Done(result) => {
                        self.error = result.is_err();
                        self.status = result.unwrap_or_else(|error| format!("{error:#}"));
                        self.worker = None;
                        self.progress = None;
                        self.next = Instant::now() + INTERVAL;
                        self.next_poll = Instant::now();
                        break;
                    }
                }
            }
        }
        if Instant::now() >= self.next_pair_check {
            self.paired = self.paths.as_ref().is_ok_and(|path| path.pair().is_ok());
            self.next_pair_check = Instant::now() + POLL;
        }
        let paired = self.paired;
        if let Some(command) = self
            .command
            .as_ref()
            .and_then(|command| command.try_recv().ok())
        {
            self.command = None;
            self.next_poll = Instant::now();
            match command {
                Ok(control) => {
                    self.update_control(control);
                    self.error = false;
                }
                Err(error) => {
                    self.pause.set(self.delivery.control.paused);
                    self.error = true;
                    self.status = format!("{error:#}");
                }
            }
        }
        if let Some(link) = &self.link {
            if let Ok(result) = link.try_recv() {
                self.link = None;
                self.next_poll = Instant::now() + POLL;
                match result {
                    Ok(mut delivery) => {
                        if self.command.is_none()
                            && delivery.control.revision >= self.delivery.control.revision
                        {
                            self.update_control(delivery.control.clone());
                        }
                        delivery.control = self.delivery.control.clone();
                        self.delivery = delivery;
                        self.link_error = None;
                    }
                    Err(error) => self.link_error = Some(format!("{error:#}")),
                }
            }
        }
        if paired && !self.connected && self.worker.is_none() {
            self.connected = true;
            self.connect(context);
        }
        if paired && self.connected && self.link.is_none() && Instant::now() >= self.next_poll {
            self.poll(context);
        }
        let laptop = self
            .paths
            .as_ref()
            .is_ok_and(|paths| paths.source == LAPTOP);
        let pending = self
            .delivery
            .incoming
            .0
            .as_ref()
            .filter(|state| state.phase == Phase::Ready)
            .cloned();
        let control = self.delivery.control.clone();
        let can_run = paired
            && !self.pause.active()
            && self.command.is_none()
            && self.link_error.is_none()
            && self.worker.is_none();
        if can_run {
            if let Some(ready) = pending.as_ref() {
                let acknowledged = matching_delivery(ready, &self.delivery.incoming.1)
                    .is_some_and(|s| already_reported(s.phase));
                let job = (ready.snapshot.clone(), ready.at);
                if ready.job == control.job
                    && !acknowledged
                    && self.attempted.as_ref() != Some(&job)
                    && !codex::is_open().unwrap_or(true)
                {
                    self.attempted = Some(job);
                    self.run(context, true);
                }
            }
        }
        let busy = self.worker.is_some();
        if busy && context.input(|input| input.viewport().close_requested()) {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if can_run
            && !busy
            && let Some(mode) = control.mode
        {
            let source = if laptop { LAPTOP } else { PC };
            let sent = self.delivery.outgoing.0.as_ref().is_some_and(|status| {
                status.job == control.job && matches!(status.phase, Phase::Ready | Phase::Failed)
            });
            let due = if mode == Mode::Automatic {
                self.sending_job != Some(control.job) || Instant::now() >= self.next
            } else {
                !sent && self.sending_job != Some(control.job)
            };
            if due && mode.sends(source) {
                if codex::is_open().unwrap_or(true) {
                    self.status = "Close ChatGPT on this computer to sync.".into();
                } else {
                    self.sending_job = Some(control.job);
                    self.run(context, false);
                }
            }
        }
        let shared = [
            (&self.delivery.incoming, true),
            (&self.delivery.outgoing, false),
        ]
        .into_iter()
        .filter_map(|((offer, receipt), incoming)| {
            let offer = offer.as_ref()?;
            if offer.job != control.job || control.mode.is_none() {
                return None;
            }
            let stage = matching_delivery(offer, receipt).unwrap_or(offer);
            Some((stage, incoming, offer.at))
        })
        .max_by_key(|(_, _, offered_at)| *offered_at);
        let receiving_here = if busy {
            self.progress
                .as_ref()
                .is_some_and(|s| !s.snapshot.is_empty())
        } else {
            shared.is_some_and(|(_, incoming, _)| incoming)
        };
        let receiver = if receiving_here {
            self.device_name(laptop)
        } else {
            self.delivery
                .peer_name
                .as_deref()
                .unwrap_or(default_pc_name(!laptop))
        }
        .to_owned();
        let shown = if busy {
            self.progress.clone()
        } else if self.link_error.is_none() && !self.error {
            shared.map(|(state, _, _)| state.clone())
        } else {
            None
        };
        let detail = shown
            .as_ref()
            .map(|status| status.detail.as_str())
            .or(self.link_error.as_deref())
            .unwrap_or(&self.status)
            .to_owned();
        if self.paths.is_ok() {
            context.request_repaint_after(
                self.next_pair_check
                    .saturating_duration_since(Instant::now()),
            );
        }
        ui::draw(
            self,
            context,
            frame,
            ui::View {
                paired,
                laptop,
                busy,
                receiver: &receiver,
                shown: shown.as_ref(),
                detail: &detail,
            },
        );
    }
}

fn check() -> Result<String> {
    let paths = Paths::new()?;
    let mut pair = paths.pair()?;
    pair.port = discovery::PORT;
    pair.address = discovery::find(&pair, paths.discovery_timeout_ms)?;
    let client = Client::new(pair)?;
    client.ping()?;
    ensure!(
        client.supports_progress()?,
        "Update BTWN on the hosting PC to enable shared progress"
    );
    let offer = client
        .offer(PC)?
        .context("The hosting PC’s backup is unavailable")?;
    let checked = client.verify_small_object(&offer)?;
    Ok(format!(
        "LAN discovery, pinned HTTPS, shared progress and one backup file verified. {} files listed. Verified {checked} bytes.",
        offer.files.len()
    ))
}

fn main() -> eframe::Result {
    if std::env::args().nth(1).as_deref() == Some("--check") {
        let result = check();
        let status = match &result {
            Ok(value) => value.clone(),
            Err(error) => format!("{error:#}"),
        };
        if let Some(path) = std::env::args_os().nth(2) {
            let _ = fs::write(path, status);
        }
        if result.is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    eframe::run_native(
        "BTWN",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([548.0, 640.0])
                .with_min_inner_size([320.0, 320.0])
                .with_icon(ui::window_icon()),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::new(Between::new()))),
    )
}
