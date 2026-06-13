use std::{
    collections::HashMap,
    env,
    fmt::Write as _,
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    store::AppStore,
};

const DEFAULT_NOUS_PORTAL_URL: &str = "https://portal.nousresearch.com";
const DASHBOARD_SCOPE: &str = "agent_dashboard:access";
const DASHBOARD_CONTRACT_VERSION: u8 = 1;
const JWKS_CACHE_SECONDS: u64 = 300;
const JWKS_FETCH_TIMEOUT_SECONDS: u64 = 10;
pub(super) const DASHBOARD_WS_TICKET_TTL_SECONDS: u64 = 30;
const DASHBOARD_REFRESH_TOKEN_MAX_AGE_SECONDS: u64 = 30 * 24 * 60 * 60;
const SESSION_AT_COOKIE: &str = "hermes_session_at";
const SESSION_RT_COOKIE: &str = "hermes_session_rt";
const PKCE_COOKIE: &str = "hermes_session_pkce";
const COOKIE_NAME_VARIANTS: [&str; 3] = ["__Host-", "__Secure-", ""];
const DASHBOARD_PUBLIC_API_PATHS: [&str; 5] = [
    "/api/status",
    "/api/config/defaults",
    "/api/config/schema",
    "/api/model/info",
    "/api/dashboard/plugins",
];
const DASHBOARD_GATE_PUBLIC_PREFIXES: [&str; 9] = [
    "/auth/login",
    "/auth/callback",
    "/auth/logout",
    "/login",
    "/api/auth/providers",
    "/assets/",
    "/favicon.ico",
    "/ds-assets/",
    "/fonts/",
];

#[derive(Clone, Debug)]
struct DashboardWsTicket {
    expires_at_ms: i64,
    info: Value,
}

#[derive(Clone, Debug)]
struct DashboardJwtParts {
    header: Value,
    claims: Value,
    signing_input: String,
    signature: Vec<u8>,
}

#[derive(Clone, Debug)]
struct DashboardJwksCacheEntry {
    url: String,
    expires_at_ms: i64,
    jwks: Value,
}

#[derive(Clone, Debug)]
struct DashboardTokenExchange {
    access_token: String,
    refresh_token: String,
}

#[derive(Clone, Debug)]
enum DashboardTokenExchangeError {
    InvalidCode(String),
    Provider(String),
}

static DASHBOARD_WS_TICKETS: OnceLock<Mutex<HashMap<String, DashboardWsTicket>>> = OnceLock::new();
static DASHBOARD_LIVE_JWKS_CACHE: OnceLock<Mutex<Option<DashboardJwksCacheEntry>>> =
    OnceLock::new();

pub(super) fn dashboard_auth_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let provider = payload
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("nous")
        .trim()
        .to_ascii_lowercase();
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if provider != "nous" {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_dashboard_auth_desktop_v1",
            "provider": provider,
            "status": "unsupported_provider",
            "supportedProviders": ["nous"],
        }))?);
    }
    if !matches!(
        action.as_str(),
        "status" | "contract" | "diagnostics" | "show"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_dashboard_auth_desktop_v1",
            "provider": "nous",
            "status": "unsupported_action",
            "supportedActions": ["status", "contract", "diagnostics"],
        }))?);
    }
    Ok(serde_json::to_string_pretty(
        &nous_dashboard_auth_snapshot(store),
    )?)
}

