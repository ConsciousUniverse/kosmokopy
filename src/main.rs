use std::cell::RefCell;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;

use std::collections::HashSet;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, Entry,
    FileDialog, Label, Orientation, ProgressBar, ScrolledWindow, Separator, TextView, Window,
    WrapMode,
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
        skipped: Vec<String>,
        excluded: usize,
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
    let dst_row = dir_row_editable("Destination Directory:");
    let dst_entry: Entry = dst_row.2.clone();
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

    // ── Exclusions ────────────────────────────────────────────────────
    let excl_heading = Label::new(Some("Exclusions:"));
    excl_heading.set_halign(Align::Start);
    root.append(&excl_heading);

    let excl_btn_row = GtkBox::new(Orientation::Horizontal, 8);
    let btn_excl_dirs = Button::with_label("Exclude Directories…");
    let btn_excl_files = Button::with_label("Exclude Files…");
    let btn_excl_clear = Button::with_label("Clear");
    excl_btn_row.append(&btn_excl_dirs);
    excl_btn_row.append(&btn_excl_files);
    excl_btn_row.append(&btn_excl_clear);
    root.append(&excl_btn_row);

    let excl_view = TextView::new();
    excl_view.set_editable(false);
    excl_view.set_cursor_visible(false);
    excl_view.set_wrap_mode(WrapMode::WordChar);
    excl_view.set_monospace(true);

    let excl_scroll = ScrolledWindow::builder()
        .child(&excl_view)
        .min_content_height(80)
        .vexpand(true)
        .build();
    root.append(&excl_scroll);

    // Shared exclusion state: dirs stored as "/dirname", files as "filename"
    let exclusions: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // ── Overwrite toggle ─────────────────────────────────────────────
    let chk_overwrite = CheckButton::with_label("Overwrite existing files");
    chk_overwrite.set_active(false);
    root.append(&chk_overwrite);

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
    let btn_start = Button::with_label("Transfer");
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
        let dst_entry_c = dst_entry.clone();
        dst_row.1.connect_clicked(move |_| {
            pick_folder(&win_clone, dst_entry_c.clone());
        });
    }

    // ── Exclusion buttons ─────────────────────────────────────────────
    {
        let win = window.clone();
        let source_sel = source_selection.clone();
        let excls = exclusions.clone();
        let view = excl_view.clone();
        btn_excl_dirs.connect_clicked(move |_| {
            let src = source_sel.borrow().clone();
            let initial = match &src {
                SourceSelection::Directory(p) => Some(p.clone()),
                _ => None,
            };
            let dialog = FileDialog::builder()
                .title("Select directory to exclude")
                .modal(true)
                .build();
            if let Some(ref dir) = initial {
                dialog.set_initial_folder(Some(&gtk4::gio::File::for_path(dir)));
            }
            let excls2 = excls.clone();
            let view2 = view.clone();
            dialog.select_folder(Some(&win), gtk4::gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let dir_name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let entry = format!("/{}", dir_name);
                        let mut list = excls2.borrow_mut();
                        if !list.contains(&entry) {
                            list.push(entry);
                        }
                        refresh_exclusion_view(&view2, &list);
                    }
                }
            });
        });
    }

    {
        let win = window.clone();
        let source_sel = source_selection.clone();
        let excls = exclusions.clone();
        let view = excl_view.clone();
        btn_excl_files.connect_clicked(move |_| {
            let src = source_sel.borrow().clone();
            let initial = match &src {
                SourceSelection::Directory(p) => Some(p.clone()),
                _ => None,
            };
            let dialog = FileDialog::builder()
                .title("Select files to exclude")
                .modal(true)
                .build();
            if let Some(ref dir) = initial {
                dialog.set_initial_folder(Some(&gtk4::gio::File::for_path(dir)));
            }
            let excls2 = excls.clone();
            let view2 = view.clone();
            dialog.open_multiple(Some(&win), gtk4::gio::Cancellable::NONE, move |result| {
                if let Ok(files) = result {
                    let mut list = excls2.borrow_mut();
                    for i in 0..files.n_items() {
                        if let Some(obj) = files.item(i) {
                            if let Ok(gfile) = obj.downcast::<gtk4::gio::File>() {
                                if let Some(p) = gfile.path() {
                                    let fname = p
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    if !list.contains(&fname) {
                                        list.push(fname);
                                    }
                                }
                            }
                        }
                    }
                    refresh_exclusion_view(&view2, &list);
                }
            });
        });
    }

    {
        let excls = exclusions.clone();
        let view = excl_view.clone();
        btn_excl_clear.connect_clicked(move |_| {
            excls.borrow_mut().clear();
            view.buffer().set_text("");
        });
    }

    // ── Start button logic ────────────────────────────────────────────
    let running = Rc::new(RefCell::new(false));

    btn_start.connect_clicked({
        let source_selection = source_selection.clone();
        let dst_entry = dst_entry.clone();
        let chk_move = chk_move.clone();
        let chk_folders_files = chk_folders_files.clone();
        let chk_overwrite = chk_overwrite.clone();
        let exclusions = exclusions.clone();
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
            let dst = dst_entry.text().to_string();

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

            if dst.is_empty() {
                status_label.set_text("Please select or type a destination directory.");
                return;
            }

            let do_move = chk_move.is_active();
            let overwrite = chk_overwrite.is_active();
            let transfer_mode = if chk_folders_files.is_active() {
                TransferMode::FoldersAndFiles
            } else {
                TransferMode::FilesOnly
            };

            let patterns: Vec<String> = exclusions.borrow().clone();

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
                let (remote_host, dest_path) = parse_destination(&dst_clone);
                match remote_host {
                    Some(host) => run_remote_worker(
                        source_sel, &host, &dest_path, do_move, overwrite,
                        transfer_mode, &patterns, tx,
                    ),
                    None => run_worker(
                        source_sel, dest_path, do_move, overwrite,
                        transfer_mode, &patterns, tx,
                    ),
                }
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
                            excluded,
                            errors,
                        } => {
                            progress_bar_c.set_fraction(1.0);
                            let verb = if do_move { "Moved" } else { "Copied" };
                            let summary = format!(
                                "{} {} file(s), {} skipped, {} excluded.",
                                verb, copied, skipped.len(), excluded
                            );
                            progress_bar_c.set_text(Some("Complete"));
                            status_label_c.set_text(&summary);
                            btn_start_c.set_sensitive(true);
                            *running_c.borrow_mut() = false;

                            let title = if errors.is_empty() && skipped.is_empty() {
                                "Complete"
                            } else if !errors.is_empty() {
                                "Completed with errors"
                            } else {
                                "Completed with skipped files"
                            };

                            // Combine skipped and errors for the dialog
                            let mut all_notes = Vec::new();
                            if !skipped.is_empty() {
                                all_notes.push(format!("Skipped ({}):", skipped.len()));
                                all_notes.extend(skipped);
                            }
                            if !errors.is_empty() {
                                all_notes.push(format!("Errors ({}):", errors.len()));
                                all_notes.extend(errors);
                            }
                            show_result_dialog(&window_c, title, &summary, &all_notes);

                            return glib::ControlFlow::Break;
                        }
                        WorkerMsg::Error(e) => {
                            progress_bar_c.set_fraction(0.0);
                            progress_bar_c.set_text(Some("Error"));
                            status_label_c.set_text(&e);
                            btn_start_c.set_sensitive(true);
                            *running_c.borrow_mut() = false;

                            show_result_dialog(&window_c, "Error", &e, &[]);

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

// ── Helper: directory chooser row (editable) ──────────────────────────

fn dir_row_editable(label_text: &str) -> (GtkBox, Button, Entry) {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    let label = Label::new(Some(label_text));
    label.set_width_chars(20);
    label.set_halign(Align::Start);

    let entry = Entry::new();
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some("Local path or host:/remote/path"));

    let btn = Button::with_label("Browse…");

    row.append(&label);
    row.append(&entry);
    row.append(&btn);

    (row, btn, entry)
}

// ── Helper: result dialog with scrollable error list ───────────────────

fn show_result_dialog(parent: &ApplicationWindow, title: &str, summary: &str, errors: &[String]) {
    let dialog = Window::builder()
        .title(title)
        .modal(true)
        .transient_for(parent)
        .default_width(500)
        .default_height(if errors.is_empty() { 150 } else { 400 })
        .resizable(true)
        .build();

    let vbox = GtkBox::new(Orientation::Vertical, 12);
    vbox.set_margin_top(16);
    vbox.set_margin_bottom(16);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);

    // Summary label (large & bold)
    let summary_label = Label::new(None);
    summary_label.set_halign(Align::Start);
    summary_label.set_wrap(true);
    summary_label.set_markup(&format!("<big><b>{}</b></big>", glib::markup_escape_text(summary)));
    vbox.append(&summary_label);

    // Scrollable error list
    if !errors.is_empty() {
        let error_heading = Label::new(None);
        error_heading.set_halign(Align::Start);
        error_heading.set_markup(&format!("<b>{} error(s):</b>", errors.len()));
        vbox.append(&error_heading);

        let error_text = errors
            .iter()
            .enumerate()
            .map(|(i, e)| format!("{}. {}", i + 1, e))
            .collect::<Vec<_>>()
            .join("\n");

        let error_view = TextView::new();
        error_view.set_editable(false);
        error_view.set_cursor_visible(false);
        error_view.set_wrap_mode(WrapMode::WordChar);
        error_view.set_monospace(true);
        error_view.buffer().set_text(&error_text);

        let scroll = ScrolledWindow::builder()
            .child(&error_view)
            .min_content_height(150)
            .vexpand(true)
            .build();
        vbox.append(&scroll);
    }

    // OK button
    let btn_ok = Button::with_label("OK");
    btn_ok.add_css_class("suggested-action");
    btn_ok.set_halign(Align::End);
    let dialog_ref = dialog.clone();
    btn_ok.connect_clicked(move |_| {
        dialog_ref.close();
    });
    vbox.append(&btn_ok);

    dialog.set_child(Some(&vbox));
    dialog.present();
}

