//! A desktop window over the `librarybridge` command line tool.
//!
//! Two jobs: repair a Steam library whose Proton data is on a filesystem that
//! cannot hold it, and add installed games that Lutris does not know about.
//!
//! Every action that changes anything is shown as the tool's own dry run
//! first, verbatim, so the window can never describe the operation differently
//! from the program that performs it.

mod backend;
mod png;

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::{channel, Receiver, Sender};

use backend::{Candidate, Library, Update};
use eframe::egui;

const WINDOW: [f32; 2] = [1280.0, 800.0];
/// How much command output the window keeps. Enough to read what happened,
/// bounded so a long copy cannot fill memory with progress lines.
const LOG_LINES: usize = 2000;
const APP_ICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/branding/librarybridge-controller-bridge-top-lb-1024.png"
));

/// Where the window remembers a `--data-dir` choice between sessions. It lives
/// in the default data home, not in the data directory itself, so the setting
/// survives the location it names being on an unmounted drive.
fn saved_data_dir_path() -> std::path::PathBuf {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if std::path::Path::new(&dir).is_absolute() => std::path::PathBuf::from(dir),
        _ => std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".local/share"),
    };
    base.join("librarybridge").join("data-dir.txt")
}

fn save_data_dir(path: &String) {
    let file = saved_data_dir_path();
    let parent = file.parent().unwrap_or(std::path::Path::new("/"));
    let _ = std::fs::create_dir_all(parent);
    let _ = std::fs::write(&file, format!("{path}\n"));
}

fn main() -> eframe::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| -> Option<String> {
        arguments
            .iter()
            .position(|a| a == name)
            .and_then(|i| arguments.get(i + 1))
            .cloned()
    };
    let roots: Vec<String> = arguments
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--root")
        .filter_map(|(i, _)| arguments.get(i + 1).cloned())
        .collect();
    let start = Start {
        screenshot: value("--screenshot").map(std::path::PathBuf::from),
        screen: value("--screen").unwrap_or_else(|| "home".to_string()),
        review: value("--review"),
        roots,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW)
            .with_min_inner_size([900.0, 600.0])
            .with_icon(app_icon())
            .with_app_id("librarybridge")
            .with_title("LibraryBridge"),
        ..Default::default()
    };
    eframe::run_native(
        "LibraryBridge",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, start)))),
    )
}

fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(APP_ICON_BYTES)
        .expect("LibraryBridge icon must be a valid PNG")
}

/// How the window opens. Everything but `roots` exists so screens can be
/// captured during development without clicking through them.
struct Start {
    screenshot: Option<std::path::PathBuf>,
    screen: String,
    review: Option<String>,
    roots: Vec<String>,
}

#[derive(Clone, PartialEq)]
enum Screen {
    Home,
    Libraries,
    /// The tool's own dry run for a library, awaiting a decision.
    Review {
        id: String,
        action: Action,
        title: String,
    },
    Games,
    Storage,
    Help,
    Running {
        title: String,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Action {
    Fix,
    Undo,
}

impl Action {
    fn command(self) -> &'static str {
        match self {
            Action::Fix => "fix",
            Action::Undo => "undo",
        }
    }
}

struct App {
    screen: Screen,
    back_to: Screen,

    libraries: Vec<Library>,
    candidates: Vec<Candidate>,
    lutris: Option<Result<String, String>>,

    roots: Vec<String>,
    root_input: String,
    /// Where relocated Proton data should live. Empty means the tool's
    /// default under the user's data home.
    data_dir: String,
    data_dir_input: String,
    selected: BTreeSet<String>,
    edits: HashMap<String, Candidate>,
    expanded: BTreeSet<String>,

    log: Vec<String>,
    busy: bool,
    finished: Option<bool>,
    error: Option<String>,
    /// Whether the dry run behind the current review succeeded. Apply is
    /// available only when it did.
    review_ok: Option<bool>,
    /// The plan identity that dry run reported, quoted back when applying so
    /// the tool refuses if the library changed in between.
    plan_id: Option<String>,
    /// The reviewed plan as fields, so the screen shows a summary rather than
    /// the tool's terminal output.
    plan: Option<backend::Plan>,
    stored: Vec<backend::Stored>,
    /// Notes the scan produced, such as metadata it could not read. Shown, not
    /// swallowed: an empty library list and an unreadable one look identical
    /// otherwise.
    warnings: Vec<String>,
    scan_complete: bool,
    /// Progress of a Lutris folder scan, when one is running: (done, total).
    /// `None` when nothing is scanning.
    scanning: Option<(usize, usize)>,
    /// The phase a running operation is in, and the child running it, so the
    /// window knows whether stopping is safe and can actually do it.
    phase: Option<backend::Phase>,
    running: backend::Running,
    cancelled: bool,
    /// A `lutris import` was started and its `Done` has not been handled yet.
    /// The window re-scans the games list only for this, so a finished scan
    /// does not immediately launch another scan (which would loop forever).
    just_imported: bool,
    /// The library a run is about, so its evidence can be shown afterwards.
    subject: Option<String>,
    evidence: Vec<backend::EvidenceRow>,

    sender: Sender<Update>,
    receiver: Receiver<Update>,

