use serde_json::{json, Value};

use crate::{
    error::{AppError, AppResult},
    mcp::{infer_osv_ecosystem, parse_osv_package_from_args, query_osv_malware},
};

use super::{required_string_arg, string_arg, string_list_arg};

pub(super) async fn osv_check_tool(payload: &Value) -> AppResult<String> {
    let resolved = resolve_osv_query(payload)?;
    if resolved.skipped {
        return Ok(serde_json::to_string_pretty(&json!({
            "ok": true,
            "skipped": true,
            "reason": resolved.reason,
            "command": resolved.command,
            "args": resolved.args,
        }))?);
    }
    let malware = query_osv_malware(
        &resolved.package,
        &resolved.ecosystem,
        resolved.version.as_deref(),
    )
    .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "ok": true,
        "blocked": !malware.is_empty(),
        "malwareOnly": true,
        "package": resolved.package,
        "ecosystem": resolved.ecosystem,
        "version": resolved.version,
        "command": resolved.command,
        "args": resolved.args,
        "malwareCount": malware.len(),
        "malware": malware,
    }))?)
}

#[derive(Debug, Clone)]
struct OsvQuery {
    package: String,
    ecosystem: String,
    version: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    skipped: bool,
    reason: Option<String>,
}

fn resolve_osv_query(payload: &Value) -> AppResult<OsvQuery> {
    if let Some(package) = string_arg(payload, &["package", "name"]) {
        let ecosystem = required_string_arg(payload, &["ecosystem"], "osv_check")?;
        return Ok(OsvQuery {
            package,
            ecosystem: normalize_osv_ecosystem(&ecosystem)?,
            version: string_arg(payload, &["version"]),
            command: None,
            args: Vec::new(),
            skipped: false,
            reason: None,
        });
    }

    let command = required_string_arg(payload, &["command"], "osv_check")?;
    let args = string_list_arg(payload, &["args", "arguments"]);
    let Some(ecosystem) = infer_osv_ecosystem(&command) else {
        return Ok(OsvQuery {
            package: String::new(),
            ecosystem: String::new(),
            version: None,
            command: Some(command),
            args,
            skipped: true,
            reason: Some("command is not npx/uvx/pipx; OSV package inference skipped".into()),
        });
    };
    let Some((package, inferred_version)) = parse_osv_package_from_args(&args, ecosystem) else {
        return Err(AppError::BadRequest(
            "osv_check could not infer package from command args".into(),
        ));
    };
    Ok(OsvQuery {
        package,
        ecosystem: ecosystem.into(),
        version: string_arg(payload, &["version"]).or(inferred_version),
        command: Some(command),
        args,
        skipped: false,
        reason: None,
    })
}

fn normalize_osv_ecosystem(value: &str) -> AppResult<String> {
    let normalized = value.trim();
    if normalized.eq_ignore_ascii_case("pypi") {
        return Ok("PyPI".into());
    }
    if normalized.eq_ignore_ascii_case("npm") {
        return Ok("npm".into());
    }
    if normalized.is_empty() {
        return Err(AppError::BadRequest(
            "osv_check ecosystem cannot be empty".into(),
        ));
    }
    Ok(normalized.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_direct_osv_query() {
        let query = resolve_osv_query(&json!({
            "package": "left-pad",
            "ecosystem": "npm",
            "version": "1.3.0"
        }))
        .unwrap();

        assert_eq!(query.package, "left-pad");
        assert_eq!(query.ecosystem, "npm");
        assert_eq!(query.version.as_deref(), Some("1.3.0"));
    }

    #[test]
    fn resolves_npx_osv_query() {
        let query = resolve_osv_query(&json!({
            "command": "npx",
            "args": ["@scope/pkg@2.0.0", "--flag"]
        }))
        .unwrap();

        assert_eq!(query.package, "@scope/pkg");
        assert_eq!(query.ecosystem, "npm");
        assert_eq!(query.version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn resolves_uvx_osv_query() {
        let query = resolve_osv_query(&json!({
            "command": "uvx",
            "args": ["demo_pkg[extra]==0.1.0"]
        }))
        .unwrap();

        assert_eq!(query.package, "demo_pkg");
        assert_eq!(query.ecosystem, "PyPI");
        assert_eq!(query.version.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn skips_non_package_command() {
        let query = resolve_osv_query(&json!({
            "command": "python",
            "args": ["script.py"]
        }))
        .unwrap();

        assert!(query.skipped);
    }
}
