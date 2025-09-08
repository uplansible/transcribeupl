use crate::util::now_stamp_ymdhms;
use chrono::{Datelike, Local};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum ArchiveResult {
    Success(PathBuf),
    Error(String),
}

pub fn archive_file(src_path: &Path) -> ArchiveResult {
    let fname = match src_path.file_name().and_then(|s| s.to_str()) {
        Some(s) => s.to_string(),
        None => return ArchiveResult::Error("Invalid source filename".into()),
    };

    // Compute destination dir: ./archive/YYYY/MM
    let today = Local::now().date_naive();
    let dest_dir = Path::new("./archive")
        .join(format!("{}", today.year()))
        .join(format!("{:02}", today.month()));

    if let Err(e) = fs::create_dir_all(&dest_dir) {
        return ArchiveResult::Error(format!("Create archive directory failed: {e}"));
    }

    // Insert timestamp suffix before extension.
    let (stem, ext) = match fname.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{}", e)),
        None => (fname.clone(), String::new()),
    };
    let stamped = format!("{}_{}{}", stem, now_stamp_ymdhms(), ext);
    let dest_path = dest_dir.join(stamped);

    // Try rename, fallback to copy+remove if needed.
    match fs::rename(src_path, &dest_path) {
        Ok(_) => {
            log::info!("Archived via rename to {}", dest_path.display());
            ArchiveResult::Success(dest_path)
        }
        Err(_e) => match fs::copy(src_path, &dest_path) {
            Ok(_) => match fs::remove_file(src_path) {
                Ok(_) => {
                    log::info!("Archived via copy+delete to {}", dest_path.display());
                    ArchiveResult::Success(dest_path)
                }
                Err(e) => ArchiveResult::Error(format!("Delete original failed: {e}")),
            },
            Err(e) => ArchiveResult::Error(format!("Copy failed: {e}")),
        },
    }
}

pub fn is_same_path(a: &Path, b: &Path) -> bool {
    // Best effort; realpath resolution would be better.
    a == b
}

pub fn archive_and_exit(src_path: &Path) -> ArchiveResult {
    archive_file(src_path)
}
