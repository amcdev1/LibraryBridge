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
const APP_ICON_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/branding/librarybridge-controller-bridge-top-lb-1024.png"
));

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
    show_known: bool,
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
        cc.egui_ctx.set_pixels_per_point(1.25);
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
            show_known: false,
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
            backend::spawn(app.sender.clone(), |tx| {
                let _ = tx.send(Update::Storage(backend::storage()));
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
        backend::spawn(sender, |tx| {
            let _ = tx.send(Update::Libraries(backend::libraries()));
        });
    }

    fn refresh_lutris(&mut self) {
        let sender = self.sender.clone();
        backend::spawn(sender, |tx| {
            let _ = tx.send(Update::Lutris(backend::lutris_status()));
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
        let include = self.show_known;
        let sender = self.sender.clone();
        backend::spawn(sender, move |tx| {
            let _ = tx.send(Update::Candidates(backend::candidates(&roots, include)));
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
            backend::spawn(self.sender.clone(), move |tx| {
                let _ = tx.send(Update::Plan(backend::plan(&library)));
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
        backend::spawn(sender, move |tx| backend::stream(&tx, &arguments));
    }

    fn run(&mut self, arguments: Vec<String>, title: String) {
        self.log.clear();
        self.error = None;
        self.finished = None;
        self.busy = true;
        self.screen = Screen::Running { title };
        let sender = self.sender.clone();
        backend::spawn(sender, move |tx| backend::stream(&tx, &arguments));
    }

    fn drain(&mut self) {
        while let Ok(update) = self.receiver.try_recv() {
            match update {
                Update::Libraries(Ok(rows)) => self.libraries = rows,
                Update::Libraries(Err(message)) => self.error = Some(message),
                Update::Candidates(Ok(rows)) => {
                    self.selected.retain(|id| rows.iter().any(|c| &c.id == id));
                    self.candidates = rows;
                }
                Update::Candidates(Err(message)) => self.error = Some(message),
                Update::Lutris(result) => self.lutris = Some(result),
                Update::Plan(Ok(plan)) => {
                    self.plan_id = Some(plan.fingerprint.clone());
                    self.plan = Some(plan);
                }
                Update::Plan(Err(_)) => self.plan = None,
                Update::Storage(Ok(rows)) => self.stored = rows,
                Update::Storage(Err(message)) => self.error = Some(message),
                Update::Line(line) => self.log.push(line),
                Update::Done(ok) => {
                    self.busy = false;
                    self.finished = Some(ok);
                    if matches!(self.screen, Screen::Review { .. }) {
                        self.review_ok = Some(ok);
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
        let path = std::env::temp_dir().join("librarybridge-gui-plan.json");
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
    }
}

// ------------------------------------------------------------------ rendering

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.poll_chooser(ctx);
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
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
                        .add_enabled(can_leave, egui::Button::new("← Back"))
                        .clicked()
                    {
                        self.screen = self.back_to.clone();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy {
                        ui.spinner();
                    }
                });
            });
            ui.add_space(8.0);
        });

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(6.0);
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            } else {
                ui.label(
                    egui::RichText::new(
                        "Nothing is ever deleted. A repair keeps your original beside the library.",
                    )
                    .weak(),
                );
            }
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.screen.clone() {
            Screen::Home => self.home(ui),
            Screen::Libraries => self.libraries_screen(ui),
            Screen::Review { id, action, title } => self.review_screen(ui, &id, action, &title),
            Screen::Games => self.games_screen(ui),
            Screen::Storage => self.storage_screen(ui),
            Screen::Help => self.help_screen(ui),
            Screen::Running { title } => self.running_screen(ui, &title),
        });

        self.maybe_screenshot(ctx);
    }
}

impl App {
    fn home(&mut self, ui: &mut egui::Ui) {
        ui.add_space(20.0);
        let needing = self.libraries.iter().filter(|l| l.actionable()).count();
        let missing = self.candidates.iter().filter(|c| !c.in_lutris).count();

        let libraries_status = if self.libraries.is_empty() {
            "Looking for Steam...".to_string()
        } else if needing == 0 {
            format!("{} found, nothing needs repair", self.libraries.len())
        } else if needing == 1 {
            "1 library needs repair".to_string()
        } else {
            format!("{needing} libraries need repair")
        };
        if card(
            ui,
            "Steam libraries",
            &libraries_status,
            "Proton stores its working data beside each library. On NTFS that data does not \
             work, so it has to live on your Linux drive instead.",
            "Review libraries",
        ) {
            self.back_to = Screen::Home;
            self.screen = Screen::Libraries;
        }

        ui.add_space(18.0);

        let games_status = match &self.lutris {
            Some(Err(_)) | None => "Lutris was not found".to_string(),
            Some(Ok(_)) if self.roots.is_empty() => "Choose a folder to look in".to_string(),
            Some(Ok(_)) if missing == 1 => "1 game is missing from Lutris".to_string(),
            Some(Ok(_)) => format!("{missing} games are missing from Lutris"),
        };
        if card(
            ui,
            "Games not in Lutris",
            &games_status,
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
        ui.label(
            egui::RichText::new(
                "Everything here runs the librarybridge command line tool and shows you exactly \
                 what it is about to do before it does it. A repair keeps working after this \
                 window is closed.",
            )
            .weak(),
        );
    }

    fn libraries_screen(&mut self, ui: &mut egui::Ui) {
        ui.heading("Steam libraries");
        ui.add_space(10.0);

        if self.libraries.is_empty() {
            ui.label("No Steam libraries found yet.");
            if ui.button("Look again").clicked() {
                self.refresh_libraries();
            }
            return;
        }

        let rows = self.libraries.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for library in &rows {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&library.name);
                        ui.label(
                            egui::RichText::new(format!("[{}]", library.id))
                                .weak()
                                .monospace(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(state_badge(&library.state));
                        });
                    });
                    ui.add(
                        egui::Label::new(egui::RichText::new(&library.path).monospace().weak())
                            .wrap(),
                    );
                    ui.add_space(4.0);
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
                    if !library.connected {
                        ui.label(
                            egui::RichText::new(
                                "The drive is not connected, so nothing can be done until it is.",
                            )
                            .weak(),
                        );
                    }
                    if !library.target.is_empty() && library.state == "repaired" {
                        field(ui, "Moved to", &library.target);
                    }
                    for backup in &library.backups {
                        field(ui, "Backup", backup);
                    }

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let enabled = !self.busy;
                        match library.state.as_str() {
                            "repair_available" | "interrupted" => {
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
                            _ => {
                                ui.label(egui::RichText::new("Nothing to do for this one").weak());
                            }
                        }
                    });
                });
                ui.add_space(8.0);
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
                    if ui.button(format!("{root}  ✕")).clicked() {
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
            if ui
                .checkbox(&mut self.show_known, "Include games Lutris already has")
                .changed()
            {
                self.refresh_candidates();
            }
            if ui.button("Scan again").clicked() {
                self.refresh_candidates();
            }
        });
        ui.add_space(10.0);

        if self.candidates.is_empty() {
            ui.label(if self.roots.is_empty() {
                "Only Steam libraries were checked. Add a folder to find everything else."
            } else {
                "Nothing found that Lutris does not already have."
            });
            return;
        }

        let ids: Vec<String> = self.candidates.iter().map(|c| c.id.clone()).collect();
        // Leave room for the action row below, whatever the window height is.
        let list_height = (ui.available_height() - 78.0).max(160.0);
        egui::ScrollArea::vertical()
            .max_height(list_height)
            .show(ui, |ui| {
                for level in ["high", "medium", "low"] {
                    let group: Vec<String> = ids
                        .iter()
                        .filter(|id| {
                            self.candidates
                                .iter()
                                .any(|c| &&c.id == id && c.confidence == level)
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

    fn candidate_row(&mut self, ui: &mut egui::Ui, id: &str, level: &str) {
        let Some(candidate) = self.candidate(id).cloned() else {
            return;
        };
        let open = self.expanded.contains(id) || level == "high";

        ui.group(|ui| {
            ui.horizontal(|ui| {
                let mut checked = self.selected.contains(id);
                // A game the tool would refuse cannot be selected here. The
                // window used to offer Steam rows the command line then
                // dropped, so the two disagreed about what would happen.
                if ui
                    .add_enabled(candidate.eligible, egui::Checkbox::new(&mut checked, ""))
                    .changed()
                {
                    if checked {
                        self.selected.insert(id.to_string());
                    } else {
                        self.selected.remove(id);
                    }
                }
                ui.strong(&candidate.name);
                ui.label(
                    egui::RichText::new(format!(
                        "{} runner, found from {}",
                        candidate.runner,
                        match candidate.source.as_str() {
                            "gog" => "GOG metadata",
                            "steam" => "Steam",
                            "linux" => "a Linux launcher",
                            _ => "the folder contents",
                        }
                    ))
                    .weak(),
                );
                if candidate.in_lutris {
                    ui.label(egui::RichText::new("already in Lutris").weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = if self.expanded.contains(id) {
                        "Hide"
                    } else {
                        "Details"
                    };
                    if ui.small_button(label).clicked() {
                        if self.expanded.contains(id) {
                            self.expanded.remove(id);
                        } else {
                            self.expanded.insert(id.to_string());
                        }
                    }
                });
            });

            if !candidate.exe.is_empty() {
                ui.add(
                    egui::Label::new(egui::RichText::new(&candidate.exe).monospace().weak()).wrap(),
                );
            }
            if !candidate.appid.is_empty() {
                ui.label(egui::RichText::new(format!("Steam app {}", candidate.appid)).weak());
            }
            if !candidate.blocking_reason.is_empty() {
                ui.label(
                    egui::RichText::new(format!("Cannot be added: {}", candidate.blocking_reason))
                        .weak(),
                );
            }
            if !candidate.filesystem_warning.is_empty() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Warning: {}", candidate.filesystem_warning),
                );
                ui.label(
                    egui::RichText::new("Adding it to Lutris does not fix that on its own.")
                        .weak(),
                );
            }

            if open {
                ui.add_space(4.0);
                for reason in &candidate.reasons {
                    ui.label(egui::RichText::new(format!("• {reason}")).weak());
                }
                if !candidate.prefix.is_empty() {
                    field(ui, "Prefix", &candidate.prefix);
                }
            }

            if self.expanded.contains(id) && !candidate.alternatives.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Or it might be one of these:").weak());
                for alternative in &candidate.alternatives {
                    if ui.small_button(alternative).clicked() {
                        self.edit(id).exe = alternative.clone();
                    }
                }
            }

            if self.expanded.contains(id) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Name");
                    let entry = self.edit(id);
                    ui.add(egui::TextEdit::singleline(&mut entry.name).desired_width(320.0));
                });
                ui.horizontal(|ui| {
                    ui.label("Runs");
                    let entry = self.edit(id);
                    ui.add(egui::TextEdit::singleline(&mut entry.exe).desired_width(560.0));
                });
            }
        });
        ui.add_space(6.0);
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
            backend::spawn(self.sender.clone(), |tx| {
                let _ = tx.send(Update::Storage(backend::storage()));
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
        match self.finished {
            None => {
                ui.label(
                    "Working. This runs as a separate program, so closing the window does not \
                     stop it, and your original stays where it is whatever happens.",
                );
            }
            Some(true) => {
                ui.label("Finished.");
            }
            Some(false) => {
                ui.colored_label(ui.visuals().error_fg_color, "Stopped without finishing.");
            }
        }
        ui.add_space(10.0);
        log_view(ui, &self.log, 460.0);
        ui.add_space(12.0);
        if self.finished.is_some() && ui.button("Done").clicked() {
            self.screen = self.back_to.clone();
            self.refresh_libraries();
            self.refresh_candidates();
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

/// A titled panel with one action. Returns true when that action is clicked.
fn card(ui: &mut egui::Ui, title: &str, status: &str, explanation: &str, action: &str) -> bool {
    let mut clicked = false;
    ui.group(|ui| {
        ui.set_width(660.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).size(19.0).strong());
            ui.add_space(5.0);
            ui.label(egui::RichText::new(status).size(15.0));
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

fn state_badge(state: &str) -> egui::RichText {
    let text = match state {
        "repair_available" => "needs repair",
        "repaired" => "repaired",
        "interrupted" => "unfinished",
        "disconnected" => "not connected",
        "dangling_link" => "broken link",
        "linked_elsewhere" => "linked by hand",
        "no_compatdata" => "no Proton data",
        _ => "check this one",
    };
    egui::RichText::new(text).monospace()
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
