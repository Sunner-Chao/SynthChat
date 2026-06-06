use std::path::{Path, PathBuf};

pub(super) const ACP_EDIT_APPROVAL_ASK: &str = "ask";
pub(super) const ACP_EDIT_APPROVAL_WORKSPACE_SESSION: &str = "workspace_session";
pub(super) const ACP_EDIT_APPROVAL_SESSION: &str = "session";

const SENSITIVE_AUTO_APPROVE_NAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    "id_rsa",
    "id_ed25519",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcpEditProposal {
    pub tool_name: String,
    pub path: PathBuf,
    pub old_text: Option<String>,
    pub new_text: String,
}

pub(super) fn acp_should_auto_approve_edit(
    proposal: &AcpEditProposal,
    policy: &str,
    cwd: Option<&Path>,
) -> bool {
    let policy = acp_normalize_edit_approval_policy(policy);
    if policy.is_empty()
        || policy.as_str() == ACP_EDIT_APPROVAL_ASK
        || acp_is_sensitive_auto_approve_path(&proposal.path)
    {
        return false;
    }

    let path = acp_resolve_logical_path(&proposal.path, cwd);
    if policy.as_str() == ACP_EDIT_APPROVAL_SESSION {
        return true;
    }
    if policy.as_str() != ACP_EDIT_APPROVAL_WORKSPACE_SESSION {
        return false;
    }

    let tmp_root = acp_resolve_logical_path(&std::env::temp_dir(), None);
    if path.starts_with(&tmp_root) {
        return true;
    }
    cwd.map(|root| {
        let root = acp_resolve_logical_path(root, None);
        path.starts_with(root)
    })
    .unwrap_or(false)
}

pub(super) fn acp_normalize_edit_approval_policy(policy: &str) -> String {
    match policy.trim() {
        "accept_edits" | "auto" | "workspace_session" => ACP_EDIT_APPROVAL_WORKSPACE_SESSION.into(),
        "dont_ask" | "never" | "bypass" | "session" => ACP_EDIT_APPROVAL_SESSION.into(),
        "ask" | "default" | "" => ACP_EDIT_APPROVAL_ASK.into(),
        other => other.to_string(),
    }
}

pub(super) fn acp_is_sensitive_auto_approve_path(path: &Path) -> bool {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if SENSITIVE_AUTO_APPROVE_NAMES
        .iter()
        .any(|name| file_name == *name)
    {
        return true;
    }
    path.components().any(|component| {
        let text = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        text == ".git" || text == ".ssh"
    })
}

fn acp_resolve_logical_path(path: &Path, cwd: Option<&Path>) -> PathBuf {
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(cwd) = cwd {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    if let Ok(canonical) = base.canonicalize() {
        return canonical;
    }
    if let Some(parent) = base.parent() {
        if let Ok(canonical_parent) = parent.canonicalize() {
            if let Some(file_name) = base.file_name() {
                return canonical_parent.join(file_name);
            }
            return canonical_parent;
        }
    }
    base
}
