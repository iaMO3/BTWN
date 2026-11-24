use serde::{Deserialize, Serialize};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    DesktopToLaptop,
    LaptopToDesktop,
    Automatic,
}

impl Mode {
    pub fn sends(self, source: &str) -> bool {
        match self {
            Self::DesktopToLaptop => source == crate::lan::PC,
            Self::LaptopToDesktop => source == crate::lan::LAPTOP,
            Self::Automatic => true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Control {
    pub mode: Option<Mode>,
    pub job: u64,
    pub paused: bool,
    pub projects: bool,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Serialize, Deserialize)]
pub enum Command {
    Start { mode: Mode, projects: bool },
    SetPaused(bool),
}

impl Control {
    pub fn change(&mut self, command: Command) {
        self.revision = self.revision.saturating_add(1);
        match command {
            Command::Start { mode, projects } => {
                self.mode = Some(mode);
                self.job = self.job.saturating_add(1);
                self.projects = projects;
            }
            Command::SetPaused(paused) => self.paused = paused,
        }
    }
}

#[derive(Default)]
pub struct Pause(AtomicBool);

#[derive(Debug)]
pub struct Paused;
impl std::fmt::Display for Paused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sync paused")
    }
}
impl std::error::Error for Paused {}

impl Pause {
    pub fn set(&self, paused: bool) {
        self.0.store(paused, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn wait(&self) {
        while self.active() {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    pub fn check_io(&self) -> io::Result<()> {
        if self.active() {
            Err(io::Error::other(Paused))
        } else {
            Ok(())
        }
    }
}

pub fn interrupted(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<Paused>()
            || cause
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .is_some_and(|cause| cause.is::<Paused>())
    })
}
