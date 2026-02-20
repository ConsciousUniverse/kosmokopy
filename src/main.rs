// Kosmokopy — GTK4 file copier/mover
// Copyright (C) 2026 Dan Bright <dan@danbright.uk>
// Licensed under the GNU General Public License v3.0
//
// This code was primarily authored using artificial intelligence
// (Claude Opus 4.6 model).

use std::cell::{Cell, RefCell};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use std::collections::HashSet;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, Entry,
    FileDialog, Label, Orientation, ProgressBar, ScrolledWindow, Separator, TextView, Window,
    WrapMode,
};
use sha2::{Sha256, Digest};
use walkdir::WalkDir;

const APP_ID: &str = "dev.kosmokopy.app";

// ── Source selection state ──────────────────────────────────────────────

#[derive(Clone, Debug)]
enum SourceSelection {
    None,
    Directory(PathBuf),
    Files(Vec<PathBuf>),
    Remote(String, String), // (host, remote_path)
}

// ── Transfer mode ──────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum TransferMode {
    FilesOnly,
    FoldersAndFiles,
}

#[derive(Clone, Copy, PartialEq)]
enum TransferMethod {
    Standard,
    Rsync,
}

#[derive(Clone, Copy, PartialEq)]
enum ConflictMode {
    Skip,
    Overwrite,
    Rename,
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "--cli" {
        std::process::exit(run_cli(&args[2..]));
    }
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

// ── CLI (headless) mode ────────────────────────────────────────────────

/// Run a transfer from the command line, printing JSON results to stdout.
///
/// Usage:
///   kosmokopy --cli [OPTIONS]
///
/// Helper to emit CLI JSON result and return an exit code.
fn cli_output_json(
    status: &str,
    copied: usize,
    skipped: &[String],
    excluded_files: usize,
    excluded_dirs: usize,
    errors: &[String],
) -> i32 {
    let skipped_json: Vec<String> = skipped
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    let errors_json: Vec<String> = errors
        .iter()
        .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    println!(
        "{{\"status\":\"{}\",\"copied\":{},\"skipped\":[{}],\"excluded_files\":{},\"excluded_dirs\":{},\"errors\":[{}]}}",
        status,
        copied,
        skipped_json.join(","),
        excluded_files,
        excluded_dirs,
        errors_json.join(","),
    );
    if !errors.is_empty() { 2 } else { 0 }
}

/// Required:
///   --src <path|host:/path>      Source directory or remote
///   --dst <path|host:/path>      Destination directory or remote
///
/// Optional:
///   --move                       Move instead of copy
///   --conflict <skip|overwrite|rename>   Conflict mode (default: skip)
///   --strip-spaces               Remove spaces from filenames
///   --mode <files|folders>       Transfer mode (default: folders)
///   --method <standard|rsync>    Transfer method (default: standard)
///   --exclude <pattern>          Exclusion pattern (repeatable)
///   --src-files <file1,file2>    Comma-separated list of individual source files
fn run_cli(args: &[String]) -> i32 {
    let mut src: Option<String> = None;
    let mut dst: Option<String> = None;
    let mut do_move = false;
    let mut conflict_mode = ConflictMode::Skip;
    let mut strip_spaces = false;
    let mut transfer_mode = TransferMode::FoldersAndFiles;
    let mut transfer_method = TransferMethod::Standard;
    let mut patterns: Vec<String> = Vec::new();
    let mut src_files: Option<Vec<PathBuf>> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--src" => {
                i += 1;
                src = args.get(i).cloned();
            }
            "--dst" => {
                i += 1;
                dst = args.get(i).cloned();
            }
            "--move" => do_move = true,
            "--conflict" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    conflict_mode = match val.as_str() {
                        "overwrite" => ConflictMode::Overwrite,
                        "rename" => ConflictMode::Rename,
                        _ => ConflictMode::Skip,
                    };
                }
            }
            "--strip-spaces" => strip_spaces = true,
            "--mode" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    transfer_mode = match val.as_str() {
                        "files" => TransferMode::FilesOnly,
                        _ => TransferMode::FoldersAndFiles,
                    };
                }
            }
            "--method" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    transfer_method = match val.as_str() {
                        "rsync" => TransferMethod::Rsync,
                        _ => TransferMethod::Standard,
                    };
                }
            }
            "--exclude" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    patterns.push(val.clone());
                }
            }
            "--src-files" => {
                i += 1;
                if let Some(val) = args.get(i) {
                    src_files = Some(
                        val.split(',')
                            .map(|s| PathBuf::from(s.trim()))
                            .collect(),
                    );
                }
            }
            other => {
                eprintln!("Unknown option: {}", other);
                return 1;
            }
        }
        i += 1;
    }

    let dst = match dst {
        Some(d) => d,
        None => {
            eprintln!("--dst is required");
            return 1;
        }
    };

    // Build source selection
    let source_sel = if let Some(files) = src_files {
        SourceSelection::Files(files)
    } else if let Some(s) = src {
        let (host, path) = parse_destination(&s);
        match host {
            Some(h) => SourceSelection::Remote(h, path),
            None => SourceSelection::Directory(PathBuf::from(path)),
        }
    } else {
        eprintln!("--src or --src-files is required");
        return 1;
    };

    let (tx, rx) = mpsc::channel::<WorkerMsg>();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Handle Ctrl+C gracefully in CLI mode
    {
        let cancel_flag_c = cancel_flag.clone();
        let _ = ctrlc::set_handler(move || {
            cancel_flag_c.store(true, Ordering::SeqCst);
            eprintln!("\nCancelling…");
        });
    }

    let src_is_remote = matches!(&source_sel, SourceSelection::Remote(_, _));
    let (dst_host, dest_path) = parse_destination(&dst);

    match (src_is_remote, dst_host, transfer_method) {
        (true, Some(dhost), TransferMethod::Standard) => {
            if let SourceSelection::Remote(shost, spath) = &source_sel {
                run_remote_to_remote_worker(
                    shost, spath, &dhost, &dest_path, do_move, conflict_mode,
                    strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
                );
            }
        }
        (true, Some(dhost), TransferMethod::Rsync) => {
            if let SourceSelection::Remote(shost, spath) = &source_sel {
                run_remote_to_remote_rsync_worker(
                    shost, spath, &dhost, &dest_path, do_move, conflict_mode,
                    strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
                );
            }
        }
        (true, None, method) => {
            if let SourceSelection::Remote(shost, spath) = &source_sel {
                run_remote_to_local_worker(
                    shost, spath, &dest_path, do_move, conflict_mode,
                    strip_spaces, transfer_mode, &patterns, method, cancel_flag.clone(), tx,
                );
            }
        }
        (false, Some(host), TransferMethod::Standard) => run_remote_worker(
            source_sel, &host, &dest_path, do_move, conflict_mode,
            strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
        ),
        (false, Some(host), TransferMethod::Rsync) => run_remote_rsync_worker(
            source_sel, &host, &dest_path, do_move, conflict_mode,
            strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
        ),
        (false, None, TransferMethod::Rsync) => run_local_rsync_worker(
            source_sel, dest_path, do_move, conflict_mode,
            strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
        ),
        (false, None, TransferMethod::Standard) => run_worker(
            source_sel, dest_path, do_move, conflict_mode,
            strip_spaces, transfer_mode, &patterns, cancel_flag.clone(), tx,
        ),
    }

    // Collect results from the worker
    for msg in rx {
        match msg {
            WorkerMsg::Finished { copied, skipped, excluded_files, excluded_dirs, errors } => {
                return cli_output_json("finished", copied, &skipped, excluded_files, excluded_dirs, &errors);
            }
            WorkerMsg::Cancelled { copied, skipped, excluded_files, excluded_dirs, errors } => {
                return cli_output_json("cancelled", copied, &skipped, excluded_files, excluded_dirs, &errors);
            }
            WorkerMsg::Error(e) => {
                let escaped = e.replace('\\', "\\\\").replace('"', "\\\"");
                println!("{{\"status\":\"error\",\"message\":\"{}\"}}", escaped);
                return 1;
            }
            WorkerMsg::Progress { .. } => {
                // Silently consume progress messages in CLI mode
            }
        }
    }

    eprintln!("Worker channel closed without result");
    1
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
        excluded_files: usize,
        excluded_dirs: usize,
        errors: Vec<String>,
    },
    Cancelled {
        copied: usize,
        skipped: Vec<String>,
        excluded_files: usize,
        excluded_dirs: usize,
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
    let src_entry = Entry::new();
    src_entry.set_hexpand(true);
    src_entry.set_placeholder_text(Some("Local path or host:/remote/path"));

    let btn_browse_folder = Button::with_label("Browse Folder…");
    let btn_browse_files = Button::with_label("Browse Files…");

    src_row.append(&src_entry);
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

    // ── Transfer method ──────────────────────────────────────────────
    let method_box = GtkBox::new(Orientation::Horizontal, 12);
    let method_label = Label::new(Some("Transfer method:"));
    method_label.set_halign(Align::Start);
    let chk_standard = CheckButton::with_label("Standard (cp/scp)");
    let chk_rsync = CheckButton::with_label("rsync");
    chk_rsync.set_group(Some(&chk_standard));
    chk_standard.set_active(true);
    method_box.append(&method_label);
    method_box.append(&chk_standard);
    method_box.append(&chk_rsync);
    root.append(&method_box);

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

    // Manual pattern entry row
    let pattern_row = GtkBox::new(Orientation::Horizontal, 8);
    let pattern_entry = Entry::new();
    pattern_entry.set_hexpand(true);
    pattern_entry.set_placeholder_text(Some("Pattern (e.g. *.jpg, /tmp*, test_*)"));
    let btn_add_file_pattern = Button::with_label("+ File Pattern");
    let btn_add_dir_pattern = Button::with_label("+ Dir Pattern");
    pattern_row.append(&pattern_entry);
    pattern_row.append(&btn_add_file_pattern);
    pattern_row.append(&btn_add_dir_pattern);
    root.append(&pattern_row);

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

    // Shared exclusion state: dirs stored as "/dirname", files as "filename",
    // wildcard dir patterns as "~/pattern", wildcard file patterns as "~pattern"
    let exclusions: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // ── Conflict handling ──────────────────────────────────────────
    let conflict_label = Label::new(Some("If file already exists:"));
    conflict_label.set_halign(Align::Start);
    root.append(&conflict_label);

    let conflict_row = GtkBox::new(Orientation::Horizontal, 12);
    let chk_skip = CheckButton::with_label("Skip");
    chk_skip.set_active(true);
    let chk_overwrite = CheckButton::with_label("Overwrite");
    chk_overwrite.set_group(Some(&chk_skip));
    let chk_rename = CheckButton::with_label("Auto-rename");
    chk_rename.set_group(Some(&chk_skip));
    conflict_row.append(&chk_skip);
    conflict_row.append(&chk_overwrite);
    conflict_row.append(&chk_rename);
    root.append(&conflict_row);

    let chk_strip_spaces = CheckButton::with_label("Remove spaces from filenames");
    chk_strip_spaces.set_active(false);
    root.append(&chk_strip_spaces);

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

    // ── Cancel button (hidden until a transfer is running) ────────────
    let btn_cancel = Button::with_label("Cancel");
    btn_cancel.add_css_class("destructive-action");
    btn_cancel.set_visible(false);
    root.append(&btn_cancel);

    window.set_child(Some(&root));

    // ── Shared source-selection state ─────────────────────────────────
    let source_selection = Rc::new(RefCell::new(SourceSelection::None));

    // ── Browse Folder button ──────────────────────────────────────────
    {
        let win_clone = window.clone();
        let src_entry_c = src_entry.clone();
        let source_sel = source_selection.clone();
        btn_browse_folder.connect_clicked(move |_| {
            let dialog = FileDialog::builder()
                .title("Select source folder")
                .modal(true)
                .build();
            let src_entry_c2 = src_entry_c.clone();
            let source_sel2 = source_sel.clone();
            dialog.select_folder(
                Some(&win_clone),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(file) = result {
                        if let Some(path) = file.path() {
                            src_entry_c2.set_text(&path.to_string_lossy());
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
        let src_entry_c = src_entry.clone();
        let source_sel = source_selection.clone();
        btn_browse_files.connect_clicked(move |_| {
            let dialog = FileDialog::builder()
                .title("Select files")
                .modal(true)
                .build();
            let src_entry_c2 = src_entry_c.clone();
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
                            src_entry_c2.set_text(&display);
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

    // ── Manual pattern buttons ────────────────────────────────────────
    {
        let excls = exclusions.clone();
        let view = excl_view.clone();
        let entry = pattern_entry.clone();
        btn_add_file_pattern.connect_clicked(move |_| {
            let text = entry.text().to_string().trim().to_string();
            if text.is_empty() {
                return;
            }
            // File wildcard pattern stored as "~pattern"
            let pattern = format!("~{}", text);
            let mut list = excls.borrow_mut();
            if !list.contains(&pattern) {
                list.push(pattern);
            }
            refresh_exclusion_view(&view, &list);
            entry.set_text("");
        });
    }

    {
        let excls = exclusions.clone();
        let view = excl_view.clone();
        let entry = pattern_entry.clone();
        btn_add_dir_pattern.connect_clicked(move |_| {
            let text = entry.text().to_string().trim().to_string();
            if text.is_empty() {
                return;
            }
            // Dir wildcard pattern stored as "~/pattern"
            let pattern = format!("~/{}", text);
            let mut list = excls.borrow_mut();
            if !list.contains(&pattern) {
                list.push(pattern);
            }
            refresh_exclusion_view(&view, &list);
            entry.set_text("");
        });
    }

    // ── Start button logic ────────────────────────────────────────────
    let running = Rc::new(RefCell::new(false));

    btn_start.connect_clicked({
        let source_selection = source_selection.clone();
        let src_entry = src_entry.clone();
        let dst_entry = dst_entry.clone();
        let chk_move = chk_move.clone();
        let chk_folders_files = chk_folders_files.clone();
        let chk_overwrite = chk_overwrite.clone();
        let chk_rename = chk_rename.clone();
        let chk_strip_spaces = chk_strip_spaces.clone();
        let chk_rsync = chk_rsync.clone();
        let exclusions = exclusions.clone();
        let progress_bar = progress_bar.clone();
        let status_label = status_label.clone();
        let btn_start = btn_start.clone();
        let btn_cancel = btn_cancel.clone();
        let running = running.clone();
        let window = window.clone();

        move |_| {
            if *running.borrow() {
                return;
            }

            let src_text = src_entry.text().to_string().trim().to_string();
            let dst = dst_entry.text().to_string();

            // Determine source: if the entry contains text, parse it;
            // otherwise fall back to the source_selection set by browse buttons.
            let source_sel = if !src_text.is_empty() {
                let (host, path) = parse_destination(&src_text);
                match host {
                    Some(h) => SourceSelection::Remote(h, path),
                    None => {
                        // Local path — could be a file or directory
                        let p = PathBuf::from(&path);
                        if p.is_file() {
                            SourceSelection::Files(vec![p])
                        } else {
                            SourceSelection::Directory(p)
                        }
                    }
                }
            } else {
                source_selection.borrow().clone()
            };

            match &source_sel {
                SourceSelection::None => {
                    status_label.set_text("Please select a source (folder, files, or remote).");
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
            let conflict_mode = if chk_overwrite.is_active() {
                ConflictMode::Overwrite
            } else if chk_rename.is_active() {
                ConflictMode::Rename
            } else {
                ConflictMode::Skip
            };
            let strip_spaces = chk_strip_spaces.is_active();
            let transfer_mode = if chk_folders_files.is_active() {
                TransferMode::FoldersAndFiles
            } else {
                TransferMode::FilesOnly
            };
            let transfer_method = if chk_rsync.is_active() {
                TransferMethod::Rsync
            } else {
                TransferMethod::Standard
            };

            let patterns: Vec<String> = exclusions.borrow().clone();

            *running.borrow_mut() = true;
            btn_start.set_sensitive(false);
            btn_cancel.set_visible(true);
            progress_bar.set_fraction(0.0);
            progress_bar.set_text(Some("Scanning…"));
            status_label.set_text("");

            // Cancel flag shared between UI and worker thread
            let cancel_flag = Arc::new(AtomicBool::new(false));

            // Wire Cancel button
            {
                let cancel_flag_c = cancel_flag.clone();
                let btn_cancel_c = btn_cancel.clone();
                btn_cancel_c.connect_clicked(move |btn| {
                    cancel_flag_c.store(true, Ordering::SeqCst);
                    btn.set_sensitive(false);
                    btn.set_label("Cancelling…");
                });
            }

            // Channel for worker → UI communication
            let (tx, rx) = mpsc::channel::<WorkerMsg>();

            // Spawn worker thread
            let dst_clone = dst.clone();
            let cancel_flag_w = cancel_flag.clone();
            thread::spawn(move || {
                let (dst_host, dest_path) = parse_destination(&dst_clone);
                let src_is_remote = matches!(&source_sel, SourceSelection::Remote(_, _));
                match (src_is_remote, dst_host, transfer_method) {
                    // Remote source → remote destination
                    (true, Some(dhost), TransferMethod::Standard) => {
                        if let SourceSelection::Remote(shost, spath) = &source_sel {
                            run_remote_to_remote_worker(
                                shost, &spath, &dhost, &dest_path, do_move, conflict_mode,
                                strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                            );
                        }
                    }
                    (true, Some(dhost), TransferMethod::Rsync) => {
                        if let SourceSelection::Remote(shost, spath) = &source_sel {
                            run_remote_to_remote_rsync_worker(
                                shost, &spath, &dhost, &dest_path, do_move, conflict_mode,
                                strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                            );
                        }
                    }
                    // Remote source → local destination
                    (true, None, transfer_method) => {
                        if let SourceSelection::Remote(shost, spath) = &source_sel {
                            run_remote_to_local_worker(
                                shost, &spath, &dest_path, do_move, conflict_mode,
                                strip_spaces, transfer_mode, &patterns, transfer_method, cancel_flag_w, tx,
                            );
                        }
                    }
                    // Local source → remote destination
                    (false, Some(host), TransferMethod::Standard) => run_remote_worker(
                        source_sel, &host, &dest_path, do_move, conflict_mode,
                        strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                    ),
                    (false, Some(host), TransferMethod::Rsync) => run_remote_rsync_worker(
                        source_sel, &host, &dest_path, do_move, conflict_mode,
                        strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                    ),
                    // Local source → local destination
                    (false, None, TransferMethod::Rsync) => run_local_rsync_worker(
                        source_sel, dest_path, do_move, conflict_mode,
                        strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                    ),
                    (false, None, TransferMethod::Standard) => run_worker(
                        source_sel, dest_path, do_move, conflict_mode,
                        strip_spaces, transfer_mode, &patterns, cancel_flag_w, tx,
                    ),
                }
            });

            // Poll for messages on the glib main loop
            let progress_bar_c = progress_bar.clone();
            let status_label_c = status_label.clone();
            let btn_start_c = btn_start.clone();
            let btn_cancel_c = btn_cancel.clone();
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
                            excluded_files,
                            excluded_dirs,
                            errors,
                        } => {
                            progress_bar_c.set_fraction(1.0);
                            let verb = if do_move { "Moved" } else { "Copied" };
                            let mut excl_parts = Vec::new();
                            if excluded_files > 0 {
                                excl_parts.push(format!("{} file(s)", excluded_files));
                            }
                            if excluded_dirs > 0 {
                                excl_parts.push(format!("{} dir(s)", excluded_dirs));
                            }
                            let excl_str = if excl_parts.is_empty() {
                                "0".to_string()
                            } else {
                                excl_parts.join(", ")
                            };
                            let summary = format!(
                                "{} {} file(s), {} skipped, {} excluded.",
                                verb, copied, skipped.len(), excl_str
                            );
                            progress_bar_c.set_text(Some("Complete"));
                            status_label_c.set_text(&summary);
                            btn_start_c.set_sensitive(true);
                            btn_cancel_c.set_visible(false);
                            btn_cancel_c.set_sensitive(true);
                            btn_cancel_c.set_label("Cancel");
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
                            btn_cancel_c.set_visible(false);
                            btn_cancel_c.set_sensitive(true);
                            btn_cancel_c.set_label("Cancel");
                            *running_c.borrow_mut() = false;

                            show_result_dialog(&window_c, "Error", &e, &[]);

                            return glib::ControlFlow::Break;
                        }
                        WorkerMsg::Cancelled {
                            copied,
                            skipped,
                            excluded_files,
                            excluded_dirs,
                            errors,
                        } => {
                            let verb = if do_move { "Moved" } else { "Copied" };
                            let mut excl_parts = Vec::new();
                            if excluded_files > 0 {
                                excl_parts.push(format!("{} file(s)", excluded_files));
                            }
                            if excluded_dirs > 0 {
                                excl_parts.push(format!("{} dir(s)", excluded_dirs));
                            }
                            let excl_str = if excl_parts.is_empty() {
                                "0".to_string()
                            } else {
                                excl_parts.join(", ")
                            };
                            let summary = format!(
                                "Cancelled. {} {} file(s) before stopping, {} skipped, {} excluded.",
                                verb, copied, skipped.len(), excl_str
                            );
                            progress_bar_c.set_text(Some("Cancelled"));
                            status_label_c.set_text(&summary);
                            btn_start_c.set_sensitive(true);
                            btn_cancel_c.set_visible(false);
                            btn_cancel_c.set_sensitive(true);
                            btn_cancel_c.set_label("Cancel");
                            *running_c.borrow_mut() = false;

                            let mut all_notes = Vec::new();
                            if !skipped.is_empty() {
                                all_notes.push(format!("Skipped ({}):", skipped.len()));
                                all_notes.extend(skipped);
                            }
                            if !errors.is_empty() {
                                all_notes.push(format!("Errors ({}):", errors.len()));
                                all_notes.extend(errors);
                            }
                            show_result_dialog(&window_c, "Cancelled", &summary, &all_notes);

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
    label.set_xalign(0.0);

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
            if item.starts_with("~/") {
                // Wildcard directory pattern
                format!("{}/ (dir pattern)", &item[1..])
            } else if item.starts_with('~') {
                // Wildcard file pattern
                format!("{} (file pattern)", &item[1..])
            } else if item.starts_with('/') {
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

/// Find a unique local path by appending " (1)", " (2)", etc. before the extension.
fn find_unique_local_path(original: &Path) -> PathBuf {
    let parent = original.parent().unwrap_or_else(|| Path::new("."));
    let stem = original.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = original.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1u32;
    loop {
        let candidate = parent.join(format!("{} ({}){}", stem, n, ext));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Find a unique remote path by appending " (1)", " (2)", etc. before the extension.
/// Checks existence via SSH.
#[allow(dead_code)]
fn find_unique_remote_path(
    original: &str,
    host: &str,
    ctl: &[&str],
) -> String {
    let path = Path::new(original);
    let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_string_lossy().to_string();
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1u32;
    loop {
        let candidate = format!("{}/{} ({}){}", parent, stem, n, ext);
        let check = Command::new("ssh")
            .args(ctl)
            .arg(host)
            .arg(format!("test -e {}", shell_quote(&candidate)))
            .status();
        match check {
            Ok(s) if s.success() => {
                // exists, try next number
                n += 1;
            }
            _ => return candidate,
        }
    }
}

/// Find a unique remote path using the pre-fetched set of existing files.
fn find_unique_remote_path_from_set(
    original: &str,
    existing: &HashSet<String>,
) -> String {
    let path = Path::new(original);
    let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_string_lossy().to_string();
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut n = 1u32;
    loop {
        let candidate = format!("{}/{} ({}){}", parent, stem, n, ext);
        if !existing.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Strip spaces from path components beyond the base destination directory.
fn strip_spaces_from_path(base: &Path, full: &Path) -> PathBuf {
    match full.strip_prefix(base) {
        Ok(rel) => {
            let cleaned: PathBuf = rel
                .components()
                .map(|c| {
                    let s = c.as_os_str().to_string_lossy();
                    std::ffi::OsString::from(s.replace(' ', ""))
                })
                .collect();
            base.join(cleaned)
        }
        Err(_) => full.to_path_buf(),
    }
}

// ── Wildcard pattern matching ──────────────────────────────────────────

/// Match a name against a pattern that may contain `*` (any chars) and `?`
/// (single char) wildcards.  Matching is case-insensitive and only ever
/// applied to a single path component (file or directory name).
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    wildcard_match_inner(&p, &n)
}

fn wildcard_match_inner(pattern: &[char], name: &[char]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some('*'), _) => {
            // '*' matches zero or more characters
            wildcard_match_inner(&pattern[1..], name)
                || (!name.is_empty() && wildcard_match_inner(pattern, &name[1..]))
        }
        (Some('?'), Some(_)) => wildcard_match_inner(&pattern[1..], &name[1..]),
        (Some(pc), Some(nc)) if *pc == *nc => wildcard_match_inner(&pattern[1..], &name[1..]),
        _ => false,
    }
}

// ── File collection (shared by local & remote workers) ─────────────────

fn collect_files(
    source: &SourceSelection,
    patterns: &[String],
) -> Result<(Vec<PathBuf>, usize, usize), String> {
    match source {
        SourceSelection::None => Err("No source selected.".to_string()),
        SourceSelection::Remote(_, _) => Err("Remote source uses its own file listing.".to_string()),
        SourceSelection::Files(paths) => Ok((paths.clone(), 0, 0)),
        SourceSelection::Directory(src_dir) => {
            // Exact directory exclusions: "/dirname"
            let excluded_dirs: HashSet<String> = patterns
                .iter()
                .filter(|p| p.starts_with('/') && !p.starts_with("~/"))
                .map(|p| p.trim_start_matches('/').to_string())
                .collect();
            // Exact file exclusions: "filename"
            let excluded_files: HashSet<String> = patterns
                .iter()
                .filter(|p| !p.starts_with('/') && !p.starts_with('~'))
                .cloned()
                .collect();
            // Wildcard directory patterns: "~/pattern" → "pattern"
            let wildcard_dirs: Vec<String> = patterns
                .iter()
                .filter(|p| p.starts_with("~/"))
                .map(|p| p[2..].to_string())
                .collect();
            // Wildcard file patterns: "~pattern" (but not "~/...")
            let wildcard_files: Vec<String> = patterns
                .iter()
                .filter(|p| p.starts_with('~') && !p.starts_with("~/"))
                .map(|p| p[1..].to_string())
                .collect();

            let src_dir = src_dir.clone();
            let mut collected = Vec::new();
            let mut excluded_file_count = 0usize;
            let excluded_dir_count = Cell::new(0usize);
            for entry in WalkDir::new(&src_dir).into_iter().filter_entry(|e| {
                if e.path() == src_dir.as_path() {
                    return true;
                }
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if excluded_dirs.contains(&name) {
                        excluded_dir_count.set(excluded_dir_count.get() + 1);
                        return false;
                    }
                    if wildcard_dirs.iter().any(|pat| wildcard_matches(pat, &name)) {
                        excluded_dir_count.set(excluded_dir_count.get() + 1);
                        return false;
                    }
                    return true;
                }
                true
            }) {
                match entry {
                    Ok(e) if e.file_type().is_file() => {
                        let name = e.file_name().to_string_lossy().to_string();
                        if excluded_files.contains(&name)
                            || wildcard_files.iter().any(|pat| wildcard_matches(pat, &name))
                        {
                            excluded_file_count += 1;
                        } else {
                            collected.push(e.into_path());
                        }
                    }
                    _ => {}
                }
            }
            Ok((collected, excluded_file_count, excluded_dir_count.get()))
        }
    }
}

// ── Worker thread (local) ──────────────────────────────────────────────

fn run_worker(
    source: SourceSelection,
    dst: String,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
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
    let (files, excluded_files, excluded_dirs) = match collect_files(&source, patterns) {
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
            excluded_files,
            excluded_dirs,
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
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        // Build destination path based on source type and transfer mode
        let dest_file = match (&src_dir, transfer_mode) {
            // Directory source + "Folders and files": preserve directory structure
            (Some(sd), TransferMode::FoldersAndFiles) => match file_path.strip_prefix(sd) {
                Ok(rel) => {
                    let root = sd.file_name().unwrap_or(sd.as_os_str());
                    dst_path.join(root).join(rel)
                }
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

        // Strip spaces from the destination path components if requested
        let mut dest_file = if strip_spaces {
            strip_spaces_from_path(&dst_path, &dest_file)
        } else {
            dest_file
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
                    match conflict_mode {
                        ConflictMode::Skip => {
                            skipped.push(format!("{}: different version exists at destination", file_path.display()));
                            let _ = tx.send(WorkerMsg::Progress {
                                done: i + 1,
                                total,
                                file: file_path.to_string_lossy().to_string(),
                            });
                            continue;
                        }
                        ConflictMode::Rename => {
                            dest_file = find_unique_local_path(&dest_file);
                        }
                        ConflictMode::Overwrite => {
                            // fall through to overwrite
                        }
                    }
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
        excluded_files,
        excluded_dirs,
        errors,
    });
}

// ── Worker thread (local via rsync) ────────────────────────────────────

fn run_local_rsync_worker(
    source: SourceSelection,
    dst: String,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
) {
    let dst_path = PathBuf::from(&dst);

    // Check that rsync is available
    match Command::new("rsync").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            let _ = tx.send(WorkerMsg::Error(
                "rsync is not installed or not found in PATH".to_string(),
            ));
            return;
        }
    }

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
    let (files, excluded_files, excluded_dirs) = match collect_files(&source, patterns) {
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
            excluded_files,
            excluded_dirs,
            errors: vec![],
        });
        return;
    }

    let src_dir = match &source {
        SourceSelection::Directory(d) => Some(d.clone()),
        _ => None,
    };

    let mut copied = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, file_path) in files.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        // Build destination path
        let dest_file = match (&src_dir, transfer_mode) {
            (Some(sd), TransferMode::FoldersAndFiles) => match file_path.strip_prefix(sd) {
                Ok(rel) => {
                    let root = sd.file_name().unwrap_or(sd.as_os_str());
                    dst_path.join(root).join(rel)
                }
                Err(_) => {
                    skipped.push(format!("{}: outside source directory", file_path.display()));
                    continue;
                }
            },
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

        // Strip spaces if requested
        let mut dest_file = if strip_spaces {
            strip_spaces_from_path(&dst_path, &dest_file)
        } else {
            dest_file
        };

        // Create parent directory
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
                    if do_move {
                        if let Err(e) = fs::remove_file(file_path) {
                            errors.push(format!(
                                "{}: identical at destination but failed to delete source: {}",
                                file_path.display(),
                                e
                            ));
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
                    match conflict_mode {
                        ConflictMode::Skip => {
                            skipped.push(format!(
                                "{}: different version exists at destination",
                                file_path.display()
                            ));
                            let _ = tx.send(WorkerMsg::Progress {
                                done: i + 1,
                                total,
                                file: file_path.to_string_lossy().to_string(),
                            });
                            continue;
                        }
                        ConflictMode::Rename => {
                            dest_file = find_unique_local_path(&dest_file);
                        }
                        ConflictMode::Overwrite => {
                            // fall through to overwrite
                        }
                    }
                }
                Err(e) => {
                    errors.push(format!(
                        "{}: could not compare with destination: {}",
                        file_path.display(),
                        e
                    ));
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total,
                        file: file_path.to_string_lossy().to_string(),
                    });
                    continue;
                }
            }
        }

        // For move on the same filesystem, try rename first (atomic, no copy needed)
        if do_move {
            if let Ok(()) = fs::rename(file_path, &dest_file) {
                copied += 1;
                let _ = tx.send(WorkerMsg::Progress {
                    done: i + 1,
                    total,
                    file: file_path.to_string_lossy().to_string(),
                });
                continue;
            }
            // rename failed (cross-device) — fall through to rsync
        }

        // Transfer via rsync with checksum verification
        let rsync_result = Command::new("rsync")
            .args(["-a", "--checksum"])
            .arg(file_path)
            .arg(&dest_file)
            .status();

        match rsync_result {
            Ok(s) if s.success() => {
                // rsync --checksum verifies during transfer; also do a full
                // byte-by-byte comparison for defense in depth
                match files_are_identical(file_path, &dest_file) {
                    Ok(true) => {
                        copied += 1;
                        if do_move {
                            if let Err(e) = fs::remove_file(file_path) {
                                errors.push(format!(
                                    "{}: transferred and verified but failed to delete source: {}",
                                    file_path.display(),
                                    e
                                ));
                            }
                        }
                    }
                    Ok(false) => {
                        let _ = fs::remove_file(&dest_file);
                        errors.push(format!(
                            "{}: integrity check failed — byte comparison mismatch (original retained, copy removed)",
                            file_path.display()
                        ));
                    }
                    Err(e) => {
                        if do_move {
                            errors.push(format!(
                                "{}: transferred but verification failed: {} (original retained)",
                                file_path.display(),
                                e
                            ));
                        } else {
                            errors.push(format!(
                                "{}: transferred but could not verify: {}",
                                file_path.display(),
                                e
                            ));
                        }
                    }
                }
            }
            Ok(s) => {
                errors.push(format!(
                    "{}: rsync failed (exit code {})",
                    file_path.display(),
                    s.code().unwrap_or(-1)
                ));
            }
            Err(e) => {
                errors.push(format!("{}: {}", file_path.display(), e));
            }
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
        excluded_files,
        excluded_dirs,
        errors,
    });
}

// ── Worker thread (remote via ssh/scp) ─────────────────────────────────

fn run_remote_worker(
    source: SourceSelection,
    host: &str,
    remote_base: &str,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
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
    let (files, excluded_files, excluded_dirs) = match collect_files(&source, patterns) {
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
            excluded_files,
            excluded_dirs,
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
                Ok(rel) => {
                    let root = sd.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    if root.is_empty() { rel.to_string_lossy().to_string() }
                    else { format!("{}/{}", root, rel.to_string_lossy()) }
                }
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
        // Strip spaces from the remote path if requested
        let remote_file = if strip_spaces {
            remote_file.split('/').map(|c| c.replace(' ', "")).collect::<Vec<_>>().join("/")
        } else {
            remote_file
        };
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

    // If not overwriting, get list of existing remote files in one SSH call
    let existing: HashSet<String> = if conflict_mode != ConflictMode::Overwrite {
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
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        // Handle conflict if file exists remotely
        let remote = if conflict_mode != ConflictMode::Overwrite && existing.contains(remote) {
            match conflict_mode {
                ConflictMode::Skip => {
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
                ConflictMode::Rename => {
                    std::borrow::Cow::Owned(find_unique_remote_path_from_set(remote, &existing))
                }
                ConflictMode::Overwrite => unreachable!(),
            }
        } else {
            std::borrow::Cow::Borrowed(remote.as_str())
        };

        // Transfer via scp
        let scp_result = Command::new("scp")
            .args(&ctl)
            .arg("-q")
            .arg(local)
            .arg(format!("{}:{}", host, remote))
            .status();

        match scp_result {
            Ok(s) if s.success() => {
                // Verify integrity with SHA-256 hash comparison
                match verify_remote_hash(local, host, &ctl, &remote) {
                    Ok(true) => {
                        copied += 1;
                        if do_move {
                            if let Err(e) = fs::remove_file(local) {
                                errors.push(format!(
                                    "{}: transferred and verified but failed to delete local: {}",
                                    local.display(),
                                    e
                                ));
                            }
                        }
                    }
                    Ok(false) => {
                        // Hash mismatch — remove corrupt remote copy, keep source
                        let _ = Command::new("ssh")
                            .args(&ctl)
                            .arg(host)
                            .arg(format!("rm -f {}", shell_quote(&remote)))
                            .status();
                        errors.push(format!(
                            "{}: integrity check failed — hash mismatch (original retained, remote copy removed)",
                            local.display()
                        ));
                    }
                    Err(e) => {
                        // Cannot verify — keep both, report error
                        if do_move {
                            errors.push(format!(
                                "{}: transferred but verification failed: {} (original retained)",
                                local.display(),
                                e
                            ));
                        } else {
                            errors.push(format!(
                                "{}: transferred but could not verify: {}",
                                local.display(),
                                e
                            ));
                        }
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
        excluded_files,
        excluded_dirs,
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

// ── Remote file listing ────────────────────────────────────────────────

/// List files on a remote host under `remote_base`, applying exclusion patterns.
/// Returns (Vec<remote_path>, excluded_count).
fn collect_remote_files(
    host: &str,
    ctl: &[&str],
    remote_base: &str,
    patterns: &[String],
) -> Result<(Vec<String>, usize, usize), String> {
    let out = Command::new("ssh")
        .args(ctl)
        .arg(host)
        .arg(format!("find {} -type f 2>/dev/null", shell_quote(remote_base)))
        .output()
        .map_err(|e| format!("Failed to list remote files: {}", e))?;

    if !out.status.success() {
        return Err(format!(
            "Failed to list remote files: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    // Parse exclusion patterns
    let excluded_dirs: HashSet<String> = patterns
        .iter()
        .filter(|p| p.starts_with('/') && !p.starts_with("~/"))
        .map(|p| p.trim_start_matches('/').to_string())
        .collect();
    let excluded_files: HashSet<String> = patterns
        .iter()
        .filter(|p| !p.starts_with('/') && !p.starts_with('~'))
        .cloned()
        .collect();
    let wildcard_dirs: Vec<String> = patterns
        .iter()
        .filter(|p| p.starts_with("~/"))
        .map(|p| p[2..].to_string())
        .collect();
    let wildcard_files: Vec<String> = patterns
        .iter()
        .filter(|p| p.starts_with('~') && !p.starts_with("~/"))
        .map(|p| p[1..].to_string())
        .collect();

    let remote_base_slash = format!("{}/", remote_base.trim_end_matches('/'));
    let mut collected = Vec::new();
    let mut excluded_file_count = 0usize;
    let mut excluded_dir_names: HashSet<String> = HashSet::new();

    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Get relative path from remote_base
        let rel = if let Some(stripped) = line.strip_prefix(&remote_base_slash) {
            stripped
        } else if line == remote_base {
            // The remote path is a single file, not a directory.
            // Use just the filename as the relative path.
            match Path::new(line).file_name() {
                Some(name) => name.to_str().unwrap_or(line),
                None => continue,
            }
        } else {
            continue;
        };

        // Check directory exclusions against each path component
        let parts: Vec<&str> = rel.split('/').collect();
        let filename = parts.last().unwrap_or(&"");

        // Check dir exclusions (all components except the filename)
        let mut dir_excluded = false;
        for part in &parts[..parts.len().saturating_sub(1)] {
            if excluded_dirs.contains(*part)
                || wildcard_dirs.iter().any(|pat| wildcard_matches(pat, part))
            {
                dir_excluded = true;
                excluded_dir_names.insert(part.to_string());
                break;
            }
        }
        if dir_excluded {
            continue;
        }

        // Check file exclusions
        if excluded_files.contains(*filename)
            || wildcard_files.iter().any(|pat| wildcard_matches(pat, filename))
        {
            excluded_file_count += 1;
            continue;
        }

        collected.push(line.to_string());
    }

    Ok((collected, excluded_file_count, excluded_dir_names.len()))
}

// ── Worker thread (remote source → local destination) ──────────────────

fn run_remote_to_local_worker(
    src_host: &str,
    src_remote_base: &str,
    local_dst: &str,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    transfer_method: TransferMethod,
    cancel_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
) {
    let ctl = [
        "-o", "ControlMaster=auto",
        "-o", "ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r",
        "-o", "ControlPersist=60",
    ];

    // Connectivity check to source
    let check = Command::new("ssh")
        .args(&ctl)
        .args([src_host, "echo ok"])
        .output();
    match check {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            let _ = tx.send(WorkerMsg::Error(format!(
                "SSH connection to source '{}' failed: {}",
                src_host,
                String::from_utf8_lossy(&o.stderr).trim()
            )));
            return;
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!("Could not run ssh: {}", e)));
            return;
        }
    }

    // List remote source files
    let (remote_files, excluded_files, excluded_dirs) = match collect_remote_files(src_host, &ctl, src_remote_base, patterns) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            return;
        }
    };

    let total = remote_files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: vec![],
            excluded_files,
            excluded_dirs,
            errors: vec![],
        });
        return;
    }

    let dst_path = PathBuf::from(local_dst);
    if !dst_path.exists() {
        if let Err(e) = fs::create_dir_all(&dst_path) {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create destination directory: {}", e
            )));
            return;
        }
    }

    let src_base = src_remote_base.trim_end_matches('/');
    let src_base_slash = format!("{}/", src_base);
    let src_root_name = Path::new(src_base).file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let ssh_cmd = "ssh -o ControlMaster=auto -o ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r -o ControlPersist=60";

    let mut copied = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, remote_file) in remote_files.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        let rel = remote_file
            .strip_prefix(&src_base_slash)
            .unwrap_or(remote_file);

        let local_dest = match transfer_mode {
            TransferMode::FoldersAndFiles => {
                if src_root_name.is_empty() { dst_path.join(rel) }
                else { dst_path.join(&src_root_name).join(rel) }
            }
            TransferMode::FilesOnly => {
                let fname = Path::new(rel)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| rel.to_string());
                dst_path.join(fname)
            }
        };

        let mut local_dest = if strip_spaces {
            strip_spaces_from_path(&dst_path, &local_dest)
        } else {
            local_dest
        };

        // Create parent directory
        if let Some(parent) = local_dest.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                errors.push(format!("{}: {}", remote_file, e));
                continue;
            }
        }

        // Check conflict
        if local_dest.exists() {
            match conflict_mode {
                ConflictMode::Skip => {
                    skipped.push(format!("{}: already exists at destination", remote_file));
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total,
                        file: remote_file.clone(),
                    });
                    continue;
                }
                ConflictMode::Rename => {
                    local_dest = find_unique_local_path(&local_dest);
                }
                ConflictMode::Overwrite => {
                    // fall through
                }
            }
        }

        // Download from source
        let download_ok = match transfer_method {
            TransferMethod::Standard => {
                let result = Command::new("scp")
                    .args(&ctl)
                    .arg("-q")
                    .arg(format!("{}:{}", src_host, remote_file))
                    .arg(&local_dest)
                    .status();
                matches!(result, Ok(s) if s.success())
            }
            TransferMethod::Rsync => {
                let result = Command::new("rsync")
                    .args(["-az", "--checksum"])
                    .arg("-e")
                    .arg(ssh_cmd)
                    .arg(format!("{}:{}", src_host, remote_file))
                    .arg(&local_dest)
                    .status();
                matches!(result, Ok(s) if s.success())
            }
        };

        if !download_ok {
            errors.push(format!("{}: download from source failed", remote_file));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total,
                file: remote_file.clone(),
            });
            continue;
        }

        // Verify download with SHA-256
        match verify_remote_hash(&local_dest, src_host, &ctl, remote_file) {
            Ok(true) => {
                copied += 1;
                if do_move {
                    // Delete from source host
                    let rm_result = Command::new("ssh")
                        .args(&ctl)
                        .arg(src_host)
                        .arg(format!("rm -f {}", shell_quote(remote_file)))
                        .status();
                    if !matches!(rm_result, Ok(s) if s.success()) {
                        errors.push(format!(
                            "{}: downloaded and verified but failed to delete from source",
                            remote_file
                        ));
                    }
                }
            }
            Ok(false) => {
                let _ = fs::remove_file(&local_dest);
                errors.push(format!(
                    "{}: download integrity check failed — hash mismatch (local copy removed)",
                    remote_file
                ));
            }
            Err(e) => {
                if do_move {
                    errors.push(format!(
                        "{}: downloaded but verification failed: {} (source retained)",
                        remote_file, e
                    ));
                } else {
                    errors.push(format!(
                        "{}: downloaded but could not verify: {}",
                        remote_file, e
                    ));
                }
            }
        }

        let _ = tx.send(WorkerMsg::Progress {
            done: i + 1,
            total,
            file: remote_file.clone(),
        });
    }

    let _ = tx.send(WorkerMsg::Finished {
        copied,
        skipped,
        excluded_files,
        excluded_dirs,
        errors,
    });
}

