use std::path::PathBuf;
use std::process::Command;

pub fn has_rt_capabilities() -> bool {
    #[cfg(unix)]
    unsafe {
        let policy = libc::sched_getscheduler(0);
        policy == libc::SCHED_FIFO || policy == libc::SCHED_RR
    }
    #[cfg(not(unix))]
    false
}

pub fn zenity_available() -> bool {
    Command::new("zenity")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn kdialog_available() -> bool {
    Command::new("kdialog")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn kdialog_file_dialog(filter: Option<&str>) -> Option<PathBuf> {
    let mut cmd = Command::new("kdialog");
    cmd.arg("--getopenfilename").arg(".");
    if let Some(filter) = filter {
        cmd.arg(filter);
    }
    cmd.output().ok().and_then(|output| {
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&output.stdout);
        let path = path.trim();
        (!path.is_empty()).then(|| PathBuf::from(path))
    })
}

pub fn save_file_dialog(
    label: &str,
    extensions: &[&str],
    filename: Option<&str>,
) -> Option<PathBuf> {
    let glob = extensions
        .iter()
        .map(|extension| format!("*.{extension}"))
        .collect::<Vec<_>>()
        .join(" ");
    if has_rt_capabilities() && zenity_available() {
        return zenity_file_dialog_with_name(
            "save",
            Some(&format!("{label} | {glob}")),
            filename,
        );
    }
    if kdialog_available() {
        let mut command = Command::new("kdialog");
        command
            .arg("--getsavefilename")
            .arg(filename.filter(|value| !value.is_empty()).unwrap_or("."))
            .arg(format!("{glob}|{label}"));
        return command.output().ok().and_then(|output| {
            if !output.status.success() {
                return None;
            }
            let path = String::from_utf8_lossy(&output.stdout);
            let path = path.trim();
            (!path.is_empty()).then(|| PathBuf::from(path))
        });
    }

    let mut dialog = rfd::FileDialog::new().add_filter(label, extensions);
    if let Some(filename) = filename.filter(|value| !value.is_empty()) {
        dialog = dialog.set_file_name(filename);
    }
    dialog.save_file()
}

pub fn zenity_file_dialog(mode: &str, filter: Option<&str>) -> Option<PathBuf> {
    zenity_file_dialog_with_name(mode, filter, None)
}

pub fn zenity_folder_dialog_multi() -> Option<Vec<PathBuf>> {
    let mut cmd = Command::new("zenity");
    cmd.arg("--file-selection")
        .arg("--directory")
        .arg("--multiple")
        .arg("--separator=\n");
    cmd.output().ok().and_then(|output| {
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let folders: Vec<PathBuf> = stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect();
        if folders.is_empty() {
            None
        } else {
            Some(folders)
        }
    })
}

pub fn zenity_file_dialog_with_name(
    mode: &str,
    filter: Option<&str>,
    filename: Option<&str>,
) -> Option<PathBuf> {
    let mut cmd = Command::new("zenity");
    cmd.arg("--file-selection");

    match mode {
        "save" => {
            cmd.arg("--save");
        }
        "folder" => {
            cmd.arg("--directory");
        }
        _ => {} // open file is default
    }

    if let Some(f) = filter {
        cmd.arg("--file-filter").arg(f);
    }

    if let Some(name) = filename {
        cmd.arg("--filename").arg(name);
    }

    cmd.output().ok().and_then(|output| {
        if output.status.success() {
            let path_string = String::from_utf8_lossy(&output.stdout);
            let path_str = path_string.trim();
            if !path_str.is_empty() {
                Some(PathBuf::from(path_str))
            } else {
                None
            }
        } else {
            None
        }
    })
}

pub fn spawn_file_dialog_thread<F, T>(f: F) -> std::thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}