// ── Helper: open folder picker ─────────────────────────────────────────

fn pick_folder(window: &ApplicationWindow, target_entry: Entry) {
    let dialog = FileDialog::builder()
        .title("Select folder")
        .modal(true)
        .build();

    dialog.select_folder(Some(window), gtk4::gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                target_entry.set_text(&path.to_string_lossy());
            }
        }
    });
}

// ── Helper: refresh the exclusion display ──────────────────────────────

fn refresh_exclusion_view(view: &TextView, items: &[String]) {
    let display: Vec<String> = items
        .iter()
        .map(|item| {
            if item.starts_with('/') {
                format!("{}/ (recursive)", item)
            } else {
                item.clone()
            }
        })
        .collect();
    view.buffer().set_text(&display.join("\n"));
}

// ── Destination parsing ─────────────────────────────────────────────────

/// Parse "host:/path" → (Some(host), path).  Plain paths → (None, path).
fn parse_destination(dst: &str) -> (Option<String>, String) {
    if let Some(pos) = dst.find(':') {
        let host = &dst[..pos];
        let path = &dst[pos + 1..];
        // Only treat as remote if host has no slashes and path is non-empty
        if !host.is_empty() && !host.contains('/') && !path.is_empty() {
            return (Some(host.to_string()), path.to_string());
        }
    }
    (None, dst.to_string())
}

