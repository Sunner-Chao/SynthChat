use std::path::{Path, PathBuf};

use crate::{error::AppResult, models::AgentDefinition};

pub(super) fn workspace_root(agent: &AgentDefinition) -> AppResult<PathBuf> {
    let root = if agent.workspace_dir.trim().is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(agent.workspace_dir.trim())
    };
    Ok(root.canonicalize()?)
}

pub(super) fn resolve_workspace_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    Ok(candidate.canonicalize()?)
}

pub(super) fn resolve_workspace_target_path(root: &Path, input: &str) -> AppResult<PathBuf> {
    let candidate = {
        let path = PathBuf::from(input);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    if candidate.exists() {
        return Ok(candidate.canonicalize()?);
    }
    let mut existing_ancestor = candidate.as_path();
    while !existing_ancestor.exists() {
        if let Some(parent) = existing_ancestor.parent() {
            existing_ancestor = parent;
        } else {
            return Ok(candidate);
        }
    }
    let ancestor_canonical = existing_ancestor.canonicalize()?;
    let relative_target = candidate
        .strip_prefix(existing_ancestor)
        .unwrap_or_else(|_| Path::new(""));
    Ok(ancestor_canonical.join(relative_target))
}

pub(super) fn should_skip_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | ".venv" | "__pycache__"
    )
}

pub(super) fn likely_binary(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "webp"
            | "tiff"
            | "tif"
            | "ico"
            // Keep PDFs out of automatic text inlining; read_file handles
            // best-effort PDF text extraction through its document path.
            | "pdf"
            | "mp4"
            | "mov"
            | "avi"
            | "mkv"
            | "webm"
            | "wmv"
            | "flv"
            | "m4v"
            | "mpeg"
            | "mpg"
            | "mp3"
            | "wav"
            | "ogg"
            | "flac"
            | "aac"
            | "m4a"
            | "wma"
            | "aiff"
            | "opus"
            | "zip"
            | "tar"
            | "gz"
            | "bz2"
            | "7z"
            | "rar"
            | "xz"
            | "z"
            | "tgz"
            | "iso"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "pdb"
            | "rlib"
            | "rmeta"
            | "bin"
            | "o"
            | "a"
            | "obj"
            | "lib"
            | "app"
            | "msi"
            | "deb"
            | "rpm"
            | "doc"
            | "docx"
            | "xls"
            | "xlsx"
            | "ppt"
            | "pptx"
            | "odt"
            | "ods"
            | "odp"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "eot"
            | "pyc"
            | "pyo"
            | "class"
            | "jar"
            | "war"
            | "ear"
            | "node"
            | "wasm"
            | "sqlite"
            | "sqlite3"
            | "db"
            | "mdb"
            | "idx"
            | "psd"
            | "ai"
            | "eps"
            | "sketch"
            | "fig"
            | "xd"
            | "blend"
            | "3ds"
            | "max"
            | "swf"
            | "fla"
            | "lockb"
            | "dat"
            | "data"
    )
}
