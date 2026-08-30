use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeModuleBuild {
    pub module_root: PathBuf,
    pub shared_library: PathBuf,
}

pub fn build_runtime_module(path: &str) -> Result<RuntimeModuleBuild> {
    let module_root = runtime_module_root(Path::new(path))?;
    let shared_library = match find_runtime_module_library(&module_root) {
        Some(shared_library) => shared_library,
        None => {
            run_xmake(&module_root, &["f", "-y", "--tests=n"])?;
            run_xmake(&module_root, &["-y"])?;
            find_runtime_module_library(&module_root).ok_or_else(|| {
                Error::Parse(format!(
                    "no shared library found under {}",
                    module_root.display()
                ))
            })?
        }
    };
    Ok(RuntimeModuleBuild {
        module_root,
        shared_library,
    })
}

pub fn rebuild_runtime_module(path: &str) -> Result<RuntimeModuleBuild> {
    let module_root = runtime_module_root(Path::new(path))?;
    run_xmake(&module_root, &["f", "-y", "--tests=n"])?;
    run_xmake(&module_root, &["build", "-y"])?;
    let shared_library = find_runtime_module_library(&module_root).ok_or_else(|| {
        Error::Parse(format!(
            "no shared library found under {}",
            module_root.display()
        ))
    })?;
    Ok(RuntimeModuleBuild {
        module_root,
        shared_library,
    })
}

pub fn runtime_module_root(path: &Path) -> Result<PathBuf> {
    let path = path
        .canonicalize()
        .map_err(|error| Error::Parse(format!("{}: {error}", path.display())))?;
    if path.is_file() {
        if path.file_name().and_then(|name| name.to_str()) == Some("xmake.lua") {
            return path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| Error::Parse("xmake.lua has no parent directory".to_string()));
        }
        return find_xmake_root(path.parent())
            .ok_or_else(|| Error::Parse(format!("no xmake.lua found above {}", path.display())));
    }
    if path.join("xmake.lua").is_file() {
        return Ok(path);
    }
    if let Some(root) = find_xmake_root(Some(&path)) {
        return Ok(root);
    }
    Err(Error::Parse(format!(
        "{} does not contain xmake.lua",
        path.display()
    )))
}

fn find_xmake_root(start: Option<&Path>) -> Option<PathBuf> {
    let mut current = start;
    for _ in 0..8 {
        let dir = current?;
        if dir.join("xmake.lua").is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn run_xmake(module_root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("xmake")
        .args(args)
        .current_dir(module_root)
        .output()
        .map_err(|error| {
            Error::Api(format!(
                "failed to run xmake in {}: {error}",
                module_root.display()
            ))
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    Err(Error::Api(if details.is_empty() {
        format!("xmake failed with status {}", output.status)
    } else {
        details.to_string()
    }))
}

fn find_runtime_module_library(module_root: &Path) -> Option<PathBuf> {
    let project_hint = module_root.file_name().and_then(|name| name.to_str());
    let mut libraries = Vec::new();
    collect_shared_libraries(&module_root.join("build"), 8, &mut libraries);
    libraries.sort_by(|left, right| {
        let left_score = runtime_library_score(left, project_hint);
        let right_score = runtime_library_score(right, project_hint);
        right_score.cmp(&left_score).then_with(|| left.cmp(right))
    });

    libraries.into_iter().next()
}

fn runtime_library_score(path: &Path, project_hint: Option<&str>) -> usize {
    let mut score = 0;
    let path_text = path.to_string_lossy();
    if path_text.contains("/release/") {
        score += 4;
    }
    if path_text.contains("/debug/") {
        score += 1;
    }
    if let Some(project_hint) = project_hint {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.contains(project_hint))
            .unwrap_or(false)
        {
            score += 8;
        }
    }
    score
}

fn collect_shared_libraries(root: &Path, depth: usize, libraries: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("so" | "dylib" | "dll")
            )
        {
            libraries.push(path);
        } else if path.is_dir() {
            collect_shared_libraries(&path, depth - 1, libraries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_runtime_module_library, runtime_module_root};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rtsyn-module-{name}-{nanos}"))
    }

    #[test]
    fn runtime_module_root_accepts_project_directory() {
        let root = unique_temp_dir("project-dir");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("xmake.lua"), "").unwrap();

        assert_eq!(
            runtime_module_root(&root).unwrap(),
            root.canonicalize().unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_module_root_accepts_xmake_file() {
        let root = unique_temp_dir("xmake-file");
        fs::create_dir_all(&root).unwrap();
        let xmake = root.join("xmake.lua");
        fs::write(&xmake, "").unwrap();

        assert_eq!(
            runtime_module_root(&xmake).unwrap(),
            root.canonicalize().unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_module_library_searches_build_tree_without_fixed_target_triple() {
        let root = unique_temp_dir("library-search").join("rtsyn-example");
        let release_dir = root.join("build").join("custom-platform").join("custom-arch").join("release");
        fs::create_dir_all(&release_dir).unwrap();
        let library = release_dir.join("librtsyn-example.so");
        fs::write(&library, "").unwrap();

        assert_eq!(find_runtime_module_library(&root), Some(library));

        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