/// Shell-escape a string with single quotes (for ssh remote commands).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ── File collection (shared by local & remote workers) ─────────────────

fn collect_files(
    source: &SourceSelection,
    patterns: &[String],
) -> Result<(Vec<PathBuf>, usize), String> {
    match source {
        SourceSelection::None => Err("No source selected.".to_string()),
        SourceSelection::Files(paths) => Ok((paths.clone(), 0)),
        SourceSelection::Directory(src_dir) => {
            let excluded_dirs: HashSet<String> = patterns
                .iter()
                .filter(|p| p.starts_with('/'))
                .map(|p| p.trim_start_matches('/').to_string())
                .collect();
            let excluded_files: HashSet<String> = patterns
                .iter()
                .filter(|p| !p.starts_with('/'))
                .cloned()
                .collect();

            let src_dir = src_dir.clone();
            let mut collected = Vec::new();
            let mut excluded_count = 0usize;
            for entry in WalkDir::new(&src_dir).into_iter().filter_entry(|e| {
                if e.path() == src_dir.as_path() {
                    return true;
                }
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy().to_string();
                    return !excluded_dirs.contains(&name);
                }
                true
            }) {
                match entry {
                    Ok(e) if e.file_type().is_file() => {
                        let name = e.file_name().to_string_lossy().to_string();
                        if excluded_files.contains(&name) {
                            excluded_count += 1;
                        } else {
                            collected.push(e.into_path());
                        }
                    }
                    _ => {}
                }
            }
            Ok((collected, excluded_count))
        }
    }
}

// ── Worker thread (local) ──────────────────────────────────────────────

