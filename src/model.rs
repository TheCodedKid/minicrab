//! Plain data types shared between the device thread and the UI.

use std::path::PathBuf;

/// NetMD time frames per second (one "sound group" = 1/512 s on the TOC).
pub const FRAMES_PER_SECOND: u64 = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// Zero-based index on the disc.
    pub index: u16,
    pub title: String,
    pub full_width_title: String,
    pub frames: u64,
    pub encoding: Encoding,
    pub stereo: bool,
    pub protected: bool,
}

impl Track {
    pub fn display_title(&self) -> &str {
        if self.title.is_empty() { &self.full_width_title } else { &self.title }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// `None` for the implicit "ungrouped tracks" group.
    pub title: Option<String>,
    pub tracks: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Disc {
    pub title: String,
    pub full_width_title: String,
    pub writable: bool,
    pub write_protected: bool,
    pub used_frames: u64,
    pub total_frames: u64,
    pub left_frames: u64,
    pub groups: Vec<Group>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Sp,
    Lp2,
    Lp4,
}

impl Encoding {
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Sp => "SP",
            Encoding::Lp2 => "LP2",
            Encoding::Lp4 => "LP4",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerStatus {
    pub disc_present: bool,
    pub state: PlayState,
    pub track: u8,
    pub minute: u16,
    pub second: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Ready,
    Playing,
    Paused,
    FastForward,
    Rewind,
    ReadingToc,
    NoDisc,
    DiscBlank,
    ReadyForTransfer,
    Unknown,
}

impl PlayState {
    pub fn label(self) -> &'static str {
        match self {
            PlayState::Ready => "Stopped",
            PlayState::Playing => "Playing",
            PlayState::Paused => "Paused",
            PlayState::FastForward => "Fast forward",
            PlayState::Rewind => "Rewind",
            PlayState::ReadingToc => "Reading TOC…",
            PlayState::NoDisc => "No disc",
            PlayState::DiscBlank => "Blank disc",
            PlayState::ReadyForTransfer => "Ready for transfer",
            PlayState::Unknown => "Unknown",
        }
    }
}

/// Recording mode chosen when sending audio to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFormat {
    /// Raw PCM; the device encodes ATRAC1 itself.
    Sp,
    /// ATRAC3 132 kbps, encoded on the host with `atracdenc`.
    Lp2,
    /// ATRAC3 66 kbps, encoded on the host with `atracdenc`.
    Lp4,
}

impl UploadFormat {
    pub fn label(self) -> &'static str {
        match self {
            UploadFormat::Sp => "SP",
            UploadFormat::Lp2 => "LP2",
            UploadFormat::Lp4 => "LP4",
        }
    }

    /// How many disc frames one second of audio consumes in this mode.
    pub fn frame_divisor(self) -> u64 {
        match self {
            UploadFormat::Sp => 1,
            UploadFormat::Lp2 => 2,
            UploadFormat::Lp4 => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UploadItem {
    pub path: PathBuf,
    pub title: String,
}

/// Commands sent from the UI to the device thread.
#[derive(Debug)]
pub enum Command {
    Connect,
    Disconnect,
    Refresh,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    GoToTrack(u16),
    Eject,
    RenameDisc(String),
    RenameTrack { index: u16, title: String },
    DeleteTracks(Vec<u16>),
    MoveTrack { from: u16, to: u16 },
    EraseDisc,
    Upload { items: Vec<UploadItem>, format: UploadFormat },
    /// Read a track back to the computer (MZ-RH1 / M200 only).
    Download { index: u16, dest: PathBuf },
}

/// Events sent from the device thread back to the UI.
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    Connected { name: String },
    Disconnected,
    Disc(Option<Disc>),
    Status(PlayerStatus),
    Busy(Option<String>),
    Progress { label: String, done: usize, total: usize },
    Info(String),
    Error(String),
}