// ── Worker thread (remote source → remote destination via SCP) ─────────

fn run_remote_to_remote_worker(
    src_host: &str,
    src_remote_base: &str,
    dst_host: &str,
    dst_remote_base: &str,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
) {
    let ctl = [
        "-o", "ControlMaster=auto",
        "-o", "ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r",
        "-o", "ControlPersist=60",
    ];

    // Connectivity check to both hosts
    for host in [src_host, dst_host] {
        let check = Command::new("ssh")
            .args(&ctl)
            .args([host, "echo ok"])
            .output();
        match check {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = tx.send(WorkerMsg::Error(format!(
                    "SSH connection to '{}' failed: {}",
                    host,
                    String::from_utf8_lossy(&o.stderr).trim()
                )));
                return;
            }
            Err(e) => {
                let _ = tx.send(WorkerMsg::Error(format!("Could not run ssh: {}", e)));
                return;
            }
        }
    }

    // List remote source files
    let (remote_files, excluded_files, excluded_dirs) = match collect_remote_files(src_host, &ctl, src_remote_base, patterns) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            return;
        }
    };

    let total = remote_files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: vec![],
            excluded_files,
            excluded_dirs,
            errors: vec![],
        });
        return;
    }

    // Create a temp directory for the local staging area
    let temp_dir = match tempdir_for_relay() {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create temp directory: {}", e
            )));
            return;
        }
    };

    let src_base = src_remote_base.trim_end_matches('/');
    let src_base_slash = format!("{}/", src_base);
    let src_root_name = Path::new(src_base).file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let dst_base = dst_remote_base.trim_end_matches('/');

    // Build destination remote paths and ensure remote dirs
    let mut transfers: Vec<(String, String, PathBuf)> = Vec::new(); // (src_remote, dst_remote, local_temp)
    let mut dst_remote_dirs: HashSet<String> = HashSet::new();
    dst_remote_dirs.insert(dst_base.to_string());

    for remote_file in &remote_files {
        let rel = remote_file
            .strip_prefix(&src_base_slash)
            .unwrap_or(remote_file);

        let dst_rel = match transfer_mode {
            TransferMode::FoldersAndFiles => {
                if src_root_name.is_empty() { rel.to_string() }
                else { format!("{}/{}", src_root_name, rel) }
            }
            TransferMode::FilesOnly => {
                Path::new(rel)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| rel.to_string())
            }
        };

        let dst_remote = format!("{}/{}", dst_base, dst_rel);
        let dst_remote = if strip_spaces {
            dst_remote.split('/').map(|c| c.replace(' ', "")).collect::<Vec<_>>().join("/")
        } else {
            dst_remote
        };

        if let Some(parent) = Path::new(&dst_remote).parent() {
            dst_remote_dirs.insert(parent.to_string_lossy().to_string());
        }

        // Local temp path preserves structure for staging
        let local_temp = temp_dir.join(rel);
        transfers.push((remote_file.clone(), dst_remote, local_temp));
    }

    // Create all destination remote directories
    let dirs_arg: Vec<String> = dst_remote_dirs.iter().map(|d| shell_quote(d)).collect();
    let mkdir_result = Command::new("ssh")
        .args(&ctl)
        .arg(dst_host)
        .arg(format!("mkdir -p {}", dirs_arg.join(" ")))
        .output();
    if let Ok(o) = &mkdir_result {
        if !o.status.success() {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create remote directories on destination: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )));
            let _ = fs::remove_dir_all(&temp_dir);
            return;
        }
    }

    // If not overwriting, get existing files on destination
    let existing: HashSet<String> = if conflict_mode != ConflictMode::Overwrite {
        let out = Command::new("ssh")
            .args(&ctl)
            .arg(dst_host)
            .arg(format!("find {} -type f 2>/dev/null", shell_quote(dst_base)))
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
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, (src_remote, dst_remote, local_temp)) in transfers.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        // Handle conflict if destination exists
        let dst_remote = if conflict_mode != ConflictMode::Overwrite && existing.contains(dst_remote) {
            match conflict_mode {
                ConflictMode::Skip => {
                    skipped.push(format!("{}: already exists at destination", src_remote));
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total: total_transfers,
                        file: src_remote.clone(),
                    });
                    continue;
                }
                ConflictMode::Rename => {
                    std::borrow::Cow::Owned(find_unique_remote_path_from_set(dst_remote, &existing))
                }
                ConflictMode::Overwrite => unreachable!(),
            }
        } else {
            std::borrow::Cow::Borrowed(dst_remote.as_str())
        };

        // Create local temp parent dir
        if let Some(parent) = local_temp.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                errors.push(format!("{}: temp dir error: {}", src_remote, e));
                continue;
            }
        }

        // Step 1: Download from source to local temp
        let dl_result = Command::new("scp")
            .args(&ctl)
            .arg("-q")
            .arg(format!("{}:{}", src_host, src_remote))
            .arg(local_temp)
            .status();
        if !matches!(dl_result, Ok(s) if s.success()) {
            errors.push(format!("{}: download from source failed", src_remote));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total: total_transfers,
                file: src_remote.clone(),
            });
            continue;
        }

        // Verify download
        match verify_remote_hash(local_temp, src_host, &ctl, src_remote) {
            Ok(true) => {}
            Ok(false) => {
                let _ = fs::remove_file(local_temp);
                errors.push(format!(
                    "{}: download integrity check failed — hash mismatch",
                    src_remote
                ));
                let _ = tx.send(WorkerMsg::Progress {
                    done: i + 1,
                    total: total_transfers,
                    file: src_remote.clone(),
                });
                continue;
            }
            Err(e) => {
                let _ = fs::remove_file(local_temp);
                errors.push(format!(
                    "{}: download verification error: {}",
                    src_remote, e
                ));
                let _ = tx.send(WorkerMsg::Progress {
                    done: i + 1,
                    total: total_transfers,
                    file: src_remote.clone(),
                });
                continue;
            }
        }

        // Step 2: Upload from local temp to destination
        let ul_result = Command::new("scp")
            .args(&ctl)
            .arg("-q")
            .arg(local_temp)
            .arg(format!("{}:{}", dst_host, dst_remote))
            .status();
        if !matches!(ul_result, Ok(s) if s.success()) {
            let _ = fs::remove_file(local_temp);
            errors.push(format!("{}: upload to destination failed", src_remote));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total: total_transfers,
                file: src_remote.clone(),
            });
            continue;
        }

        // Verify upload
        match verify_remote_hash(local_temp, dst_host, &ctl, &dst_remote) {
            Ok(true) => {
                copied += 1;
                // Clean up local temp
                let _ = fs::remove_file(local_temp);
                if do_move {
                    let rm_result = Command::new("ssh")
                        .args(&ctl)
                        .arg(src_host)
                        .arg(format!("rm -f {}", shell_quote(src_remote)))
                        .status();
                    if !matches!(rm_result, Ok(s) if s.success()) {
                        errors.push(format!(
                            "{}: transferred and verified but failed to delete from source",
                            src_remote
                        ));
                    }
                }
            }
            Ok(false) => {
                let _ = fs::remove_file(local_temp);
                // Remove corrupt destination copy
                let _ = Command::new("ssh")
                    .args(&ctl)
                    .arg(dst_host)
                    .arg(format!("rm -f {}", shell_quote(&dst_remote)))
                    .status();
                errors.push(format!(
                    "{}: upload integrity check failed — hash mismatch (source retained, dest copy removed)",
                    src_remote
                ));
            }
            Err(e) => {
                let _ = fs::remove_file(local_temp);
                if do_move {
                    errors.push(format!(
                        "{}: uploaded but verification failed: {} (source retained)",
                        src_remote, e
                    ));
                } else {
                    errors.push(format!(
                        "{}: uploaded but could not verify: {}",
                        src_remote, e
                    ));
                }
            }
        }

        let _ = tx.send(WorkerMsg::Progress {
            done: i + 1,
            total: total_transfers,
            file: src_remote.clone(),
        });
    }

    // Clean up temp directory
    let _ = fs::remove_dir_all(&temp_dir);

    let _ = tx.send(WorkerMsg::Finished {
        copied,
        skipped,
        excluded_files,
        excluded_dirs,
        errors,
    });
}