fn run_worker(
    source: SourceSelection,
    dst: String,
    do_move: bool,
    overwrite: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    tx: mpsc::Sender<WorkerMsg>,
) {
    let dst_path = PathBuf::from(&dst);

    // Create destination directory if it doesn't exist
    if !dst_path.exists() {
        if let Err(e) = fs::create_dir_all(&dst_path) {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create destination directory: {}",
                e
            )));
            return;
        }
    }

    // Collect the files to process
    let (files, excluded) = match collect_files(&source, patterns) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            return;
        }
    };

    let total = files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: vec![],
            excluded,
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
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, file_path) in files.iter().enumerate() {
        // Build destination path based on source type and transfer mode
        let dest_file = match (&src_dir, transfer_mode) {
            // Directory source + "Folders and files": preserve directory structure
            (Some(sd), TransferMode::FoldersAndFiles) => match file_path.strip_prefix(sd) {
                Ok(rel) => dst_path.join(rel),
                Err(_) => {
                    skipped.push(format!("{}: outside source directory", file_path.display()));
                    continue;
                }
            },
            // Directory source + "Files only": flat copy (just the filename)
            // Individual files: always flat copy
            _ => {
                let fname = match file_path.file_name() {
                    Some(f) => f,
                    None => {
                        skipped.push(format!("{}: no filename", file_path.display()));
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

        // Check if destination already exists
        if dest_file.exists() {
            match files_are_identical(file_path, &dest_file) {
                Ok(true) => {
                    // Destination is already identical — no copy needed
                    if do_move {
                        // Just delete the source
                        if let Err(e) = fs::remove_file(file_path) {
                            errors.push(format!("{}: identical at destination but failed to delete source: {}", file_path.display(), e));
                        } else {
                            copied += 1;
                        }
                    } else {
                        skipped.push(format!("{}: identical at destination", file_path.display()));
                    }
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total,
                        file: file_path.to_string_lossy().to_string(),
                    });
                    continue;
                }
                Ok(false) => {
                    // File differs — skip if overwrite is off
                    if !overwrite {
                        skipped.push(format!("{}: different version exists at destination", file_path.display()));
                        let _ = tx.send(WorkerMsg::Progress {
                            done: i + 1,
                            total,
                            file: file_path.to_string_lossy().to_string(),
                        });
                        continue;
                    }
                    // Otherwise fall through to overwrite
                }
                Err(e) => {
                    errors.push(format!("{}: could not compare with destination: {}", file_path.display(), e));
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total,
                        file: file_path.to_string_lossy().to_string(),
                    });
                    continue;
                }
            }
        }

        let result = if do_move {
            // Try rename first (instant pointer change on same filesystem)
            match fs::rename(file_path, &dest_file) {
                Ok(()) => Ok(()),
                Err(_) => {
                    // Cross-device: copy + verify + delete original
                    match fs::copy(file_path, &dest_file) {
                        Ok(_) => match files_are_identical(file_path, &dest_file) {
                            Ok(true) => fs::remove_file(file_path),
                            Ok(false) => {
                                let _ = fs::remove_file(&dest_file);
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "integrity check failed — original retained",
                                ))
                            }
                            Err(e) => Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("verification error (original retained): {}", e),
                            )),
                        },
                        Err(e) => Err(e),
                    }
                }
            }
        } else {
            // Copy + verify
            match fs::copy(file_path, &dest_file) {
                Ok(_) => match files_are_identical(file_path, &dest_file) {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        let _ = fs::remove_file(&dest_file);
                        Err(std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "integrity check failed — copy removed",
                        ))
                    }
                    Err(e) => Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("verification error: {}", e),
                    )),
                },
                Err(e) => Err(e),
            }
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
        excluded,
        errors,
    });
}

// ── Worker thread (remote via ssh/scp) ─────────────────────────────────

