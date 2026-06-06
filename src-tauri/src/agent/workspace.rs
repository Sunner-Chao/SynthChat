use std::path::{Path, PathBuf};

use crate::{
    error::{AppError, AppResult},
    models::AgentDefinition,
};

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
    let canonical = candidate.canonicalize()?;
    if canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(AppError::BadRequest(format!(
            "path is outside workspace: {}",
            candidate.display()
        )))
    }
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
        return resolve_workspace_path(root, input);
    }
    let mut existing_ancestor = candidate.as_path();
    while !existing_ancestor.exists() {
        existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
            AppError::BadRequest(format!("path has no existing ancestor: {input}"))
        })?;
    }
    let ancestor_canonical = existing_ancestor.canonicalize()?;
    if !ancestor_canonical.starts_with(root) {
        return Err(AppError::BadRequest(format!(
            "path is outside workspace: {}",
            candidate.display()
        )));
    }
    Ok(candidate)
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
            // Hermes allows PDFs through a separate document path. SynthChat's
            // read_file is UTF-8 text only, so keep PDFs out of text tools here.
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
