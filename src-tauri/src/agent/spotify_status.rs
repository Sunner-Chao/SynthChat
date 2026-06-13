use std::env;

use serde_json::{json, Value};

use crate::{error::AppResult, store::AppStore};

use super::{spotify_hermes_auth_state, spotify_settings};

const SPOTIFY_TOOLS: &[&str] = &[
    "spotify_playback",
    "spotify_devices",
    "spotify_queue",
    "spotify_search",
    "spotify_playlists",
    "spotify_albums",
    "spotify_library",
];

pub(super) fn spotify_status_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    if !matches!(
        action.as_str(),
        "status" | "manifest" | "tools" | "auth" | "diagnostics"
    ) {
        return Ok(serde_json::to_string_pretty(&json!({
            "schema": "hermes_spotify_plugin_desktop_v1",
            "status": "unsupported_action",
            "supportedActions": ["status", "manifest", "tools", "auth", "diagnostics"],
        }))?);
    }

    Ok(serde_json::to_string_pretty(&spotify_status_snapshot(
        store, &action,
    ))?)
}

pub(super) fn spotify_status_snapshot(store: &AppStore, action: &str) -> Value {
    let config = store.config().ok();
    let spotify_config = config
        .as_ref()
        .map(|config| config.spotify.clone())
        .unwrap_or_else(|| json!({}));
    let readiness = spotify_readiness(&spotify_config);
    let settings = spotify_settings(&spotify_config).ok();
    let api_base_url = settings
        .as_ref()
        .map(|settings| settings.api_base_url.clone())
        .unwrap_or_else(|| {
            spotify_string_config(
                &spotify_config,
                &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
            )
            .or_else(|| env::var("SPOTIFY_API_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.spotify.com/v1".into())
            .trim_end_matches('/')
            .to_string()
        });
    let token_url = settings
        .as_ref()
        .map(|settings| settings.token_url.clone())
        .unwrap_or_else(|| {
            spotify_string_config(&spotify_config, &["tokenUrl", "token_url"])
                .or_else(|| env::var("SPOTIFY_TOKEN_URL").ok())
                .unwrap_or_else(|| "https://accounts.spotify.com/api/token".into())
        });

    json!({
        "schema": "hermes_spotify_plugin_desktop_v1",
        "status": "ok",
        "action": action,
        "manifest": {
            "name": "spotify",
            "version": "1.0.0",
            "kind": "backend",
            "author": "NousResearch",
            "description": "Native Spotify integration - 7 tools using Spotify Web API + PKCE OAuth.",
            "manifestPath": "plugins/spotify/plugin.yaml",
            "modulePath": "plugins/spotify/__init__.py",
            "clientPath": "plugins/spotify/client.py",
            "toolsPath": "plugins/spotify/tools.py",
            "autoLoaded": true,
            "pluginsEnabledConfigRequired": false
        },
        "hermesReference": {
            "toolset": "spotify",
            "toolCount": SPOTIFY_TOOLS.len(),
            "tools": SPOTIFY_TOOLS,
            "registration": "plugins/spotify/register(ctx) registers all seven tools with toolset='spotify', check_fn=_check_spotify_available, and per-tool emoji metadata.",
            "authCommand": "hermes auth spotify",
            "authStatePath": "~/.hermes/auth.json providers.spotify",
            "runtimeCredentialResolver": "hermes_cli.auth.resolve_spotify_runtime_credentials",
            "handlerGate": "_check_spotify_available returns get_auth_status('spotify').logged_in",
            "toolsRemainRegisteredWhenLoggedOut": true,
            "pkceOAuth": true,
            "refreshOn401": true
        },
        "synthChatNativeAdaptation": {
            "registeredInternalTools": SPOTIFY_TOOLS,
            "toolset": "spotify",
            "nativeModule": "agent/integrations.rs",
            "toolRegistryPrompt": true,
            "riskPolicy": {
                "readActions": [
                    "spotify_playback get_state/get_currently_playing/recently_played",
                    "spotify_devices list",
                    "spotify_queue get",
                    "spotify_search",
                    "spotify_playlists list/get",
                    "spotify_albums get/tracks",
                    "spotify_library list"
                ],
                "mutatingActionsRequireApproval": [
                    "playback control",
                    "device transfer",
                    "queue add",
                    "playlist create/add_items/remove_items/update_details",
                    "library save/remove"
                ]
            },
            "authSources": [
                "settings.spotify.accessToken",
                "SPOTIFY_ACCESS_TOKEN",
                "settings.spotify.refreshToken + clientId (+ optional clientSecret)",
                "SPOTIFY_REFRESH_TOKEN + SPOTIFY_CLIENT_ID (+ optional SPOTIFY_CLIENT_SECRET)",
                "Hermes auth.json providers.spotify access_token or refresh_token + client_id"
            ],
            "accessTokenReady": readiness.access_token_ready,
            "refreshCredentialsReady": readiness.refresh_credentials_ready,
            "runtimeReady": readiness.runtime_ready,
            "apiBaseUrl": api_base_url.clone(),
            "tokenUrl": token_url.clone(),
            "timeoutSeconds": settings.as_ref().map(|settings| settings.timeout_seconds),
            "credentialShape": readiness.credential_shape
        },
        "liveE2eDiagnostics": spotify_live_e2e_diagnostics(&readiness, &api_base_url, &token_url),
        "boundary": {
            "networkProbePerformed": false,
            "tokenRefreshPerformed": false,
            "authJsonImportedAutomatically": readiness.auth_json_ready,
            "liveSpotifyE2eReady": readiness.runtime_ready,
            "live_spotify_e2e_ready": readiness.runtime_ready,
            "boundary": "SynthChat exposes the same seven agent-facing Spotify tools natively and gates runtime dispatch on configured Spotify credentials, including Hermes auth.json providers.spotify. This status tool does not run Hermes' Python plugin loader, print private auth token values, perform PKCE login, refresh tokens, or call the Spotify Web API."
        }
    })
}

#[derive(Debug)]
struct SpotifyReadiness {
    access_token_ready: bool,
    refresh_credentials_ready: bool,
    runtime_ready: bool,
    auth_json_ready: bool,
    credential_shape: Value,
}

fn spotify_readiness(config: &Value) -> SpotifyReadiness {
    let auth_state = spotify_hermes_auth_state();
    let access_token_configured =
        spotify_string_config(config, &["accessToken", "access_token", "token"]).is_some();
    let access_token_env = env::var("SPOTIFY_ACCESS_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    let access_token_auth = auth_state
        .as_ref()
        .and_then(|state| spotify_string_config(state, &["access_token", "accessToken"]))
        .is_some();
    let refresh_token_configured =
        spotify_string_config(config, &["refreshToken", "refresh_token"]).is_some();
    let refresh_token_env = env::var("SPOTIFY_REFRESH_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    let refresh_token_auth = auth_state
        .as_ref()
        .and_then(|state| spotify_string_config(state, &["refresh_token", "refreshToken"]))
        .is_some();
    let client_id_configured = spotify_string_config(config, &["clientId", "client_id"]).is_some();
    let client_id_env = env::var("SPOTIFY_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    let client_id_auth = auth_state
        .as_ref()
        .and_then(|state| spotify_string_config(state, &["client_id", "clientId"]))
        .is_some();
    let client_secret_configured =
        spotify_string_config(config, &["clientSecret", "client_secret"]).is_some();
    let client_secret_env = env::var("SPOTIFY_CLIENT_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    let access_token_ready = access_token_configured || access_token_env || access_token_auth;
    let refresh_credentials_ready =
        (refresh_token_configured || refresh_token_env || refresh_token_auth)
            && (client_id_configured || client_id_env || client_id_auth);
    let auth_json_ready = access_token_auth || (refresh_token_auth && client_id_auth);
    SpotifyReadiness {
        access_token_ready,
        refresh_credentials_ready,
        runtime_ready: access_token_ready || refresh_credentials_ready,
        auth_json_ready,
        credential_shape: json!({
            "config": {
                "accessToken": access_token_configured,
                "refreshToken": refresh_token_configured,
                "clientId": client_id_configured,
                "clientSecret": client_secret_configured
            },
            "env": {
                "SPOTIFY_ACCESS_TOKEN": access_token_env,
                "SPOTIFY_REFRESH_TOKEN": refresh_token_env,
                "SPOTIFY_CLIENT_ID": client_id_env,
                "SPOTIFY_CLIENT_SECRET": client_secret_env,
                "SPOTIFY_API_BASE_URL": env::var("SPOTIFY_API_BASE_URL").ok().filter(|value| !value.trim().is_empty()).is_some(),
                "SPOTIFY_TOKEN_URL": env::var("SPOTIFY_TOKEN_URL").ok().filter(|value| !value.trim().is_empty()).is_some()
            },
            "hermesAuthJson": {
                "providersSpotify": auth_state.is_some(),
                "accessToken": access_token_auth,
                "refreshToken": refresh_token_auth,
                "clientId": client_id_auth
            }
        }),
    }
}

fn spotify_live_e2e_diagnostics(
    readiness: &SpotifyReadiness,
    api_base_url: &str,
    token_url: &str,
) -> Value {
    let mut missing = Vec::new();
    if !readiness.access_token_ready && !readiness.refresh_credentials_ready {
        missing.push("access_token_or_refresh_credentials");
    }
    if !readiness.refresh_credentials_ready {
        missing.push("refresh_token_and_client_id");
    }
    json!({
        "schema": "hermes_spotify_live_e2e_diagnostics_v1",
        "runtimeReady": readiness.runtime_ready,
        "runtime_ready": readiness.runtime_ready,
        "accessTokenSmokeReady": readiness.access_token_ready || readiness.refresh_credentials_ready,
        "access_token_smoke_ready": readiness.access_token_ready || readiness.refresh_credentials_ready,
        "refreshGrantReady": readiness.refresh_credentials_ready,
        "refresh_grant_ready": readiness.refresh_credentials_ready,
        "missing": missing,
        "safeSmokeTests": [
            {
                "name": "current_user_profile",
                "method": "GET",
                "url": format!("{api_base_url}/me"),
                "requires": "valid access token"
            },
            {
                "name": "devices",
                "method": "GET",
                "url": format!("{api_base_url}/me/player/devices"),
                "requires": "valid access token"
            }
        ],
        "refreshPlan": if readiness.refresh_credentials_ready {
            json!({
                "method": "POST",
                "url": token_url,
                "grant_type": "refresh_token",
                "clientSecretRequired": false,
                "client_secret_required": false,
                "secretsRedacted": true,
                "secrets_redacted": true
            })
        } else {
            Value::Null
        },
        "networkProbePerformed": false,
        "tokenRefreshPerformed": false,
        "boundary": "This diagnostic reports whether a live Spotify E2E smoke or refresh grant can be attempted, without making network requests or exposing secrets."
    })
}

fn spotify_string_config(config: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| config.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}