fn run_remote_worker(
    source: SourceSelection,
    host: &str,
    remote_base: &str,
    do_move: bool,
    overwrite: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    tx: mpsc::Sender<WorkerMsg>,
) {
    // SSH control-socket args — reuses a single TCP connection for all calls
    let ctl = ["-o", "ControlMaster=auto",
               "-o", "ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r",
               "-o", "ControlPersist=60"];

    // Quick connectivity check
    let check = Command::new("ssh")
        .args(&ctl)
        .args([host, "echo ok"])
        .output();
    match check {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let msg = String::from_utf8_lossy(&o.stderr);
            let _ = tx.send(WorkerMsg::Error(format!(
                "SSH connection to '{}' failed: {}", host, msg.trim()
            )));
            return;
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Could not run ssh command: {}", e
            )));
            return;
        }
    }

    // Collect files locally
    let (files, excluded) = match collect_files(&source, patterns) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            return;
        }
    };

    let total = files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: vec![],
            excluded,
            errors: vec![],
        });
        return;
    }

    let src_dir = match &source {
        SourceSelection::Directory(d) => Some(d.clone()),
        _ => None,
    };

    // Build list of (local_path, remote_path) pairs
    let remote_base = remote_base.trim_end_matches('/');
    let mut transfers: Vec<(PathBuf, String)> = Vec::new();
    let mut remote_dirs: HashSet<String> = HashSet::new();
    remote_dirs.insert(remote_base.to_string());
    let mut early_skipped: Vec<String> = Vec::new();

    for file_path in &files {
        let rel_dest = match (&src_dir, transfer_mode) {
            (Some(sd), TransferMode::FoldersAndFiles) => match file_path.strip_prefix(sd) {
                Ok(rel) => rel.to_string_lossy().to_string(),
                Err(_) => {
                    early_skipped.push(format!(
                        "{}: outside source directory",
                        file_path.display()
                    ));
                    continue;
                }
            },
            _ => match file_path.file_name() {
                Some(f) => f.to_string_lossy().to_string(),
                None => {
                    early_skipped.push(format!("{}: no filename", file_path.display()));
                    continue;
                }
            },
        };
        let remote_file = format!("{}/{}", remote_base, rel_dest);
        if let Some(parent) = Path::new(&remote_file).parent() {
            remote_dirs.insert(parent.to_string_lossy().to_string());
        }
        transfers.push((file_path.clone(), remote_file));
    }

    // Create all remote directories in one SSH call
    let dirs_arg: Vec<String> = remote_dirs.iter().map(|d| shell_quote(d)).collect();
    let mkdir_result = Command::new("ssh")
        .args(&ctl)
        .arg(host)
        .arg(format!("mkdir -p {}", dirs_arg.join(" ")))
        .output();
    if let Ok(o) = &mkdir_result {
        if !o.status.success() {
            let msg = String::from_utf8_lossy(&o.stderr);
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create remote directories: {}", msg.trim()
            )));
            return;
        }
    }

    // If !overwrite, get list of existing remote files in one SSH call
    let existing: HashSet<String> = if !overwrite {
        let out = Command::new("ssh")
            .args(&ctl)
            .arg(host)
            .arg(format!("find {} -type f 2>/dev/null", shell_quote(remote_base)))
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.to_string())
                .collect(),
            Err(_) => HashSet::new(),
        }
    } else {
        HashSet::new()
    };

    let total_transfers = transfers.len();
    let mut copied = 0usize;
    let mut skipped = early_skipped;
    let mut errors: Vec<String> = Vec::new();

    for (i, (local, remote)) in transfers.iter().enumerate() {
        // Skip if file exists remotely and overwrite is off
        if !overwrite && existing.contains(remote) {
            skipped.push(format!(
                "{}: already exists at destination",
                local.display()
            ));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total: total_transfers,
                file: local.to_string_lossy().to_string(),
            });
            continue;
        }

        // Transfer via scp
        let scp_result = Command::new("scp")
            .args(&ctl)
            .arg("-q")
            .arg(local)
            .arg(format!("{}:{}", host, remote))
            .status();

        match scp_result {
            Ok(s) if s.success() => {
                copied += 1;
                if do_move {
                    if let Err(e) = fs::remove_file(local) {
                        errors.push(format!(
                            "{}: transferred but failed to delete local: {}",
                            local.display(),
                            e
                        ));
                    }
                }
            }
            Ok(s) => {
                errors.push(format!(
                    "{}: scp failed (exit code {})",
                    local.display(),
                    s.code().unwrap_or(-1)
                ));
            }
            Err(e) => {
                errors.push(format!("{}: {}", local.display(), e));
            }
        }

        let _ = tx.send(WorkerMsg::Progress {
            done: i + 1,
            total: total_transfers,
            file: local.to_string_lossy().to_string(),
        });
    }

    let _ = tx.send(WorkerMsg::Finished {
        copied,
        skipped,
        excluded,
        errors,
    });
}

// ── Byte-by-byte file comparison ───────────────────────────────────────

fn files_are_identical(a: &Path, b: &Path) -> std::io::Result<bool> {
    let meta_a = fs::metadata(a)?;
    let meta_b = fs::metadata(b)?;
    if meta_a.len() != meta_b.len() {
        return Ok(false);
    }

    let mut fa = fs::File::open(a)?;
    let mut fb = fs::File::open(b)?;
    let mut buf_a = [0u8; 8192];
    let mut buf_b = [0u8; 8192];

    loop {
        let n_a = fa.read(&mut buf_a)?;
        let n_b = fb.read(&mut buf_b)?;
        if n_a != n_b || buf_a[..n_a] != buf_b[..n_b] {
            return Ok(false);
        }
        if n_a == 0 {
            return Ok(true);
        }
    }
}