    icon_texture: egui::TextureHandle,
    screenshot: Option<std::path::PathBuf>,
    frames: u32,
    /// A folder chooser that is currently open, polled a little each frame.
    chooser: Option<std::pin::Pin<Box<dyn std::future::Future<Output = Option<rfd::FileHandle>>>>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, start: Start) -> App {
        // Respect whatever scale the desktop asks for rather than imposing
        // one. A fixed factor fights the user's own display settings.
        let mut style = (*cc.egui_ctx.style()).clone();
        // Controls large enough to hit with a trackpad or a thumb.
        style.spacing.interact_size.y = style.spacing.interact_size.y.max(30.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        cc.egui_ctx.set_style(style);
        let decoded_icon = image::load_from_memory(APP_ICON_BYTES)
            .expect("LibraryBridge icon must be decodable")
            .to_rgba8();
        let icon_size = [
            decoded_icon.width() as usize,
            decoded_icon.height() as usize,
        ];
        let icon_image = egui::ColorImage::from_rgba_unmultiplied(icon_size, decoded_icon.as_raw());
        let icon_texture = cc.egui_ctx.load_texture(
            "librarybridge-app-icon",
            icon_image,
            egui::TextureOptions::LINEAR,
        );
        let (sender, receiver) = channel();
        let mut app = App {
            screen: match start.screen.as_str() {
                "games" => Screen::Games,
                "libraries" => Screen::Libraries,
                "storage" => Screen::Storage,
                "help" => Screen::Help,
                _ => Screen::Home,
            },
            back_to: Screen::Home,
            libraries: Vec::new(),
            candidates: Vec::new(),
            lutris: None,
            roots: start.roots.clone(),
            root_input: String::new(),
            data_dir: std::fs::read_to_string(saved_data_dir_path())
                .map(|text| text.trim().to_string())
                .unwrap_or_default(),
            data_dir_input: String::new(),
            selected: BTreeSet::new(),
            edits: HashMap::new(),
            expanded: BTreeSet::new(),
            log: Vec::new(),
            busy: false,
            finished: None,
            error: None,
            review_ok: None,
            plan_id: None,
            plan: None,
            stored: Vec::new(),
            warnings: Vec::new(),
            scan_complete: true,
            scanning: None,
            phase: None,
            running: backend::Running::default(),
            cancelled: false,
            just_imported: false,
            subject: None,
            evidence: Vec::new(),
            sender,
            receiver,
            icon_texture,
            screenshot: start.screenshot,
            frames: 0,
            chooser: None,
        };
        app.refresh_libraries();
        app.refresh_lutris();
        if !app.roots.is_empty() {
            app.refresh_candidates();
        }
        if matches!(app.screen, Screen::Storage) {
            let data_dir = app.data_dir.clone();
            backend::spawn(app.sender.clone(), move |tx| {
                let _ = tx.send(Update::Storage(backend::storage(&data_dir)));
            });
        }
        if let Some(id) = start.review {
            app.back_to = Screen::Libraries;
            app.review(&id, Action::Fix, "Repair this library".to_string());
        }
        app
    }

    fn refresh_libraries(&mut self) {
        let sender = self.sender.clone();
        let data_dir = self.data_dir.clone();
        backend::spawn(sender, move |tx| {
            let _ = tx.send(Update::Libraries(backend::libraries(&data_dir)));
        });
    }

    fn refresh_lutris(&mut self) {
        let sender = self.sender.clone();
        let data_dir = self.data_dir.clone();
        backend::spawn(sender, move |tx| {
            let _ = tx.send(Update::Lutris(backend::lutris_status(&data_dir)));
        });
    }

    /// Open the desktop's own folder chooser.
    ///
    /// The dialog is asked for asynchronously and polled from `update` rather
    /// than called as a blocking function. A blocking modal driven from inside
    /// the window's own event loop is dismissed immediately on macOS, and the
    /// async form is how this dialog is meant to be used alongside a winit
    /// event loop. The window keeps painting while it is open.
    ///
    /// It starts where the last chosen folder was, so adding a second drive
    /// does not begin at the home directory again.
    fn open_chooser(&mut self) {
        if self.chooser.is_some() {
            return;
        }
        let start = self
            .roots
            .last()
            .map(std::path::PathBuf::from)
            .filter(|path| path.is_dir())
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
            .filter(|path| path.is_dir());

        let mut dialog =
            rfd::AsyncFileDialog::new().set_title("Choose a folder to look for games in");
        if let Some(start) = start {
            dialog = dialog.set_directory(start);
        }
        self.chooser = Some(Box::pin(dialog.pick_folder()));
    }

    fn poll_chooser(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.chooser.as_mut() else {
            return;
        };
        // Keep frames coming while the dialog is up, so it is polled again.
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        if let std::task::Poll::Ready(result) = pending.as_mut().poll(&mut context) {
            self.chooser = None;
            if let Some(handle) = result {
                self.add_root(handle.path().to_string_lossy().to_string());
            }
        }
    }

    /// Take on a folder to scan, from either the chooser or the text field.
    /// A folder that is already listed, or that is not there, is not added.
    fn add_root(&mut self, path: String) {
        if self.roots.iter().any(|existing| existing == &path) {
            return;
        }
        if !std::path::Path::new(&path).is_dir() {
            self.error = Some(format!("{path} is not a folder"));
            return;
        }
        self.error = None;
        self.roots.push(path);
        self.refresh_candidates();
    }

    fn refresh_candidates(&mut self) {
        let roots = self.roots.clone();
        let data_dir = self.data_dir.clone();
        let sender = self.sender.clone();
        self.scanning = Some((0, self.roots.len()));
        backend::spawn(sender, move |tx| {
            backend::stream_candidates(&tx, &roots, &data_dir);
        });
    }

    /// Show the tool's own dry run before anything is changed.
    fn review(&mut self, id: &str, action: Action, title: String) {
        self.log.clear();
        self.error = None;
        self.finished = None;
        self.review_ok = None;
        self.plan_id = None;
        self.plan = None;
        self.busy = true;
        // The same review, as fields. The window shows those and keeps the
        // tool's own words under Details.
        if action == Action::Fix {
            let library = id.to_string();
            let data_dir = self.data_dir.clone();
            backend::spawn(self.sender.clone(), move |tx| {
                let _ = tx.send(Update::Plan(backend::plan(&library, &data_dir)));
            });
        }
        self.screen = Screen::Review {
            id: id.to_string(),
            action,
            title,
        };
        let arguments = vec![
            action.command().to_string(),
            id.to_string(),
            "--dry-run".to_string(),
        ];
        let sender = self.sender.clone();
        let running = self.running.clone();
        let data_dir = self.data_dir.clone();
        backend::spawn(sender, move |tx| {
            backend::stream(&tx, &arguments, &running, &data_dir)
        });
    }

    fn run(&mut self, arguments: Vec<String>, title: String) {
        self.log.clear();
        self.error = None;
        self.finished = None;
        self.phase = None;
        self.cancelled = false;
        self.busy = true;
        self.screen = Screen::Running { title };
        let sender = self.sender.clone();
        let running = self.running.clone();
        let data_dir = self.data_dir.clone();
        backend::spawn(sender, move |tx| {
            backend::stream(&tx, &arguments, &running, &data_dir)
        });
    }

    /// Stop a running operation. Only offered before the game drive is
    /// touched, so what is left behind is a staging copy the tool will never
    /// reclaim on its own and the user can delete.
    fn cancel(&mut self) {
        self.cancelled = true;
        if let Ok(mut slot) = self.running.lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
        }
        self.log.push("[stopped at your request]".to_string());
    }

    /// What the status bar says. Facts that change, rather than a sentence
    /// that does not.
    fn status_line(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        parts.push(match self.libraries.len() {
            0 if self.busy => "Looking for Steam".to_string(),
            0 => "No Steam libraries".to_string(),
            1 => "1 library".to_string(),
            n => format!("{n} libraries"),
        });
        if !self.scan_complete {
            parts.push("scan incomplete".to_string());
        }
        if let Some((done, total)) = self.scanning {
            let percent = if total > 0 { 100 * done / total } else { 0 };
            parts.push(format!("scanning {done}/{total} ({percent}%)"));
        }
        parts.push(match &self.lutris {
            Some(Ok(_)) => "Lutris found".to_string(),
            _ => "no Lutris".to_string(),
        });
        if !self.roots.is_empty() {
            let missing = self.candidates.iter().filter(|c| c.eligible).count();
            parts.push(match missing {
                0 => "no games to add".to_string(),
                1 => "1 game to add".to_string(),
                n => format!("{n} games to add"),
            });
        }
        parts.join(" · ")
    }

    fn drain(&mut self) {
        while let Ok(update) = self.receiver.try_recv() {
            match update {
                Update::Libraries(Ok(scan)) => {
                    self.libraries = scan.libraries;
                    self.warnings = scan.warnings;
                    self.scan_complete = scan.complete;
                }
                Update::Libraries(Err(message)) => self.error = Some(message),
                Update::Candidates(Ok(rows)) => {
                    self.scanning = None;
                    self.selected.retain(|id| rows.iter().any(|c| &c.id == id));
                    self.candidates = rows;
                }
                Update::Candidates(Err(message)) => {
                    self.scanning = None;
                    self.error = Some(message);
                }
                Update::Scanning((done, total)) => self.scanning = Some((done, total)),
                Update::Lutris(result) => self.lutris = Some(result),
                Update::Plan(Ok(plan)) => {
                    self.plan_id = Some(plan.fingerprint.clone());
                    self.plan = Some(plan);
                }
                Update::Plan(Err(_)) => self.plan = None,
                Update::Storage(Ok(rows)) => self.stored = rows,
                Update::Evidence(Ok(rows)) => self.evidence = rows,
                Update::Evidence(Err(message)) => self.error = Some(message),
                Update::Storage(Err(message)) => self.error = Some(message),
                Update::Event(phase) => self.phase = Some(phase),
                Update::Line(line) => {
                    // Bounded, so a long copy cannot grow this without limit.
                    // The tail is what matters, so the head is dropped.
                    self.log.push(line);
                    if self.log.len() > LOG_LINES {
                        let excess = self.log.len() - LOG_LINES;
                        self.log.drain(..excess);
                        if !self.log.first().map(|l| l.starts_with('[')).unwrap_or(false) {
                            self.log.insert(
                                0,
                                "[earlier output dropped to keep this bounded]".to_string(),
                            );
                        }
                    }
                }
                Update::Done(ok) => {
                    self.busy = false;
                    self.finished = Some(ok);
                    if matches!(self.screen, Screen::Review { .. }) {
                        self.review_ok = Some(ok);
                    }
                    // Re-scan the games list only when the command that just
                    // finished was `lutris import` (the `just_imported`
                    // flag). A finished scan must not start another scan — the
                    // scan's own Done would re-trigger refresh_candidates and
                    // loop forever.
                    let was_import = self.just_imported;
                    self.just_imported = false;
                    if ok && was_import {
                        // The import wrote new entries into Lutris. Re-scan so
                        // the just-added games move from "can be added" into
                        // "Already in Lutris".
                        self.refresh_candidates();
                    }
                    self.refresh_libraries();
                }
            }
        }
    }