// ── Worker thread (remote source → remote destination via rsync) ───────

fn run_remote_to_remote_rsync_worker(
    src_host: &str,
    src_remote_base: &str,
    dst_host: &str,
    dst_remote_base: &str,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
) {
    let ctl = [
        "-o", "ControlMaster=auto",
        "-o", "ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r",
        "-o", "ControlPersist=60",
    ];
    let ssh_cmd = "ssh -o ControlMaster=auto -o ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r -o ControlPersist=60";

    // Connectivity check to both hosts
    for host in [src_host, dst_host] {
        let check = Command::new("ssh")
            .args(&ctl)
            .args([host, "echo ok"])
            .output();
        match check {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let _ = tx.send(WorkerMsg::Error(format!(
                    "SSH connection to '{}' failed: {}",
                    host,
                    String::from_utf8_lossy(&o.stderr).trim()
                )));
                return;
            }
            Err(e) => {
                let _ = tx.send(WorkerMsg::Error(format!("Could not run ssh: {}", e)));
                return;
            }
        }
    }

    // Check rsync availability
    match Command::new("rsync").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            let _ = tx.send(WorkerMsg::Error(
                "rsync is not installed or not found in PATH".to_string(),
            ));
            return;
        }
    }

    // List remote source files
    let (remote_files, excluded_files, excluded_dirs) = match collect_remote_files(src_host, &ctl, src_remote_base, patterns) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(e));
            return;
        }
    };

    let total = remote_files.len();
    if total == 0 {
        let _ = tx.send(WorkerMsg::Finished {
            copied: 0,
            skipped: vec![],
            excluded_files,
            excluded_dirs,
            errors: vec![],
        });
        return;
    }

    let temp_dir = match tempdir_for_relay() {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create temp directory: {}", e
            )));
            return;
        }
    };

    let src_base = src_remote_base.trim_end_matches('/');
    let src_base_slash = format!("{}/", src_base);
    let src_root_name = Path::new(src_base).file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let dst_base = dst_remote_base.trim_end_matches('/');

    let mut transfers: Vec<(String, String, PathBuf)> = Vec::new();
    let mut dst_remote_dirs: HashSet<String> = HashSet::new();
    dst_remote_dirs.insert(dst_base.to_string());

    for remote_file in &remote_files {
        let rel = remote_file
            .strip_prefix(&src_base_slash)
            .unwrap_or(remote_file);

        let dst_rel = match transfer_mode {
            TransferMode::FoldersAndFiles => {
                if src_root_name.is_empty() { rel.to_string() }
                else { format!("{}/{}", src_root_name, rel) }
            }
            TransferMode::FilesOnly => {
                Path::new(rel)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| rel.to_string())
            }
        };

        let dst_remote = format!("{}/{}", dst_base, dst_rel);
        let dst_remote = if strip_spaces {
            dst_remote.split('/').map(|c| c.replace(' ', "")).collect::<Vec<_>>().join("/")
        } else {
            dst_remote
        };

        if let Some(parent) = Path::new(&dst_remote).parent() {
            dst_remote_dirs.insert(parent.to_string_lossy().to_string());
        }

        let local_temp = temp_dir.join(rel);
        transfers.push((remote_file.clone(), dst_remote, local_temp));
    }

    // Create destination remote directories
    let dirs_arg: Vec<String> = dst_remote_dirs.iter().map(|d| shell_quote(d)).collect();
    let mkdir_result = Command::new("ssh")
        .args(&ctl)
        .arg(dst_host)
        .arg(format!("mkdir -p {}", dirs_arg.join(" ")))
        .output();
    if let Ok(o) = &mkdir_result {
        if !o.status.success() {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Failed to create remote directories on destination: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )));
            let _ = fs::remove_dir_all(&temp_dir);
            return;
        }
    }

    let existing: HashSet<String> = if conflict_mode != ConflictMode::Overwrite {
        let out = Command::new("ssh")
            .args(&ctl)
            .arg(dst_host)
            .arg(format!("find {} -type f 2>/dev/null", shell_quote(dst_base)))
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
    let mut skipped: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for (i, (src_remote, dst_remote, local_temp)) in transfers.iter().enumerate() {
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        let dst_remote = if conflict_mode != ConflictMode::Overwrite && existing.contains(dst_remote) {
            match conflict_mode {
                ConflictMode::Skip => {
                    skipped.push(format!("{}: already exists at destination", src_remote));
                    let _ = tx.send(WorkerMsg::Progress {
                        done: i + 1,
                        total: total_transfers,
                        file: src_remote.clone(),
                    });
                    continue;
                }
                ConflictMode::Rename => {
                    std::borrow::Cow::Owned(find_unique_remote_path_from_set(dst_remote, &existing))
                }
                ConflictMode::Overwrite => unreachable!(),
            }
        } else {
            std::borrow::Cow::Borrowed(dst_remote.as_str())
        };

        if let Some(parent) = local_temp.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                errors.push(format!("{}: temp dir error: {}", src_remote, e));
                continue;
            }
        }

        // Download from source via rsync
        let dl_result = Command::new("rsync")
            .args(["-az", "--checksum"])
            .arg("-e")
            .arg(ssh_cmd)
            .arg(format!("{}:{}", src_host, src_remote))
            .arg(local_temp)
            .status();
        if !matches!(dl_result, Ok(s) if s.success()) {
            errors.push(format!("{}: rsync download from source failed", src_remote));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total: total_transfers,
                file: src_remote.clone(),
            });
            continue;
        }

        // Verify download
        match verify_remote_hash(local_temp, src_host, &ctl, src_remote) {
            Ok(true) => {}
            Ok(false) => {
                let _ = fs::remove_file(local_temp);
                errors.push(format!(
                    "{}: download integrity check failed — hash mismatch",
                    src_remote
                ));
                let _ = tx.send(WorkerMsg::Progress {
                    done: i + 1,
                    total: total_transfers,
                    file: src_remote.clone(),
                });
                continue;
            }
            Err(e) => {
                let _ = fs::remove_file(local_temp);
                errors.push(format!(
                    "{}: download verification error: {}",
                    src_remote, e
                ));
                let _ = tx.send(WorkerMsg::Progress {
                    done: i + 1,
                    total: total_transfers,
                    file: src_remote.clone(),
                });
                continue;
            }
        }

        // Upload to destination via rsync
        let ul_result = Command::new("rsync")
            .args(["-az", "--checksum"])
            .arg("-e")
            .arg(ssh_cmd)
            .arg(local_temp)
            .arg(format!("{}:{}", dst_host, dst_remote))
            .status();
        if !matches!(ul_result, Ok(s) if s.success()) {
            let _ = fs::remove_file(local_temp);
            errors.push(format!("{}: rsync upload to destination failed", src_remote));
            let _ = tx.send(WorkerMsg::Progress {
                done: i + 1,
                total: total_transfers,
                file: src_remote.clone(),
            });
            continue;
        }

        // Verify upload
        match verify_remote_hash(local_temp, dst_host, &ctl, &dst_remote) {
            Ok(true) => {
                copied += 1;
                let _ = fs::remove_file(local_temp);
                if do_move {
                    let rm_result = Command::new("ssh")
                        .args(&ctl)
                        .arg(src_host)
                        .arg(format!("rm -f {}", shell_quote(src_remote)))
                        .status();
                    if !matches!(rm_result, Ok(s) if s.success()) {
                        errors.push(format!(
                            "{}: transferred and verified but failed to delete from source",
                            src_remote
                        ));
                    }
                }
            }
            Ok(false) => {
                let _ = fs::remove_file(local_temp);
                let _ = Command::new("ssh")
                    .args(&ctl)
                    .arg(dst_host)
                    .arg(format!("rm -f {}", shell_quote(&dst_remote)))
                    .status();
                errors.push(format!(
                    "{}: upload integrity check failed — hash mismatch (source retained, dest copy removed)",
                    src_remote
                ));
            }
            Err(e) => {
                let _ = fs::remove_file(local_temp);
                if do_move {
                    errors.push(format!(
                        "{}: uploaded but verification failed: {} (source retained)",
                        src_remote, e
                    ));
                } else {
                    errors.push(format!(
                        "{}: uploaded but could not verify: {}",
                        src_remote, e
                    ));
                }
            }
        }

        let _ = tx.send(WorkerMsg::Progress {
            done: i + 1,
            total: total_transfers,
            file: src_remote.clone(),
        });
    }

    let _ = fs::remove_dir_all(&temp_dir);

    let _ = tx.send(WorkerMsg::Finished {
        copied,
        skipped,
        excluded_files,
        excluded_dirs,
        errors,
    });
}

