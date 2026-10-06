use std::path::PathBuf;
use std::sync::mpsc::Sender;

use dioxus::prelude::*;
use futures::StreamExt;

use crate::model::*;

const STYLE: &str = include_str!("style.css");
const AUDIO_EXTENSIONS: &[&str] =
    &["wav", "flac", "mp3", "m4a", "aac", "ogg", "oga", "opus", "aif", "aiff", "caf", "mka", "webm"];

/// All shared UI state. Every field is a `Copy` signal handle.
#[derive(Clone, Copy)]
struct AppState {
    device_name: Signal<Option<String>>,
    disc: Signal<Option<Disc>>,
    status: Signal<Option<PlayerStatus>>,
    busy: Signal<Option<String>>,
    progress: Signal<Option<(String, usize, usize)>>,
    toasts: Signal<Vec<Toast>>,
    selected: Signal<Vec<u16>>,
    modal: Signal<Option<Modal>>,
}

#[derive(Clone, PartialEq)]
struct Toast {
    id: u64,
    error: bool,
    text: String,
}

#[derive(Clone, PartialEq)]
enum Modal {
    RenameDisc(String),
    RenameTrack { index: u16, title: String },
    Upload { items: Vec<UploadItem>, format: UploadFormat },
    ConfirmDelete(Vec<u16>),
    ConfirmErase,
}

/// Handle used by components to talk to the device thread.
#[derive(Clone)]
struct DeviceHandle(Sender<Command>);

impl DeviceHandle {
    fn send(&self, cmd: Command) {
        let _ = self.0.send(cmd);
    }
}

#[component]
pub fn App() -> Element {
    let state = use_context_provider(|| AppState {
        device_name: Signal::new(None),
        disc: Signal::new(None),
        status: Signal::new(None),
        busy: Signal::new(None),
        progress: Signal::new(None),
        toasts: Signal::new(Vec::new()),
        selected: Signal::new(Vec::new()),
        modal: Signal::new(None),
    });

    let device = use_hook(|| {
        let (events_tx, mut events_rx) = futures::channel::mpsc::unbounded::<DeviceEvent>();
        let tx = crate::device::spawn(events_tx);
        spawn(async move {
            while let Some(event) = events_rx.next().await {
                apply_event(state, event);
            }
        });
        DeviceHandle(tx)
    });
    use_context_provider(|| device.clone());

    let connected = state.device_name.read().is_some();

    rsx! {
        style { {STYLE} }
        div {
            class: "app",
            ondragover: move |e| e.prevent_default(),
            ondrop: move |e| {
                e.prevent_default();
                if state.disc.read().is_none() {
                    return;
                }
                let paths: Vec<PathBuf> = e.data_transfer().files().iter().map(|f| f.path()).collect();
                open_upload_dialog(state, paths);
            },
            Header {}
            if connected {
                DiscView {}
                PlayerBar {}
            } else {
                Welcome {}
            }
            if let Some(modal) = state.modal.read().clone() {
                ModalView { modal }
            }
            BusyOverlay {}
            Toasts {}
        }
    }
}

fn apply_event(mut state: AppState, event: DeviceEvent) {
    match event {
        DeviceEvent::Connected { name } => state.device_name.set(Some(name)),
        DeviceEvent::Disconnected => {
            state.device_name.set(None);
            state.disc.set(None);
            state.status.set(None);
            state.selected.write().clear();
        }
        DeviceEvent::Disc(disc) => {
            let count = disc.as_ref().map_or(0, |d| d.tracks.len() as u16);
            state.selected.write().retain(|i| *i < count);
            state.disc.set(disc);
        }
        DeviceEvent::Status(status) => {
            if state.status.peek().as_ref() != Some(&status) {
                state.status.set(Some(status));
            }
        }
        DeviceEvent::Busy(label) => {
            if label.is_none() {
                state.progress.set(None);
            }
            state.busy.set(label);
        }
        DeviceEvent::Progress { label, done, total } => state.progress.set(Some((label, done, total))),
        DeviceEvent::Info(text) => push_toast(state, text, false),
        DeviceEvent::Error(text) => push_toast(state, text, true),
    }
}

fn push_toast(mut state: AppState, text: String, error: bool) {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    state.toasts.write().push(Toast { id, error, text });
    spawn(async move {
        sleep_ms(if error { 8000 } else { 4000 }).await;
        state.toasts.write().retain(|t| t.id != id);
    });
}

