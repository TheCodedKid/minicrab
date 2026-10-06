//! The device thread. It owns the NetMD connection and processes commands one
//! at a time.
//!
//! `minidisc` sleeps with `std::thread::sleep` inside its async functions on
//! native targets, so device I/O must never run on the UI thread.

use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures::channel::mpsc::UnboundedSender;
use futures::executor::block_on;
use minidisc::netmd::commands::OperatingStatus;
use minidisc::netmd::interface::{
    Channels, DiscFlag, DiscFormat, Encoding as MdEncoding, InterfaceError, MDTrack, WireFormat,
};
use minidisc::netmd::{DEVICE_IDS_CROSSUSB, NetMDContext};

use crate::audio;
use crate::model::*;

const POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Start the device thread. Returns the command sender.
pub fn spawn(events: UnboundedSender<DeviceEvent>) -> mpsc::Sender<Command> {
    let (tx, rx) = mpsc::channel::<Command>();
    std::thread::Builder::new()
        .name("netmd".into())
        .spawn(move || run(rx, events))
        .expect("failed to spawn device thread");
    tx
}

fn run(rx: mpsc::Receiver<Command>, events: UnboundedSender<DeviceEvent>) {
    let mut worker = Worker { ctx: None, events, last_disc_present: None };

    loop {
        let cmd = if worker.ctx.is_some() {
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => return,
            }
        };

        // The protocol library has a few `unwrap`s on device replies; keep a
        // malformed reply from taking the whole thread down.
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| match cmd {
            Some(cmd) => block_on(worker.handle(cmd)),
            None => block_on(worker.poll()),
        }));

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                worker.send(DeviceEvent::Busy(None));
                worker.send(DeviceEvent::Error(format!("{e:#}")));
                if is_fatal(&e) {
                    worker.disconnect();
                }
            }
            Err(_) => {
                worker.send(DeviceEvent::Busy(None));
                worker.send(DeviceEvent::Error("The device sent an unexpected reply.".into()));
            }
        }
    }
}

/// USB-level failures mean the device is gone or wedged; drop the connection.
fn is_fatal(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<InterfaceError>(),
            Some(InterfaceError::CommunicationError(_) | InterfaceError::MaxRetries)
        )
    })
}

/// `minidisc` returns `Box<dyn Error>` (not `Send`) from its high-level calls.
fn boxed(e: Box<dyn std::error::Error>) -> anyhow::Error {
    anyhow!("{e}")
}

struct Worker {
    ctx: Option<NetMDContext>,
    events: UnboundedSender<DeviceEvent>,
    last_disc_present: Option<bool>,
}

impl Worker {
    fn send(&self, event: DeviceEvent) {
        let _ = self.events.unbounded_send(event);
    }

    fn ctx(&mut self) -> Result<&mut NetMDContext> {
        self.ctx.as_mut().ok_or_else(|| anyhow!("No device connected"))
    }

    fn disconnect(&mut self) {
        self.ctx = None;
        self.last_disc_present = None;
        self.send(DeviceEvent::Disconnected);
    }

    async fn handle(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::Connect => return self.connect().await,
            Command::Disconnect => {
                self.disconnect();
                return Ok(());
            }
            Command::Refresh => {}
            Command::Play => self.ctx()?.interface_mut().play().await?,
            Command::Pause => self.ctx()?.interface_mut().pause().await?,
            Command::Stop => self.ctx()?.interface_mut().stop().await?,
            Command::Next => self.ctx()?.next_track().await?,
            Command::Previous => self.ctx()?.previous_track().await?,
            Command::GoToTrack(index) => {
                let md = self.ctx()?.interface_mut();
                md.go_to_track(index).await?;
                md.play().await?;
            }
            Command::Eject => {
                self.ctx()?.interface_mut().eject_disc().await?;
            }
            Command::RenameDisc(title) => {
                self.ctx()?.rename_disc(&title, None).await.map_err(boxed)?;
            }
            Command::RenameTrack { index, title } => self.rename_track(index, &title).await?,
            Command::DeleteTracks(mut indices) => {
                // Delete from the end so earlier indices stay valid.
                indices.sort_unstable_by(|a, b| b.cmp(a));
                indices.dedup();
                let md = self.ctx()?.interface_mut();
                for index in indices {
                    md.erase_track(index).await?;
                }
            }
            Command::MoveTrack { from, to } => {
                self.ctx()?.interface_mut().move_track(from, to).await?;
            }
            Command::EraseDisc => self.ctx()?.interface_mut().erase_disc().await?,
            Command::Upload { items, format } => self.upload(items, format).await?,
            Command::Download { index, dest } => self.download(index, dest).await?,
        }

