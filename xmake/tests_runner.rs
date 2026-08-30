use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let project_dir = find_project_dir().unwrap_or_else(|| {
        eprintln!("failed to find rtsyn-ui Cargo.toml");
        std::process::exit(1);
    });

    let status = Command::new("cargo")
        .arg("test")
        .arg("--workspace")
        .current_dir(project_dir)
        .status()
        .unwrap_or_else(|error| {
            eprintln!("failed to run cargo test: {error}");
            std::process::exit(1);
        });

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn find_project_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    if let Some(found) = cwd.as_deref().and_then(find_from) {
        return Some(found);
    }

    let exe = std::env::current_exe().ok();
    exe.as_deref().and_then(Path::parent).and_then(find_from)
}

fn find_from(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if candidate.join("Cargo.toml").is_file()
            && candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == "rtsyn-ui")
        {
            return Some(candidate.to_path_buf());
        }
    }
    None
}