/// Sleep without blocking the UI thread or depending on a specific runtime.
async fn sleep_ms(ms: u64) {
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        let _ = tx.send(());
    });
    let _ = rx.await;
}

fn open_upload_dialog(mut state: AppState, paths: Vec<PathBuf>) {
    let items: Vec<UploadItem> = paths
        .into_iter()
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        })
        .map(|path| UploadItem {
            title: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            path,
        })
        .collect();
    if items.is_empty() {
        push_toast(state, "No supported audio files in that selection.".into(), true);
        return;
    }
    state.modal.set(Some(Modal::Upload { items, format: UploadFormat::Sp }));
}

pub fn format_frames(frames: u64) -> String {
    let secs = frames / FRAMES_PER_SECOND;
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

#[component]
fn Header() -> Element {
    let state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let name = state.device_name.read().clone();

    rsx! {
        header { class: "header",
            div { class: "brand",
                span { class: "logo" }
                "minicrab"
            }
            div { class: "device",
                if let Some(name) = name {
                    span { class: "dot on" }
                    span { "{name}" }
                    button { class: "ghost", onclick: move |_| device.send(Command::Disconnect), "Disconnect" }
                } else {
                    span { class: "dot" }
                    span { class: "muted", "No device" }
                }
            }
        }
    }
}

#[component]
fn Welcome() -> Element {
    let device = use_context::<DeviceHandle>();
    rsx! {
        main { class: "welcome",
            div { class: "disc-art" }
            h1 { "Connect your NetMD device" }
            p { class: "muted",
                "Plug in a NetMD recorder or deck over USB, switch it on and insert a disc."
            }
            button { class: "primary big", onclick: move |_| device.send(Command::Connect), "Connect" }
            p { class: "fine muted",
                "Works with Sony, Sharp, Aiwa, Kenwood and Panasonic NetMD units. Hi-MD units must be in NetMD mode."
            }
        }
    }
}

#[component]
fn DiscView() -> Element {
    let mut state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let disc = state.disc.read().clone();

    let Some(disc) = disc else {
        let label = state.status.read().as_ref().map_or("Reading…", |s| s.state.label());
        return rsx! {
            main { class: "welcome",
                div { class: "disc-art empty" }
                h1 { "{label}" }
                p { class: "muted", "Insert a MiniDisc to see its contents." }
            }
        };
    };

    let selected = state.selected.read().clone();
    let single = (selected.len() == 1).then(|| selected[0]);
    let track_count = disc.tracks.len() as u16;
    let can_write = disc.writable && !disc.write_protected;
    let pct = if disc.total_frames == 0 {
        0.0
    } else {
        disc.used_frames as f64 / disc.total_frames as f64 * 100.0
    };
    let title = if disc.title.is_empty() { "Untitled disc".to_string() } else { disc.title.clone() };
    let title_for_edit = disc.title.clone();

    rsx! {
        main { class: "disc",
            section { class: "disc-head",
                div { class: "disc-title-row",
                    h1 {
                        class: "disc-title",
                        title: "Rename disc",
                        onclick: move |_| state.modal.set(Some(Modal::RenameDisc(title_for_edit.clone()))),
                        "{title}"
                    }
                    if disc.write_protected {
                        span { class: "badge warn", "Write-protected" }
                    }
                    if !disc.writable {
                        span { class: "badge", "Premastered" }
                    }
                }
                div { class: "capacity",
                    div { class: "bar", div { class: "fill", style: "width: {pct:.1}%" } }
                    div { class: "capacity-text muted",
                        span { "{disc.tracks.len()} tracks · {format_frames(disc.used_frames)} used" }
                        span {
                            "Free: {format_frames(disc.left_frames)} SP · "
                            "{format_frames(disc.left_frames * 2)} LP2 · "
                            "{format_frames(disc.left_frames * 4)} LP4"
                        }
                    }
                }
            }

            div { class: "toolbar",
                button {
                    class: "primary",
                    disabled: !can_write,
                    onclick: move |_| async move {
                        let files = rfd::AsyncFileDialog::new()
                            .set_title("Choose audio files")
                            .add_filter("Audio", AUDIO_EXTENSIONS)
                            .pick_files()
                            .await;
                        if let Some(files) = files {
                            open_upload_dialog(state, files.iter().map(|f| f.path().to_path_buf()).collect());
                        }
                    },
                    "＋ Add tracks"
                }
                button {
                    disabled: single.is_none() || !can_write,
                    onclick: move |_| {
                        if let Some(index) = single {
                            let title = state.disc.read().as_ref()
                                .and_then(|d| d.tracks.get(index as usize).map(|t| t.title.clone()))
                                .unwrap_or_default();
                            state.modal.set(Some(Modal::RenameTrack { index, title }));
                        }
                    },
                    "Rename"
                }
                button {
                    disabled: single.is_none_or(|i| i == 0) || !can_write,
                    onclick: {
                        let device = device.clone();
                        move |_| if let Some(i) = single {
                            state.selected.set(vec![i - 1]);
                            device.send(Command::MoveTrack { from: i, to: i - 1 });
                        }
                    },
                    "↑"
                }
                button {
                    disabled: single.is_none_or(|i| i + 1 >= track_count) || !can_write,
                    onclick: {
                        let device = device.clone();
                        move |_| if let Some(i) = single {
                            state.selected.set(vec![i + 1]);
                            device.send(Command::MoveTrack { from: i, to: i + 1 });
                        }
                    },
                    "↓"
                }
                button {
                    class: "danger",
                    disabled: selected.is_empty() || !can_write,
                    onclick: move |_| state.modal.set(Some(Modal::ConfirmDelete(state.selected.read().clone()))),
                    "Delete"
                }
                button {
                    title: "Copy a track to this computer (MZ-RH1 / M200 only)",
                    disabled: single.is_none(),
                    onclick: {
                        let device = device.clone();
                        move |_| {
                            let device = device.clone();
                            async move {
                                let Some(index) = single else { return };
                                let encoding = state.disc.read().as_ref()
                                    .and_then(|d| d.tracks.get(index as usize).map(|t| t.encoding));
                                let ext = if encoding == Some(Encoding::Sp) { "aea" } else { "wav" };
                                let dest = rfd::AsyncFileDialog::new()
                                    .set_file_name(format!("Track {:02}.{ext}", index + 1))
                                    .save_file()
                                    .await;
                                if let Some(dest) = dest {
                                    device.send(Command::Download { index, dest: dest.path().to_path_buf() });
                                }
                            }
                        }
                    },
                    "Save to computer"
                }
                div { class: "spacer" }
                button {
                    class: "ghost",
                    onclick: {
                        let device = device.clone();
                        move |_| device.send(Command::Refresh)
                    },
                    "Refresh"
                }
                button {
                    class: "ghost",
                    onclick: {
                        let device = device.clone();
                        move |_| device.send(Command::Eject)
                    },
                    "Eject"
                }
                button {
                    class: "ghost danger",
                    disabled: !can_write,
                    onclick: move |_| state.modal.set(Some(Modal::ConfirmErase)),
                    "Erase disc"
                }
            }

            TrackList { disc: disc.clone() }
        }
    }
}

#[component]
fn TrackList(disc: Disc) -> Element {
    let mut state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let selected = state.selected.read().clone();
    let now_playing = state.status.read().as_ref().and_then(|s| {
        matches!(s.state, PlayState::Playing | PlayState::Paused).then_some(s.track as u16)
    });

    if disc.tracks.is_empty() {
        return rsx! {
            div { class: "empty-list muted",
                p { "This disc is empty." }
                p { "Drop audio files here or use “Add tracks”." }
            }
        };
    }

    // Ungrouped tracks first, then each named group with a header row.
    let mut groups = disc.groups.clone();
    groups.sort_by_key(|g| g.title.is_some());

    rsx! {
        div { class: "tracks",
            div { class: "track-row head",
                span { class: "num", "#" }
                span { class: "title", "Title" }
                span { class: "mode", "Mode" }
                span { class: "dur", "Length" }
            }
            for group in groups {
                if let Some(title) = group.title.clone() {
                    div { class: "group-row", key: "g-{title}", "▾ {title}" }
                }
                for index in group.tracks.iter().copied() {
                    if let Some(track) = disc.tracks.get(index as usize).cloned() {
                        div {
                            key: "t-{index}",
                            class: format!(
                                "track-row{}{}{}",
                                if selected.contains(&index) { " selected" } else { "" },
                                if now_playing == Some(index) { " playing" } else { "" },
                                if group.title.is_some() { " grouped" } else { "" },
                            ),
                            onclick: move |e: MouseEvent| {
                                let m = e.modifiers();
                                let mut sel = state.selected.write();
                                if m.meta() || m.ctrl() {
                                    if let Some(pos) = sel.iter().position(|i| *i == index) {
                                        sel.remove(pos);
                                    } else {
                                        sel.push(index);
                                    }
                                } else if m.shift() && !sel.is_empty() {
                                    let anchor = *sel.last().unwrap();
                                    let (a, b) = (anchor.min(index), anchor.max(index));
                                    *sel = (a..=b).collect();
                                } else {
                                    *sel = vec![index];
                                }
                            },
                            ondoubleclick: {
                                let device = device.clone();
                                move |_| device.send(Command::GoToTrack(index))
                            },
                            span { class: "num",
                                if now_playing == Some(index) { "▶" } else { "{index + 1}" }
                            }
                            span { class: "title",
                                "{track.display_title()}"
                                if track.protected {
                                    span { class: "lock", title: "Copy-protected (SCMS)", " 🔒" }
                                }
                            }
                            span { class: "mode",
                                span { class: "badge small", "{track.encoding.label()}" }
                                if !track.stereo { span { class: "badge small", "Mono" } }
                            }
                            span { class: "dur", "{format_frames(track.frames)}" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn PlayerBar() -> Element {
    let state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let status = state.status.read().clone();
    let disc = state.disc.read();

    let (state_label, track_line, time) = match &status {
        Some(s) if s.disc_present => {
            let title = disc
                .as_ref()
                .and_then(|d| d.tracks.get(s.track as usize))
                .map(|t| t.display_title().to_string())
                .unwrap_or_default();
            (
                s.state.label(),
                format!("{:02}  {}", s.track as u16 + 1, title),
                format!("{}:{:02}", s.minute, s.second),
            )
        }
        Some(s) => (s.state.label(), String::new(), String::new()),
        None => ("", String::new(), String::new()),
    };
    let playing = status.as_ref().is_some_and(|s| s.state == PlayState::Playing);

    let cmd = move |c: fn() -> Command| {
        let device = device.clone();
        move |_| device.send(c())
    };

    rsx! {
        footer { class: "player",
            div { class: "transport",
                button { class: "round", title: "Previous", onclick: cmd(|| Command::Previous), "⏮" }
                if playing {
                    button { class: "round main", title: "Pause", onclick: cmd(|| Command::Pause), "⏸" }
                } else {
                    button { class: "round main", title: "Play", onclick: cmd(|| Command::Play), "▶" }
                }
                button { class: "round", title: "Stop", onclick: cmd(|| Command::Stop), "⏹" }
                button { class: "round", title: "Next", onclick: cmd(|| Command::Next), "⏭" }
            }
            div { class: "lcd",
                div { class: "lcd-track", "{track_line}" }
                div { class: "lcd-meta",
                    span { "{state_label}" }
                    span { class: "lcd-time", "{time}" }
                }
            }
        }
    }
}

#[component]
fn ModalView(modal: Modal) -> Element {
    let mut state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let close = move |_| state.modal.set(None);

    let body = match modal {
        Modal::RenameDisc(title) => rsx! {
            RenameForm {
                heading: "Rename disc",
                initial: title,
                on_submit: move |title: String| {
                    state.modal.set(None);
                    device.send(Command::RenameDisc(title));
                },
            }
        },
        Modal::RenameTrack { index, title } => rsx! {
            RenameForm {
                heading: format!("Rename track {}", index + 1),
                initial: title,
                on_submit: move |title: String| {
                    state.modal.set(None);
                    device.send(Command::RenameTrack { index, title });
                },
            }
        },
        Modal::Upload { items, format } => rsx! { UploadForm { items, format } },
        Modal::ConfirmDelete(indices) => {
            let n = indices.len();
            rsx! {
                h2 { "Delete {n} track{plural(n)}?" }
                p { class: "muted", "This can't be undone." }
                div { class: "actions",
                    button { onclick: close, "Cancel" }
                    button {
                        class: "danger solid",
                        onclick: move |_| {
                            state.modal.set(None);
                            state.selected.write().clear();
                            device.send(Command::DeleteTracks(indices.clone()));
                        },
                        "Delete"
                    }
                }
            }
        }
        Modal::ConfirmErase => rsx! {
            h2 { "Erase the whole disc?" }
            p { class: "muted", "All tracks, groups and the disc title will be removed. This can't be undone." }
            div { class: "actions",
                button { onclick: close, "Cancel" }
                button {
                    class: "danger solid",
                    onclick: move |_| {
                        state.modal.set(None);
                        device.send(Command::EraseDisc);
                    },
                    "Erase"
                }
            }
        },
    };

    rsx! {
        div { class: "modal-backdrop", onclick: close,
            div { class: "modal", onclick: move |e| e.stop_propagation(), {body} }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[component]
fn RenameForm(heading: String, initial: String, on_submit: EventHandler<String>) -> Element {
    let mut state = use_context::<AppState>();
    let mut value = use_signal(|| initial);
    rsx! {
        form {
            onsubmit: move |e| {
                e.prevent_default();
                on_submit.call(value.read().trim().to_string());
            },
            h2 { "{heading}" }
            input {
                r#type: "text",
                autofocus: true,
                value: "{value}",
                oninput: move |e| value.set(e.value()),
            }
            p { class: "fine muted",
                "Half-width titles are limited to ASCII; accented characters are simplified by the device."
            }
            div { class: "actions",
                button { r#type: "button", onclick: move |_| state.modal.set(None), "Cancel" }
                button { class: "primary", r#type: "submit", "Save" }
            }
        }
    }
}

#[component]
fn UploadForm(items: Vec<UploadItem>, format: UploadFormat) -> Element {
    let mut state = use_context::<AppState>();
    let device = use_context::<DeviceHandle>();
    let mut items = use_signal(|| items);
    let mut format = use_signal(|| format);
    let has_encoder = use_hook(|| crate::audio::find_atracdenc().is_some());
    let left = state.disc.read().as_ref().map_or(0, |d| d.left_frames);

    let count = items.read().len();
    rsx! {
        h2 { "Send {count} track{plural(count)} to the disc" }
        div { class: "formats",
            for f in [UploadFormat::Sp, UploadFormat::Lp2, UploadFormat::Lp4] {
                label {
                    class: if *format.read() == f { "format active" } else { "format" },
                    class: if f != UploadFormat::Sp && !has_encoder { "disabled" },
                    input {
                        r#type: "radio",
                        name: "format",
                        checked: *format.read() == f,
                        disabled: f != UploadFormat::Sp && !has_encoder,
                        onchange: move |_| format.set(f),
                    }
                    strong { "{f.label()}" }
                    span { class: "muted", "{format_frames(left * f.frame_divisor())} free" }
                }
            }
        }
        if !has_encoder {
            p { class: "fine muted",
                "LP2/LP4 need the "
                code { "atracdenc" }
                " encoder on your PATH (or set "
                code { "ATRACDENC" }
                "). SP is encoded by the device itself."
            }
        }
        div { class: "upload-list",
            for (i, item) in items.read().iter().cloned().enumerate() {
                div { class: "upload-item", key: "{item.path.display()}",
                    span { class: "num muted", "{i + 1}" }
                    input {
                        r#type: "text",
                        value: "{item.title}",
                        oninput: move |e| items.write()[i].title = e.value(),
                    }
                    button {
                        class: "ghost",
                        title: "Remove",
                        onclick: move |_| { items.write().remove(i); },
                        "✕"
                    }
                }
            }
        }
        div { class: "actions",
            button { onclick: move |_| state.modal.set(None), "Cancel" }
            button {
                class: "primary",
                disabled: count == 0,
                onclick: move |_| {
                    state.modal.set(None);
                    device.send(Command::Upload { items: items.read().clone(), format: *format.read() });
                },
                "Send to disc"
            }
        }
    }
}

#[component]
fn BusyOverlay() -> Element {
    let state = use_context::<AppState>();
    let Some(label) = state.busy.read().clone() else { return rsx! {} };
    let progress = state.progress.read().clone();
    let pct = progress
        .as_ref()
        .filter(|(_, _, total)| *total > 0)
        .map(|(_, done, total)| *done as f64 / *total as f64 * 100.0);

    rsx! {
        div { class: "busy",
            div { class: "busy-card",
                div { class: "spinner" }
                div { class: "busy-label", "{label}" }
                if let Some(pct) = pct {
                    div { class: "bar", div { class: "fill", style: "width: {pct:.1}%" } }
                    div { class: "fine muted", "{pct:.0}%" }
                }
            }
        }
    }
}

#[component]
fn Toasts() -> Element {
    let mut state = use_context::<AppState>();
    rsx! {
        div { class: "toasts",
            for toast in state.toasts.read().iter().cloned() {
                div {
                    key: "{toast.id}",
                    class: if toast.error { "toast error" } else { "toast" },
                    onclick: move |_| state.toasts.write().retain(|t| t.id != toast.id),
                    "{toast.text}"
                }
            }
        }
    }
}