        // Every command that may have changed the disc or player ends with a refresh.
        self.refresh().await
    }

    async fn connect(&mut self) -> Result<()> {
        self.send(DeviceEvent::Busy(Some("Looking for a NetMD device…".into())));
        let device = cross_usb::get_device(DEVICE_IDS_CROSSUSB.to_vec())
            .await
            .map_err(|_| anyhow!("No NetMD device found. Is it plugged in and switched on?"))?;
        let ctx = NetMDContext::new(device).await.context("Could not open the device")?;
        let name = ctx.interface().device.device_name().unwrap_or("NetMD device").to_string();
        self.ctx = Some(ctx);
        self.send(DeviceEvent::Connected { name });
        self.refresh().await
    }

    async fn poll(&mut self) -> Result<()> {
        let status = self.status().await?;
        let changed = self.last_disc_present != Some(status.disc_present);
        self.send(DeviceEvent::Status(status));
        if changed {
            self.refresh().await?;
        }
        Ok(())
    }

    async fn status(&mut self) -> Result<PlayerStatus> {
        let s = self.ctx()?.device_status().await.map_err(boxed)?;
        let state = match s.state {
            Some(OperatingStatus::Ready) => PlayState::Ready,
            Some(OperatingStatus::Playing) => PlayState::Playing,
            Some(OperatingStatus::Paused) => PlayState::Paused,
            Some(OperatingStatus::FastForward) => PlayState::FastForward,
            Some(OperatingStatus::Rewind) => PlayState::Rewind,
            Some(OperatingStatus::ReadingTOC) => PlayState::ReadingToc,
            Some(OperatingStatus::NoDisc) => PlayState::NoDisc,
            Some(OperatingStatus::DiscBlank) => PlayState::DiscBlank,
            Some(OperatingStatus::ReadyForTransfer) => PlayState::ReadyForTransfer,
            None => PlayState::Unknown,
        };
        Ok(PlayerStatus {
            disc_present: s.disc_present,
            state,
            track: s.track,
            minute: s.time.minute,
            second: s.time.second,
        })
    }

    async fn refresh(&mut self) -> Result<()> {
        self.send(DeviceEvent::Busy(Some("Reading disc…".into())));
        let status = self.status().await?;
        self.last_disc_present = Some(status.disc_present);
        self.send(DeviceEvent::Status(status.clone()));

        let disc = if status.disc_present { Some(self.read_disc().await?) } else { None };
        self.send(DeviceEvent::Disc(disc));
        self.send(DeviceEvent::Busy(None));
        Ok(())
    }

    async fn read_disc(&mut self) -> Result<Disc> {
        let md = self.ctx()?.interface_mut();

        let flags = md.disc_flags().await?;
        let title = md.disc_title(false).await?;
        let full_width_title = md.disc_title(true).await?;
        let [used, total, left] = md.disc_capacity().await?;
        let (mut used, mut total, mut left) = (used.as_frames(), total.as_frames(), left.as_frames());
        // Some devices (Sharp) report capacity in the current recording mode.
        while total > FRAMES_PER_SECOND * 60 * 82 {
            used /= 2;
            total /= 2;
            left /= 2;
        }

        let groups: Vec<Group> = md
            .track_group_list()
            .await?
            .into_iter()
            .map(|(title, _, tracks)| Group { title, tracks })
            .collect();

        let track_count = md.track_count().await?;
        let mut tracks = Vec::with_capacity(track_count as usize);
        for index in 0..track_count {
            let (encoding, channels) = md.track_encoding(index).await?;
            tracks.push(Track {
                index,
                title: md.track_title(index, false).await?,
                full_width_title: md.track_title(index, true).await?,
                frames: md.track_length(index).await?.as_frames(),
                encoding: match encoding {
                    MdEncoding::SP => Encoding::Sp,
                    MdEncoding::LP2 => Encoding::Lp2,
                    MdEncoding::LP4 => Encoding::Lp4,
                },
                stereo: matches!(channels, Channels::Stereo),
                protected: md.track_flags(index).await? != 0,
            });
        }

        Ok(Disc {
            title,
            full_width_title,
            writable: flags & DiscFlag::Writable as u8 != 0,
            write_protected: flags & DiscFlag::WriteProtected as u8 != 0,
            used_frames: used,
            total_frames: total,
            left_frames: left,
            groups,
            tracks,
        })
    }

    async fn rename_track(&mut self, index: u16, title: &str) -> Result<()> {
        let md = self.ctx()?.interface_mut();
        match md.set_track_title(index, title, false).await {
            Ok(()) | Err(InterfaceError::TitleError) => {}
            Err(e) => return Err(e.into()),
        }
        // Titles with Japanese / non-ASCII text also get a full-width title.
        let full_width = if title.is_ascii() { "" } else { title };
        match md.set_track_title(index, full_width, true).await {
            Ok(()) | Err(InterfaceError::TitleError) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn upload(&mut self, items: Vec<UploadItem>, format: UploadFormat) -> Result<()> {
        let count = items.len();
        for (n, item) in items.into_iter().enumerate() {
            let prefix = format!("[{}/{}] {}", n + 1, count, item.title);

            self.send(DeviceEvent::Busy(Some(format!("{prefix}: converting…"))));
            let pcm = audio::decode_to_cd_pcm(&item.path)?;
            let (wire, data) = match format {
                UploadFormat::Sp => (WireFormat::Pcm, audio::pcm_to_netmd_bytes(&pcm)),
                UploadFormat::Lp2 => (WireFormat::LP2, audio::encode_atrac3(&pcm, format)?),
                UploadFormat::Lp4 => (WireFormat::LP4, audio::encode_atrac3(&pcm, format)?),
            };
            drop(pcm);

            let track = MDTrack {
                title: item.title.clone(),
                format: wire,
                data,
                chunk_size: 0x10_0000,
                full_width_title: (!item.title.is_ascii()).then(|| item.title.clone()),
            };

            self.send(DeviceEvent::Busy(Some(format!("{prefix}: sending…"))));
            let events = self.events.clone();
            let label = prefix.clone();
            self.ctx()?
                .download(track, move |total, done| {
                    let _ = events.unbounded_send(DeviceEvent::Progress { label: label.clone(), done, total });
                })
                .await
                .map_err(boxed)
                .with_context(|| format!("Sending \"{}\" failed", item.title))?;
            self.send(DeviceEvent::Info(format!("Sent \"{}\"", item.title)));
        }
        Ok(())
    }

    async fn download(&mut self, index: u16, dest: std::path::PathBuf) -> Result<()> {
        let events = self.events.clone();
        let label = format!("Reading track {}", index + 1);
        self.send(DeviceEvent::Busy(Some(format!("{label}…"))));

        let md = self.ctx()?.interface_mut();
        let title = md.track_title(index, false).await.unwrap_or_default();
        let (format, _frames, data) = md
            .save_track_to_array(
                index,
                Some(move |total, done| {
                    let _ = events.unbounded_send(DeviceEvent::Progress { label: label.clone(), done, total });
                }),
            )
            .await
            .map_err(|e| match e {
                InterfaceError::Rejected(_) | InterfaceError::NotImplemented(_) => {
                    anyhow!("This device can't send audio back to the computer (only MZ-RH1/M200 can).")
                }
                e => e.into(),
            })?;

        let header = match format {
            DiscFormat::SPStereo | DiscFormat::SPMono => {
                let channels = if format == DiscFormat::SPStereo { 2 } else { 1 };
                audio::aea_header(&title, channels, (data.len() / 212) as u32)
            }
            DiscFormat::LP2 => audio::atrac3_wav_header(false, data.len() as u32),
            DiscFormat::LP4 => audio::atrac3_wav_header(true, data.len() as u32),
        };
        if data.is_empty() {
            bail!("The device returned no audio data");
        }
        let mut file = header;
        file.extend_from_slice(&data);
        std::fs::write(&dest, file).with_context(|| format!("writing {}", dest.display()))?;
        self.send(DeviceEvent::Info(format!("Saved {}", dest.display())));
        Ok(())
    }
}