    fn candidate(&self, id: &str) -> Option<&Candidate> {
        self.edits
            .get(id)
            .or_else(|| self.candidates.iter().find(|c| c.id == id))
    }

    fn edit(&mut self, id: &str) -> &mut Candidate {
        if !self.edits.contains_key(id) {
            if let Some(original) = self.candidates.iter().find(|c| c.id == id) {
                self.edits.insert(id.to_string(), original.clone());
            }
        }
        self.edits.get_mut(id).expect("candidate exists")
    }

    fn import_selected(&mut self) {
        let chosen: Vec<Candidate> = self
            .selected
            .iter()
            .filter_map(|id| self.candidate(id).cloned())
            .collect();
        if chosen.is_empty() {
            return;
        }
        let games: Vec<serde_json::Value> = chosen
            .iter()
            .map(|c| {
                let mut map = serde_json::Map::new();
                map.insert("name".into(), c.name.clone().into());
                map.insert("runner".into(), c.runner.clone().into());
                for (key, value) in [
                    ("exe", &c.exe),
                    ("appid", &c.appid),
                    ("prefix", &c.prefix),
                    ("working_dir", &c.working_dir),
                ] {
                    if !value.is_empty() {
                        map.insert(key.into(), value.clone().into());
                    }
                }
                serde_json::Value::Object(map)
            })
            .collect();
        let document = serde_json::json!({ "schema": 1, "games": games });
        // A private name per run. A shared, predictable one in a world-
        // writable directory is something another process can replace between
        // this window writing it and the tool reading it.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let directory = std::env::temp_dir().join(format!(
            "librarybridge-{}-{stamp}",
            std::process::id()
        ));
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.error = Some(format!("could not prepare the import file: {error}"));
            return;
        }
        let _ = std::fs::set_permissions(
            &directory,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        );
        let path = directory.join("plan.json");
        if let Err(error) = std::fs::write(&path, document.to_string()) {
            self.error = Some(format!("could not write the import file: {error}"));
            return;
        }
        self.run(
            vec![
                "lutris".into(),
                "import".into(),
                "--plan".into(),
                path.to_string_lossy().to_string(),
            ],
            format!("Adding {} games to Lutris", chosen.len()),
        );
        // This Done belongs to an import, so the completion handler can
        // re-scan the games list without looping on a scan's own Done.
        self.just_imported = true;
        // These games are being handed to Lutris now. Clear the selection so
        // the button does not keep offering them (the page refreshes on the
        // completion event and re-lists them under "Already in Lutris").
        self.selected.clear();
    }
}

// ------------------------------------------------------------------ rendering

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.poll_chooser(ctx);
        if self.busy || self.scanning.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            let measure = ui.available_width().min(940.0);
            ui.vertical_centered(|ui| {
            ui.set_max_width(measure);
            ui.horizontal(|ui| {
                ui.add(
                    egui::Image::from_texture(&self.icon_texture)
                        .fit_to_exact_size(egui::vec2(38.0, 38.0)),
                );
                ui.add_space(4.0);
                ui.heading("LibraryBridge");
                ui.add_space(12.0);
                if !matches!(self.screen, Screen::Home) {
                    let can_leave = !self.busy;
                    if ui
                        .add_enabled(can_leave, egui::Button::new("Back"))
                        .clicked()
                    {
                        // The change of screen should always change the
                        // screen. `back_to` is a hint from wherever we
                        // navigated from, and this window's own Cancel and
                        // Done actions can leave it pointing at the screen we
                        // are already on. When that happens, falling back to
                        // Home is the only target that is always reachable
                        // and always correct.
                        if self.screen == self.back_to {
                            self.screen = Screen::Home;
                        } else {
                            self.screen = self.back_to.clone();
                        }
                        self.back_to = Screen::Home;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy || self.scanning.is_some() {
                        ui.spinner();
                    }
                });
            });
            });
            ui.add_space(8.0);
        });

        // A status bar, not a slogan. A line that never changes stops being
        // read within a minute, and an error down here is far from whatever
        // it broke, so errors are shown in place instead.
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(6.0);
            let measure = ui.available_width().min(940.0);
            ui.vertical_centered(|ui| {
            ui.set_max_width(measure);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.status_line()).weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy {
                        ui.label(egui::RichText::new("working").weak());
                    }
                });
            });
            });
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // The home page's chart reads better a little wider; the detail
            // screens stay at the comfortable paragraph width.
            let cap = if matches!(self.screen, Screen::Home) {
                1120.0
            } else {
                940.0
            };
            let measure = ui.available_width().min(cap);
            ui.vertical_centered(|ui| {
            ui.set_max_width(measure);
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
            if let Some(error) = self.error.clone() {
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.small_button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
                ui.add_space(4.0);
            }
            match self.screen.clone() {
                Screen::Home => self.home(ui),
                Screen::Libraries => self.libraries_screen(ui),
                Screen::Review { id, action, title } => {
                    self.review_screen(ui, &id, action, &title)
                }
                Screen::Games => self.games_screen(ui),
                Screen::Storage => self.storage_screen(ui),
                Screen::Help => self.help_screen(ui),
                Screen::Running { title } => self.running_screen(ui, &title),
            }
            });
            });
        });

        self.maybe_screenshot(ctx);
    }
}

