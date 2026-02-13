use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use globset::{Glob, GlobSet, GlobSetBuilder};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, AlertDialog, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton,
    FileDialog, Label, Orientation, ProgressBar, ScrolledWindow, Separator, TextView, WrapMode,
};
use walkdir::WalkDir;

const APP_ID: &str = "dev.kosmokopy.app";

// ── Source selection state ──────────────────────────────────────────────

#[derive(Clone, Debug)]
enum SourceSelection {
    None,
    Directory(PathBuf),
    Files(Vec<PathBuf>),
}

// ── Transfer mode ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum TransferMode {
    FilesOnly,
    FoldersAndFiles,
}

fn main() -> glib::ExitCode {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

// ── Messages from worker thread to UI ──────────────────────────────────

enum WorkerMsg {
    Progress {
        done: usize,
        total: usize,
        file: String,
    },
    Finished {
        copied: usize,
        skipped: usize,
        errors: Vec<String>,
    },
    Error(String),
}

// ── UI construction ────────────────────────────────────────────────────

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Kosmokopy")
        .default_width(560)
        .default_height(520)
        .resizable(true)
        .build();

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    // ── Source selection ───────────────────────────────────────────────
    let src_heading = Label::new(Some("Source:"));
    src_heading.set_halign(Align::Start);
    root.append(&src_heading);

    let src_row = GtkBox::new(Orientation::Horizontal, 8);
    let src_label = Label::new(Some("(none)"));
    src_label.set_hexpand(true);
    src_label.set_halign(Align::Start);
    src_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    src_label.set_wrap(true);
    src_label.set_max_width_chars(60);

    let btn_browse_folder = Button::with_label("Browse Folder…");
    let btn_browse_files = Button::with_label("Browse Files…");

    src_row.append(&src_label);
    src_row.append(&btn_browse_folder);
    src_row.append(&btn_browse_files);
    root.append(&src_row);

    // ── Destination directory ─────────────────────────────────────────
    let dst_row = dir_row("Destination Directory:");
    let dst_label: Label = dst_row.2.clone();
    root.append(&dst_row.0);

    // ── Copy / Move toggle ────────────────────────────────────────────
    let mode_box = GtkBox::new(Orientation::Horizontal, 12);
    let chk_copy = CheckButton::with_label("Copy");
    let chk_move = CheckButton::with_label("Move");
    chk_move.set_group(Some(&chk_copy));
    chk_copy.set_active(true);
    mode_box.append(&chk_copy);
    mode_box.append(&chk_move);
    root.append(&mode_box);

    // ── Transfer mode: Files only / Folders and files ─────────────────
    let transfer_box = GtkBox::new(Orientation::Horizontal, 12);
    let chk_files_only = CheckButton::with_label("Files only");
    let chk_folders_files = CheckButton::with_label("Folders and files");
    chk_folders_files.set_group(Some(&chk_files_only));
    chk_files_only.set_active(true);
    transfer_box.append(&chk_files_only);
    transfer_box.append(&chk_folders_files);
    root.append(&transfer_box);

    root.append(&Separator::new(Orientation::Horizontal));

    // ── Exclude patterns ──────────────────────────────────────────────
    let excl_label = Label::new(Some("Exclude directory patterns (one per line):"));
    excl_label.set_halign(Align::Start);
    root.append(&excl_label);

    let excl_view = TextView::new();
    excl_view.set_wrap_mode(WrapMode::WordChar);
    excl_view.set_monospace(true);
    excl_view.buffer().set_text(".*");

    let excl_scroll = ScrolledWindow::builder()
        .child(&excl_view)
        .min_content_height(80)
        .vexpand(true)
        .build();
    root.append(&excl_scroll);

    root.append(&Separator::new(Orientation::Horizontal));

    // ── Progress area ─────────────────────────────────────────────────
    let progress_bar = ProgressBar::new();
    progress_bar.set_show_text(true);
    progress_bar.set_text(Some("Ready"));
    root.append(&progress_bar);

    let status_label = Label::new(Some(""));
    status_label.set_halign(Align::Start);
    status_label.set_wrap(true);
    root.append(&status_label);

    // ── Start button ──────────────────────────────────────────────────
    let btn_start = Button::with_label("Start");
    btn_start.add_css_class("suggested-action");
    root.append(&btn_start);

    window.set_child(Some(&root));

    // ── Shared source-selection state ─────────────────────────────────
    let source_selection = Rc::new(RefCell::new(SourceSelection::None));

    // ── Browse Folder button ──────────────────────────────────────────
    {
        let win_clone = window.clone();
        let src_label_c = src_label.clone();
        let source_sel = source_selection.clone();
        btn_browse_folder.connect_clicked(move |_| {
            let dialog = FileDialog::builder()
                .title("Select source folder")
                .modal(true)
                .build();
            let src_label_c2 = src_label_c.clone();
            let source_sel2 = source_sel.clone();
            dialog.select_folder(
                Some(&win_clone),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            src_label_c2.set_text(&path.to_string_lossy());
                            *source_sel2.borrow_mut() = SourceSelection::Directory(path);
                        }
                    }
                },
            );
        });
    }

    // ── Browse Files button ───────────────────────────────────────────
    {
        let win_clone = window.clone();
        let src_label_c = src_label.clone();
        let source_sel = source_selection.clone();
        btn_browse_files.connect_clicked(move |_| {
            let dialog = FileDialog::builder()
                .title("Select files")
                .modal(true)
                .build();
            let src_label_c2 = src_label_c.clone();
            let source_sel2 = source_sel.clone();
            dialog.open_multiple(
                Some(&win_clone),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(files) = result {
                        let mut paths = Vec::new();
                        for i in 0..files.n_items() {
                            if let Some(obj) = files.item(i) {
                                if let Ok(gfile) = obj.downcast::<gtk4::gio::File>() {
                                    if let Some(p) = gfile.path() {
                                        paths.push(p);
                                    }
                                }
                            }
                        }
                        if !paths.is_empty() {
                            let display = if paths.len() == 1 {
                                paths[0].to_string_lossy().to_string()
                            } else {
                                format!("{} files selected", paths.len())
                            };
                            src_label_c2.set_text(&display);
                            *source_sel2.borrow_mut() = SourceSelection::Files(paths);
                        }
                    }
                },
            );
        });
    }

    // ── Destination browse ────────────────────────────────────────────
    {
        let win_clone = window.clone();
        let dst_label_c = dst_label.clone();
        dst_row.1.connect_clicked(move |_| {
            pick_folder(&win_clone, dst_label_c.clone());
        });
    }

    // ── Start button logic ────────────────────────────────────────────
    let running = Rc::new(RefCell::new(false));

    btn_start.connect_clicked({
        let source_selection = source_selection.clone();
        let dst_label = dst_label.clone();
        let chk_move = chk_move.clone();
        let chk_folders_files = chk_folders_files.clone();
        let excl_view = excl_view.clone();
        let progress_bar = progress_bar.clone();
        let status_label = status_label.clone();
        let btn_start = btn_start.clone();
        let running = running.clone();
        let window = window.clone();

        move |_| {
            if *running.borrow() {
                return;
            }

            let source_sel = source_selection.borrow().clone();
            let dst = dst_label.text().to_string();

            match &source_sel {
                SourceSelection::None => {
                    status_label.set_text("Please select a source (folder or files).");
                    return;
                }
                SourceSelection::Directory(p) if p.to_string_lossy() == dst => {
                    status_label.set_text("Source and destination must be different.");
                    return;
                }
                _ => {}
            }

            if dst.is_empty() || dst == "(none)" {
                status_label.set_text("Please select a destination directory.");
                return;
            }

            let do_move = chk_move.is_active();
            let transfer_mode = if chk_folders_files.is_active() {
                TransferMode::FoldersAndFiles
            } else {
                TransferMode::FilesOnly
            };

            let buf = excl_view.buffer();
            let text = buf
                .text(&buf.start_iter(), &buf.end_iter(), false)
                .to_string();
            let patterns: Vec<String> = text
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();

            *running.borrow_mut() = true;
            btn_start.set_sensitive(false);
            progress_bar.set_fraction(0.0);
            progress_bar.set_text(Some("Scanning…"));
            status_label.set_text("");

            // Channel for worker → UI communication
            let (tx, rx) = mpsc::channel::<WorkerMsg>();

            // Spawn worker thread
            let dst_clone = dst.clone();
            thread::spawn(move || {
                run_worker(source_sel, dst_clone, do_move, transfer_mode, &patterns, tx);
            });

            // Poll for messages on the glib main loop
            let progress_bar_c = progress_bar.clone();
            let status_label_c = status_label.clone();
            let btn_start_c = btn_start.clone();
            let window_c = window.clone();
            let running_c = running.clone();

            glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        WorkerMsg::Progress { done, total, file } => {
                            let frac = if total > 0 {
                                done as f64 / total as f64
                            } else {
                                0.0
                            };
                            progress_bar_c.set_fraction(frac);
                            let filename = Path::new(&file)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or(file);
                            progress_bar_c
                                .set_text(Some(&format!("{}/{} — {}", done, total, filename)));
                        }
                        WorkerMsg::Finished {
                            copied,
                            skipped,
                            errors,
                        } => {
                            progress_bar_c.set_fraction(1.0);
                            let verb = if do_move { "Moved" } else { "Copied" };
                            let mut summary =
                                format!("{} {} file(s), {} skipped.", verb, copied, skipped);
                            if !errors.is_empty() {
                                summary.push_str(&format!(
                                    "\n\n{} error(s).\nFirst: {}",
                                    errors.len(),
                                    errors[0]
                                ));
                            }
                            progress_bar_c.set_text(Some("Complete"));
                            status_label_c.set_text(&summary);
                            btn_start_c.set_sensitive(true);
                            *running_c.borrow_mut() = false;

                            // Show prominent completion dialog
                            let title = if errors.is_empty() {
                                "Complete"
                            } else {
                                "Completed with errors"
                            };
                            let dialog = AlertDialog::builder()
                                .modal(true)
                                .message(title)
                                .detail(&summary)
                                .build();
                            dialog.set_buttons(&["OK"]);
                            dialog.show(Some(&window_c));

                            return glib::ControlFlow::Break;
                        }
                        WorkerMsg::Error(e) => {
                            progress_bar_c.set_fraction(0.0);
                            progress_bar_c.set_text(Some("Error"));
                            status_label_c.set_text(&e);
                            btn_start_c.set_sensitive(true);
                            *running_c.borrow_mut() = false;

                            let dialog = AlertDialog::builder()
                                .modal(true)
                                .message("Error")
                                .detail(&e)
                                .build();
                            dialog.set_buttons(&["OK"]);
                            dialog.show(Some(&window_c));

                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });
        }
    });

    window.present();
}