/// Create a temporary directory for relay transfers.
fn tempdir_for_relay() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("kosmokopy_relay_{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// ── SHA-256 hashing for remote transfer verification ───────────────────

/// Compute SHA-256 hash of a local file, returned as a lowercase hex string.
fn compute_sha256_local(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute SHA-256 hash of a remote file via SSH.
/// Tries sha256sum first, then falls back to shasum -a 256.
fn compute_sha256_remote(host: &str, ctl: &[&str], remote_path: &str) -> Result<String, String> {
    let cmd = format!(
        "sha256sum {} 2>/dev/null || shasum -a 256 {} 2>/dev/null",
        shell_quote(remote_path),
        shell_quote(remote_path)
    );
    let output = Command::new("ssh")
        .args(ctl)
        .arg(host)
        .arg(&cmd)
        .output()
        .map_err(|e| format!("Failed to run SSH for hash verification: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Remote hash command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Both sha256sum and shasum output: <hash>  <filename>
    let hash = stdout
        .trim()
        .split_whitespace()
        .next()
        .ok_or_else(|| "Could not parse remote hash output".to_string())?;

    Ok(hash.to_lowercase().to_string())
}

/// Verify a local file against a remote file by comparing SHA-256 hashes.
fn verify_remote_hash(
    local: &Path,
    host: &str,
    ctl: &[&str],
    remote: &str,
) -> Result<bool, String> {
    let local_hash =
        compute_sha256_local(local).map_err(|e| format!("local hash error: {}", e))?;
    let remote_hash = compute_sha256_remote(host, ctl, remote)?;
    Ok(local_hash == remote_hash)
}

// ── Worker thread (remote via rsync) ───────────────────────────────────

fn run_remote_rsync_worker(
    source: SourceSelection,
    host: &str,
    remote_base: &str,
    do_move: bool,
    conflict_mode: ConflictMode,
    strip_spaces: bool,
    transfer_mode: TransferMode,
    patterns: &[String],
    cancel_flag: Arc<AtomicBool>,
    tx: mpsc::Sender<WorkerMsg>,
) {
    // SSH options — reused for direct ssh calls and passed to rsync via -e
    let ctl = [
        "-o", "ControlMaster=auto",
        "-o", "ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r",
        "-o", "ControlPersist=60",
    ];
    let ssh_cmd = "ssh -o ControlMaster=auto -o ControlPath=/tmp/kosmokopy_ssh_%h_%p_%r -o ControlPersist=60";

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
                "SSH connection to '{}' failed: {}",
                host,
                msg.trim()
            )));
            return;
        }
        Err(e) => {
            let _ = tx.send(WorkerMsg::Error(format!(
                "Could not run ssh command: {}",
                e
            )));
            return;
        }
    }

    // Check that rsync is available locally
    match Command::new("rsync").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            let _ = tx.send(WorkerMsg::Error(
                "rsync is not installed or not found in PATH".to_string(),
            ));
            return;
        }
    }

    // Collect files locally
    let (files, excluded_files, excluded_dirs) = match collect_files(&source, patterns) {
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
            excluded_files,
            excluded_dirs,
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
                Ok(rel) => {
                    let root = sd.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    if root.is_empty() { rel.to_string_lossy().to_string() }
                    else { format!("{}/{}", root, rel.to_string_lossy()) }
                }
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
        let remote_file = if strip_spaces {
            remote_file
                .split('/')
                .map(|c| c.replace(' ', ""))
                .collect::<Vec<_>>()
                .join("/")
        } else {
            remote_file
        };
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
                "Failed to create remote directories: {}",
                msg.trim()
            )));
            return;
        }
    }

    // If not overwriting, get list of existing remote files in one SSH call
    let existing: HashSet<String> = if conflict_mode != ConflictMode::Overwrite {
        let out = Command::new("ssh")
            .args(&ctl)
            .arg(host)
            .arg(format!(
                "find {} -type f 2>/dev/null",
                shell_quote(remote_base)
            ))
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
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMsg::Cancelled {
                copied,
                skipped,
                excluded_files,
                excluded_dirs,
                errors,
            });
            return;
        }
        // Handle conflict if file exists remotely
        let remote = if conflict_mode != ConflictMode::Overwrite && existing.contains(remote) {
            match conflict_mode {
                ConflictMode::Skip => {
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
                ConflictMode::Rename => {
                    std::borrow::Cow::Owned(find_unique_remote_path_from_set(remote, &existing))
                }
                ConflictMode::Overwrite => unreachable!(),
            }
        } else {
            std::borrow::Cow::Borrowed(remote.as_str())
        };

        // Transfer via rsync with checksum verification
        let rsync_result = Command::new("rsync")
            .args(["-az", "--checksum"])
            .arg("-e")
            .arg(ssh_cmd)
            .arg(local)
            .arg(format!("{}:{}", host, remote))
            .status();

        match rsync_result {
            Ok(s) if s.success() => {
                // rsync --checksum already verifies integrity during transfer,
                // but we perform an additional SHA-256 comparison to be safe,
                // especially before deleting source files in move mode.
                match verify_remote_hash(local, host, &ctl, &remote) {
                    Ok(true) => {
                        copied += 1;
                        if do_move {
                            if let Err(e) = fs::remove_file(local) {
                                errors.push(format!(
                                    "{}: transferred and verified but failed to delete local: {}",
                                    local.display(),
                                    e
                                ));
                            }
                        }
                    }
                    Ok(false) => {
                        // Hash mismatch — remove corrupt remote copy, keep source
                        let _ = Command::new("ssh")
                            .args(&ctl)
                            .arg(host)
                            .arg(format!("rm -f {}", shell_quote(&remote)))
                            .status();
                        errors.push(format!(
                            "{}: integrity check failed — hash mismatch (original retained, remote copy removed)",
                            local.display()
                        ));
                    }
                    Err(e) => {
                        // Cannot verify — keep both, report error
                        if do_move {
                            errors.push(format!(
                                "{}: transferred but verification failed: {} (original retained)",
                                local.display(),
                                e
                            ));
                        } else {
                            errors.push(format!(
                                "{}: transferred but could not verify: {}",
                                local.display(),
                                e
                            ));
                        }
                    }
                }
            }
            Ok(s) => {
                errors.push(format!(
                    "{}: rsync failed (exit code {})",
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
        excluded_files,
        excluded_dirs,
        errors,
    });
}