impl App {
    fn home(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        // What the tool says is eligible, not what this window infers from the
        // shape of a state name.
        let needing = self.libraries.iter().filter(|l| l.eligible).count();
        // Games that are genuinely missing from Lutris and can be added.
        // Steam games are shown for reference and are not addable, so they
        // must not inflate the "missing" count.
        let missing = self
            .candidates
            .iter()
            .filter(|c| c.eligible && !c.in_lutris && c.source != "steam")
            .count();

        // The same colours as the library list, so a glance at the home page
        // and a glance at the list read the same way.
        let ok_green = egui::Color32::from_rgb(0x2E, 0x7D, 0x32);
        let libraries_rich = if self.busy && self.libraries.is_empty() {
            egui::RichText::new("Looking for Steam...").weak()
        } else if self.libraries.is_empty() && !self.scan_complete {
            egui::RichText::new("The scan could not read everything, and found nothing so far")
                .color(ui.visuals().warn_fg_color)
                .strong()
        } else if self.libraries.is_empty() {
            egui::RichText::new("No Steam libraries found").weak()
        } else if needing > 0 {
            egui::RichText::new(format!("{needing} libraries can be repaired",))
                .color(ui.visuals().warn_fg_color)
                .strong()
        } else if !self.scan_complete {
            egui::RichText::new(format!(
                "{} found, none needing repair, but the scan was incomplete",
                self.libraries.len()
            ))
            .color(ui.visuals().warn_fg_color)
        } else {
            egui::RichText::new(format!(
                "{} found, none needing repair",
                self.libraries.len()
            ))
            .color(ok_green)
            .strong()
        };
        if card(
            ui,
            "Steam libraries",
            libraries_rich,
            "Proton stores its working data beside each library. On NTFS that data does not \
             work, so it has to live on your Linux drive instead.",
            "Review libraries",
        ) {
            self.back_to = Screen::Home;
            self.screen = Screen::Libraries;
        }

        if !self.libraries.is_empty() {
            ui.add_space(6.0);
            // A small chart of what the card summarises: one row per library
            // with name, location (fixed-width so the chart is wider) and the
            // same status colours as the list. Rows are plain horizontals so
            // the widths are deterministic instead of grid-converged.
            // Every row uses the same top-aligned layout and the same fixed
            // column widths, so the columns line up and the status badge
            // sits in its own right-hand column.
            let name_w = 130.0;
            let loc_w = 640.0;
            let status_w = 150.0;

            // A subtle frame around the whole chart, with thin separators
            // between rows so it reads as a table rather than a loose list.
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(8.0))
                .show(ui, |ui| {
                    ui.set_width(name_w + loc_w + status_w + 40.0);

                    ui.horizontal_top(|ui| {
                        ui.add_sized(
                            egui::vec2(name_w, 22.0),
                            egui::Label::new("Name".to_string()),
                        );
                        ui.add_sized(
                            egui::vec2(loc_w, 22.0),
                            egui::Label::new("Location".to_string()),
                        );
                        ui.add_sized(
                            egui::vec2(status_w, 22.0),
                            egui::Label::new("Status".to_string()),
                        );
                    });

                    for (i, library) in self.libraries.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(2.0);
                            ui.separator();
                            ui.add_space(2.0);
                        }
                        ui.horizontal_top(|ui| {
                            ui.add_sized(
                                egui::vec2(name_w, 22.0),
                                egui::Label::new(egui::RichText::new(&library.name).monospace()),
                            );
                            ui.add_sized(
                                egui::vec2(loc_w, 40.0),
                                egui::Label::new(
                                    egui::RichText::new(&library.path).monospace().weak(),
                                )
                                .wrap(),
                            );
                            // Status centred in its own fixed column.
                            let badge = status_badge(ui, library);
                            ui.add_sized(egui::vec2(status_w, 22.0), egui::Label::new(badge));
                        });
                    }
                });
        }

        ui.add_space(18.0);

        let games_status = match &self.lutris {
            Some(Err(_)) | None => egui::RichText::new("Lutris was not found").weak(),
            Some(Ok(_)) if self.roots.is_empty() => {
                egui::RichText::new("Choose a folder to look in").weak()
            }
            Some(Ok(_)) if missing == 0 => {
                egui::RichText::new("Nothing more to add to Lutris").color(ok_green)
            }
            Some(Ok(_)) if missing == 1 => egui::RichText::new("1 game is missing from Lutris")
                .color(ui.visuals().warn_fg_color),
            Some(Ok(_)) => egui::RichText::new(format!("{missing} games are missing from Lutris"))
                .color(ui.visuals().warn_fg_color),
        };
        if card(
            ui,
            "Games not in Lutris",
            games_status,
            "Finds installed games that no launcher knows about, such as GOG installs and \
             standalone Windows games on an external drive.",
            "Find games",
        ) {
            self.back_to = Screen::Home;
            self.screen = Screen::Games;
        }

        ui.add_space(18.0);
        ui.horizontal(|ui| {
            if ui.button("Storage and backups").clicked() {
                self.back_to = Screen::Home;
                self.screen = Screen::Storage;
            }
            if ui.button("Help").clicked() {
                self.back_to = Screen::Home;
                self.screen = Screen::Help;
            }
        });

        ui.add_space(26.0);
        ui.separator();
        ui.add_space(10.0);
        // The reassurance that used to sit in the footer forever. Said once,
        // where someone deciding whether to trust this will read it.
        ui.label(
            egui::RichText::new(
                "Nothing here is ever deleted. A repair copies your data, checks every file, \
                 and keeps the original on the game drive. It shows you exactly what it will \
                 do before it does it, and keeps working after this window is closed.",
            )
            .weak(),
        );
    }

    /// Where relocated Proton data should live. Empty means the tool's
    /// default under the user's data home. Changes here apply to the next
    /// scan, so the list below reflects the choice.
    fn data_location_row(&mut self, ui: &mut egui::Ui) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Where Proton data goes").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let current = if self.data_dir.is_empty() {
                        "Default · under your data home".to_string()
                    } else {
                        self.data_dir.clone()
                    };
                    ui.label(egui::RichText::new(format!("Now: {current}")).weak());
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.data_dir_input)
                        .desired_width(440.0)
                        .hint_text("/mnt/your-games/librarybridge"),
                );
                if ui.button("Set data location").clicked() {
                    self.apply_data_dir();
                }
                if ui.button("Reset to default").clicked() {
                    self.clear_data_dir();
                }
            });
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "The moved prefixes live here instead of filling the drive holding your \
                     system. It must be a Linux filesystem — the same reason the data had to \
                     leave NTFS in the first place. Flatpak Steam needs the location reachable \
                     from inside its sandbox.",
                )
                .weak(),
            );
        });
        ui.add_space(10.0);
    }

    fn apply_data_dir(&mut self) {
        let chosen = self.data_dir_input.trim().to_string();
        if chosen.is_empty() {
            self.error = Some("Enter a folder first, or use Reset to default.".to_string());
            return;
        }
        if !std::path::Path::new(&chosen).is_dir() {
            self.error = Some(format!("{chosen} is not a folder"));
            return;
        }
        self.error = None;
        self.data_dir = chosen;
        self.data_dir_input.clear();
        save_data_dir(&self.data_dir);
        self.refresh_libraries();
        self.refresh_lutris();
    }

    fn clear_data_dir(&mut self) {
        self.data_dir = String::new();
        self.data_dir_input.clear();
        let _ = std::fs::remove_file(saved_data_dir_path());
        self.refresh_libraries();
        self.refresh_lutris();
    }

    #[allow(clippy::collapsible_match)]
    fn libraries_screen(&mut self, ui: &mut egui::Ui) {
        if self.libraries.is_empty() {
            ui.heading("Steam libraries");
        } else {
            ui.heading(format!("Steam libraries — {} found", self.libraries.len()));
        }
        ui.add_space(10.0);

        self.data_location_row(ui);

        if !self.warnings.is_empty() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "The scan could not read everything, so this list may be incomplete:",
                );
                for warning in &self.warnings {
                    ui.label(egui::RichText::new(format!("• {warning}")).weak());
                }
            });
            ui.add_space(10.0);
        }

        if self.libraries.is_empty() {
            ui.label(if self.busy {
                "Looking for Steam..."
            } else {
                "No Steam libraries found."
            });
            ui.add_space(6.0);
            if ui.button("Look again").clicked() {
                self.refresh_libraries();
            }
            return;
        }

        let rows = self.libraries.clone();

        // A library with two sets of data needs a decision before anything
        // else on this screen matters, so it goes above the list.
        for library in rows.iter().filter(|l| !l.destination_occupied.is_empty()) {
            self.conflict_row(ui, library);
        }

        // Original per-library cards: name, location, diagnosis, action, Details.
        egui::ScrollArea::vertical().show(ui, |ui| {
            for library in &rows {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());

                    // Name and short id on the left, a coloured status on the
                    // right. The eye lands on what is true of this library
                    // before digging into anything.
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&library.name).size(17.0).strong());
                        ui.label(
                            egui::RichText::new(format!("[{}]", library.id))
                                .weak()
                                .monospace(),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let badge = status_badge(ui, library);
                                ui.label(badge);
                            },
                        );
                    });

                    // The location, out in the open. Two installs can have the
                    // same name, so the path is what tells them apart.
                    ui.add_space(2.0);
                    ui.horizontal_top(|ui| {
                        ui.label(
                            egui::RichText::new("Location:  ").weak().monospace(),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&library.path).monospace()
                            )
                            .wrap(),
                        );
                    });

                    ui.add_space(4.0);
                    ui.label(diagnosis(library));

                    if !library.connected {
                        ui.label(
                            egui::RichText::new(
                                "Connect the drive and it will be checked again.",
                            )
                            .weak(),
                        );
                    }

                    let has_action = match library.state.as_str() {
                        "repair_available" | "interrupted" => library.eligible,
                        "repaired" => true,
                        _ => false,
                    };
                    if has_action {
                        ui.add_space(8.0);
                    }
                    ui.horizontal(|ui| {
                        let enabled = !self.busy;
                        match library.state.as_str() {
                            "repair_available" | "interrupted" if library.eligible => {
                                if ui
                                    .add_enabled(enabled, egui::Button::new("Review repair"))
                                    .clicked()
                                {
                                    self.back_to = Screen::Libraries;
                                    self.review(
                                        &library.id,
                                        Action::Fix,
                                        format!("Repair {}", library.name),
                                    );
                                }
                            }
                            "repaired" => {
                                if ui
                                    .add_enabled(enabled, egui::Button::new("Review undo"))
                                    .clicked()
                                {
                                    self.back_to = Screen::Libraries;
                                    self.review(
                                        &library.id,
                                        Action::Undo,
                                        format!("Move {} back", library.name),
                                    );
                                }
                            }
                            _ => {}
                        }
                    });

                    ui.add_space(4.0);
                    egui::CollapsingHeader::new("Details")
                        .id_salt(&library.id)
                        .show(ui, |ui| {
                            field(
                                ui,
                                "Filesystem",
                                if library.filesystem.is_empty() {
                                    "not identified on this system"
                                } else {
                                    &library.filesystem
                                },
                            );
                            field(ui, "Steam", &library.steam);
                            field(ui, "State", library.headline());
                            if !library.target.is_empty() && library.state == "repaired" {
                                field(ui, "Moved to", &library.target);
                            }
                            for backup in &library.backups {
                                field(ui, "Original kept", backup);
                            }
                            field(ui, "Id", &library.id);
                        });
                });
                ui.add_space(10.0);
            }
        });
    }

    fn review_screen(&mut self, ui: &mut egui::Ui, id: &str, action: Action, title: &str) {
        ui.heading(title);
        ui.add_space(6.0);
        // Do not promise a plan above an error message.
        ui.label(match (self.review_ok, action) {
            (Some(false), _) => "This library cannot be repaired as things stand. Nothing has \
                                 been changed.",
            (_, Action::Fix) => "This is exactly what will happen. Nothing has changed yet.",
            (_, Action::Undo) => "This copies the current data back to the game drive first.",
        });
        ui.add_space(10.0);

        if let Some(plan) = self.plan.clone() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                field(
                    ui,
                    "Moving",
                    &format!(
                        "{} prefix folders for {} installed games",
                        plan.prefix_folders, plan.installed_games
                    ),
                );
                if !plan.unknown_folders.is_empty() {
                    field(
                        ui,
                        "Including",
                        &format!(
                            "{} with no installed game: {}",
                            plan.unknown_folders.len(),
                            plan.unknown_folders.join(", ")
                        ),
                    );
                }
                field(ui, "From", &plan.source);
                field(ui, "To", &plan.destination);
                field(ui, "Original", &format!("{} (kept)", plan.backup));
                ui.add_space(6.0);

                let needed = plan.copy_bytes + plan.reserve_bytes;
                field(
                    ui,
                    "Space",
                    &match (plan.available_bytes, plan.remaining_bytes) {
                        (Some(available), Some(remaining)) => format!(
                            "{} to copy plus {} spare, needs {}. {} free, {} left afterwards.",
                            backend::human_bytes(plan.copy_bytes),
                            backend::human_bytes(plan.reserve_bytes),
                            backend::human_bytes(needed),
                            backend::human_bytes(available),
                            backend::human_bytes(remaining)
                        ),
                        _ => format!(
                            "{} to copy plus {} spare. Free space could not be measured.",
                            backend::human_bytes(plan.copy_bytes),
                            backend::human_bytes(plan.reserve_bytes)
                        ),
                    },
                );
                for line in &plan.consequences {
                    ui.label(egui::RichText::new(format!("• {line}")).weak());
                }
            });
            ui.add_space(10.0);
        }

        egui::CollapsingHeader::new("Details from the command line tool")
            .default_open(self.plan.is_none())
            .show(ui, |ui| {
                log_view(ui, &self.log, 300.0);
            });

        // Apply is available only for a review that actually succeeded. It
        // used to test only whether the window was busy, so an error message
        // and an enabled Repair button could sit on screen together.
        if self.review_ok == Some(false) {
            ui.add_space(8.0);
            ui.colored_label(
                ui.visuals().error_fg_color,
                "This cannot be applied. Nothing has been changed.",
            );
        }

        ui.add_space(12.0);
        if action == Action::Fix {
            ui.label(
                egui::RichText::new(
                    "Your original stays on the game drive as compatdata.backup. \
                     Close Steam before continuing.",
                )
                .weak(),
            );
            ui.add_space(8.0);
        }

        ui.horizontal(|ui| {
            let ready = !self.busy && self.review_ok == Some(true);
            let label = match action {
                Action::Fix => "Repair now",
                Action::Undo => "Move it back",
            };
            if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
                let mut arguments = vec![
                    action.command().to_string(),
                    id.to_string(),
                    "--yes".to_string(),
                    // Ask for the phase events, so this screen knows when
                    // stopping stops being safe.
                    "--json".to_string(),
                ];
                // Quote back the plan that was on screen. If the library has
                // changed since, the tool refuses rather than applying to
                // something the user never saw.
                if action == Action::Fix {
                    if let Some(plan) = &self.plan_id {
                        arguments.push("--expect".to_string());
                        arguments.push(plan.clone());
                    }
                }
                self.back_to = Screen::Libraries;
                self.subject = Some(id.to_string());
                self.run(arguments, title.to_string());
            }
            if ui.add_enabled(ready, egui::Button::new("Cancel")).clicked() {
                self.screen = Screen::Libraries;
            }
        });
    }

    fn games_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Games not in Lutris");
        ui.add_space(6.0);

        match &self.lutris {
            Some(Err(_)) | None => {
                ui.label("Lutris was not found, so nothing can be added yet.");
                ui.label(
                    egui::RichText::new(
                        "Install it from your distribution's packages, or from Flathub as \
                         net.lutris.Lutris. Scanning still works without it.",
                    )
                    .weak(),
                );
                ui.add_space(8.0);
            }
            Some(Ok(_)) => {}
        }

        ui.horizontal(|ui| {
            let idle = self.chooser.is_none();
            if ui
                .add_enabled(idle, egui::Button::new("Choose folder…"))
                .clicked()
            {
                self.open_chooser();
            }
            ui.label("or type a path:");
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.root_input)
                    .desired_width(360.0)
                    .hint_text("/run/media/you/Games"),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Add").clicked() || entered) && !self.root_input.trim().is_empty() {
                let typed = self.root_input.trim().to_string();
                self.root_input.clear();
                self.add_root(typed);
            }
        });
        if !self.roots.is_empty() {
            ui.horizontal_wrapped(|ui| {
                let mut drop = None;
                for (index, root) in self.roots.iter().enumerate() {
                    if ui.button(format!("{root}   remove")).clicked() {
                        drop = Some(index);
                    }
                }
                if let Some(index) = drop {
                    self.roots.remove(index);
                    self.refresh_candidates();
                }
            });
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Scan again").clicked() {
                self.refresh_candidates();
            }
            if !self.candidates.is_empty() {
                if ui.button("Check all").clicked() {
                    for candidate in &self.candidates {
                        if candidate.eligible {
                            self.selected.insert(candidate.id.clone());
                        }
                    }
                }
                if ui.button("Clear all").clicked() {
                    self.selected.clear();
                }
            }
        });
        ui.add_space(10.0);

        if self.candidates.is_empty() {
            if let Some((done, total)) = self.scanning {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let percent = if total > 0 { 100 * done / total } else { 0 };
                    ui.label(
                        egui::RichText::new(
                            format!("Scanning folders… {} of {} ({percent}%)", done, total),
                        )
                        .weak(),
                    );
                });
            } else {
                ui.label(if self.busy {
                    "Scanning…"
                } else if self.roots.is_empty() {
                    "Only Steam libraries were checked. Add a folder to find everything else."
                } else {
                    "Nothing found that Lutris does not already have."
                });
            }
            return;
        }

        let all: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
        let can_add = |candidate: &backend::Candidate| candidate.eligible;
        let is_steam = |candidate: &backend::Candidate| candidate.source == "steam";

        // Leave room for the action row below, whatever the window height is.
        let list_height = (ui.available_height() - 108.0).max(160.0);
        egui::ScrollArea::vertical()
            .max_height(list_height)
            .show(ui, |ui| {
                // 1. Games that can actually be added (grouped by confidence).
                let addable: Vec<backend::Candidate> = self
                    .candidates
                    .iter()
                    .filter(|c| can_add(c))
                    .cloned()
                    .collect();
                let already = self
                    .candidates
                    .iter()
                    .filter(|c| c.in_lutris && c.source != "steam")
                    .count();
                if addable.is_empty() {
                    ui.label(
                        egui::RichText::new("No games can be added from what was found.").weak(),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} can be added to Lutris",
                                addable.len(),
                            ))
                            .size(15.0)
                            .strong(),
                        );
                        if already > 0 {
                            ui.label(
                                egui::RichText::new(format!("({already} already added)"))
                                    .size(15.0)
                                    .color(egui::Color32::from_rgb(0x2E, 0x7D, 0x32))
                                    .strong(),
                            );
                        }
                    });
                }
                for level in ["high", "medium", "low"] {
                    let group: Vec<String> = all
                        .iter()
                        .filter(|id| {
                            self.candidates
                                .iter()
                                .any(|c| &&c.id == id && c.confidence == level && can_add(c))
                        })
                        .cloned()
                        .collect();
                    if group.is_empty() {
                        continue;
                    }
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(confidence_heading(level)).strong());
                    ui.add_space(4.0);
                    for id in group {
                        self.candidate_row(ui, &id, level);
                    }
                }

                // 2. Games already in Lutris, so the page shows what has been added. Always
                // shown: the point of the page is finding what is missing, but
                // the "what is already there" half is just as important.
                {
                    let known: Vec<String> = all
                        .iter()
                        .filter(|id| {
                            self.candidates
                                .iter()
                                .any(|c| &&c.id == id && c.in_lutris && c.source != "steam")
                        })
                        .cloned()
                        .collect();
                    if !known.is_empty() {
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);
                        ui.colored_label(
                            egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
                            format!("Already in Lutris ({} — these are set up)", known.len(),),
                        );
                        ui.add_space(4.0);
                        for id in known {
                            self.candidate_row(ui, &id, "low");
                        }
                    }
                }

                // 3. Steam games, for reference only, at the bottom.
                let steam: Vec<String> = all
                    .iter()
                    .filter(|id| self.candidates.iter().any(|c| &&c.id == id && is_steam(c)))
                    .cloned()
                    .collect();
                if !steam.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Steam games ({} — shown for reference, not addable)",
                            steam.len(),
                        ))
                        .size(15.0)
                        .strong(),
                    );
                    ui.add_space(4.0);
                    for id in steam {
                        self.candidate_row(ui, &id, "low");
                    }
                }
            });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let count = self.selected.len();
            let ready = count > 0 && !self.busy && matches!(self.lutris, Some(Ok(_)));
            if ui
                .add_enabled(ready, egui::Button::new(format!("Add {count} to Lutris")))
                .clicked()
            {
                self.back_to = Screen::Games;
                self.import_selected();
            }
            ui.label(
                egui::RichText::new(
                    "Lutris shows its own dialog for each game. No game files are changed.",
                )
                .weak(),
            );
        });
    }

    /// Two sets of data exist for one library and only the user can say which
    /// counts. Both are kept whichever is chosen, and neither is preselected.
    fn conflict_row(&mut self, ui: &mut egui::Ui, library: &backend::Library) {
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Two data copies need review: {}", library.name),
            );
            ui.add_space(4.0);
            ui.label(
                "A repair was interrupted and something created new Proton data before it \
                 could finish. Both sets are real, and neither has been changed.",
            );
            field(ui, "On the drive", &library.path);
            field(ui, "Moved copy", &library.destination_occupied);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Whichever you keep, the other is set aside and kept, not deleted.",
                )
                .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let ready = !self.busy;
                if ui
                    .add_enabled(ready, egui::Button::new("Keep the moved copy"))
                    .clicked()
                {
                    self.back_to = Screen::Libraries;
                    self.run(
                        vec![
                            "fix".to_string(),
                            library.id.clone(),
                            "--yes".to_string(),
                            "--json".to_string(),
                            "--keep-destination".to_string(),
                        ],
                        format!("Keeping the moved copy for {}", library.name),
                    );
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Keep what is on the drive"))
                    .clicked()
                {
                    self.back_to = Screen::Libraries;
                    self.run(
                        vec![
                            "fix".to_string(),
                            library.id.clone(),
                            "--yes".to_string(),
                            "--json".to_string(),
                            "--replace-destination".to_string(),
                        ],
                        format!("Keeping the drive copy for {}", library.name),
                    );
                }
            });
        });
        ui.add_space(10.0);
    }

    fn candidate_row(&mut self, ui: &mut egui::Ui, id: &str, level: &str) {
        let Some(candidate) = self.candidate(id).cloned() else {
            return;
        };
        let open = self.expanded.contains(id);
        let _ = level;

        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if candidate.eligible {
                    let mut checked = self.selected.contains(id);
                    let response = ui.add_enabled(true, egui::Checkbox::new(&mut checked, ""));
                    if response.changed() {
                        if checked {
                            self.selected.insert(id.to_string());
                        } else {
                            self.selected.remove(id);
                        }
                    }
                } else if candidate.in_lutris {
                    // Already set up: a green check, no checkbox.
                    ui.label(
                        egui::RichText::new("\u{2713}")
                            .color(egui::Color32::from_rgb(0x2E, 0x7D, 0x32)),
                    );
                } else {
                    // Not selectable: an explicit "nope" badge replaces the
                    // checkbox so the row reads at a glance.
                    ui.label(egui::RichText::new("\u{2715}").color(ui.visuals().warn_fg_color));
                }
                ui.label(egui::RichText::new(&candidate.name).size(16.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button(if open { "Hide" } else { "Details" })
                        .clicked()
                    {
                        if open {
                            self.expanded.remove(id);
                        } else {
                            self.expanded.insert(id.to_string());
                        }
                    }
                });
            });

            // A status line so every row's disposition is visually clear:
            // green "can add", red/gray "not addable", blue "Steam (reference)".
            if candidate.eligible {
                ui.add_space(2.0);
                ui.colored_label(
                    egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
                    format!(
                        "Add to Lutris — {}",
                        match candidate.confidence.as_str() {
                            "high" => "confident",
                            "medium" => "probably right",
                            _ => "a guess, check it",
                        }
                    ),
                );
            } else if candidate.in_lutris {
                ui.add_space(2.0);
                ui.colored_label(
                    egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
                    "Already in Lutris — no need to add it again",
                );
            } else if candidate.source == "steam" {
                ui.add_space(2.0);
                ui.colored_label(
                    egui::Color32::from_rgb(0x1E, 0x5A, 0xAA),
                    "Steam — shown for reference, already visible in Lutris",
                );
            } else {
                ui.add_space(2.0);
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("Cannot be added — {}", candidate.blocking_reason),
                );
            }

            // One line saying what this is and why it is being offered. The
            // path is reference detail and lives under Details, as it does on
            // the libraries screen.
            ui.add_space(2.0);
            if candidate.eligible {
                let file = candidate
                    .exe
                    .rsplit('/')
                    .next()
                    .unwrap_or(&candidate.exe)
                    .to_string();
                ui.label(format!(
                    "Would run {file} through {}, {}.",
                    candidate.runner,
                    match candidate.source.as_str() {
                        "gog" => "identified from GOG's own metadata",
                        "linux" => "identified from a native Linux launcher",
                        "steam" => "listed by Steam",
                        _ => "chosen from the executables in its folder",
                    }
                ));
            } else {
                ui.label(format!("Cannot be added. {}.", candidate.blocking_reason));
            }

            // The filesystem warning lives in Details, not on every row: it is
            // the same text for everything on a drive and cluttered the list.

            if candidate.confidence == "low" {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "This one is a guess. Check it under Details before adding it.",
                    )
                    .weak(),
                );
            }

            if open {
                ui.add_space(6.0);
                if !candidate.exe.is_empty() {
                    field(ui, "Runs", &candidate.exe);
                }
                if !candidate.prefix.is_empty() {
                    field(ui, "Prefix", &candidate.prefix);
                }
                if !candidate.appid.is_empty() {
                    field(ui, "Steam app", &candidate.appid);
                }
                if !candidate.filesystem_warning.is_empty() {
                    ui.add_space(4.0);
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Warning: {}.", candidate.filesystem_warning),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Adding it to Lutris does not fix that on its own.",
                        )
                        .weak(),
                    );
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Why this one:").weak());
                for reason in &candidate.reasons {
                    ui.label(egui::RichText::new(format!("• {reason}")).weak());
                }

                if !candidate.alternatives.is_empty() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Or it might be one of these:").weak());
                    for alternative in &candidate.alternatives {
                        if ui.small_button(alternative).clicked() {
                            self.edit(id).exe = alternative.clone();
                        }
                    }
                }

                if candidate.eligible {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        let entry = self.edit(id);
                        ui.add(egui::TextEdit::singleline(&mut entry.name).desired_width(320.0));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Runs");
                        let entry = self.edit(id);
                        ui.add(egui::TextEdit::singleline(&mut entry.exe).desired_width(520.0));
                    });
                }
            }
        });
        ui.add_space(8.0);
    }


    fn storage_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Storage and backups");
        ui.add_space(6.0);
        ui.label(
            "Originals are kept on purpose. Nothing here is deleted by LibraryBridge, and \
             nothing here needs deleting for the repair to work.",
        );
        ui.add_space(10.0);

        if ui.button("Measure now").clicked() {
            self.busy = true;
            let data_dir = self.data_dir.clone();
            backend::spawn(self.sender.clone(), move |tx| {
                let _ = tx.send(Update::Storage(backend::storage(&data_dir)));
                let _ = tx.send(Update::Done(true));
            });
        }
        ui.add_space(10.0);

        if self.stored.is_empty() {
            ui.label(
                egui::RichText::new(
                    "Nothing measured yet. Sizes are read on demand, because walking every \
                     backup on an external drive is slow.",
                )
                .weak(),
            );
            return;
        }

        let rows = self.stored.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for row in &rows {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&row.name);
                        ui.label(
                            egui::RichText::new(format!("[{}]", row.id)).weak().monospace(),
                        );
                    });
                    if let Some((path, bytes)) = &row.live {
                        field(ui, "In use", &format!("{path}  ({})", backend::human_bytes(*bytes)));
                    }
                    for (path, bytes) in &row.backups {
                        field(
                            ui,
                            "Original",
                            &format!("{path}  ({})", backend::human_bytes(*bytes)),
                        );
                    }
                    if let Some((path, bytes)) = &row.leftover {
                        field(
                            ui,
                            "Unfinished",
                            &format!("{path}  ({})", backend::human_bytes(*bytes)),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Left by a copy that stopped. It is not in use, and it is not \
                                 removed automatically because its contents cannot be proven \
                                 to belong to this tool.",
                            )
                            .weak(),
                        );
                    }
                });
                ui.add_space(8.0);
            }
        });

        ui.add_space(8.0);
        let free = self
            .stored
            .first()
            .map(|row| row.data_free_bytes)
            .unwrap_or_default();
        let root = self
            .stored
            .first()
            .map(|row| row.data_root.to_string())
            .unwrap_or_default();
        if !root.is_empty() {
            ui.label(
                egui::RichText::new(match free {
                    Some(bytes) => format!(
                        "Originals are kept on purpose. The copies live at {root} with {} free \
                         there right now.",
                        backend::human_bytes(bytes)
                    ),
                    None => format!(
                        "Originals are kept on purpose. The copies live at {root}.",
                    ),
                })
                .weak(),
            );
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Delete an original yourself once a game has launched and loaded a save from \
                 the copy in use.",
            )
            .weak(),
        );
    }

    fn help_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Help");
        ui.add_space(10.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (heading, body) in [
                (
                    "What a repair does",
                    "It copies the Proton working data for a whole library to your Linux \
                     drive, checks every file, renames the original aside, and leaves a link \
                     where Steam expects to find it. Your game installations do not move.",
                ),
                (
                    "What it does not do",
                    "It does not make a game work under Proton, and it does not touch saves \
                     stored outside the prefix, your Steam settings, or shader caches. \
                     A repair is a filesystem fix, nothing more.",
                ),
                (
                    "Where your original goes",
                    "It stays on the game drive, next to the library, named compatdata.backup. \
                     Nothing is deleted. Storage and backups shows where everything is.",
                ),
                (
                    "If something goes wrong",
                    "Every state this tool can leave behind is readable from the disk, and \
                     the command line tool can finish or reverse an interrupted repair \
                     without this window: run librarybridge scan in a terminal.",
                ),
                (
                    "Removing LibraryBridge",
                    "Delete the program. Repairs keep working, because they are ordinary \
                     symlinks and ordinary directories.",
                ),
                (
                    "Installing the desktop icon",
                    "The taskbar and app-menu icon comes from a desktop entry, not from this \
                     window. Run packaging/install-desktop.sh from the repository (after the \
                     build) to install it. Without it the desktop shows a generic icon.",
                ),
                (
                    "Unsupported filesystems",
                    "exFAT cannot hold a Wine prefix at all, so repair is refused rather \
                     than attempted. A filesystem this build cannot identify is also \
                     refused, because there is no way to tell whether the repair applies.",
                ),
            ] {
                ui.label(egui::RichText::new(heading).strong());
                ui.add_space(2.0);
                ui.label(body);
                ui.add_space(12.0);
            }
        });
    }

    fn running_screen(&mut self, ui: &mut egui::Ui, title: &str) {
        ui.heading(title);
        ui.add_space(6.0);
        // A Lutris import is a different beast from a repair: its "Finished"
        // is about games added to Lutris, not bytes moved. Detect it by the
        // title the window chose.
        let is_lutris = title.starts_with("Adding");
        match self.finished {
            None => {
                let phase = self.phase.clone();
                ui.label(match &phase {
                    Some(phase) => phase.label(),
                    None => "Starting",
                });
                if let Some(backend::Phase::Progress { files, bytes }) = &phase {
                    ui.label(
                        egui::RichText::new(format!(
                            "{files} files, {}",
                            backend::human_bytes(*bytes)
                        ))
                        .weak(),
                    );
                }
                ui.add_space(6.0);

                let safe = phase.as_ref().map(|p| p.can_stop()).unwrap_or(true);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(safe && !self.cancelled, egui::Button::new("Stop"))
                        .clicked()
                    {
                        self.cancel();
                    }
                    ui.label(
                        egui::RichText::new(if self.cancelled {
                            "Stopping. Your original has not been touched."
                        } else if safe {
                            "Stopping now is safe: nothing on the game drive has changed yet."
                        } else {
                            "The switch takes a moment and cannot be interrupted."
                        })
                        .weak(),
                    );
                });
            }
            Some(true) => {
                if is_lutris {
                    // Read the tool's own "N added, N skipped" line out of the
                    // log and repeat it with the status colours.
                    let mut added = 0usize;
                    let mut skipped = 0usize;
                    for line in self.log.iter().rev() {
                        let t = line.trim();
                        if t.ends_with("skipped.") && t.contains(" added, ") {
                            let nums: Vec<usize> = t
                                .split(|c: char| !c.is_ascii_digit())
                                .filter_map(|s| s.parse().ok())
                                .collect();
                            if nums.len() >= 2 {
                                added = nums[0];
                                skipped = nums[1];
                            }
                            break;
                        }
                    }
                    if skipped == 0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
                            format!("Done — {added} game(s) added to Lutris."),
                        );
                    } else if added > 0 {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "Done — {added} added, {skipped} skipped. See the log below."
                            ),
                        );
                    } else {
                        ui.colored_label(
                            ui.visuals().error_fg_color,
                            "Nothing was added to Lutris. See the log below.",
                        );
                    }
                } else {
                    ui.label("Finished.");
                }
            }
            Some(false) if self.cancelled => {
                ui.label("Stopped. Nothing on the game drive was changed.");
            }
            Some(false) => {
                if is_lutris {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "The Lutris import did not complete cleanly — see the log below.",
                    );
                } else {
                    // A non-zero exit is not always a crash: a repair can be
                    // told to stop. The log below is the truth either way.
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "Finished with a problem — the log below shows what happened.",
                    );
                }
            }
        }
        ui.add_space(10.0);
        log_view(ui, &self.log, 460.0);
        if self.finished == Some(true) && !self.evidence.is_empty() {
            ui.add_space(14.0);
            ui.label(egui::RichText::new("What this does and does not establish").strong());
            ui.add_space(6.0);
            let rows = self.evidence.clone();
            let subject = self.subject.clone();
            for row in &rows {
                ui.horizontal(|ui| {
                    ui.label(format!("{:<38}", backend::describe_evidence(&row.field)));
                    ui.label(&row.result);
                    if row.by_tool {
                        ui.label(egui::RichText::new("checked here").weak());
                    }
                    if row.field != "files" {
                        if let Some(library) = &subject {
                            for (label, answer) in
                                [("Worked", "yes"), ("Did not", "no"), ("N/A", "na")]
                            {
                                if ui.small_button(label).clicked() {
                                    let library = library.clone();
                                    let field = row.field.clone();
                                    let data_dir = self.data_dir.clone();
                                    backend::spawn(self.sender.clone(), move |tx| {
                                        let _ = backend::record_evidence(
                                            &library,
                                            &field,
                                            answer,
                                            &data_dir,
                                        );
                                        let _ = tx
                                            .send(Update::Evidence(backend::evidence(
                                                &library,
                                                &data_dir,
                                            )));
                                    });
                                }
                            }
                        }
                    }
                });
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "Only the first row was established by this repair. The rest need a game, \
                     and stay unanswered until you say otherwise.",
                )
                .weak(),
            );
        }

        ui.add_space(12.0);
        if self.finished.is_some() && ui.button("Done").clicked() {
            self.screen = self.back_to.clone();
            // No candidates re-scan here: an import already refreshed the list
            // when it finished (Done -> just_imported -> refresh_candidates),
            // and a repair changes nothing about the Lutris list. Re-scanning
            // again would just run the slow scan a second time for nothing.
            self.refresh_libraries();
        }
    }

    /// Development helper: render a few frames, save the window, and exit.
    fn maybe_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        self.frames += 1;
        ctx.request_repaint();
        if self.frames == 45 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        }
        if self.frames < 46 {
            return;
        }
        let image = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
            for pixel in &image.pixels {
                bytes.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b(), pixel.a()]);
            }
            let (width, height, bytes) = png::halve(image.size[0], image.size[1], &bytes);
            let encoded = png::encode(width, height, &bytes);
            let _ = std::fs::write(&path, encoded);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.frames > 150 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