// ── Helper: directory chooser row ──────────────────────────────────────

fn dir_row(label_text: &str) -> (GtkBox, Button, Label) {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some(label_text));
    label.set_width_chars(20);
    label.set_halign(Align::Start);

    let path_label = Label::new(Some("(none)"));
    path_label.set_hexpand(true);
    path_label.set_halign(Align::Start);
    path_label.set_ellipsize(gtk4::pango::EllipsizeMode::Start);

    let btn = Button::with_label("Browse…");

    row.append(&label);
    row.append(&path_label);
    row.append(&btn);

    (row, btn, path_label)
}

// ── Helper: open folder picker ─────────────────────────────────────────

fn pick_folder(window: &ApplicationWindow, target_label: Label) {
    let dialog = FileDialog::builder()
        .title("Select folder")
        .modal(true)
        .build();

    dialog.select_folder(Some(window), gtk4::gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                target_label.set_text(&path.to_string_lossy());
            }
        }
    });
}

// ── Worker thread ──────────────────────────────────────────────────────

fn run_worker(
    source: SourceSelection,
    dst: String,
    do_move: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    tx: mpsc::Sender<WorkerMsg>,
) {
    let dst_path = PathBuf::from(&dst);

    // Collect the files to process
    let files: Vec<PathBuf> = match &source {
        SourceSelection::None => {
            let _ = tx.send(WorkerMsg::Error("No source selected.".to_string()));
            return;
        }
        SourceSelection::Files(paths) => paths.clone(),
        SourceSelection::Directory(src_dir) => {
            let glob_set = match build_glob_set(patterns) {
                Ok(gs) => gs,
                Err(e) => {
                    let _ =
                        tx.send(WorkerMsg::Error(format!("Invalid exclude pattern: {}", e)));
                    return;
                }
            };

            let src_dir = src_dir.clone();
            let mut collected = Vec::new();
            for entry in WalkDir::new(&src_dir).into_iter().filter_entry(|e| {
                if e.path() == src_dir.as_path() {
                    return true;
                }
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy();
                    return !glob_set.is_match(name.as_ref());
                }
                true
            }) {
                match entry {
                    Ok(e) if e.file_type().is_file() => {
                        collected.push(e.into_path());
                    }
                    _ => {}
                }
            }
            collected
        }
    };

    let total = files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: 0,
            errors: vec![],
        });
        return;
    }

    // Determine the source directory (only relevant for "Folders and files" mode)
    let src_dir = match &source {
        SourceSelection::Directory(d) => Some(d.clone()),
        _ => None,
    };

    let mut copied = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for (i, file_path) in files.iter().enumerate() {
        // Build destination path based on source type and transfer mode
        let dest_file = match (&src_dir, transfer_mode) {
            // Directory source + "Folders and files": preserve directory structure
            (Some(sd), TransferMode::FoldersAndFiles) => match file_path.strip_prefix(sd) {
                Ok(rel) => dst_path.join(rel),
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            },
            // Directory source + "Files only": flat copy (just the filename)
            // Individual files: always flat copy
            _ => {
                let fname = match file_path.file_name() {
                    Some(f) => f,
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                dst_path.join(fname)
            }
        };

        // Create parent directory in destination
        if let Some(parent) = dest_file.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                errors.push(format!("{}: {}", file_path.display(), e));
                continue;
            }
        }

        let result = if do_move {
            // Try rename first (instant if same filesystem), fall back to copy+delete
            fs::rename(file_path, &dest_file).or_else(|_| {
                fs::copy(file_path, &dest_file).and_then(|_| fs::remove_file(file_path))
            })
        } else {
            fs::copy(file_path, &dest_file).map(|_| ())
        };

        match result {
            Ok(()) => copied += 1,
            Err(e) => errors.push(format!("{}: {}", file_path.display(), e)),
        }

        let _ = tx.send(WorkerMsg::Progress {
            done: i + 1,
            total,
            file: file_path.to_string_lossy().to_string(),
        });
    }

    let _ = tx.send(WorkerMsg::Finished {
        copied,
        skipped,
        errors,
    });
}

// ── Build a GlobSet from user-entered patterns ─────────────────────────

fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p)?);
    }
    builder.build()
}