pub(super) fn nous_dashboard_auth_snapshot(store: &AppStore) -> Value {
    let config_path = hermes_config_yaml_path(store);
    let config = config_path
        .as_ref()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| parse_dashboard_oauth_config(&text));
    let env_client_id = non_empty_env("HERMES_DASHBOARD_OAUTH_CLIENT_ID");
    let env_portal_url = non_empty_env("HERMES_DASHBOARD_PORTAL_URL");
    let config_client_id = config
        .as_ref()
        .and_then(|value| value.get("client_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let config_portal_url = config
        .as_ref()
        .and_then(|value| value.get("portal_url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty());
    let client_id = env_client_id
        .clone()
        .or(config_client_id.clone())
        .unwrap_or_default();
    let portal_url = env_portal_url
        .clone()
        .or(config_portal_url.clone())
        .unwrap_or_else(|| DEFAULT_NOUS_PORTAL_URL.into());
    let configured = !client_id.is_empty() && client_id.starts_with("agent:");
    let status = if configured {
        "configured"
    } else if client_id.is_empty() {
        "not_configured"
    } else {
        "misconfigured"
    };
    let skip_reason = if client_id.is_empty() {
        Some("HERMES_DASHBOARD_OAUTH_CLIENT_ID is not set and dashboard.oauth.client_id in Hermes config.yaml was not found or is empty.")
    } else if !client_id.starts_with("agent:") {
        Some("dashboard OAuth client_id must match Hermes contract shape 'agent:{instance_id}'.")
    } else {
        None
    };
    let agent_instance_id = client_id.strip_prefix("agent:").unwrap_or("").to_string();
    json!({
        "schema": "hermes_dashboard_auth_desktop_v1",
        "plugin": "dashboard_auth/nous",
        "provider": {
            "name": "nous",
            "displayName": "Nous Research",
            "registered": configured,
            "status": status,
            "skipReason": skip_reason,
        },
        "configuration": {
            "clientIdConfigured": !client_id.is_empty(),
            "clientIdShapeValid": client_id.starts_with("agent:"),
            "clientIdSource": if env_client_id.is_some() { "env:HERMES_DASHBOARD_OAUTH_CLIENT_ID" } else if config_client_id.is_some() { "config.yaml:dashboard.oauth.client_id" } else { "unset" },
            "clientIdPreview": redact_client_id(&client_id),
            "agentInstanceIdPreview": redact_agent_instance_id(&agent_instance_id),
            "portalUrl": portal_url,
            "portalUrlSource": if env_portal_url.is_some() { "env:HERMES_DASHBOARD_PORTAL_URL" } else if config_portal_url.is_some() { "config.yaml:dashboard.oauth.portal_url" } else { "default" },
            "configYamlPath": config_path.map(|path| path.to_string_lossy().to_string()),
            "configYamlDetected": config.is_some(),
        },
        "contract": {
            "version": DASHBOARD_CONTRACT_VERSION,
            "scope": DASHBOARD_SCOPE,
            "oauthFlow": "authorization_code_pkce_s256",
            "clientIdShape": "agent:{instance_id}",
            "audience": "bare client_id",
            "issuer": portal_url,
            "authorizeEndpoint": format!("{}/oauth/authorize", portal_url.trim_end_matches('/')),
            "tokenEndpoint": format!("{}/api/oauth/token", portal_url.trim_end_matches('/')),
            "jwksEndpoint": format!("{}/.well-known/jwks.json", portal_url.trim_end_matches('/')),
            "jwksCacheSeconds": JWKS_CACHE_SECONDS,
            "jwtAlgorithms": ["RS256"],
            "requiredClaims": ["exp", "iat", "aud", "iss", "sub"],
            "optionalClaims": ["agent_instance_id", "org_id", "oauth_contract_version"],
            "nativeClaimValidation": {
                "supported": true,
                "signatureVerifiedWhenJwksConfigured": true,
                "jwksSources": ["env:HERMES_DASHBOARD_JWKS_JSON", "env:HERMES_DASHBOARD_JWKS_PATH", "live:<portal_url>/.well-known/jwks.json"],
                "externalJwksFetchBoundary": false,
                "behavior": "parses dashboard access-token cookies, validates Hermes required claims, expiry, issuer, audience, optional agent_instance_id, oauth_contract_version, and verifies RS256 signatures from env/path JWKS first, then live Portal JWKS with a 5-minute cache unless HERMES_DASHBOARD_JWKS_FETCH disables it.",
            },
            "contractVersionClaim": {
                "missing": "warn_and_proceed",
                "mismatch": "refuse",
            },
            "refreshTokens": {
                "issuedInV1": false,
                "refreshSessionBehavior": "RefreshExpiredError / redirect to login",
                "revokeBehavior": "client_side_cookie_clear_noop",
            },
            "cookieGate": dashboard_auth_gate_contract_summary(),
            "redirectUriRules": {
                "schemes": ["https", "http"],
                "httpHosts": ["localhost", "127.0.0.1"],
                "pathSuffix": "/auth/callback",
            },
        },
        "desktopAdaptation": {
            "runtime": "SynthChat desktop",
            "nativeWebGate": true,
            "nativeWebGateMode": "contract_and_api_envelope",
            "nativeWsTickets": true,
            "wsTicketRoute": "/api/auth/ws-ticket",
            "wsTicketTtlSeconds": DASHBOARD_WS_TICKET_TTL_SECONDS,
            "boundary": "Hermes dashboard auth provider contract, cookie naming rules, public-path allowlist, safe next-target rules, and unauthenticated API envelope are exposed natively; the Python DashboardAuthProvider verifier and full FastAPI middleware stack are not embedded in the Tauri desktop runtime.",
            "relatedAuthSurface": "Nous LLM/device-code OAuth remains handled by /auth login nous and the credential pool; this dashboard auth contract is separate from LLM provider auth.",
        }
    })
}

pub(super) fn dashboard_auth_gate_contract_summary() -> Value {
    let prefixed = dashboard_auth_normalise_prefix(Some("/hermes/"));
    json!({
        "schema": "hermes_dashboard_auth_gate_desktop_v1",
        "publicApiPaths": DASHBOARD_PUBLIC_API_PATHS,
        "publicPrefixes": DASHBOARD_GATE_PUBLIC_PREFIXES,
        "cookieNames": {
            "accessToken": SESSION_AT_COOKIE,
            "refreshToken": SESSION_RT_COOKIE,
            "pkce": PKCE_COOKIE,
            "variants": COOKIE_NAME_VARIANTS,
            "examples": {
                "loopbackHttp": dashboard_auth_cookie_name(SESSION_AT_COOKIE, false, ""),
                "httpsRootPath": dashboard_auth_cookie_name(SESSION_AT_COOKIE, true, ""),
                "httpsForwardedPrefix": dashboard_auth_cookie_name(SESSION_AT_COOKIE, true, &prefixed),
            },
        },
        "cookiePrefixRules": {
            "http": "bare",
            "httpsPathRoot": "__Host-",
            "httpsWithForwardedPrefix": "__Secure-",
            "sameSite": "lax",
            "httpOnly": true,
            "secureOnlyWhenHttps": true,
            "path": "X-Forwarded-Prefix when valid, otherwise /",
            "pathExamples": {
                "direct": dashboard_auth_cookie_path(""),
                "forwardedPrefix": dashboard_auth_cookie_path(&prefixed),
            },
        },
        "publicPathExamples": {
            "apiStatus": dashboard_auth_path_is_public("/api/status"),
            "apiStatusExtension": dashboard_auth_path_is_public("/api/status/secret-extension"),
            "asset": dashboard_auth_path_is_public("/assets/app.js"),
        },
        "unauthenticatedBehavior": {
            "api": {
                "status": 401,
                "errorValues": ["unauthenticated", "session_expired"],
                "fields": ["error", "detail", "reason", "login_url"],
            },
            "html": {
                "status": 302,
                "location": "/login?next={same-origin-relative-target}",
            },
        },
        "safeNextRules": {
            "allow": "same-origin relative non-auth non-api paths",
            "drop": ["absolute URLs", "protocol-relative URLs", "/login", "/auth/*", "/api/*"],
        },
        "examples": {
            "apiExpiredSession": dashboard_auth_unauth_response_contract("/api/plugins/kanban/board", "", Some("/hermes"), "invalid_or_expired_session"),
            "htmlNoCookie": dashboard_auth_unauth_response_contract("/sessions", "page=2", Some("/hermes"), "no_cookie"),
        },
        "bootstrapRoutes": {
            "loginPage": {"method": "GET", "path": "/login", "contentType": "text/html", "native": true},
            "providers": {"method": "GET", "path": "/api/auth/providers", "native": true, "public": true},
            "authLogin": {"method": "GET", "path": "/auth/login?provider=nous", "native": true, "redirect": true, "setsPkceCookie": true},
            "authCallback": {"method": "GET", "path": "/auth/callback", "native": true, "tokenExchange": true, "setsSessionCookies": true},
            "authLogout": {"method": "POST", "path": "/auth/logout", "native": true, "redirect": true, "clearsCookies": true},
            "authMe": {"method": "GET", "path": "/api/auth/me", "native": true, "authRequired": true},
        },
    })
}

pub(super) fn dashboard_auth_normalise_prefix(raw: Option<&str>) -> String {
    let Some(raw) = raw else {
        return String::new();
    };
    let mut prefix = raw.trim().to_string();
    if prefix.is_empty() {
        return String::new();
    }
    if !prefix.starts_with('/') {
        prefix.insert(0, '/');
    }
    while prefix.ends_with('/') {
        prefix.pop();
    }
    if prefix.is_empty()
        || prefix.len() > 64
        || prefix.contains("//")
        || prefix.contains("..")
        || prefix
            .chars()
            .any(|ch| matches!(ch, '"' | '\'' | '<' | '>' | ' ' | '\n' | '\r' | '\t'))
    {
        return String::new();
    }
    prefix
}

pub(super) fn dashboard_auth_cookie_name(bare_name: &str, use_https: bool, prefix: &str) -> String {
    if !use_https {
        bare_name.to_string()
    } else if prefix.is_empty() {
        format!("__Host-{bare_name}")
    } else {
        format!("__Secure-{bare_name}")
    }
}

pub(super) fn dashboard_auth_cookie_path(prefix: &str) -> &str {
    if prefix.is_empty() {
        "/"
    } else {
        prefix
    }
}

pub(super) fn dashboard_auth_path_is_public(path: &str) -> bool {
    DASHBOARD_PUBLIC_API_PATHS.contains(&path)
        || DASHBOARD_GATE_PUBLIC_PREFIXES
            .iter()
            .any(|prefix| path == *prefix || path.starts_with(prefix))
}

pub(super) fn dashboard_auth_unauth_response_contract(
    path: &str,
    query: &str,
    forwarded_prefix: Option<&str>,
    reason: &str,
) -> Value {
    let prefix = dashboard_auth_normalise_prefix(forwarded_prefix);
    let next = dashboard_auth_safe_next_target(path, query);
    let login_url = if next.is_empty() {
        format!("{prefix}/login")
    } else {
        format!("{prefix}/login?next={next}")
    };
    if path.starts_with("/api/") {
        json!({
            "status": 401,
            "body": {
                "error": if reason == "invalid_or_expired_session" { "session_expired" } else { "unauthenticated" },
                "detail": "Unauthorized",
                "reason": reason,
                "login_url": login_url,
            }
        })
    } else {
        json!({
            "status": 302,
            "location": login_url,
        })
    }
}

pub(super) fn dashboard_auth_providers_response(store: &AppStore) -> (u16, Value) {
    let snapshot = nous_dashboard_auth_snapshot(store);
    if snapshot["provider"]["registered"]
        .as_bool()
        .unwrap_or(false)
    {
        (
            200,
            json!({
                "providers": [{
                    "name": "nous",
                    "display_name": "Nous Research",
                    "displayName": "Nous Research",
                }],
                "schema": "hermes_dashboard_auth_providers_desktop_v1",
                "nativeApiServerRoute": "/api/auth/providers",
            }),
        )
    } else {
        (
            503,
            json!({
                "detail": "no auth providers registered",
                "providers": [],
                "schema": "hermes_dashboard_auth_providers_desktop_v1",
                "nativeApiServerRoute": "/api/auth/providers",
                "skipReason": snapshot["provider"]["skipReason"].clone(),
            }),
        )
    }
}

pub(super) fn dashboard_auth_me_response(user_id: &str, provider: &str) -> Value {
    let now = chrono::Utc::now().timestamp().max(0);
    json!({
        "user_id": user_id,
        "userId": user_id,
        "email": "",
        "display_name": "",
        "displayName": "",
        "org_id": "",
        "orgId": "",
        "provider": provider,
        "expires_at": now + 3600,
        "expiresAt": now + 3600,
        "schema": "hermes_dashboard_auth_me_desktop_v1",
        "nativeApiServerRoute": "/api/auth/me",
    })
}

pub(super) fn dashboard_auth_session_from_cookie(
    store: &AppStore,
    cookie_header: &str,
) -> AppResult<Option<Value>> {
    let Some(access_token) = dashboard_auth_read_cookie(cookie_header, SESSION_AT_COOKIE) else {
        return Ok(None);
    };
    dashboard_auth_session_from_access_token(store, &access_token).map(Some)
}

pub(super) fn dashboard_auth_session_from_access_token(
    store: &AppStore,
    access_token: &str,
) -> AppResult<Value> {
    let parts = dashboard_auth_jwt_parts(access_token)?;
    let claims = parts.claims.clone();
    let snapshot = nous_dashboard_auth_snapshot(store);
    let client_id = dashboard_auth_client_id_from_snapshot(&snapshot)?;
    let portal_url = snapshot["configuration"]["portalUrl"]
        .as_str()
        .unwrap_or(DEFAULT_NOUS_PORTAL_URL)
        .trim_end_matches('/')
        .to_string();
    dashboard_auth_validate_session_claims(&claims, &client_id, &portal_url)?;
    let jwks = dashboard_auth_jwks_for_session(&portal_url)?;
    let signature_verified = if let Some((jwks, _source)) = jwks.as_ref() {
        dashboard_auth_verify_jwt_signature(&parts, &jwks)?;
        true
    } else {
        false
    };
    let user_id = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let org_id = claims
        .get("org_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let expires_at = claims
        .get("exp")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    Ok(json!({
        "user_id": user_id,
        "userId": user_id,
        "email": "",
        "display_name": "",
        "displayName": "",
        "org_id": org_id,
        "orgId": org_id,
        "provider": "nous",
        "expires_at": expires_at,
        "expiresAt": expires_at,
        "schema": "hermes_dashboard_auth_me_desktop_v1",
        "nativeApiServerRoute": "/api/auth/me",
        "claimsValidated": true,
        "signatureVerified": signature_verified,
        "externalJwksBoundary": !signature_verified,
        "verification": {
            "requiredClaims": ["exp", "iat", "aud", "iss", "sub"],
            "audience": client_id,
            "issuer": portal_url,
            "agentInstanceIdChecked": claims.get("agent_instance_id").is_some(),
            "contractVersionChecked": claims.get("oauth_contract_version").is_some(),
            "signature": if signature_verified { "rs256_jwks_verified" } else { "external_jwks_boundary" },
            "jwksSource": jwks.map(|(_, source)| source).unwrap_or(Value::Null),
        }
    }))
}

pub(super) fn dashboard_auth_login_page_html(store: &AppStore, next: &str) -> String {
    let (_status, providers) = dashboard_auth_providers_response(store);
    let safe_next = dashboard_auth_validate_post_login_target(next);
    let next_qs = if safe_next.is_empty() {
        String::new()
    } else {
        format!("&next={}", percent_encode_next_target(&safe_next))
    };
    let provider_links = providers
        .get("providers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|provider| {
                    let name = provider.get("name").and_then(Value::as_str)?;
                    let display = provider
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or(name);
                    Some(format!(
                        r#"<a class="provider" href="/auth/login?provider={}{}">Sign in with {}</a>"#,
                        html_escape(name),
                        next_qs,
                        html_escape(display)
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|html| !html.is_empty())
        .unwrap_or_else(|| {
            r#"<p class="empty">No dashboard auth providers are registered.</p>"#.into()
        });
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Hermes Dashboard Login</title>
<style>
body {{ margin:0; font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background:#101418; color:#f5f7fa; display:grid; min-height:100vh; place-items:center; }}
main {{ width:min(420px, calc(100vw - 32px)); }}
h1 {{ font-size:24px; font-weight:650; margin:0 0 18px; }}
.provider {{ display:block; padding:12px 14px; border:1px solid #3b4652; color:#f5f7fa; text-decoration:none; background:#18202a; }}
.provider + .provider {{ margin-top:10px; }}
.empty {{ color:#aab4c0; }}
</style>
</head>
<body>
<main>
<h1>Hermes Dashboard Login</h1>
{provider_links}
</main>
</body>
</html>"#
    )
}

pub(super) fn dashboard_auth_start_login_response(
    store: &AppStore,
    provider: &str,
    redirect_uri: &str,
    next: &str,
    use_https: bool,
    prefix: &str,
) -> AppResult<(u16, Value)> {
    let provider = provider.trim();
    if provider != "nous" {
        return Ok((
            404,
            json!({
                "detail": format!("Unknown provider: {provider:?}"),
                "schema": "hermes_dashboard_auth_login_desktop_v1",
            }),
        ));
    }
    let snapshot = nous_dashboard_auth_snapshot(store);
    if !snapshot["provider"]["registered"]
        .as_bool()
        .unwrap_or(false)
    {
        return Ok((
            503,
            json!({
                "detail": "Provider unreachable: Nous dashboard auth is not configured",
                "skipReason": snapshot["provider"]["skipReason"].clone(),
                "schema": "hermes_dashboard_auth_login_desktop_v1",
            }),
        ));
    }
    let client_id = dashboard_auth_client_id_from_snapshot(&snapshot)?;
    let authorize_endpoint = snapshot["contract"]["authorizeEndpoint"]
        .as_str()
        .unwrap_or("https://portal.nousresearch.com/oauth/authorize");
    let verifier = dashboard_auth_random_url_token(96);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = dashboard_auth_random_url_token(32);
    let mut redirect_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        authorize_endpoint,
        percent_encode_query_value(&client_id),
        percent_encode_query_value(redirect_uri),
        percent_encode_query_value(DASHBOARD_SCOPE),
        percent_encode_query_value(&state),
        percent_encode_query_value(&challenge),
    );
    if redirect_url.contains(' ') {
        redirect_url = redirect_url.replace(' ', "%20");
    }
    let safe_next = dashboard_auth_validate_post_login_target(next);
    let mut pkce = format!("provider={provider};state={state};verifier={verifier}");
    if !safe_next.is_empty() {
        pkce.push_str(";next=");
        pkce.push_str(&percent_encode_next_target(&safe_next));
    }
    let cookie_value = percent_encode_query_value(&pkce);
    let cookie =
        dashboard_auth_set_cookie_header(PKCE_COOKIE, &cookie_value, 600, use_https, prefix);
    Ok((
        302,
        json!({
            "schema": "hermes_dashboard_auth_login_desktop_v1",
            "provider": provider,
            "redirect_url": redirect_url,
            "redirectUrl": redirect_url,
            "status": 302,
            "setCookie": cookie,
            "pkceCookieName": dashboard_auth_cookie_name(PKCE_COOKIE, use_https, prefix),
            "pkceCookiePayloadPreview": "provider=<provider>;state=<state>;verifier=<redacted>;next=<optional>",
            "nativeApiServerRoute": "/auth/login",
        }),
    ))
}

pub(super) fn dashboard_auth_callback_response(
    store: &AppStore,
    request_path: &str,
    cookie_header: Option<&str>,
    redirect_uri: &str,
    use_https: bool,
    prefix: &str,
) -> AppResult<(u16, Value)> {
    let query = dashboard_auth_query_params(request_path);
    let provider_error = query.get("error").cloned().unwrap_or_default();
    let provider_error_description = query.get("error_description").cloned().unwrap_or_default();
    let code = query.get("code").cloned().unwrap_or_default();
    let state = query.get("state").cloned().unwrap_or_default();
    let Some(pkce_cookie) = cookie_header
        .and_then(|cookie| dashboard_auth_read_cookie(cookie, PKCE_COOKIE))
        .map(|value| percent_decode_query_value(&value))
    else {
        return Ok((
            400,
            json!({
                "detail": "Missing PKCE state cookie",
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    };
    let pkce = dashboard_auth_parse_cookie_segments(&pkce_cookie);
    let provider = pkce.get("provider").map(String::as_str).unwrap_or_default();
    let expected_state = pkce.get("state").map(String::as_str).unwrap_or_default();
    let verifier = pkce.get("verifier").map(String::as_str).unwrap_or_default();
    let next = pkce.get("next").map(String::as_str).unwrap_or_default();
    if provider != "nous" {
        return Ok((
            400,
            json!({
                "detail": format!("Unknown provider in cookie: {provider:?}"),
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    }
    if !provider_error.is_empty() {
        return Ok((
            400,
            json!({
                "detail": format!("OAuth error from provider: {provider_error} ({provider_error_description})"),
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    }
    if code.trim().is_empty() {
        return Ok((
            400,
            json!({
                "detail": "OAuth callback missing code",
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    }
    if state.is_empty() || state != expected_state {
        return Ok((
            400,
            json!({
                "detail": "OAuth state mismatch (CSRF check failed)",
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    }
    if verifier.is_empty() {
        return Ok((
            400,
            json!({
                "detail": "Missing PKCE verifier",
                "schema": "hermes_dashboard_auth_callback_desktop_v1",
                "nativeApiServerRoute": "/auth/callback",
            }),
        ));
    }

    let snapshot = nous_dashboard_auth_snapshot(store);
    let client_id = dashboard_auth_client_id_from_snapshot(&snapshot)?;
    let token_endpoint = snapshot["contract"]["tokenEndpoint"]
        .as_str()
        .unwrap_or("https://portal.nousresearch.com/api/oauth/token")
        .to_string();
    let token = match dashboard_auth_exchange_code_for_token(
        &token_endpoint,
        &code,
        redirect_uri,
        &client_id,
        verifier,
    ) {
        Ok(token) => token,
        Err(DashboardTokenExchangeError::InvalidCode(detail)) => {
            return Ok((
                400,
                json!({
                    "detail": format!("Invalid code: {detail}"),
                    "schema": "hermes_dashboard_auth_callback_desktop_v1",
                    "nativeApiServerRoute": "/auth/callback",
                }),
            ));
        }
        Err(DashboardTokenExchangeError::Provider(detail)) => {
            return Ok((
                503,
                json!({
                    "detail": format!("Provider unreachable: {detail}"),
                    "schema": "hermes_dashboard_auth_callback_desktop_v1",
                    "nativeApiServerRoute": "/auth/callback",
                }),
            ));
        }
    };
    let session = match dashboard_auth_session_from_access_token(store, &token.access_token) {
        Ok(session) => session,
        Err(error) => {
            return Ok((
                503,
                json!({
                    "detail": format!("Provider unreachable: access token verification failed: {error}"),
                    "schema": "hermes_dashboard_auth_callback_desktop_v1",
                    "nativeApiServerRoute": "/auth/callback",
                }),
            ));
        }
    };
    let expires_at = session
        .get("expires_at")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| chrono::Utc::now().timestamp() + 3600);
    let expires_in = (expires_at - chrono::Utc::now().timestamp()).max(60) as u64;
    let landing = dashboard_auth_validate_post_login_target(next);
    let location = if landing.is_empty() {
        "/".to_string()
    } else {
        landing
    };
    let mut set_cookies = Vec::new();
    set_cookies.push(dashboard_auth_set_cookie_header(
        SESSION_AT_COOKIE,
        &token.access_token,
        expires_in,
        use_https,
        prefix,
    ));
    if !token.refresh_token.is_empty() {
        set_cookies.push(dashboard_auth_set_cookie_header(
            SESSION_RT_COOKIE,
            &token.refresh_token,
            DASHBOARD_REFRESH_TOKEN_MAX_AGE_SECONDS,
            use_https,
            prefix,
        ));
    }
    set_cookies.extend(dashboard_auth_clear_pkce_cookie_headers(prefix));
    Ok((
        302,
        json!({
            "status": 302,
            "location": location,
            "setCookies": set_cookies,
            "schema": "hermes_dashboard_auth_callback_desktop_v1",
            "nativeApiServerRoute": "/auth/callback",
            "provider": "nous",
            "user_id": session["user_id"].clone(),
            "userId": session["userId"].clone(),
            "org_id": session["org_id"].clone(),
            "orgId": session["orgId"].clone(),
            "expires_at": session["expires_at"].clone(),
            "expiresAt": session["expiresAt"].clone(),
            "signatureVerified": session["signatureVerified"].clone(),
            "jwksSource": session["verification"]["jwksSource"].clone(),
        }),
    ))
}

pub(super) fn dashboard_auth_logout_response(prefix: &str) -> Value {
    json!({
        "schema": "hermes_dashboard_auth_logout_desktop_v1",
        "status": 302,
        "location": format!("{prefix}/login"),
        "clearCookies": dashboard_auth_clear_cookie_headers(prefix),
        "nativeApiServerRoute": "/auth/logout",
    })
}

fn dashboard_auth_exchange_code_for_token(
    token_endpoint: &str,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> Result<DashboardTokenExchange, DashboardTokenExchangeError> {
    let token_endpoint = token_endpoint.to_string();
    let body = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ]
    .into_iter()
    .map(|(key, value)| format!("{key}={}", percent_encode_query_value(value)))
    .collect::<Vec<_>>()
    .join("&");
    std::thread::spawn(
        move || -> Result<DashboardTokenExchange, DashboardTokenExchangeError> {
            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(JWKS_FETCH_TIMEOUT_SECONDS))
                .build()
                .map_err(|error| {
                    DashboardTokenExchangeError::Provider(format!(
                        "Portal token client failed: {error}"
                    ))
                })?;
            let response = client
                .post(&token_endpoint)
                .header("Accept", "application/json")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .map_err(|error| {
                    DashboardTokenExchangeError::Provider(format!(
                        "Portal token endpoint unreachable: {error}"
                    ))
                })?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let text = response.text().unwrap_or_default();
            if status.as_u16() == 400 {
                let error_code = dashboard_auth_json_error_code(&text, &content_type)
                    .unwrap_or_else(|| "invalid_request".into());
                return Err(DashboardTokenExchangeError::InvalidCode(format!(
                    "Portal rejected code: {error_code}"
                )));
            }
            if !status.is_success() {
                return Err(DashboardTokenExchangeError::Provider(format!(
                    "Portal token endpoint returned {status}: {:?}",
                    text.chars().take(200).collect::<String>()
                )));
            }
            let payload = if content_type.starts_with("application/json") {
                serde_json::from_str::<Value>(&text).map_err(|error| {
                    DashboardTokenExchangeError::Provider(format!(
                        "Portal token response JSON failed: {error}"
                    ))
                })?
            } else {
                Value::Null
            };
            let access_token = payload
                .get("access_token")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    DashboardTokenExchangeError::Provider(
                        "Portal token response missing access_token".into(),
                    )
                })?
                .to_string();
            let token_type = payload
                .get("token_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !token_type.is_empty() && token_type != "bearer" {
                return Err(DashboardTokenExchangeError::Provider(format!(
                    "unexpected token_type={token_type:?}"
                )));
            }
            let refresh_token = payload
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(DashboardTokenExchange {
                access_token,
                refresh_token,
            })
        },
    )
    .join()
    .map_err(|_| DashboardTokenExchangeError::Provider("token exchange thread panicked".into()))?
}

fn dashboard_auth_json_error_code(text: &str, content_type: &str) -> Option<String> {
    if !content_type.starts_with("application/json") {
        return None;
    }
    serde_json::from_str::<Value>(text).ok().and_then(|value| {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

pub(super) fn dashboard_auth_set_cookie_header(
    bare_name: &str,
    value: &str,
    max_age: u64,
    use_https: bool,
    prefix: &str,
) -> String {
    let mut header = format!(
        "{}={}; Max-Age={max_age}; Path={}; HttpOnly; SameSite=Lax",
        dashboard_auth_cookie_name(bare_name, use_https, prefix),
        value,
        dashboard_auth_cookie_path(prefix)
    );
    if use_https {
        header.push_str("; Secure");
    }
    header
}

pub(super) fn dashboard_auth_clear_cookie_headers(prefix: &str) -> Vec<String> {
    let path = dashboard_auth_cookie_path(prefix);
    let mut headers = Vec::new();
    for variant in COOKIE_NAME_VARIANTS {
        for bare in [SESSION_AT_COOKIE, SESSION_RT_COOKIE, PKCE_COOKIE] {
            headers.push(format!(
                "{variant}{bare}=; Max-Age=0; Path={path}; HttpOnly; SameSite=Lax"
            ));
        }
    }
    headers
}

pub(super) fn dashboard_auth_clear_pkce_cookie_headers(prefix: &str) -> Vec<String> {
    let path = dashboard_auth_cookie_path(prefix);
    COOKIE_NAME_VARIANTS
        .iter()
        .map(|variant| {
            format!("{variant}{PKCE_COOKIE}=; Max-Age=0; Path={path}; HttpOnly; SameSite=Lax")
        })
        .collect()
}

pub(super) fn dashboard_auth_validate_post_login_target(raw: &str) -> String {
    let decoded = percent_decode_query_value(raw);
    if decoded.is_empty()
        || !decoded.starts_with('/')
        || decoded.starts_with("//")
        || decoded == "/api"
        || decoded.starts_with("/api/")
        || decoded == "/login"
        || decoded.starts_with("/auth/")
    {
        String::new()
    } else {
        decoded
    }
}

fn dashboard_auth_read_cookie(cookie_header: &str, bare_name: &str) -> Option<String> {
    for pair in cookie_header.split(';') {
        let (name, value) = pair.trim().split_once('=').unwrap_or((pair.trim(), ""));
        for variant in COOKIE_NAME_VARIANTS {
            if name == format!("{variant}{bare_name}") {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn dashboard_auth_parse_cookie_segments(value: &str) -> HashMap<String, String> {
    let mut parts = HashMap::new();
    for segment in value.split(';') {
        let Some((key, raw_value)) = segment.trim().split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        parts.insert(key.to_string(), raw_value.trim().to_string());
    }
    parts
}

fn dashboard_auth_query_params(path: &str) -> HashMap<String, String> {
    let query = path.split_once('?').map(|(_, query)| query).unwrap_or("");
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(
            percent_decode_query_value(key),
            percent_decode_query_value(value),
        );
    }
    params
}

fn dashboard_auth_unverified_jwt_claims(access_token: &str) -> AppResult<Value> {
    dashboard_auth_jwt_parts(access_token).map(|parts| parts.claims)
}

fn dashboard_auth_jwt_parts(access_token: &str) -> AppResult<DashboardJwtParts> {
    let mut parts = access_token.split('.');
    let header = parts
        .next()
        .ok_or_else(|| AppError::BadRequest("dashboard access token missing JWT header".into()))?;
    let payload = parts
        .next()
        .ok_or_else(|| AppError::BadRequest("dashboard access token missing JWT payload".into()))?;
    let signature = parts.next().ok_or_else(|| {
        AppError::BadRequest("dashboard access token missing JWT signature".into())
    })?;
    if parts.next().is_some() || header.is_empty() || payload.is_empty() || signature.is_empty() {
        return Err(AppError::BadRequest(
            "dashboard access token must be a three-part JWT".into(),
        ));
    }
    let header_value = dashboard_auth_decode_jwt_segment(header, "header")?;
    let alg = header_value
        .get("alg")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if alg != "RS256" {
        return Err(AppError::BadRequest(format!(
            "dashboard access token alg must be RS256, got {alg:?}"
        )));
    }
    let claims = dashboard_auth_decode_jwt_segment(payload, "payload")?;
    let signature = URL_SAFE_NO_PAD
        .decode(signature.as_bytes())
        .map_err(|error| {
            AppError::BadRequest(format!(
                "invalid dashboard JWT signature base64url: {error}"
            ))
        })?;
    Ok(DashboardJwtParts {
        header: header_value,
        claims,
        signing_input: format!("{header}.{payload}"),
        signature,
    })
}

fn dashboard_auth_decode_jwt_segment(segment: &str, label: &str) -> AppResult<Value> {
    let bytes = URL_SAFE_NO_PAD
        .decode(segment.as_bytes())
        .map_err(|error| {
            AppError::BadRequest(format!("invalid dashboard JWT {label} base64url: {error}"))
        })?;
    serde_json::from_slice::<Value>(&bytes).map_err(|error| {
        AppError::BadRequest(format!("invalid dashboard JWT {label} JSON: {error}"))
    })
}

fn dashboard_auth_validate_session_claims(
    claims: &Value,
    client_id: &str,
    portal_url: &str,
) -> AppResult<()> {
    for claim in ["exp", "iat", "aud", "iss", "sub"] {
        if claims.get(claim).is_none() {
            return Err(AppError::BadRequest(format!(
                "dashboard access token missing required claim {claim:?}"
            )));
        }
    }
    let now = chrono::Utc::now().timestamp();
    let exp = claims
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::BadRequest("dashboard access token exp must be numeric".into()))?;
    if exp <= now {
        return Err(AppError::BadRequest(
            "dashboard access token expired".into(),
        ));
    }
    if claims.get("iat").and_then(Value::as_i64).is_none() {
        return Err(AppError::BadRequest(
            "dashboard access token iat must be numeric".into(),
        ));
    }
    if !dashboard_auth_audience_matches(claims.get("aud").unwrap_or(&Value::Null), client_id) {
        return Err(AppError::BadRequest(format!(
            "dashboard access token audience mismatch; expected {client_id:?}"
        )));
    }
    let iss = claims
        .get("iss")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if iss.trim_end_matches('/') != portal_url.trim_end_matches('/') {
        return Err(AppError::BadRequest(format!(
            "dashboard access token issuer mismatch; expected {portal_url:?}"
        )));
    }
    let sub = claims
        .get("sub")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sub.trim().is_empty() {
        return Err(AppError::BadRequest(
            "dashboard access token missing non-empty sub".into(),
        ));
    }
    if let Some(instance_id) = claims.get("agent_instance_id").and_then(Value::as_str) {
        let expected = client_id.strip_prefix("agent:").unwrap_or_default();
        if instance_id != expected {
            return Err(AppError::BadRequest(format!(
                "dashboard access token agent_instance_id mismatch; expected {expected:?}"
            )));
        }
    }
    if let Some(version) = claims.get("oauth_contract_version") {
        let valid = version.as_u64() == Some(DASHBOARD_CONTRACT_VERSION as u64);
        if !valid {
            return Err(AppError::BadRequest(format!(
                "unsupported dashboard oauth_contract_version={version}"
            )));
        }
    }
    Ok(())
}

fn dashboard_auth_audience_matches(aud: &Value, client_id: &str) -> bool {
    match aud {
        Value::String(value) => value == client_id,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(client_id)),
        _ => false,
    }
}

fn dashboard_auth_jwks_for_session(portal_url: &str) -> AppResult<Option<(Value, Value)>> {
    if let Some(jwks) = dashboard_auth_configured_jwks()? {
        return Ok(Some(jwks));
    }
    if !dashboard_auth_live_jwks_enabled() {
        return Ok(None);
    }
    let url = dashboard_auth_live_jwks_url(portal_url);
    let jwks = dashboard_auth_fetch_live_jwks(&url)?;
    Ok(Some((jwks, json!(format!("live:{url}")))))
}

fn dashboard_auth_configured_jwks() -> AppResult<Option<(Value, Value)>> {
    if let Some(raw) = non_empty_env("HERMES_DASHBOARD_JWKS_JSON") {
        return serde_json::from_str::<Value>(&raw)
            .map(|jwks| Some((jwks, json!("env:HERMES_DASHBOARD_JWKS_JSON"))))
            .map_err(|error| {
                AppError::BadRequest(format!(
                    "HERMES_DASHBOARD_JWKS_JSON is invalid JSON: {error}"
                ))
            });
    }
    if let Some(path) = non_empty_env("HERMES_DASHBOARD_JWKS_PATH") {
        let text = fs::read_to_string(&path).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to read HERMES_DASHBOARD_JWKS_PATH {path}: {error}"
            ))
        })?;
        return serde_json::from_str::<Value>(&text)
            .map(|jwks| Some((jwks, json!("env:HERMES_DASHBOARD_JWKS_PATH"))))
            .map_err(|error| {
                AppError::BadRequest(format!(
                    "HERMES_DASHBOARD_JWKS_PATH JSON is invalid: {error}"
                ))
            });
    }
    Ok(None)
}

fn dashboard_auth_live_jwks_enabled() -> bool {
    non_empty_env("HERMES_DASHBOARD_JWKS_FETCH")
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off" | "never"
            )
        })
        .unwrap_or(true)
}

fn dashboard_auth_live_jwks_url(portal_url: &str) -> String {
    format!("{}/.well-known/jwks.json", portal_url.trim_end_matches('/'))
}

fn dashboard_auth_fetch_live_jwks(url: &str) -> AppResult<Value> {
    let now = dashboard_auth_now_ms();
    let cache = DASHBOARD_LIVE_JWKS_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(entry) = cache.lock().unwrap().as_ref() {
        if entry.url == url && entry.expires_at_ms > now {
            return Ok(entry.jwks.clone());
        }
    }

    let url_for_thread = url.to_string();
    let jwks = std::thread::spawn(move || -> Result<Value, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(JWKS_FETCH_TIMEOUT_SECONDS))
            .build()
            .map_err(|error| format!("dashboard JWKS client failed: {error}"))?;
        let response = client
            .get(&url_for_thread)
            .send()
            .map_err(|error| format!("dashboard JWKS lookup failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("dashboard JWKS endpoint returned {status}"));
        }
        response
            .json::<Value>()
            .map_err(|error| format!("dashboard JWKS JSON failed: {error}"))
    })
    .join()
    .map_err(|_| AppError::BadRequest("dashboard JWKS fetch thread panicked".into()))?
    .map_err(AppError::BadRequest)?;
    if jwks.get("keys").and_then(Value::as_array).is_none() {
        return Err(AppError::BadRequest(
            "dashboard JWKS must contain keys array".into(),
        ));
    }
    *cache.lock().unwrap() = Some(DashboardJwksCacheEntry {
        url: url.to_string(),
        expires_at_ms: now + (JWKS_CACHE_SECONDS as i64 * 1000),
        jwks: jwks.clone(),
    });
    Ok(jwks)
}

fn dashboard_auth_now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn dashboard_auth_verify_jwt_signature(parts: &DashboardJwtParts, jwks: &Value) -> AppResult<()> {
    use rsa::{
        pkcs1v15::{Signature, VerifyingKey},
        signature::Verifier,
        BigUint, RsaPublicKey,
    };

    let kid = parts.header.get("kid").and_then(Value::as_str);
    let keys = jwks
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::BadRequest("dashboard JWKS must contain keys array".into()))?;
    let key = keys
        .iter()
        .find(|key| {
            let key_kid = key.get("kid").and_then(Value::as_str);
            kid.is_none() || key_kid == kid
        })
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "dashboard JWKS did not contain matching kid {:?}",
                kid.unwrap_or("<none>")
            ))
        })?;
    if key.get("kty").and_then(Value::as_str) != Some("RSA") {
        return Err(AppError::BadRequest(
            "dashboard JWKS key kty must be RSA".into(),
        ));
    }
    if key
        .get("alg")
        .and_then(Value::as_str)
        .map(|alg| alg != "RS256")
        .unwrap_or(false)
    {
        return Err(AppError::BadRequest(
            "dashboard JWKS key alg must be RS256 when present".into(),
        ));
    }
    let n = dashboard_auth_jwk_uint(key, "n")?;
    let e = dashboard_auth_jwk_uint(key, "e")?;
    let public_key = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
        .map_err(|error| {
            AppError::BadRequest(format!("dashboard JWKS RSA key is invalid: {error}"))
        })?;
    let signature = Signature::try_from(parts.signature.as_slice()).map_err(|error| {
        AppError::BadRequest(format!("dashboard JWT signature shape is invalid: {error}"))
    })?;
    let verifier = VerifyingKey::<Sha256>::new(public_key);
    verifier
        .verify(parts.signing_input.as_bytes(), &signature)
        .map_err(|error| {
            AppError::BadRequest(format!(
                "dashboard JWT RS256 signature verification failed: {error}"
            ))
        })
}

fn dashboard_auth_jwk_uint(key: &Value, field: &str) -> AppResult<Vec<u8>> {
    let value = key
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest(format!("dashboard JWKS RSA key missing {field}")))?;
    URL_SAFE_NO_PAD.decode(value.as_bytes()).map_err(|error| {
        AppError::BadRequest(format!(
            "dashboard JWKS RSA {field} is invalid base64url: {error}"
        ))
    })
}

fn dashboard_auth_safe_next_target(path: &str, query: &str) -> String {
    if path.is_empty()
        || !path.starts_with('/')
        || path.starts_with("//")
        || path == "/api"
        || path.starts_with("/api/")
        || path == "/login"
        || path.starts_with("/auth/")
    {
        return String::new();
    }
    let target = if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    };
    percent_encode_next_target(&target)
}

fn percent_encode_next_target(value: &str) -> String {
    percent_encode_query_value(value)
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn percent_decode_query_value(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hi = (bytes[index + 1] as char).to_digit(16);
                let lo = (bytes[index + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    output.push(((hi << 4) | lo) as u8);
                    index += 3;
                } else {
                    output.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).to_string()
}

fn dashboard_auth_random_url_token(uuid_count: usize) -> String {
    let mut bytes = Vec::new();
    while bytes.len() < uuid_count {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    URL_SAFE_NO_PAD.encode(&bytes[..uuid_count])
}

fn dashboard_auth_client_id_from_snapshot(snapshot: &Value) -> AppResult<String> {
    let preview = snapshot["configuration"]["clientIdPreview"]
        .as_str()
        .unwrap_or_default();
    if let Some(value) = non_empty_env("HERMES_DASHBOARD_OAUTH_CLIENT_ID") {
        return Ok(value);
    }
    let config_path = snapshot["configuration"]["configYamlPath"]
        .as_str()
        .map(PathBuf::from);
    if let Some(config_path) = config_path {
        if let Some(config) = fs::read_to_string(config_path)
            .ok()
            .and_then(|text| parse_dashboard_oauth_config(&text))
        {
            if let Some(client_id) = config.get("client_id").and_then(Value::as_str) {
                return Ok(client_id.to_string());
            }
        }
    }
    Err(AppError::BadRequest(format!(
        "dashboard auth client_id unavailable from snapshot preview {preview}"
    )))
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(super) fn dashboard_auth_mint_ws_ticket(user_id: &str, provider: &str) -> AppResult<Value> {
    let user_id = user_id.trim();
    let provider = provider.trim();
    if user_id.is_empty() {
        return Err(AppError::BadRequest(
            "dashboard ws-ticket requires user_id".into(),
        ));
    }
    if provider.is_empty() {
        return Err(AppError::BadRequest(
            "dashboard ws-ticket requires provider".into(),
        ));
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let ticket = format!(
        "dashws-{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let info = json!({
        "user_id": user_id,
        "userId": user_id,
        "provider": provider,
        "minted_at": now_ms / 1000,
        "mintedAtMs": now_ms,
        "schema": "hermes_dashboard_ws_ticket_desktop_v1",
    });
    let entry = DashboardWsTicket {
        expires_at_ms: now_ms + (DASHBOARD_WS_TICKET_TTL_SECONDS as i64 * 1000),
        info: info.clone(),
    };
    let mut tickets = dashboard_ws_tickets()
        .lock()
        .map_err(|_| AppError::BadRequest("dashboard ws-ticket store lock poisoned".into()))?;
    tickets.insert(ticket.clone(), entry);
    dashboard_auth_gc_ws_tickets_locked(&mut tickets, now_ms);
    Ok(json!({
        "ticket": ticket,
        "ttl_seconds": DASHBOARD_WS_TICKET_TTL_SECONDS,
        "ttlSeconds": DASHBOARD_WS_TICKET_TTL_SECONDS,
        "expires_at_ms": now_ms + (DASHBOARD_WS_TICKET_TTL_SECONDS as i64 * 1000),
        "expiresAtMs": now_ms + (DASHBOARD_WS_TICKET_TTL_SECONDS as i64 * 1000),
        "info": info,
        "schema": "hermes_dashboard_ws_ticket_desktop_v1",
        "singleUse": true,
        "nativeApiServerRoute": "/api/auth/ws-ticket",
    }))
}

pub(super) fn dashboard_auth_consume_ws_ticket(ticket: &str) -> AppResult<Value> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut tickets = dashboard_ws_tickets()
        .lock()
        .map_err(|_| AppError::BadRequest("dashboard ws-ticket store lock poisoned".into()))?;
    dashboard_auth_gc_ws_tickets_locked(&mut tickets, now_ms);
    let Some(entry) = tickets.remove(ticket.trim()) else {
        return Err(AppError::BadRequest(format!(
            "unknown dashboard ws-ticket: {}",
            truncate_ticket_for_error(ticket)
        )));
    };
    if entry.expires_at_ms < now_ms {
        return Err(AppError::BadRequest("expired dashboard ws-ticket".into()));
    }
    Ok(entry.info)
}

#[cfg(test)]
pub(super) fn dashboard_auth_reset_ws_tickets_for_tests() {
    if let Ok(mut tickets) = dashboard_ws_tickets().lock() {
        tickets.clear();
    }
}

#[cfg(test)]
pub(super) fn dashboard_auth_reset_jwks_cache_for_tests() {
    if let Some(cache) = DASHBOARD_LIVE_JWKS_CACHE.get() {
        if let Ok(mut entry) = cache.lock() {
            *entry = None;
        }
    }
}

fn dashboard_ws_tickets() -> &'static Mutex<HashMap<String, DashboardWsTicket>> {
    DASHBOARD_WS_TICKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn dashboard_auth_gc_ws_tickets_locked(
    tickets: &mut HashMap<String, DashboardWsTicket>,
    now_ms: i64,
) {
    tickets.retain(|_, entry| entry.expires_at_ms >= now_ms);
}

fn truncate_ticket_for_error(ticket: &str) -> String {
    let ticket = ticket.trim();
    if ticket.is_empty() {
        "<empty>".into()
    } else {
        format!("{}...", ticket.chars().take(8).collect::<String>())
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn hermes_config_yaml_path(store: &AppStore) -> Option<PathBuf> {
    let base = env::var_os("HERMES_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| store.data_dir().join(".hermes"));
    let path = base.join("config.yaml");
    path.exists().then_some(path)
}

fn parse_dashboard_oauth_config(text: &str) -> Option<Value> {
    let mut in_dashboard = false;
    let mut in_oauth = false;
    let mut client_id = String::new();
    let mut portal_url = String::new();
    for raw in text.lines() {
        let without_comment = raw.split_once('#').map(|(left, _)| left).unwrap_or(raw);
        let trimmed = without_comment.trim();
        if trimmed.is_empty() {
            continue;
        }
        let indent = without_comment
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .count();
        if indent == 0 {
            in_dashboard = trimmed == "dashboard:" || trimmed.starts_with("dashboard: ");
            in_oauth = false;
            continue;
        }
        if in_dashboard && indent <= 2 {
            in_oauth = trimmed == "oauth:" || trimmed.starts_with("oauth: ");
            continue;
        }
        if in_dashboard && in_oauth && indent > 2 {
            if let Some((key, value)) = trimmed.split_once(':') {
                let value = yaml_scalar(value);
                match key.trim() {
                    "client_id" => client_id = value,
                    "portal_url" => portal_url = value,
                    _ => {}
                }
            }
        }
    }
    if client_id.is_empty() && portal_url.is_empty() {
        None
    } else {
        Some(json!({
            "client_id": client_id,
            "portal_url": portal_url,
        }))
    }
}

fn yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 {
        let first = value.as_bytes()[0] as char;
        let last = value.as_bytes()[value.len() - 1] as char;
        if (first == '"' && last == '"') || (first == '\'' && last == '\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn redact_client_id(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if let Some(rest) = value.strip_prefix("agent:") {
        return format!("agent:{}", redact_agent_instance_id(rest));
    }
    if value.len() <= 8 {
        "***".into()
    } else {
        format!("{}***{}", &value[..4], &value[value.len() - 4..])
    }
}

fn redact_agent_instance_id(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.len() <= 8 {
        "***".into()
    } else {
        format!("{}***{}", &value[..4], &value[value.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_oauth_config_parser_extracts_contract_fields() {
        let parsed = parse_dashboard_oauth_config(
            r#"
dashboard:
  oauth:
    client_id: "agent:instance-123"
    portal_url: https://portal.rewbs.uk
"#,
        )
        .unwrap();
        assert_eq!(parsed["client_id"], "agent:instance-123");
        assert_eq!(parsed["portal_url"], "https://portal.rewbs.uk");
    }
}