// ------------------------------------------------------------------- widgets

/// A titled panel with one action. The status line is pre-coloured by the
/// caller, so the home page uses the same status colours as the library list.
/// Returns true when that action is clicked.
fn card(
    ui: &mut egui::Ui,
    title: &str,
    status: egui::RichText,
    explanation: &str,
    action: &str,
) -> bool {
    let mut clicked = false;
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).size(19.0).strong());
            ui.add_space(5.0);
            ui.label(status.size(15.0));
            ui.add_space(7.0);
            ui.label(egui::RichText::new(explanation).weak());
            ui.add_space(11.0);
            clicked = ui.button(action).clicked();
        });
    });
    clicked
}

/// A labelled value. Paths are long and the window is not, so the value
/// wraps rather than running off the edge.
fn field(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.label(
            egui::RichText::new(format!("{label:<11}"))
                .monospace()
                .weak(),
        );
        ui.add(egui::Label::new(egui::RichText::new(value).monospace()).wrap());
    });
}

/// The short, coloured status shown beside a library's name. The state codes
/// are for the tool to act on; these words are for a person to scan.
fn status_badge(ui: &mut egui::Ui, library: &backend::Library) -> egui::RichText {
    let (text, color) = if !library.connected {
        (
            "Drive not connected".to_string(),
            ui.visuals().weak_text_color(),
        )
    } else if library.state.as_str() == "repair_available" {
        if library.eligible {
            ("Repair available".to_string(), ui.visuals().warn_fg_color)
        } else {
            ("No repair needed".to_string(), ui.visuals().weak_text_color())
        }
    } else {
        (
            library.headline().to_string(),
            match library.state.as_str() {
                "repaired" => egui::Color32::from_rgb(0x2E, 0x7D, 0x32),
                "dangling_link" | "linked_elsewhere" => ui.visuals().error_fg_color,
                "interrupted" => ui.visuals().warn_fg_color,
                _ => ui.visuals().weak_text_color(),
            },
        )
    };
    egui::RichText::new(text).color(color).strong()
}

/// One sentence saying what is true of this library, in the words a person
/// would use. This is the line the row exists to deliver.
fn diagnosis(library: &backend::Library) -> String {
    if !library.connected {
        return "The drive is not connected, so nothing can be checked.".to_string();
    }
    match library.state.as_str() {
        "repair_available" if library.eligible => format!(
            "Proton stores its working data here, on {}, where it does not work properly.",
            if library.filesystem.is_empty() {
                "this filesystem".to_string()
            } else {
                library.filesystem.clone()
            }
        ),
        "interrupted" => {
            "A repair stopped before it finished. Nothing was lost, and it can be \
             completed."
                .to_string()
        }
        "repaired" => {
            "Proton's working data has been moved to your Linux drive. Your original is \
             still on the game drive."
                .to_string()
        }
        "dangling_link" => {
            "This library points at data that is not there. Nothing will be changed until \
             it is."
                .to_string()
        }
        "linked_elsewhere" => {
            "Something already redirects this library's Proton data. It was not set up \
             here, so it is left alone."
                .to_string()
        }
        _ if !library.blocking_reason.is_empty() => {
            format!("No repair applies here: {}.", library.blocking_reason)
        }
        _ => library.headline().to_string(),
    }
}


fn confidence_heading(level: &str) -> &'static str {
    match level {
        "high" => "Confident",
        "medium" => "Probably right",
        _ => "Guesses, check before adding",
    }
}

fn log_view(ui: &mut egui::Ui, lines: &[String], height: f32) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::ScrollArea::vertical()
            .max_height(height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if lines.is_empty() {
                    ui.label(egui::RichText::new("Working...").weak());
                }
                for line in lines {
                    ui.label(egui::RichText::new(line).monospace().size(12.5));
                }
            });
    });
}
