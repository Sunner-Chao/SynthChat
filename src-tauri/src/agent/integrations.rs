use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use lettre::{
    message::Mailbox, transport::smtp::authentication::Credentials, Message, SmtpTransport,
    Transport,
};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

use crate::{
    error::{AppError, AppResult},
    models::{new_id, now_iso},
    store::AppStore,
};

use super::{
    required_string_arg,
    shell_hooks::{run_pre_gateway_dispatch_hooks, PreGatewayDispatchDecision},
    spawn_background_chat_turn_for_job, string_arg, truncate_output,
};
pub(super) async fn weather_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let config = store.config()?.weather;
    let settings = qweather_settings(&config)?;
    let location = string_arg(
        payload,
        &["location", "city", "query", "q", "place", "address"],
    )
    .or_else(|| settings.default_location.clone())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        AppError::BadRequest(
            "weather requires payload.location or settings.weather.defaultLocation".into(),
        )
    })?;
    let lang = string_arg(payload, &["lang", "language"]).unwrap_or_else(|| "zh".into());
    let unit = string_arg(payload, &["unit", "units"]).unwrap_or_else(|| "m".into());
    let include_forecast = payload
        .get("includeForecast")
        .or_else(|| payload.get("forecast"))
        .or_else(|| payload.get("include_forecast"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let days = payload
        .get("days")
        .or_else(|| payload.get("forecastDays"))
        .or_else(|| payload.get("forecast_days"))
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .clamp(3, 30);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds.max(1)))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build weather client: {error}"))
        })?;
    let lookup_url = qweather_url(
        &settings.host,
        "/geo/v2/city/lookup",
        &[
            ("location", location.as_str()),
            ("key", settings.api_key.as_str()),
            ("lang", lang.as_str()),
        ],
    )?;
    let lookup = fetch_weather_json(&client, lookup_url.clone(), "weather location lookup").await?;
    let place = lookup
        .get("location")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "weather location lookup returned no result for {location}"
            ))
        })?
        .clone();
    let location_id = place
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("weather location lookup missing location id".into())
        })?;
    let now_url = qweather_url(
        &settings.host,
        "/v7/weather/now",
        &[
            ("location", location_id),
            ("key", settings.api_key.as_str()),
            ("lang", lang.as_str()),
            ("unit", unit.as_str()),
        ],
    )?;
    let now = fetch_weather_json(&client, now_url.clone(), "weather now").await?;
    let forecast = if include_forecast {
        let endpoint = match days {
            0..=3 => "/v7/weather/3d",
            4..=7 => "/v7/weather/7d",
            _ => "/v7/weather/30d",
        };
        let forecast_url = qweather_url(
            &settings.host,
            endpoint,
            &[
                ("location", location_id),
                ("key", settings.api_key.as_str()),
                ("lang", lang.as_str()),
                ("unit", unit.as_str()),
            ],
        )?;
        Some(fetch_weather_json(&client, forecast_url.clone(), "weather forecast").await?)
    } else {
        None
    };
    Ok(serde_json::to_string_pretty(&normalize_qweather_result(
        &location, lookup_url, now_url, place, now, forecast,
    ))?)
}

#[derive(Debug, Clone)]
pub(super) struct QWeatherSettings {
    pub(super) host: String,
    pub(super) api_key: String,
    pub(super) default_location: Option<String>,
    pub(super) timeout_seconds: u64,
}

pub(super) fn qweather_settings(config: &Value) -> AppResult<QWeatherSettings> {
    let host = config
        .get("qweatherApiHost")
        .or_else(|| config.get("qweather_api_host"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://devapi.qweather.com")
        .trim_end_matches('/')
        .to_string();
    let api_key = config
        .get("qweatherApiKey")
        .or_else(|| config.get("qweather_api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest("weather requires settings.weather.qweatherApiKey".into())
        })?
        .to_string();
    let default_location = config
        .get("defaultLocation")
        .or_else(|| config.get("default_location"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(QWeatherSettings {
        host,
        api_key,
        default_location,
        timeout_seconds,
    })
}

pub(super) fn qweather_url(
    host: &str,
    endpoint: &str,
    pairs: &[(&str, &str)],
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(host)
        .map_err(|error| AppError::BadRequest(format!("invalid QWeather API host: {error}")))?;
    let mut path = url.path().trim_end_matches('/').to_string();
    path.push('/');
    path.push_str(endpoint.trim_start_matches('/'));
    url.set_path(&path);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            if !value.trim().is_empty() {
                query.append_pair(key, value.trim());
            }
        }
    }
    Ok(url)
}

pub(super) async fn fetch_weather_json(
    client: &reqwest::Client,
    url: reqwest::Url,
    label: &str,
) -> AppResult<Value> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if value.get("code").and_then(Value::as_str) != Some("200") {
        return Err(AppError::BadRequest(format!(
            "{label} returned QWeather code {}: {}",
            value
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            truncate_output(&text, 2000)
        )));
    }
    Ok(value)
}

pub(super) fn normalize_qweather_result(
    query: &str,
    lookup_url: reqwest::Url,
    now_url: reqwest::Url,
    place: Value,
    now: Value,
    forecast: Option<Value>,
) -> Value {
    let current = now.get("now").cloned().unwrap_or_else(|| json!({}));
    let forecast_days = forecast
        .as_ref()
        .and_then(|value| value.get("daily"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    json!({
                        "date": item.get("fxDate").and_then(Value::as_str).unwrap_or_default(),
                        "textDay": item.get("textDay").and_then(Value::as_str).unwrap_or_default(),
                        "textNight": item.get("textNight").and_then(Value::as_str).unwrap_or_default(),
                        "tempMin": item.get("tempMin").and_then(Value::as_str).unwrap_or_default(),
                        "tempMax": item.get("tempMax").and_then(Value::as_str).unwrap_or_default(),
                        "windDirDay": item.get("windDirDay").and_then(Value::as_str).unwrap_or_default(),
                        "windScaleDay": item.get("windScaleDay").and_then(Value::as_str).unwrap_or_default(),
                        "humidity": item.get("humidity").and_then(Value::as_str).unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "query": query,
        "place": {
            "id": place.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name": place.get("name").and_then(Value::as_str).unwrap_or_default(),
            "adm1": place.get("adm1").and_then(Value::as_str).unwrap_or_default(),
            "adm2": place.get("adm2").and_then(Value::as_str).unwrap_or_default(),
            "country": place.get("country").and_then(Value::as_str).unwrap_or_default(),
            "lat": place.get("lat").and_then(Value::as_str).unwrap_or_default(),
            "lon": place.get("lon").and_then(Value::as_str).unwrap_or_default(),
        },
        "current": {
            "obsTime": current.get("obsTime").and_then(Value::as_str).unwrap_or_default(),
            "temp": current.get("temp").and_then(Value::as_str).unwrap_or_default(),
            "feelsLike": current.get("feelsLike").and_then(Value::as_str).unwrap_or_default(),
            "text": current.get("text").and_then(Value::as_str).unwrap_or_default(),
            "windDir": current.get("windDir").and_then(Value::as_str).unwrap_or_default(),
            "windScale": current.get("windScale").and_then(Value::as_str).unwrap_or_default(),
            "humidity": current.get("humidity").and_then(Value::as_str).unwrap_or_default(),
            "precip": current.get("precip").and_then(Value::as_str).unwrap_or_default(),
            "vis": current.get("vis").and_then(Value::as_str).unwrap_or_default(),
        },
        "forecast": forecast_days,
        "requestUrls": {
            "lookup": lookup_url.to_string(),
            "now": now_url.to_string(),
        },
        "raw": {
            "now": now,
            "forecast": forecast,
        }
    })
}

#[derive(Debug, Clone)]
pub(super) struct HomeAssistantSettings {
    pub(super) base_url: String,
    pub(super) token: String,
    pub(super) timeout_seconds: u64,
    pub(super) blocked_domains: HashSet<String>,
}

pub(super) async fn homeassistant_list_entities_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = homeassistant_settings(&store.config()?.homeassistant)?;
    let domain = string_arg(payload, &["domain"]);
    if let Some(domain) = domain.as_deref() {
        ensure_ha_service_name(domain, "Home Assistant domain")?;
    }
    let area = string_arg(payload, &["area"]);
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 500) as usize;
    let client = homeassistant_client(&settings)?;
    let states = fetch_homeassistant_json(
        &client,
        &settings,
        homeassistant_url(&settings, &["api", "states"])?,
        "Home Assistant entity list",
        None,
    )
    .await?;
    let entities =
        normalize_homeassistant_entities(&states, domain.as_deref(), area.as_deref(), limit)?;
    Ok(serde_json::to_string_pretty(&json!({
        "tool": "ha_list_entities",
        "count": entities.len(),
        "entities": entities,
    }))?)
}

pub(super) async fn homeassistant_get_state_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = homeassistant_settings(&store.config()?.homeassistant)?;
    let entity_id = required_string_arg(payload, &["entityId", "entity_id"], "ha_get_state")?;
    ensure_ha_entity_id(&entity_id)?;
    let client = homeassistant_client(&settings)?;
    let state = fetch_homeassistant_json(
        &client,
        &settings,
        homeassistant_url(&settings, &["api", "states", &entity_id])?,
        "Home Assistant entity state",
        None,
    )
    .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "tool": "ha_get_state",
        "entity": normalize_homeassistant_state(&state),
        "raw": state,
    }))?)
}

pub(super) async fn homeassistant_list_services_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = homeassistant_settings(&store.config()?.homeassistant)?;
    let domain = string_arg(payload, &["domain"]);
    if let Some(domain) = domain.as_deref() {
        ensure_ha_service_name(domain, "Home Assistant domain")?;
    }
    let client = homeassistant_client(&settings)?;
    let services = fetch_homeassistant_json(
        &client,
        &settings,
        homeassistant_url(&settings, &["api", "services"])?,
        "Home Assistant services",
        None,
    )
    .await?;
    let normalized = normalize_homeassistant_services(&services, domain.as_deref())?;
    Ok(serde_json::to_string_pretty(&json!({
        "tool": "ha_list_services",
        "count": normalized.len(),
        "services": normalized,
    }))?)
}

pub(super) async fn homeassistant_call_service_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = homeassistant_settings(&store.config()?.homeassistant)?;
    let domain = required_string_arg(payload, &["domain"], "ha_call_service")?;
    let service = required_string_arg(payload, &["service"], "ha_call_service")?;
    ensure_ha_service_name(&domain, "Home Assistant service domain")?;
    ensure_ha_service_name(&service, "Home Assistant service name")?;
    if settings.blocked_domains.contains(&domain) {
        return Err(AppError::BadRequest(format!(
            "Home Assistant domain '{domain}' is blocked for safety"
        )));
    }
    let body = homeassistant_service_payload(payload)?;
    let client = homeassistant_client(&settings)?;
    let response = fetch_homeassistant_json(
        &client,
        &settings,
        homeassistant_url(&settings, &["api", "services", &domain, &service])?,
        "Home Assistant service call",
        Some(body.clone()),
    )
    .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "tool": "ha_call_service",
        "success": true,
        "domain": domain,
        "service": service,
        "payload": body,
        "response": response,
    }))?)
}

pub(super) async fn homeassistant_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = homeassistant_settings(&store.config()?.homeassistant)?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message Home Assistant requires message text".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "Home Assistant notify message text cannot exceed 4000 characters in one send_message chunk"
                .into(),
        ));
    }
    let notify_target = string_arg(
        payload,
        &[
            "notifyTarget",
            "notify_target",
            "target",
            "chat_id",
            "chatId",
        ],
    );
    let mut body = json!({ "message": message });
    if let Some(target) = notify_target.as_deref() {
        if !target.trim().is_empty() {
            body["target"] = json!(target.trim());
        }
    }
    let client = homeassistant_client(&settings)?;
    let response = fetch_homeassistant_json(
        &client,
        &settings,
        homeassistant_url(&settings, &["api", "services", "notify", "notify"])?,
        "Home Assistant notify send",
        Some(body.clone()),
    )
    .await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "homeassistant",
        "chat_id": notify_target.unwrap_or_else(|| "notify".into()),
        "payload": body,
        "response": response,
    }))?)
}

pub(super) fn homeassistant_settings(config: &Value) -> AppResult<HomeAssistantSettings> {
    let base_url = string_arg(config, &["url", "baseUrl", "hassUrl", "hass_url"])
        .or_else(|| {
            std::env::var("HASS_URL")
                .ok()
                .map(|value| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "http://homeassistant.local:8123".into())
        .trim_end_matches('/')
        .to_string();
    let token = string_arg(config, &["token", "accessToken", "hassToken", "hass_token"])
        .or_else(|| {
            std::env::var("HASS_TOKEN")
                .ok()
                .map(|value| value.trim().to_string())
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Home Assistant tools require settings.homeassistant.token or HASS_TOKEN".into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    let mut blocked_domains = [
        "shell_command",
        "command_line",
        "python_script",
        "pyscript",
        "hassio",
        "rest_command",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<HashSet<_>>();
    if let Some(items) = config
        .get("blockedDomains")
        .or_else(|| config.get("blocked_domains"))
        .and_then(Value::as_array)
    {
        for item in items {
            if let Some(domain) = item
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                ensure_ha_service_name(domain, "Home Assistant blocked domain")?;
                blocked_domains.insert(domain.to_string());
            }
        }
    }
    reqwest::Url::parse(&base_url).map_err(|error| {
        AppError::BadRequest(format!("invalid Home Assistant URL '{base_url}': {error}"))
    })?;
    Ok(HomeAssistantSettings {
        base_url,
        token,
        timeout_seconds,
        blocked_domains,
    })
}

pub(super) fn homeassistant_configured(config: &Value) -> bool {
    homeassistant_settings(config).is_ok()
}

pub(super) fn homeassistant_client(settings: &HomeAssistantSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Home Assistant client: {error}"))
        })
}

pub(super) fn homeassistant_url(
    settings: &HomeAssistantSettings,
    segments: &[&str],
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&settings.base_url).map_err(|error| {
        AppError::BadRequest(format!(
            "invalid Home Assistant URL '{}': {error}",
            settings.base_url
        ))
    })?;
    let mut path = url.path().trim_end_matches('/').to_string();
    for segment in segments {
        if segment.contains('/') || segment.contains('\\') || segment.trim().is_empty() {
            return Err(AppError::BadRequest(format!(
                "invalid Home Assistant URL segment: {segment}"
            )));
        }
        path.push('/');
        path.push_str(segment);
    }
    url.set_path(&path);
    Ok(url)
}

pub(super) async fn fetch_homeassistant_json(
    client: &reqwest::Client,
    settings: &HomeAssistantSettings,
    url: reqwest::Url,
    label: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let request = if let Some(body) = body {
        client.post(url.clone()).json(&body)
    } else {
        client.get(url.clone())
    };
    let response = request
        .bearer_auth(&settings.token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    if text.trim().is_empty() {
        return Ok(json!(null));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))
}

pub(super) fn normalize_homeassistant_entities(
    states: &Value,
    domain: Option<&str>,
    area: Option<&str>,
    limit: usize,
) -> AppResult<Vec<Value>> {
    let items = states.as_array().ok_or_else(|| {
        AppError::BadRequest("Home Assistant entity list response was not an array".into())
    })?;
    let area = area.map(str::to_lowercase);
    let mut entities = Vec::new();
    for item in items {
        let entity_id = item
            .get("entity_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(domain) = domain {
            if !entity_id.starts_with(&format!("{domain}.")) {
                continue;
            }
        }
        let attributes = item.get("attributes").unwrap_or(&Value::Null);
        let friendly_name = attributes
            .get("friendly_name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let item_area = attributes
            .get("area")
            .or_else(|| attributes.get("area_id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(area) = area.as_deref() {
            let haystack = format!(
                "{} {}",
                friendly_name.to_lowercase(),
                item_area.to_lowercase()
            );
            if !haystack.contains(area) {
                continue;
            }
        }
        entities.push(json!({
            "entityId": entity_id,
            "state": item.get("state").and_then(Value::as_str).unwrap_or_default(),
            "friendlyName": friendly_name,
            "area": item_area,
            "lastChanged": item.get("last_changed").and_then(Value::as_str).unwrap_or_default(),
            "lastUpdated": item.get("last_updated").and_then(Value::as_str).unwrap_or_default(),
        }));
        if entities.len() >= limit {
            break;
        }
    }
    Ok(entities)
}

pub(super) fn normalize_homeassistant_state(state: &Value) -> Value {
    json!({
        "entityId": state.get("entity_id").and_then(Value::as_str).unwrap_or_default(),
        "state": state.get("state").and_then(Value::as_str).unwrap_or_default(),
        "attributes": state.get("attributes").cloned().unwrap_or_else(|| json!({})),
        "lastChanged": state.get("last_changed").and_then(Value::as_str).unwrap_or_default(),
        "lastUpdated": state.get("last_updated").and_then(Value::as_str).unwrap_or_default(),
    })
}

pub(super) fn normalize_homeassistant_services(
    services: &Value,
    domain_filter: Option<&str>,
) -> AppResult<Vec<Value>> {
    let items = services.as_array().ok_or_else(|| {
        AppError::BadRequest("Home Assistant services response was not an array".into())
    })?;
    let mut normalized = Vec::new();
    for item in items {
        let domain = item
            .get("domain")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(filter) = domain_filter {
            if domain != filter {
                continue;
            }
        }
        let service_names = item
            .get("services")
            .and_then(Value::as_object)
            .map(|services| services.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        normalized.push(json!({
            "domain": domain,
            "services": service_names,
            "raw": item,
        }));
    }
    Ok(normalized)
}

pub(super) fn homeassistant_service_payload(payload: &Value) -> AppResult<Value> {
    let mut body = payload
        .get("data")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(entity_id) = string_arg(payload, &["entityId", "entity_id"]) {
        ensure_ha_entity_id(&entity_id)?;
        body.insert("entity_id".into(), Value::String(entity_id));
    }
    Ok(Value::Object(body))
}

#[derive(Debug, Clone)]
pub(super) struct FeishuSettings {
    pub(super) base_url: String,
    pub(super) app_id: Option<String>,
    pub(super) app_secret: Option<String>,
    pub(super) tenant_access_token: Option<String>,
    pub(super) timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub(super) struct FeishuWebhookSettings {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) path: String,
    pub(super) verification_token: Option<String>,
    pub(super) require_mention: bool,
    pub(super) bot_open_id: Option<String>,
    pub(super) bot_user_id: Option<String>,
    pub(super) bot_name: Option<String>,
}

pub(super) async fn feishu_tool(
    store: &AppStore,
    tool_name: &str,
    payload: &Value,
) -> AppResult<String> {
    let settings = feishu_settings(&store.config()?.feishu)?;
    let client = feishu_client(&settings)?;
    let result = match tool_name {
        "feishu_doc_read" => feishu_doc_read(&client, &settings, payload).await?,
        "feishu_drive_list_comments" => {
            feishu_drive_list_comments(&client, &settings, payload).await?
        }
        "feishu_drive_list_comment_replies" => {
            feishu_drive_list_comment_replies(&client, &settings, payload).await?
        }
        "feishu_drive_reply_comment" => {
            feishu_drive_reply_comment(&client, &settings, payload).await?
        }
        "feishu_drive_add_comment" => feishu_drive_add_comment(&client, &settings, payload).await?,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Feishu tool: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "tool": tool_name,
        "success": true,
        "result": result,
    }))?)
}

pub(super) async fn feishu_doc_read(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    payload: &Value,
) -> AppResult<Value> {
    let doc_token = required_string_arg(payload, &["doc_token", "docToken"], "feishu_doc_read")?;
    let response = feishu_request(
        client,
        settings,
        "GET",
        &format!(
            "/open-apis/docx/v1/documents/{}/raw_content",
            percent_encode_path_segment(&doc_token)
        ),
        &[],
        None,
        "Feishu document read",
    )
    .await?;
    Ok(json!({
        "content": response.get("data").and_then(|data| data.get("content")).and_then(Value::as_str).unwrap_or_default(),
        "raw": response,
    }))
}

pub(super) async fn feishu_drive_list_comments(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    payload: &Value,
) -> AppResult<Value> {
    let file_token = required_string_arg(
        payload,
        &["file_token", "fileToken"],
        "feishu_drive_list_comments",
    )?;
    let file_type =
        string_arg(payload, &["file_type", "fileType"]).unwrap_or_else(|| "docx".into());
    let page_size = payload
        .get("page_size")
        .or_else(|| payload.get("pageSize"))
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 100)
        .to_string();
    let mut query = vec![
        ("file_type".to_string(), file_type),
        ("user_id_type".to_string(), "open_id".into()),
        ("page_size".to_string(), page_size),
    ];
    if payload
        .get("is_whole")
        .or_else(|| payload.get("isWhole"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        query.push(("is_whole".into(), "true".into()));
    }
    if let Some(page_token) = string_arg(payload, &["page_token", "pageToken"]) {
        query.push(("page_token".into(), page_token));
    }
    feishu_request(
        client,
        settings,
        "GET",
        &format!(
            "/open-apis/drive/v1/files/{}/comments",
            percent_encode_path_segment(&file_token)
        ),
        &query,
        None,
        "Feishu comment list",
    )
    .await
}

pub(super) async fn feishu_drive_list_comment_replies(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    payload: &Value,
) -> AppResult<Value> {
    let file_token = required_string_arg(
        payload,
        &["file_token", "fileToken"],
        "feishu_drive_list_comment_replies",
    )?;
    let comment_id = required_string_arg(
        payload,
        &["comment_id", "commentId"],
        "feishu_drive_list_comment_replies",
    )?;
    let file_type =
        string_arg(payload, &["file_type", "fileType"]).unwrap_or_else(|| "docx".into());
    let page_size = payload
        .get("page_size")
        .or_else(|| payload.get("pageSize"))
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 100)
        .to_string();
    let mut query = vec![
        ("file_type".to_string(), file_type),
        ("user_id_type".to_string(), "open_id".into()),
        ("page_size".to_string(), page_size),
    ];
    if let Some(page_token) = string_arg(payload, &["page_token", "pageToken"]) {
        query.push(("page_token".into(), page_token));
    }
    feishu_request(
        client,
        settings,
        "GET",
        &format!(
            "/open-apis/drive/v1/files/{}/comments/{}/replies",
            percent_encode_path_segment(&file_token),
            percent_encode_path_segment(&comment_id)
        ),
        &query,
        None,
        "Feishu comment replies",
    )
    .await
}

pub(super) async fn feishu_drive_reply_comment(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    payload: &Value,
) -> AppResult<Value> {
    let file_token = required_string_arg(
        payload,
        &["file_token", "fileToken"],
        "feishu_drive_reply_comment",
    )?;
    let comment_id = required_string_arg(
        payload,
        &["comment_id", "commentId"],
        "feishu_drive_reply_comment",
    )?;
    let content = required_string_arg(payload, &["content", "text"], "feishu_drive_reply_comment")?;
    let file_type =
        string_arg(payload, &["file_type", "fileType"]).unwrap_or_else(|| "docx".into());
    let query = vec![("file_type".to_string(), file_type)];
    let body = json!({
        "content": {
            "elements": [{
                "type": "text_run",
                "text_run": {"text": content}
            }]
        }
    });
    feishu_request(
        client,
        settings,
        "POST",
        &format!(
            "/open-apis/drive/v1/files/{}/comments/{}/replies",
            percent_encode_path_segment(&file_token),
            percent_encode_path_segment(&comment_id)
        ),
        &query,
        Some(body),
        "Feishu comment reply",
    )
    .await
}

pub(super) async fn feishu_drive_add_comment(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    payload: &Value,
) -> AppResult<Value> {
    let file_token = required_string_arg(
        payload,
        &["file_token", "fileToken"],
        "feishu_drive_add_comment",
    )?;
    let content = required_string_arg(payload, &["content", "text"], "feishu_drive_add_comment")?;
    let file_type =
        string_arg(payload, &["file_type", "fileType"]).unwrap_or_else(|| "docx".into());
    let body = json!({
        "file_type": file_type,
        "reply_elements": [{
            "type": "text",
            "text": content
        }]
    });
    feishu_request(
        client,
        settings,
        "POST",
        &format!(
            "/open-apis/drive/v1/files/{}/new_comments",
            percent_encode_path_segment(&file_token)
        ),
        &[],
        Some(body),
        "Feishu add comment",
    )
    .await
}

pub(super) async fn feishu_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = feishu_settings(&store.config()?.feishu)?;
    let client = feishu_client(&settings)?;
    let receive_id = required_string_arg(
        payload,
        &["receive_id", "receiveId", "chat_id", "chatId", "target"],
        "send_message feishu",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let media_files = discord_media_file_paths(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Feishu requires message text or media_files".into(),
        ));
    }
    let thread_id = string_arg(
        payload,
        &[
            "thread_id",
            "threadId",
            "message_id",
            "messageId",
            "reply_to",
            "replyTo",
        ],
    );
    let receive_id_type = string_arg(payload, &["receive_id_type", "receiveIdType"])
        .unwrap_or_else(|| infer_feishu_receive_id_type(&receive_id));
    let mut events = Vec::new();
    if !message.trim().is_empty() {
        events.push(
            feishu_send_message(
                &client,
                &settings,
                &receive_id,
                &receive_id_type,
                &message,
                thread_id.as_deref(),
            )
            .await?,
        );
    }
    for file_path in &media_files {
        events.push(
            feishu_upload_and_send_media(
                &client,
                &settings,
                &receive_id,
                &receive_id_type,
                file_path,
                thread_id.as_deref(),
            )
            .await?,
        );
    }
    let message_id = events
        .last()
        .and_then(|event| event.get("data"))
        .and_then(|data| data.get("message_id"))
        .and_then(Value::as_str);
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "feishu",
        "chat_id": receive_id,
        "receive_id_type": receive_id_type,
        "message_id": message_id,
        "media_count": media_files.len(),
        "events": events
    }))?)
}

pub(super) async fn feishu_send_message(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    receive_id: &str,
    receive_id_type: &str,
    message: &str,
    thread_id: Option<&str>,
) -> AppResult<Value> {
    let content = serde_json::to_string(&json!({ "text": message }))
        .map_err(|error| AppError::BadRequest(format!("Feishu message encode failed: {error}")))?;
    let body = if thread_id.is_some() {
        json!({
            "msg_type": "text",
            "content": content,
        })
    } else {
        json!({
            "receive_id": receive_id,
            "msg_type": "text",
            "content": content,
        })
    };
    if let Some(thread_id) = thread_id.map(str::trim).filter(|value| !value.is_empty()) {
        feishu_request(
            client,
            settings,
            "POST",
            &format!(
                "/open-apis/im/v1/messages/{}/reply",
                percent_encode_path_segment(thread_id)
            ),
            &[],
            Some(body),
            "Feishu send message reply",
        )
        .await
    } else {
        feishu_request(
            client,
            settings,
            "POST",
            "/open-apis/im/v1/messages",
            &[("receive_id_type".to_string(), receive_id_type.to_string())],
            Some(body),
            "Feishu send message",
        )
        .await
    }
}

pub(super) async fn feishu_upload_and_send_media(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    receive_id: &str,
    receive_id_type: &str,
    file_path: &str,
    thread_id: Option<&str>,
) -> AppResult<Value> {
    let path = Path::new(file_path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("attachment")
        .to_string();
    if feishu_is_image_file(&file_name) {
        let image_key = feishu_upload_image(client, settings, path).await?;
        return feishu_send_media_message(
            client,
            settings,
            receive_id,
            receive_id_type,
            "image",
            json!({ "image_key": image_key }),
            thread_id,
        )
        .await;
    }
    let (file_type, msg_type) = feishu_file_routing(&file_name);
    let file_key = feishu_upload_file(client, settings, path, &file_name, file_type).await?;
    feishu_send_media_message(
        client,
        settings,
        receive_id,
        receive_id_type,
        msg_type,
        json!({ "file_key": file_key }),
        thread_id,
    )
    .await
}

async fn feishu_send_media_message(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    receive_id: &str,
    receive_id_type: &str,
    msg_type: &str,
    payload: Value,
    thread_id: Option<&str>,
) -> AppResult<Value> {
    let content = serde_json::to_string(&payload).map_err(|error| {
        AppError::BadRequest(format!("Feishu media message encode failed: {error}"))
    })?;
    let body = if thread_id.is_some() {
        json!({
            "msg_type": msg_type,
            "content": content,
        })
    } else {
        json!({
            "receive_id": receive_id,
            "msg_type": msg_type,
            "content": content,
        })
    };
    if let Some(thread_id) = thread_id.map(str::trim).filter(|value| !value.is_empty()) {
        feishu_request(
            client,
            settings,
            "POST",
            &format!(
                "/open-apis/im/v1/messages/{}/reply",
                percent_encode_path_segment(thread_id)
            ),
            &[],
            Some(body),
            "Feishu send media reply",
        )
        .await
    } else {
        feishu_request(
            client,
            settings,
            "POST",
            "/open-apis/im/v1/messages",
            &[("receive_id_type".to_string(), receive_id_type.to_string())],
            Some(body),
            "Feishu send media",
        )
        .await
    }
}

async fn feishu_upload_image(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    path: &Path,
) -> AppResult<String> {
    let bytes = fs::read(path).map_err(|error| {
        AppError::BadRequest(format!("failed to read Feishu image file: {error}"))
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("image");
    let form = reqwest::multipart::Form::new()
        .text("image_type", "message")
        .part(
            "image",
            reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string()),
        );
    let value = feishu_multipart_request(
        client,
        settings,
        "/open-apis/im/v1/images",
        form,
        "Feishu image upload",
    )
    .await?;
    value
        .get("data")
        .and_then(|data| data.get("image_key"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("Feishu image upload missing image_key".into()))
}

async fn feishu_upload_file(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    path: &Path,
    file_name: &str,
    file_type: &str,
) -> AppResult<String> {
    let bytes = fs::read(path).map_err(|error| {
        AppError::BadRequest(format!("failed to read Feishu media file: {error}"))
    })?;
    let form = reqwest::multipart::Form::new()
        .text("file_type", file_type.to_string())
        .text("file_name", file_name.to_string())
        .part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string()),
        );
    let value = feishu_multipart_request(
        client,
        settings,
        "/open-apis/im/v1/files",
        form,
        "Feishu file upload",
    )
    .await?;
    value
        .get("data")
        .and_then(|data| data.get("file_key"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("Feishu file upload missing file_key".into()))
}

async fn feishu_multipart_request(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    path: &str,
    form: reqwest::multipart::Form,
    label: &str,
) -> AppResult<Value> {
    let token = feishu_tenant_access_token(client, settings).await?;
    let url = feishu_url(settings, path, &[])?;
    let response = client
        .post(url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
        .multipart(form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    feishu_response_json(response, label).await
}

pub(super) fn feishu_is_image_file(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("webp") | Some("bmp")
    )
}

pub(super) fn feishu_file_routing(file_name: &str) -> (&'static str, &'static str) {
    match Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("ogg") | Some("opus") => ("opus", "audio"),
        Some("mp4") | Some("mov") | Some("avi") | Some("m4v") => ("mp4", "media"),
        Some("pdf") => ("pdf", "file"),
        Some("doc") | Some("docx") => ("doc", "file"),
        Some("xls") | Some("xlsx") => ("xls", "file"),
        Some("ppt") | Some("pptx") => ("ppt", "file"),
        _ => ("stream", "file"),
    }
}

pub(super) fn infer_feishu_receive_id_type(receive_id: &str) -> String {
    let value = receive_id.trim();
    if value.contains('@') {
        "email".into()
    } else if value.starts_with("oc_") || value.starts_with("chat_") {
        "chat_id".into()
    } else if value.starts_with("ou_") || value.starts_with("open_") {
        "open_id".into()
    } else if value.starts_with("on_") {
        "union_id".into()
    } else {
        "chat_id".into()
    }
}

pub(super) fn feishu_settings(config: &Value) -> AppResult<FeishuSettings> {
    let base_url = string_arg(config, &["baseUrl", "base_url", "url"])
        .or_else(|| std::env::var("FEISHU_BASE_URL").ok())
        .unwrap_or_else(|| "https://open.feishu.cn".into())
        .trim_end_matches('/')
        .to_string();
    reqwest::Url::parse(&base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Feishu baseUrl: {error}")))?;
    let app_id = string_arg(config, &["appId", "app_id"])
        .or_else(|| std::env::var("FEISHU_APP_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let app_secret = string_arg(config, &["appSecret", "app_secret"])
        .or_else(|| std::env::var("FEISHU_APP_SECRET").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let tenant_access_token = string_arg(
        config,
        &["tenantAccessToken", "tenant_access_token", "token"],
    )
    .or_else(|| std::env::var("FEISHU_TENANT_ACCESS_TOKEN").ok())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    if tenant_access_token.is_none() && (app_id.is_none() || app_secret.is_none()) {
        return Err(AppError::BadRequest(
            "Feishu tools require settings.feishu.tenantAccessToken, FEISHU_TENANT_ACCESS_TOKEN, or appId/appSecret".into(),
        ));
    }
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(FeishuSettings {
        base_url,
        app_id,
        app_secret,
        tenant_access_token,
        timeout_seconds,
    })
}

fn feishu_webhook_configured(config: &Value) -> bool {
    feishu_webhook_settings(config).is_ok()
}

fn feishu_webhook_settings(config: &Value) -> AppResult<FeishuWebhookSettings> {
    let host = string_arg(config, &["webhookHost", "webhook_host", "host", "bindHost"])
        .or_else(|| std::env::var("FEISHU_WEBHOOK_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".into())
        .trim()
        .to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest(
            "Feishu webhook host cannot be empty".into(),
        ));
    }
    let port = config
        .get("webhookPort")
        .or_else(|| config.get("webhook_port"))
        .or_else(|| config.get("port"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("FEISHU_WEBHOOK_PORT")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(8765)
        .clamp(1, u16::MAX as u64) as u16;
    let mut path = string_arg(config, &["webhookPath", "webhook_path", "path"])
        .or_else(|| std::env::var("FEISHU_WEBHOOK_PATH").ok())
        .unwrap_or_else(|| "/feishu/webhook".into())
        .trim()
        .to_string();
    if path.is_empty() {
        path = "/feishu/webhook".into();
    }
    if !path.starts_with('/') {
        path = format!("/{path}");
    }
    let verification_token = string_arg(
        config,
        &["verificationToken", "verification_token", "token"],
    )
    .or_else(|| std::env::var("FEISHU_VERIFICATION_TOKEN").ok())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty());
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("FEISHU_REQUIRE_MENTION"))
        .unwrap_or(true);
    let bot_open_id = string_arg(config, &["botOpenId", "bot_open_id"])
        .or_else(|| std::env::var("FEISHU_BOT_OPEN_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let bot_user_id = string_arg(config, &["botUserId", "bot_user_id"])
        .or_else(|| std::env::var("FEISHU_BOT_USER_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let bot_name = string_arg(config, &["botName", "bot_name"])
        .or_else(|| std::env::var("FEISHU_BOT_NAME").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(FeishuWebhookSettings {
        host,
        port,
        path,
        verification_token,
        require_mention,
        bot_open_id,
        bot_user_id,
        bot_name,
    })
}

pub(super) fn feishu_client(settings: &FeishuSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Feishu client: {error}")))
}

pub(super) async fn feishu_tenant_access_token(
    client: &reqwest::Client,
    settings: &FeishuSettings,
) -> AppResult<String> {
    if let Some(token) = settings.tenant_access_token.as_deref() {
        return Ok(token.to_string());
    }
    let app_id = settings.app_id.as_deref().ok_or_else(|| {
        AppError::BadRequest("Feishu appId is required to fetch tenant_access_token".into())
    })?;
    let app_secret = settings.app_secret.as_deref().ok_or_else(|| {
        AppError::BadRequest("Feishu appSecret is required to fetch tenant_access_token".into())
    })?;
    let url = feishu_url(
        settings,
        "/open-apis/auth/v3/tenant_access_token/internal",
        &[],
    )?;
    let response = client
        .post(url.clone())
        .json(&json!({"app_id": app_id, "app_secret": app_secret}))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Feishu token request failed: {error}")))?;
    let value = feishu_response_json(response, "Feishu token request").await?;
    value
        .get("tenant_access_token")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| data.get("tenant_access_token"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::BadRequest("Feishu token response missing tenant_access_token".into())
        })
}

pub(super) async fn feishu_request(
    client: &reqwest::Client,
    settings: &FeishuSettings,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: Option<Value>,
    label: &str,
) -> AppResult<Value> {
    let token = feishu_tenant_access_token(client, settings).await?;
    let url = feishu_url(settings, path, query)?;
    let request = match method {
        "GET" => client.get(url.clone()),
        "POST" => client.post(url.clone()),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Feishu HTTP method: {other}"
            )));
        }
    };
    let request = request
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json");
    let request = if let Some(body) = body {
        request.json(&body)
    } else {
        request
    };
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    feishu_response_json(response, label).await
}

pub(super) fn feishu_url(
    settings: &FeishuSettings,
    path: &str,
    query: &[(String, String)],
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&settings.base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Feishu baseUrl: {error}")))?;
    let mut full_path = url.path().trim_end_matches('/').to_string();
    full_path.push('/');
    full_path.push_str(path.trim_start_matches('/'));
    url.set_path(&full_path);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            if !value.trim().is_empty() {
                pairs.append_pair(key, value);
            }
        }
    }
    Ok(url)
}

pub(super) async fn feishu_response_json(
    response: reqwest::Response,
    label: &str,
) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let msg = value
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(AppError::BadRequest(format!(
            "{label} returned Feishu code {code}: {msg}"
        )));
    }
    Ok(value)
}

pub(super) fn percent_encode_path_segment(value: &str) -> String {
    let mut output = String::new();
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            output.push(ch);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

#[derive(Debug, Clone)]
pub(super) struct YuanbaoSettings {
    pub(super) gateway_url: Option<String>,
    pub(super) token: Option<String>,
    pub(super) timeout_seconds: u64,
    pub(super) config: Value,
}

pub(super) async fn yuanbao_tool(
    store: &AppStore,
    tool_name: &str,
    payload: &Value,
) -> AppResult<String> {
    let settings = yuanbao_settings(&store.config()?.yuanbao)?;
    let result = match tool_name {
        "yb_search_sticker" => {
            if let Some(local) = yuanbao_search_local_stickers(&settings, payload)? {
                local
            } else {
                yuanbao_bridge_request(&settings, "yb_search_sticker", payload).await?
            }
        }
        "yb_query_group_info" | "yb_query_group_members" | "yb_send_dm" | "yb_send_sticker" => {
            yuanbao_bridge_request(&settings, tool_name, payload).await?
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Yuanbao tool: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "tool": tool_name,
        "success": result.get("success").and_then(Value::as_bool).unwrap_or(true),
        "result": result,
    }))?)
}

pub(super) fn yuanbao_settings(config: &Value) -> AppResult<YuanbaoSettings> {
    let gateway_url = string_arg(
        config,
        &["gatewayUrl", "gateway_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("YUANBAO_GATEWAY_URL").ok())
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty());
    if let Some(url) = gateway_url.as_deref() {
        reqwest::Url::parse(url).map_err(|error| {
            AppError::BadRequest(format!("invalid Yuanbao gatewayUrl: {error}"))
        })?;
    }
    let token = string_arg(config, &["token", "accessToken", "access_token"])
        .or_else(|| std::env::var("YUANBAO_GATEWAY_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(YuanbaoSettings {
        gateway_url,
        token,
        timeout_seconds,
        config: config.clone(),
    })
}

pub(super) fn yuanbao_bridge_available(config: &Value) -> bool {
    string_arg(
        config,
        &["gatewayUrl", "gateway_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("YUANBAO_GATEWAY_URL").ok())
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
    .and_then(|value| reqwest::Url::parse(&value).ok())
    .is_some()
}

pub(super) fn yuanbao_stickers_available(config: &Value) -> bool {
    config
        .get("stickers")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false)
}

pub(super) async fn yuanbao_bridge_request(
    settings: &YuanbaoSettings,
    tool_name: &str,
    payload: &Value,
) -> AppResult<Value> {
    let gateway_url = settings.gateway_url.as_deref().ok_or_else(|| {
        AppError::BadRequest(
            "Yuanbao tools require settings.yuanbao.gatewayUrl or YUANBAO_GATEWAY_URL unless yb_search_sticker uses a local sticker catalogue".into(),
        )
    })?;
    let path = yuanbao_bridge_path(settings, tool_name);
    let mut url = reqwest::Url::parse(gateway_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Yuanbao gatewayUrl: {error}")))?;
    let mut full_path = url.path().trim_end_matches('/').to_string();
    full_path.push('/');
    full_path.push_str(path.trim_start_matches('/'));
    url.set_path(&full_path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Yuanbao client: {error}"))
        })?;
    let mut request = client.post(url.clone()).json(payload);
    if let Some(token) = settings.token.as_deref() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|error| {
        AppError::BadRequest(format!("{tool_name} bridge request failed: {error}"))
    })?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read {tool_name} bridge response: {error}"
        ))
    })?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{tool_name} bridge returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {tool_name} bridge JSON: {error}")))
}

pub(super) fn yuanbao_bridge_path(settings: &YuanbaoSettings, tool_name: &str) -> String {
    let key = match tool_name {
        "yb_query_group_info" => "queryGroupInfo",
        "yb_query_group_members" => "queryGroupMembers",
        "yb_send_dm" => "sendDm",
        "yb_search_sticker" => "searchSticker",
        "yb_send_sticker" => "sendSticker",
        _ => tool_name,
    };
    settings
        .config
        .get("paths")
        .and_then(Value::as_object)
        .and_then(|paths| paths.get(key).or_else(|| paths.get(tool_name)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("/yuanbao/{tool_name}"))
}

#[derive(Debug, Clone)]
pub(super) struct TelegramSettings {
    pub(super) api_base_url: String,
    pub(super) bot_token: String,
    pub(super) timeout_seconds: u64,
    pub(super) proxy_url: Option<String>,
    pub(super) parse_mode: Option<String>,
    pub(super) disable_web_page_preview: Option<bool>,
    pub(super) disable_notification: Option<bool>,
    pub(super) protect_content: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TelegramSendOptions {
    pub(super) parse_mode: Option<String>,
    pub(super) disable_web_page_preview: Option<bool>,
    pub(super) disable_notification: Option<bool>,
    pub(super) protect_content: Option<bool>,
}

pub(super) async fn telegram_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = telegram_settings(&store.config()?.telegram)?;
    let client = telegram_client(&settings)?;
    let chat_id = required_string_arg(
        payload,
        &["chat_id", "chatId", "channel_id", "channelId", "target"],
        "send_message telegram",
    )?;
    let raw_message =
        string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let force_document = payload
        .get("force_document")
        .or_else(|| payload.get("forceDocument"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| raw_message.contains("[[as_document]]"));
    let message = raw_message
        .replace("[[as_document]]", "")
        .trim()
        .to_string();
    let media_files = telegram_media_files(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Telegram requires message text or media_files".into(),
        ));
    }
    if message.chars().count() > 4_096 {
        return Err(AppError::BadRequest(
            "Telegram message text cannot exceed 4096 characters".into(),
        ));
    }
    let thread_id = string_arg(
        payload,
        &[
            "thread_id",
            "threadId",
            "message_thread_id",
            "messageThreadId",
        ],
    );
    let send_options = telegram_send_options(&settings, payload);
    let mut results = Vec::new();
    if !message.trim().is_empty() {
        let mut body = json!({
            "chat_id": chat_id,
            "text": message,
        });
        if let Some(thread_id) = thread_id
            .as_deref()
            .and_then(telegram_effective_thread_id_for_send)
        {
            body["message_thread_id"] = json!(thread_id);
        }
        telegram_apply_send_options_to_body(&mut body, &send_options, true);
        results.push(
            telegram_request_with_thread_fallback(&client, &settings, "sendMessage", body).await?,
        );
    }
    for media_file in &media_files {
        results.push(
            telegram_send_media_file(
                &client,
                &settings,
                &chat_id,
                thread_id.as_deref(),
                media_file,
                force_document,
                &send_options,
            )
            .await?,
        );
    }
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "telegram",
        "chat_id": chat_id,
        "thread_id": thread_id,
        "media_count": media_files.len(),
        "results": results,
    }))?)
}

#[derive(Debug, Clone)]
pub(super) struct TelegramMediaFile {
    pub(super) path: String,
    pub(super) is_voice: bool,
}

fn telegram_media_files(payload: &Value) -> AppResult<Vec<TelegramMediaFile>> {
    let Some(files) = json_array_arg(payload, &["media_files", "mediaFiles"]) else {
        return Ok(Vec::new());
    };
    files
        .into_iter()
        .map(|file| {
            if let Some(path) = file.as_str() {
                return Ok(TelegramMediaFile {
                    path: path.trim().to_string(),
                    is_voice: false,
                });
            }
            let path =
                string_arg(&file, &["path", "file", "file_path", "filePath"]).ok_or_else(|| {
                    AppError::BadRequest(
                        "Telegram media_files entries must be strings or objects with path".into(),
                    )
                })?;
            let is_voice = file
                .get("is_voice")
                .or_else(|| file.get("isVoice"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(TelegramMediaFile { path, is_voice })
        })
        .filter(|result| {
            result
                .as_ref()
                .map(|file| !file.path.trim().is_empty())
                .unwrap_or(true)
        })
        .collect()
}

pub(super) fn telegram_settings(config: &Value) -> AppResult<TelegramSettings> {
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("TELEGRAM_API_BASE_URL").ok())
    .unwrap_or_else(|| "https://api.telegram.org".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Telegram apiBaseUrl: {error}")))?;
    let bot_token = string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Telegram send_message requires settings.telegram.botToken or TELEGRAM_BOT_TOKEN"
                    .into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    let proxy_url = string_arg(config, &["proxyUrl", "proxy_url", "proxy"])
        .or_else(|| std::env::var("TELEGRAM_PROXY").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(TelegramSettings {
        api_base_url,
        bot_token,
        timeout_seconds,
        proxy_url,
        parse_mode: telegram_parse_mode_arg(config, &["parseMode", "parse_mode"])
            .or_else(|| std::env::var("TELEGRAM_PARSE_MODE").ok())
            .and_then(|value| telegram_normalize_parse_mode(&value)),
        disable_web_page_preview: telegram_bool_arg(
            config,
            &["disableWebPagePreview", "disable_web_page_preview"],
        )
        .or_else(|| telegram_env_bool("TELEGRAM_DISABLE_WEB_PAGE_PREVIEW")),
        disable_notification: telegram_bool_arg(
            config,
            &["disableNotification", "disable_notification", "silent"],
        )
        .or_else(|| telegram_env_bool("TELEGRAM_DISABLE_NOTIFICATION")),
        protect_content: telegram_bool_arg(config, &["protectContent", "protect_content"])
            .or_else(|| telegram_env_bool("TELEGRAM_PROTECT_CONTENT")),
    })
}

pub(super) fn telegram_configured(config: &Value) -> bool {
    string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub(super) fn telegram_client(settings: &TelegramSettings) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0");
    if let Some(proxy_url) = settings.proxy_url.as_deref() {
        builder = builder.proxy(reqwest::Proxy::all(proxy_url).map_err(|error| {
            AppError::BadRequest(format!("invalid Telegram proxy URL: {error}"))
        })?);
    }
    builder
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Telegram client: {error}")))
}

pub(super) fn telegram_send_options(
    settings: &TelegramSettings,
    payload: &Value,
) -> TelegramSendOptions {
    TelegramSendOptions {
        parse_mode: telegram_parse_mode_arg(payload, &["parseMode", "parse_mode"])
            .and_then(|value| telegram_normalize_parse_mode(&value))
            .or_else(|| settings.parse_mode.clone()),
        disable_web_page_preview: telegram_bool_arg(
            payload,
            &["disableWebPagePreview", "disable_web_page_preview"],
        )
        .or(settings.disable_web_page_preview),
        disable_notification: telegram_bool_arg(
            payload,
            &["disableNotification", "disable_notification", "silent"],
        )
        .or(settings.disable_notification),
        protect_content: telegram_bool_arg(payload, &["protectContent", "protect_content"])
            .or(settings.protect_content),
    }
}

pub(super) fn telegram_apply_send_options_to_body(
    body: &mut Value,
    options: &TelegramSendOptions,
    allow_web_page_preview: bool,
) {
    if let Some(object) = body.as_object_mut() {
        if let Some(parse_mode) = options.parse_mode.as_deref() {
            object.insert("parse_mode".into(), json!(parse_mode));
        }
        if allow_web_page_preview {
            if let Some(value) = options.disable_web_page_preview {
                object.insert("disable_web_page_preview".into(), json!(value));
            }
        }
        if let Some(value) = options.disable_notification {
            object.insert("disable_notification".into(), json!(value));
        }
        if let Some(value) = options.protect_content {
            object.insert("protect_content".into(), json!(value));
        }
    }
}

pub(super) fn telegram_normalize_parse_mode(value: &str) -> Option<String> {
    match value
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "")
        .as_str()
    {
        "markdownv2" | "mdv2" => Some("MarkdownV2".into()),
        "markdown" | "md" => Some("Markdown".into()),
        "html" => Some("HTML".into()),
        "" | "none" | "off" | "false" => None,
        _ => Some(value.trim().to_string()),
    }
}

pub(super) fn telegram_parse_mode_arg(payload: &Value, keys: &[&str]) -> Option<String> {
    string_arg(payload, keys).filter(|value| !value.trim().is_empty())
}

pub(super) fn telegram_bool_arg(payload: &Value, keys: &[&str]) -> Option<bool> {
    for key in keys {
        if let Some(value) = payload.get(*key) {
            if let Some(value) = value.as_bool() {
                return Some(value);
            }
            if let Some(value) = value.as_str().and_then(telegram_parse_bool) {
                return Some(value);
            }
        }
    }
    None
}

pub(super) fn telegram_env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .and_then(|value| telegram_parse_bool(&value))
}

fn telegram_parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

const TELEGRAM_MAX_REQUEST_ATTEMPTS: usize = 3;

async fn telegram_request(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    method: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let mut retry_count = 0usize;
    loop {
        match telegram_request_once(client, settings, method, body.clone()).await {
            Ok(value) => {
                return Ok(telegram_mark_retry_result(
                    value,
                    retry_count,
                    "retry_after",
                ));
            }
            Err(error) => {
                let Some(delay_seconds) = telegram_retry_after_seconds(&error) else {
                    return Err(error);
                };
                if retry_count + 1 >= TELEGRAM_MAX_REQUEST_ATTEMPTS {
                    return Err(error);
                }
                retry_count += 1;
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
            }
        }
    }
}

async fn telegram_request_once(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    method: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let url = telegram_url(settings, method)?;
    let mut request = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&strip_null_json_object(body));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Telegram {method} failed: {error}")))?;
    telegram_response_json(response, method).await
}

pub(super) fn telegram_retry_after_seconds(error: &AppError) -> Option<u64> {
    let text = error.to_string().to_ascii_lowercase();
    if !(text.contains("retry after")
        || text.contains("too many requests")
        || text.contains("flood")
        || text.contains("http 429"))
    {
        return None;
    }
    if let Some((_prefix, suffix)) = text.split_once("retry after") {
        for token in suffix.split(|ch: char| !ch.is_ascii_digit()) {
            if token.is_empty() {
                continue;
            }
            if let Ok(value) = token.parse::<u64>() {
                return Some(value.clamp(1, 30));
            }
        }
    }
    Some(1)
}

pub(super) fn telegram_mark_retry_result(
    mut value: Value,
    retry_count: usize,
    reason: &str,
) -> Value {
    if retry_count == 0 {
        return value;
    }
    if !value.is_object() {
        value = json!({ "result": value });
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("telegram_retry_count".into(), json!(retry_count));
        object.insert("telegram_retry_reason".into(), json!(reason));
    }
    value
}

async fn telegram_request_with_parse_mode_fallback(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    method: &str,
    body: Value,
) -> AppResult<Value> {
    let parse_mode = telegram_body_parse_mode(&body);
    match telegram_request(client, settings, method, Some(body.clone())).await {
        Ok(value) => Ok(value),
        Err(error) if parse_mode.is_some() && telegram_error_is_parse_mode_failure(&error) => {
            let requested_parse_mode = parse_mode.unwrap_or_default();
            let fallback_body = telegram_body_without_parse_mode(body);
            let value = telegram_request(client, settings, method, Some(fallback_body)).await?;
            Ok(telegram_mark_parse_mode_fallback_result(
                value,
                &requested_parse_mode,
            ))
        }
        Err(error) => Err(error),
    }
}

pub(super) fn telegram_error_is_parse_mode_failure(error: &AppError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    (text.contains("parse") || text.contains("can't parse") || text.contains("can't find"))
        && (text.contains("markdown") || text.contains("html") || text.contains("entities"))
}

pub(super) fn telegram_body_parse_mode(body: &Value) -> Option<String> {
    body.get("parse_mode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn telegram_body_without_parse_mode(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("parse_mode");
    }
    body
}

pub(super) fn telegram_mark_parse_mode_fallback_result(
    mut value: Value,
    requested_parse_mode: &str,
) -> Value {
    if !value.is_object() {
        value = json!({ "result": value });
    }
    if let Some(object) = value.as_object_mut() {
        object.insert("telegram_parse_mode_fallback".into(), json!(true));
        object.insert(
            "requested_parse_mode".into(),
            json!(requested_parse_mode.to_string()),
        );
    }
    value
}

async fn telegram_request_with_thread_fallback(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    method: &str,
    body: Value,
) -> AppResult<Value> {
    let Some(requested_thread_id) = telegram_body_message_thread_id(&body) else {
        return telegram_request_with_parse_mode_fallback(client, settings, method, body).await;
    };
    match telegram_request_with_parse_mode_fallback(client, settings, method, body.clone()).await {
        Ok(value) => Ok(value),
        Err(error) if telegram_error_is_thread_not_found(&error) => {
            match telegram_request_with_parse_mode_fallback(client, settings, method, body.clone())
                .await
            {
                Ok(value) => Ok(telegram_mark_thread_fallback_result(
                    value,
                    &requested_thread_id,
                    1,
                    false,
                )),
                Err(second_error) if telegram_error_is_thread_not_found(&second_error) => {
                    let fallback_body = telegram_body_without_message_thread_id(body);
                    let value = telegram_request_with_parse_mode_fallback(
                        client,
                        settings,
                        method,
                        fallback_body,
                    )
                    .await?;
                    Ok(telegram_mark_thread_fallback_result(
                        value,
                        &requested_thread_id,
                        2,
                        true,
                    ))
                }
                Err(second_error) => Err(second_error),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) fn telegram_error_is_thread_not_found(error: &AppError) -> bool {
    error
        .to_string()
        .to_ascii_lowercase()
        .contains("thread not found")
}

pub(super) fn telegram_body_message_thread_id(body: &Value) -> Option<String> {
    body.get("message_thread_id")
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|value| value.to_string()))
                .or_else(|| value.as_u64().map(|value| value.to_string()))
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn telegram_body_without_message_thread_id(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("message_thread_id");
    }
    body
}

pub(super) fn telegram_mark_thread_fallback_result(
    mut value: Value,
    requested_thread_id: &str,
    retry_count: usize,
    fallback_without_thread: bool,
) -> Value {
    if !value.is_object() {
        value = json!({ "result": value });
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "requested_thread_id".into(),
            json!(requested_thread_id.to_string()),
        );
        object.insert("telegram_thread_retry_count".into(), json!(retry_count));
        object.insert(
            "telegram_thread_fallback_without_thread".into(),
            json!(fallback_without_thread),
        );
    }
    value
}

async fn telegram_send_media_file(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    chat_id: &str,
    thread_id: Option<&str>,
    media_file: &TelegramMediaFile,
    force_document: bool,
    send_options: &TelegramSendOptions,
) -> AppResult<Value> {
    let file_path = media_file.path.as_str();
    let bytes = fs::read(file_path).map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read Telegram media file {file_path}: {error}"
        ))
    })?;
    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("attachment");
    let (method, field_name) =
        telegram_media_method(file_path, media_file.is_voice, force_document);
    telegram_send_media_file_with_thread_fallback(
        client,
        settings,
        chat_id,
        thread_id,
        method,
        field_name,
        bytes,
        file_name,
        send_options,
    )
    .await
}

async fn telegram_send_media_file_with_thread_fallback(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    chat_id: &str,
    thread_id: Option<&str>,
    method: &str,
    field_name: &str,
    bytes: Vec<u8>,
    file_name: &str,
    send_options: &TelegramSendOptions,
) -> AppResult<Value> {
    let requested_thread_id = thread_id
        .and_then(telegram_effective_thread_id_for_send)
        .map(str::to_string);
    let first = telegram_send_media_file_request(
        client,
        settings,
        chat_id,
        requested_thread_id.as_deref(),
        method,
        field_name,
        &bytes,
        file_name,
        send_options,
    )
    .await;
    let Some(requested_thread_id) = requested_thread_id else {
        return first;
    };
    match first {
        Ok(value) => Ok(value),
        Err(error) if telegram_error_is_thread_not_found(&error) => {
            let second = telegram_send_media_file_request(
                client,
                settings,
                chat_id,
                Some(&requested_thread_id),
                method,
                field_name,
                &bytes,
                file_name,
                send_options,
            )
            .await;
            match second {
                Ok(value) => Ok(telegram_mark_thread_fallback_result(
                    value,
                    &requested_thread_id,
                    1,
                    false,
                )),
                Err(second_error) if telegram_error_is_thread_not_found(&second_error) => {
                    let value = telegram_send_media_file_request(
                        client,
                        settings,
                        chat_id,
                        None,
                        method,
                        field_name,
                        &bytes,
                        file_name,
                        send_options,
                    )
                    .await?;
                    Ok(telegram_mark_thread_fallback_result(
                        value,
                        &requested_thread_id,
                        2,
                        true,
                    ))
                }
                Err(second_error) => Err(second_error),
            }
        }
        Err(error) => Err(error),
    }
}

async fn telegram_send_media_file_request(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    chat_id: &str,
    thread_id: Option<&str>,
    method: &str,
    field_name: &str,
    bytes: &[u8],
    file_name: &str,
    send_options: &TelegramSendOptions,
) -> AppResult<Value> {
    let mut retry_count = 0usize;
    loop {
        let value = telegram_send_media_file_request_once(
            client,
            settings,
            chat_id,
            thread_id,
            method,
            field_name,
            bytes,
            file_name,
            send_options,
        )
        .await;
        match value {
            Ok(value) => {
                return Ok(telegram_mark_retry_result(
                    value,
                    retry_count,
                    "retry_after",
                ));
            }
            Err(error) => {
                let Some(delay_seconds) = telegram_retry_after_seconds(&error) else {
                    return Err(error);
                };
                if retry_count + 1 >= TELEGRAM_MAX_REQUEST_ATTEMPTS {
                    return Err(error);
                }
                retry_count += 1;
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
            }
        }
    }
}

async fn telegram_send_media_file_request_once(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    chat_id: &str,
    thread_id: Option<&str>,
    method: &str,
    field_name: &str,
    bytes: &[u8],
    file_name: &str,
    send_options: &TelegramSendOptions,
) -> AppResult<Value> {
    let mut form = reqwest::multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part(
            field_name.to_string(),
            reqwest::multipart::Part::bytes(bytes.to_vec()).file_name(file_name.to_string()),
        );
    if let Some(thread_id) = thread_id {
        form = form.text("message_thread_id", thread_id.to_string());
    }
    if let Some(value) = send_options.disable_notification {
        form = form.text("disable_notification", value.to_string());
    }
    if let Some(value) = send_options.protect_content {
        form = form.text("protect_content", value.to_string());
    }
    let response = client
        .post(telegram_url(settings, method)?)
        .header(reqwest::header::ACCEPT, "application/json")
        .multipart(form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Telegram {method} failed: {error}")))?;
    telegram_response_json(response, method).await
}

pub(super) fn telegram_media_method(
    file_path: &str,
    is_voice: bool,
    force_document: bool,
) -> (&'static str, &'static str) {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !force_document {
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "webp" | "gif" => return ("sendPhoto", "photo"),
            "mp4" | "mov" | "avi" | "mkv" | "3gp" => return ("sendVideo", "video"),
            "ogg" | "opus" if is_voice => return ("sendVoice", "voice"),
            "mp3" | "m4a" => return ("sendAudio", "audio"),
            _ => {}
        }
    }
    ("sendDocument", "document")
}

pub(super) fn telegram_effective_thread_id_for_send(thread_id: &str) -> Option<&str> {
    let thread_id = thread_id.trim();
    if thread_id.is_empty() || thread_id == "1" {
        None
    } else {
        Some(thread_id)
    }
}

pub(super) fn telegram_url(settings: &TelegramSettings, method: &str) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!(
        "{}/bot{}/{}",
        settings.api_base_url,
        percent_encode_path_segment(&settings.bot_token),
        percent_encode_path_segment(method)
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Telegram URL: {error}")))
}

async fn telegram_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Telegram {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid Telegram {label} JSON: {error}")))?;
    if !status.is_success() || value.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text.trim());
        return Err(AppError::BadRequest(format!(
            "Telegram {label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(crate) async fn start_telegram_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.telegram;
    let settings = telegram_settings(&config)?;
    let client = telegram_client(&settings)?;
    let me = telegram_request(&client, &settings, "getMe", None).await?;
    let user = me.get("result").cloned().unwrap_or_else(|| json!({}));
    let bot_user_id = user.get("id").map(value_to_string).unwrap_or_default();
    let bot_username = user
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim_start_matches('@')
        .to_string();
    if bot_user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Telegram getMe returned no bot user id".into(),
        ));
    }

    let state = store.update_telegram_adapter_state(Some("starting"), None, None, 0, 0)?;
    emit_platform_adapter_event(&app, "starting", "telegram", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        telegram_adapter_loop(app, store_for_task, settings, bot_user_id, bot_username).await;
    });
    store.register_telegram_adapter_task(task.abort_handle())?;
    store.telegram_adapter_state()
}

pub(crate) fn stop_telegram_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_telegram_adapter_task()
}

pub(super) fn telegram_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| telegram_env_bool("TELEGRAM_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| telegram_env_bool("TELEGRAM_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && telegram_configured(config)
}

async fn telegram_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: TelegramSettings,
    bot_user_id: String,
    bot_username: String,
) {
    let mut offset: Option<i64> = None;
    loop {
        let result = telegram_poll_once(
            &app,
            &store,
            &settings,
            &bot_user_id,
            &bot_username,
            &mut offset,
        )
        .await;
        match result {
            Ok(()) => {
                if let Ok(state) =
                    store.update_telegram_adapter_state(Some("running"), None, None, 0, 0)
                {
                    emit_platform_adapter_event(&app, "poll", "telegram", &state);
                }
            }
            Err(error) => {
                let message = error.to_string();
                let lower = message.to_ascii_lowercase();
                let conflict = message.contains("409") || lower.contains("conflict");
                let auth_error = message.contains("401") || message.contains("403");
                if let Ok(state) = store.update_telegram_adapter_state(
                    Some(if conflict || auth_error {
                        "stopped"
                    } else {
                        "reconnecting"
                    }),
                    None,
                    Some(message),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app,
                        if conflict {
                            "conflict"
                        } else if auth_error {
                            "auth_failed"
                        } else {
                            "reconnecting"
                        },
                        "telegram",
                        &state,
                    );
                }
                if conflict || auth_error {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn telegram_poll_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &TelegramSettings,
    bot_user_id: &str,
    bot_username: &str,
    offset: &mut Option<i64>,
) -> AppResult<()> {
    let config = store.config()?.telegram;
    let client = telegram_client(settings)?;
    let timeout = config
        .get("pollTimeoutSeconds")
        .or_else(|| config.get("poll_timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| settings.timeout_seconds.saturating_sub(2).clamp(1, 50));
    let mut body = json!({
        "timeout": timeout,
        "allowed_updates": ["message", "edited_message"],
    });
    if let Some(offset) = offset {
        body["offset"] = json!(*offset);
    }
    let response = telegram_request(&client, settings, "getUpdates", Some(body)).await?;
    let updates = response
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !updates.is_empty() {
        let state = store.update_telegram_adapter_state(
            Some("running"),
            Some(json!({"type": "poll", "count": updates.len()})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(app, "connected", "telegram", &state);
    }
    let mut inbound_events = Vec::new();
    for update in updates {
        if let Some(update_id) = update.get("update_id").and_then(Value::as_i64) {
            *offset = Some(update_id + 1);
        }
        let Some(inbound) =
            telegram_inbound_event_from_update(&update, &config, bot_user_id, bot_username)
        else {
            continue;
        };
        inbound_events.push(inbound);
    }
    for inbound in telegram_merge_inbound_events(inbound_events, &config) {
        let inbound_fallback = inbound.clone();
        let inbound = telegram_enrich_inbound_files(store, settings, inbound)
            .await
            .unwrap_or_else(|error| {
                let mut fallback = inbound_fallback;
                fallback["fileDownloadError"] = json!(error.to_string());
                fallback["file_download_error"] = json!(error.to_string());
                fallback
            });
        let prompt = telegram_inbound_prompt(&inbound);
        let Some(prompt) =
            apply_pre_gateway_dispatch_hooks(store, "telegram", &inbound, prompt).await
        else {
            let state =
                store.update_telegram_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
            emit_platform_adapter_event(app, "inbound_ignored", "telegram", &state);
            continue;
        };
        let conversation_id = telegram_inbound_conversation_id(store, &config)?;
        let persona_id = telegram_inbound_persona_id(store, &config)?;
        let state =
            store.update_telegram_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
        emit_platform_adapter_event(app, "inbound_triggered", "telegram", &state);
        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    }
    Ok(())
}

pub(super) fn telegram_inbound_event_from_update(
    update: &Value,
    config: &Value,
    bot_user_id: &str,
    bot_username: &str,
) -> Option<Value> {
    let message = update
        .get("message")
        .or_else(|| update.get("edited_message"))?;
    let message_id = message
        .get("message_id")
        .map(value_to_string)
        .filter(|value| !value.is_empty())?;
    let chat = message.get("chat")?;
    let chat_id = chat
        .get("id")
        .map(value_to_string)
        .filter(|value| !value.is_empty())?;
    let chat_type = chat
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("private")
        .to_string();
    let from = message.get("from").cloned().unwrap_or_else(|| json!({}));
    let sender_id = from.get("id").map(value_to_string).unwrap_or_default();
    if !bot_user_id.is_empty() && sender_id == bot_user_id {
        return None;
    }
    if from.get("is_bot").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let raw_text = string_arg(message, &["text", "caption"]).unwrap_or_default();
    let files = telegram_update_files(message);
    let file_ids = files
        .iter()
        .filter_map(|file| file.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    if raw_text.trim().is_empty() && file_ids.is_empty() {
        return None;
    }
    let topic_id = message
        .get("message_thread_id")
        .map(value_to_string)
        .filter(|value| !value.is_empty());
    if !telegram_chat_allowed(
        config,
        "allowedChats",
        "allowed_chats",
        "TELEGRAM_ALLOWED_CHATS",
        &chat_id,
    ) {
        return None;
    }
    if let Some(topic_id) = topic_id.as_deref() {
        if !telegram_topic_allowed(config, topic_id) {
            return None;
        }
    }
    let is_private = chat_type == "private";
    let free_response = telegram_chat_allowed(
        config,
        "freeResponseChats",
        "free_response_chats",
        "TELEGRAM_FREE_RESPONSE_CHATS",
        &chat_id,
    );
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| telegram_env_bool("TELEGRAM_REQUIRE_MENTION"))
        .unwrap_or(!is_private);
    let mention_patterns = telegram_mention_patterns(bot_username);
    let lower_text = raw_text.to_ascii_lowercase();
    let mentioned = mention_patterns
        .iter()
        .any(|pattern| lower_text.contains(pattern));
    let command = raw_text.trim_start().starts_with('/');
    if !is_private && require_mention && !free_response && !mentioned && !command {
        return None;
    }
    let text = telegram_strip_mentions(&raw_text, &mention_patterns);
    let message_type = telegram_message_type(message);
    let media_group_id = message
        .get("media_group_id")
        .or_else(|| message.get("mediaGroupId"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(json!({
        "platform": "telegram",
        "updateId": update.get("update_id").cloned().unwrap_or(Value::Null),
        "update_id": update.get("update_id").cloned().unwrap_or(Value::Null),
        "messageId": message_id,
        "message_id": message_id,
        "text": text,
        "messageType": message_type,
        "message_type": message_type,
        "files": files,
        "fileIds": file_ids.clone(),
        "file_ids": file_ids,
        "source": {
            "platform": "telegram",
            "chatId": chat_id,
            "chat_id": chat_id,
            "chatType": chat_type,
            "chat_type": chat_type,
            "chatTitle": chat.get("title").or_else(|| chat.get("username")).cloned().unwrap_or(Value::Null),
            "chat_title": chat.get("title").or_else(|| chat.get("username")).cloned().unwrap_or(Value::Null),
            "userId": sender_id,
            "user_id": sender_id,
            "userName": telegram_sender_name(&from),
            "user_name": telegram_sender_name(&from),
            "threadId": topic_id.clone(),
            "thread_id": topic_id,
            "mediaGroupId": media_group_id.clone(),
            "media_group_id": media_group_id,
        }
    }))
}

fn telegram_merge_inbound_events(events: Vec<Value>, config: &Value) -> Vec<Value> {
    if events.len() <= 1 {
        return events;
    }
    let text_batching = config
        .get("textBatching")
        .or_else(|| config.get("text_batching"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut merged = Vec::<Value>::new();
    let mut indexes = HashMap::<String, usize>::new();
    for event in events {
        let Some(key) = telegram_merge_key(&event, text_batching) else {
            merged.push(event);
            continue;
        };
        if let Some(index) = indexes.get(&key).copied() {
            telegram_merge_event_into(&mut merged[index], event);
        } else {
            indexes.insert(key, merged.len());
            merged.push(event);
        }
    }
    merged
}

fn telegram_merge_key(event: &Value, text_batching: bool) -> Option<String> {
    let source = event.get("source")?;
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_id = source
        .get("userId")
        .or_else(|| source.get("user_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = source
        .get("threadId")
        .or_else(|| source.get("thread_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(media_group_id) = source
        .get("mediaGroupId")
        .or_else(|| source.get("media_group_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(format!("media:{chat_id}:{thread_id}:{media_group_id}"));
    }
    let has_files = event
        .get("files")
        .and_then(Value::as_array)
        .map(|files| !files.is_empty())
        .unwrap_or(false);
    let message_type = event
        .get("messageType")
        .or_else(|| event.get("message_type"))
        .and_then(Value::as_str)
        .unwrap_or("text");
    if text_batching && !has_files && message_type == "text" {
        return Some(format!("text:{chat_id}:{thread_id}:{user_id}"));
    }
    None
}

fn telegram_merge_event_into(target: &mut Value, next: Value) {
    let target_text = target
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let next_text = next
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !next_text.is_empty() {
        target["text"] = json!(if target_text.is_empty() {
            next_text
        } else {
            format!("{target_text}\n{next_text}")
        });
    }
    telegram_append_json_array(target, "files", next.get("files").cloned());
    telegram_append_json_array(target, "fileIds", next.get("fileIds").cloned());
    telegram_append_json_array(target, "file_ids", next.get("file_ids").cloned());
    let target_message_id = target.get("messageId").cloned();
    let next_message_id = next.get("messageId").cloned();
    let target_message_id_snake = target.get("message_id").cloned();
    let next_message_id_snake = next.get("message_id").cloned();
    let target_update_id = target.get("updateId").cloned();
    let next_update_id = next.get("updateId").cloned();
    let target_update_id_snake = target.get("update_id").cloned();
    let next_update_id_snake = next.get("update_id").cloned();
    telegram_append_scalar_as_array(target, "messageIds", target_message_id);
    telegram_append_scalar_as_array(target, "messageIds", next_message_id);
    telegram_append_scalar_as_array(target, "message_ids", target_message_id_snake);
    telegram_append_scalar_as_array(target, "message_ids", next_message_id_snake);
    telegram_append_scalar_as_array(target, "updateIds", target_update_id);
    telegram_append_scalar_as_array(target, "updateIds", next_update_id);
    telegram_append_scalar_as_array(target, "update_ids", target_update_id_snake);
    telegram_append_scalar_as_array(target, "update_ids", next_update_id_snake);
    let count = target
        .get("batchCount")
        .or_else(|| target.get("batch_count"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
        + 1;
    target["batched"] = json!(true);
    target["batchCount"] = json!(count);
    target["batch_count"] = json!(count);
}

fn telegram_append_json_array(target: &mut Value, key: &str, values: Option<Value>) {
    let Some(values) = values.and_then(|value| value.as_array().cloned()) else {
        return;
    };
    if values.is_empty() {
        return;
    }
    if !target.get(key).is_some_and(Value::is_array) {
        target[key] = json!([]);
    }
    if let Some(existing) = target.get_mut(key).and_then(Value::as_array_mut) {
        existing.extend(values);
    }
}

fn telegram_append_scalar_as_array(target: &mut Value, key: &str, value: Option<Value>) {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return;
    };
    if !target.get(key).is_some_and(Value::is_array) {
        target[key] = json!([]);
    }
    if let Some(existing) = target.get_mut(key).and_then(Value::as_array_mut) {
        if !existing.iter().any(|item| item == &value) {
            existing.push(value);
        }
    }
}

fn telegram_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = source
        .get("threadId")
        .or_else(|| source.get("thread_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let batch_count = inbound
        .get("batchCount")
        .or_else(|| inbound.get("batch_count"))
        .and_then(Value::as_u64)
        .unwrap_or(1);
    let file_ids = inbound
        .get("fileIds")
        .or_else(|| inbound.get("file_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut prompt = format!(
        "Telegram inbound message\nchat_id: {chat_id}\nthread_id: {thread_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}"
    );
    if batch_count > 1 {
        prompt.push_str(&format!("\n\nBatch count: {batch_count}"));
        if let Some(message_ids) = inbound
            .get("messageIds")
            .or_else(|| inbound.get("message_ids"))
            .and_then(Value::as_array)
        {
            let ids = message_ids
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                prompt.push_str(&format!("\nMerged message_ids: {}", ids.join(", ")));
            }
        }
    }
    if !file_ids.is_empty() {
        prompt.push_str("\n\nTelegram file_ids:");
        for file_id in file_ids {
            if let Some(file_id) = file_id.as_str() {
                prompt.push_str(&format!("\n- {file_id}"));
            }
        }
    }
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    prompt
}

fn telegram_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("telegram")
    }) {
        return Ok(existing.id);
    }
    let persona_id = telegram_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Telegram".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("telegram"))?;
    Ok(conversation.id)
}

fn telegram_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

fn telegram_update_files(message: &Value) -> Vec<Value> {
    let mut files = Vec::new();
    if let Some(photos) = message.get("photo").and_then(Value::as_array) {
        if let Some(file_id) = photos
            .last()
            .and_then(|photo| photo.get("file_id"))
            .and_then(Value::as_str)
        {
            files.push(json!({
                "id": file_id,
                "name": "photo.jpg",
                "mimeType": "image/jpeg",
                "mime_type": "image/jpeg",
                "type": "photo",
            }));
        }
    }
    if let Some(document) = message.get("document") {
        telegram_push_file_metadata(&mut files, document, "document", "document");
    }
    if let Some(voice) = message.get("voice") {
        telegram_push_file_metadata(&mut files, voice, "voice.ogg", "voice");
    }
    if let Some(audio) = message.get("audio") {
        telegram_push_file_metadata(&mut files, audio, "audio.mp3", "voice");
    }
    if let Some(video) = message.get("video") {
        telegram_push_file_metadata(&mut files, video, "video.mp4", "video");
    }
    if let Some(animation) = message.get("animation") {
        telegram_push_file_metadata(&mut files, animation, "animation.mp4", "video");
    }
    if let Some(sticker) = message.get("sticker") {
        telegram_push_file_metadata(&mut files, sticker, "sticker.webp", "document");
    }
    files
}

fn telegram_push_file_metadata(
    files: &mut Vec<Value>,
    value: &Value,
    fallback_name: &str,
    kind: &str,
) {
    let Some(file_id) = value.get("file_id").and_then(Value::as_str) else {
        return;
    };
    let name = value
        .get("file_name")
        .or_else(|| value.get("fileName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_name);
    let mime = value
        .get("mime_type")
        .or_else(|| value.get("mimeType"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| guess_content_type(name));
    files.push(json!({
        "id": file_id,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": kind,
        "size": value.get("file_size").or_else(|| value.get("fileSize")).cloned().unwrap_or(Value::Null),
    }));
}

fn telegram_message_type(message: &Value) -> &'static str {
    if message.get("photo").is_some() {
        "photo"
    } else if message.get("voice").is_some() || message.get("audio").is_some() {
        "voice"
    } else if message.get("video").is_some() || message.get("animation").is_some() {
        "video"
    } else if message.get("document").is_some() || message.get("sticker").is_some() {
        "document"
    } else {
        "text"
    }
}

fn telegram_chat_allowed(
    config: &Value,
    camel_key: &str,
    snake_key: &str,
    env_key: &str,
    chat_id: &str,
) -> bool {
    let allowed = telegram_string_set(config, &[camel_key, snake_key], env_key);
    allowed.is_empty() || allowed.contains("*") || allowed.contains(chat_id)
}

fn telegram_topic_allowed(config: &Value, topic_id: &str) -> bool {
    let allowed = telegram_string_set(
        config,
        &["allowedTopics", "allowed_topics"],
        "TELEGRAM_ALLOWED_TOPICS",
    );
    allowed.is_empty() || allowed.contains("*") || allowed.contains(topic_id)
}

fn telegram_string_set(config: &Value, keys: &[&str], env_key: &str) -> HashSet<String> {
    let mut values = HashSet::new();
    for key in keys {
        if let Some(value) = config.get(*key) {
            collect_string_set_value(value, &mut values);
        }
    }
    if let Ok(value) = std::env::var(env_key) {
        collect_string_set_text(&value, &mut values);
    }
    values
}

fn collect_string_set_value(value: &Value, output: &mut HashSet<String>) {
    if let Some(value) = value.as_str() {
        collect_string_set_text(value, output);
        return;
    }
    if let Some(items) = value.as_array() {
        for item in items {
            if let Some(item) = item.as_str() {
                collect_string_set_text(item, output);
            } else if !item.is_null() {
                collect_string_set_text(&value_to_string(item), output);
            }
        }
    }
}

fn collect_string_set_text(value: &str, output: &mut HashSet<String>) {
    for item in value.split(',') {
        let item = item.trim();
        if !item.is_empty() {
            output.insert(item.to_string());
        }
    }
}

fn telegram_mention_patterns(bot_username: &str) -> Vec<String> {
    let username = bot_username.trim().trim_start_matches('@');
    if username.is_empty() {
        Vec::new()
    } else {
        vec![format!("@{}", username.to_ascii_lowercase())]
    }
}

fn telegram_strip_mentions(text: &str, patterns: &[String]) -> String {
    let mut output = text.to_string();
    for pattern in patterns {
        output = replace_ascii_case_insensitive(&output, pattern, "");
    }
    output.trim().to_string()
}

fn telegram_sender_name(from: &Value) -> String {
    string_arg(from, &["username"])
        .or_else(|| {
            let first = string_arg(from, &["first_name", "firstName"]).unwrap_or_default();
            let last = string_arg(from, &["last_name", "lastName"]).unwrap_or_default();
            let full = format!("{first} {last}").trim().to_string();
            if full.is_empty() {
                None
            } else {
                Some(full)
            }
        })
        .unwrap_or_default()
}

fn value_to_string(value: &Value) -> String {
    if let Some(value) = value.as_str() {
        value.trim().to_string()
    } else if let Some(value) = value.as_i64() {
        value.to_string()
    } else if let Some(value) = value.as_u64() {
        value.to_string()
    } else {
        value.to_string().trim_matches('"').to_string()
    }
}

async fn telegram_enrich_inbound_files(
    store: &AppStore,
    settings: &TelegramSettings,
    mut inbound: Value,
) -> AppResult<Value> {
    let files = inbound
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            inbound
                .get("fileIds")
                .or_else(|| inbound.get("file_ids"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| {
                    value.as_str().map(|file_id| {
                        json!({
                            "id": file_id,
                            "name": file_id,
                            "mimeType": "application/octet-stream",
                            "mime_type": "application/octet-stream",
                            "type": "document",
                        })
                    })
                })
                .collect()
        });
    if files.is_empty() {
        return Ok(inbound);
    }

    let client = telegram_client(settings)?;
    let cache_dir = store.data_dir().join("attachments").join("telegram");
    fs::create_dir_all(&cache_dir)?;
    let mut attachments = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    let mut skipped_files = Vec::new();

    for file in files {
        match telegram_download_inbound_file(&client, settings, &cache_dir, &file).await {
            Ok(attachment) => {
                if let Some(path) = attachment.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                attachments.push(attachment);
            }
            Err(error) => skipped_files.push(json!({
                "id": file.get("id").cloned().unwrap_or(Value::Null),
                "error": error.to_string(),
            })),
        }
    }

    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        if inbound.get("messageType").and_then(Value::as_str) == Some("text")
            || inbound.get("message_type").and_then(Value::as_str) == Some("text")
        {
            let message_type = telegram_message_type_from_media(&inbound["mediaTypes"]);
            inbound["messageType"] = json!(message_type);
            inbound["message_type"] = json!(message_type);
        }
    }
    if !skipped_files.is_empty() {
        inbound["skippedFiles"] = json!(skipped_files);
        inbound["skipped_files"] = inbound["skippedFiles"].clone();
    }
    Ok(inbound)
}

async fn telegram_download_inbound_file(
    client: &reqwest::Client,
    settings: &TelegramSettings,
    cache_dir: &Path,
    file: &Value,
) -> AppResult<Value> {
    let file_id = file
        .get("id")
        .or_else(|| file.get("file_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("empty Telegram file id".into()))?;
    let info = telegram_request(
        client,
        settings,
        "getFile",
        Some(json!({"file_id": file_id})),
    )
    .await?
    .get("result")
    .cloned()
    .unwrap_or_else(|| json!({}));
    let file_path = info
        .get("file_path")
        .or_else(|| info.get("filePath"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Telegram getFile returned no file_path".into()))?;
    let download_response = client
        .get(telegram_file_url(settings, file_path)?)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Telegram file download failed: {error}")))?;
    let status = download_response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Telegram file download failed ({})",
            status.as_u16()
        )));
    }
    let bytes = download_response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Telegram file download: {error}"))
    })?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Path::new(file_path)
                .file_name()
                .and_then(|value| value.to_str())
        })
        .unwrap_or("attachment");
    let mime = file
        .get("mimeType")
        .or_else(|| file.get("mime_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| guess_content_type(name));
    let safe_name = mattermost_safe_file_name(name);
    let safe_id = mattermost_safe_file_name(file_id);
    let path = cache_dir.join(format!("{safe_id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    Ok(json!({
        "id": file_id,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": telegram_media_kind(mime),
        "size": bytes.len(),
        "path": path.to_string_lossy(),
        "telegramFilePath": file_path,
        "telegram_file_path": file_path,
    }))
}

fn telegram_file_url(settings: &TelegramSettings, file_path: &str) -> AppResult<reqwest::Url> {
    let encoded_path = file_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/");
    reqwest::Url::parse(&format!(
        "{}/file/bot{}/{}",
        settings.api_base_url,
        percent_encode_path_segment(&settings.bot_token),
        encoded_path
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Telegram file URL: {error}")))
}

fn telegram_message_type_from_media(media_types: &Value) -> &'static str {
    let media_types = media_types.as_array().cloned().unwrap_or_default();
    if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
    }) {
        "photo"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("audio/"))
            .unwrap_or(false)
    }) {
        "voice"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("video/"))
            .unwrap_or(false)
    }) {
        "video"
    } else {
        "document"
    }
}

fn telegram_media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "document"
    }
}

#[derive(Debug, Clone)]
pub(super) struct SlackSettings {
    pub(super) api_base_url: String,
    pub(super) bot_token: String,
    pub(super) app_token: Option<String>,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn slack_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = slack_settings(&store.config()?.slack)?;
    let client = slack_client(&settings)?;
    let mut channel = required_string_arg(
        payload,
        &["channel_id", "channelId", "chat_id", "chatId", "target"],
        "send_message slack",
    )?;
    let message = required_string_arg(
        payload,
        &["message", "content", "text", "body"],
        "send_message slack",
    )?;
    if !discord_media_file_paths(payload)?.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Slack routing does not support MEDIA attachments yet".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "Slack message text cannot exceed 4000 characters in one send_message chunk".into(),
        ));
    }
    let slack_user_id = string_arg(
        payload,
        &["slack_user_id", "slackUserId", "user_id", "userId"],
    )
    .or_else(|| {
        if slack_target_is_user_id(&channel) {
            Some(channel.clone())
        } else {
            None
        }
    });
    if let Some(user_id) = slack_user_id.as_deref() {
        channel = slack_open_dm_channel(&client, &settings, user_id).await?;
    }
    let thread_ts = string_arg(
        payload,
        &[
            "thread_ts",
            "threadTs",
            "thread_id",
            "threadId",
            "message_id",
            "messageId",
        ],
    );
    let mut body = json!({
        "channel": channel,
        "text": message,
    });
    if let Some(thread_ts) = thread_ts.as_deref() {
        body["thread_ts"] = json!(thread_ts);
    }
    let result = slack_request(&client, &settings, "chat.postMessage", Some(body)).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "slack",
        "channel": channel,
        "slack_user_id": slack_user_id,
        "thread_ts": thread_ts,
        "message_id": result.get("ts").and_then(Value::as_str),
        "raw": result,
    }))?)
}

pub(super) async fn slack_open_dm_channel(
    client: &reqwest::Client,
    settings: &SlackSettings,
    user_id: &str,
) -> AppResult<String> {
    let user_id = user_id.trim();
    if !slack_target_is_user_id(user_id) {
        return Err(AppError::BadRequest(format!(
            "Slack DM target must be a Slack user id like U12345678, got '{user_id}'"
        )));
    }
    let result = slack_request(
        client,
        settings,
        "conversations.open",
        Some(json!({ "users": user_id })),
    )
    .await?;
    result
        .get("channel")
        .and_then(|channel| channel.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Slack conversations.open did not return a DM channel id; check bot permissions (im:write)"
                    .into(),
            )
        })
}

pub(super) fn slack_target_is_user_id(value: &str) -> bool {
    let value = value.trim();
    value.len() >= 9
        && value.starts_with('U')
        && value
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

pub(super) fn slack_settings(config: &Value) -> AppResult<SlackSettings> {
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("SLACK_API_BASE_URL").ok())
    .unwrap_or_else(|| "https://slack.com/api".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Slack apiBaseUrl: {error}")))?;
    let bot_token = string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("SLACK_BOT_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Slack send_message requires settings.slack.botToken or SLACK_BOT_TOKEN".into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(SlackSettings {
        api_base_url,
        bot_token,
        app_token: string_arg(config, &["appToken", "app_token", "socketModeToken"])
            .or_else(|| std::env::var("SLACK_APP_TOKEN").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        timeout_seconds,
    })
}

pub(super) fn slack_configured(config: &Value) -> bool {
    string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("SLACK_BOT_TOKEN").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

pub(super) fn slack_runtime_configured(config: &Value) -> bool {
    slack_configured(config)
        && string_arg(config, &["appToken", "app_token", "socketModeToken"])
            .or_else(|| std::env::var("SLACK_APP_TOKEN").ok())
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
}

pub(super) fn slack_client(settings: &SlackSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Slack client: {error}")))
}

async fn slack_request(
    client: &reqwest::Client,
    settings: &SlackSettings,
    method: &str,
    body: Option<Value>,
) -> AppResult<Value> {
    let url = slack_url(settings, method)?;
    let mut request = client
        .post(url)
        .bearer_auth(&settings.bot_token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&strip_null_json_object(body));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Slack {method} failed: {error}")))?;
    slack_response_json(response, method).await
}

pub(super) fn slack_url(settings: &SlackSettings, method: &str) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!(
        "{}/{}",
        settings.api_base_url,
        percent_encode_path_segment(method)
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Slack URL: {error}")))
}

async fn slack_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Slack {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid Slack {label} JSON: {error}")))?;
    if !status.is_success() || value.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text.trim());
        return Err(AppError::BadRequest(format!(
            "Slack {label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(crate) async fn start_slack_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.slack;
    let settings = slack_settings(&config)?;
    if settings.app_token.as_deref().unwrap_or_default().is_empty() {
        return Err(AppError::BadRequest(
            "Slack runtime requires settings.slack.appToken or SLACK_APP_TOKEN for Socket Mode"
                .into(),
        ));
    }
    let client = slack_client(&settings)?;
    let auth = slack_request(&client, &settings, "auth.test", None).await?;
    let bot_user_id = auth
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if bot_user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Slack auth.test returned no bot user_id".into(),
        ));
    }
    let state = store.update_slack_adapter_state(Some("starting"), None, None, 0, 0)?;
    emit_platform_adapter_event(&app, "starting", "slack", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        slack_adapter_loop(app, store_for_task, settings, bot_user_id).await;
    });
    store.register_slack_adapter_task(task.abort_handle())?;
    store.slack_adapter_state()
}

pub(crate) fn stop_slack_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_slack_adapter_task()
}

pub(super) fn slack_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SLACK_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SLACK_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && slack_runtime_configured(config)
}

async fn slack_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: SlackSettings,
    bot_user_id: String,
) {
    loop {
        match slack_adapter_connect_once(&app, &store, &settings, &bot_user_id).await {
            Ok(()) => {
                if let Ok(state) =
                    store.update_slack_adapter_state(Some("reconnecting"), None, None, 0, 0)
                {
                    emit_platform_adapter_event(&app, "reconnecting", "slack", &state);
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(error) => {
                let message = error.to_string();
                let auth_error = message.contains("not_authed")
                    || message.contains("invalid_auth")
                    || message.contains("token_revoked")
                    || message.contains("account_inactive");
                if let Ok(state) = store.update_slack_adapter_state(
                    Some(if auth_error {
                        "stopped"
                    } else {
                        "reconnecting"
                    }),
                    None,
                    Some(message),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app,
                        if auth_error {
                            "auth_failed"
                        } else {
                            "reconnecting"
                        },
                        "slack",
                        &state,
                    );
                }
                if auth_error {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn slack_adapter_connect_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &SlackSettings,
    bot_user_id: &str,
) -> AppResult<()> {
    let client = slack_client(settings)?;
    let connection = slack_apps_connection_open(&client, settings).await?;
    let websocket_url = connection
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Slack apps.connections.open returned no url".into()))?
        .to_string();
    let (ws, _) = connect_async(&websocket_url).await.map_err(|error| {
        AppError::BadRequest(format!("Slack Socket Mode connect failed: {error}"))
    })?;
    let (mut writer, mut reader) = ws.split();
    let state = store.update_slack_adapter_state(
        Some("running"),
        Some(json!({
            "type": "connected",
            "websocketUrl": websocket_url,
        })),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(app, "connected", "slack", &state);

    while let Some(message) = reader.next().await {
        let message = message.map_err(|error| {
            AppError::BadRequest(format!("Slack Socket Mode read failed: {error}"))
        })?;
        let text = match message {
            WsMessage::Text(text) => text.to_string(),
            WsMessage::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            WsMessage::Close(frame) => {
                return Err(AppError::BadRequest(format!(
                    "Slack Socket Mode closed: {:?}",
                    frame
                )));
            }
            _ => continue,
        };
        let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(envelope_id) = envelope.get("envelope_id").and_then(Value::as_str) {
            writer
                .send(WsMessage::Text(
                    json!({"envelope_id": envelope_id}).to_string().into(),
                ))
                .await
                .map_err(|error| {
                    AppError::BadRequest(format!("Slack Socket Mode ack failed: {error}"))
                })?;
        }
        let config = store.config()?.slack;
        let Some(inbound) = slack_inbound_event_from_envelope(&envelope, &config, bot_user_id)
        else {
            continue;
        };
        let inbound_fallback = inbound.clone();
        let inbound = slack_enrich_inbound_files(store, settings, inbound)
            .await
            .unwrap_or_else(|error| {
                let mut fallback = inbound_fallback;
                fallback["fileDownloadError"] = json!(error.to_string());
                fallback["file_download_error"] = json!(error.to_string());
                fallback
            });
        let prompt = slack_inbound_prompt(&inbound);
        let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, "slack", &inbound, prompt).await
        else {
            let state =
                store.update_slack_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
            emit_platform_adapter_event(app, "inbound_ignored", "slack", &state);
            continue;
        };
        let conversation_id = slack_inbound_conversation_id(store, &config)?;
        let persona_id = slack_inbound_persona_id(store, &config)?;
        let state = store.update_slack_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
        emit_platform_adapter_event(app, "inbound_triggered", "slack", &state);
        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    }
    Ok(())
}

async fn slack_apps_connection_open(
    client: &reqwest::Client,
    settings: &SlackSettings,
) -> AppResult<Value> {
    let app_token = settings
        .app_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Slack Socket Mode app token is missing".into()))?;
    let response = client
        .post(slack_url(settings, "apps.connections.open")?)
        .bearer_auth(app_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Slack apps.connections.open failed: {error}"))
        })?;
    slack_response_json(response, "apps.connections.open").await
}

fn slack_inbound_event_from_envelope(
    envelope: &Value,
    config: &Value,
    bot_user_id: &str,
) -> Option<Value> {
    if envelope.get("type").and_then(Value::as_str) != Some("events_api") {
        return None;
    }
    let payload = envelope.get("payload")?;
    let event = payload.get("event")?;
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    if event.get("subtype").and_then(Value::as_str).is_some()
        || event.get("bot_id").and_then(Value::as_str).is_some()
    {
        return None;
    }
    let user_id = event
        .get("user")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if user_id == bot_user_id {
        return None;
    }
    let channel_id = event
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if channel_id.is_empty()
        || !matrix_allowed(
            config,
            &["allowedChannels", "allowed_channels"],
            "SLACK_ALLOWED_CHANNELS",
            channel_id,
        )
    {
        return None;
    }
    let text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    let channel_type = event
        .get("channel_type")
        .or_else(|| event.get("channelType"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_dm = channel_type == "im" || channel_id.starts_with('D');
    let free_channels = telegram_string_set(
        config,
        &["freeResponseChannels", "free_response_channels"],
        "SLACK_FREE_RESPONSE_CHANNELS",
    );
    let free_channel = free_channels.contains("*") || free_channels.contains(channel_id);
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SLACK_REQUIRE_MENTION"))
        .unwrap_or(true);
    let mention = format!("<@{bot_user_id}>");
    let mentioned = text.contains(&mention) || text.contains(bot_user_id);
    let command = text.trim_start().starts_with('/');
    if !is_dm && require_mention && !free_channel && !mentioned && !command {
        return None;
    }
    let ts = event.get("ts").and_then(Value::as_str).unwrap_or_default();
    let thread_ts = event
        .get("thread_ts")
        .or_else(|| event.get("threadTs"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cleaned = replace_ascii_case_insensitive(&text, &mention, "")
        .trim()
        .to_string();
    let mut inbound = json!({
        "platform": "slack",
        "eventId": envelope.get("envelope_id").and_then(Value::as_str).unwrap_or_default(),
        "event_id": envelope.get("envelope_id").and_then(Value::as_str).unwrap_or_default(),
        "messageId": ts,
        "message_id": ts,
        "text": cleaned,
        "messageType": if command { "command" } else { "text" },
        "message_type": if command { "command" } else { "text" },
        "source": {
            "platform": "slack",
            "channelId": channel_id,
            "channel_id": channel_id,
            "chatId": channel_id,
            "chat_id": channel_id,
            "userId": user_id,
            "user_id": user_id,
            "threadTs": thread_ts,
            "thread_ts": thread_ts,
            "threadId": thread_ts,
            "thread_id": thread_ts,
            "chatType": if is_dm { "dm" } else { "channel" },
            "chat_type": if is_dm { "dm" } else { "channel" },
        },
        "raw": event,
    });
    let files = slack_files_from_event(event);
    if !files.is_empty() {
        inbound["files"] = json!(files);
    }
    Some(inbound)
}

fn slack_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let channel_id = source
        .get("channelId")
        .or_else(|| source.get("channel_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_id = source
        .get("userId")
        .or_else(|| source.get("user_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_ts = source
        .get("threadTs")
        .or_else(|| source.get("thread_ts"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!(
        "Slack inbound message\nchannel_id: {channel_id}\nthread_ts: {thread_ts}\nmessage_id: {message_id}\nuser: {user_id}\n\n{text}"
    );
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    if let Some(skipped) = inbound
        .get("skippedFiles")
        .or_else(|| inbound.get("skipped_files"))
        .and_then(Value::as_array)
    {
        if !skipped.is_empty() {
            prompt.push_str("\n\nSkipped Slack attachments:");
            for item in skipped {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                let error = item
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                prompt.push_str(&format!("\n- {id}: {error}"));
            }
        }
    }
    prompt
}

fn slack_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("slack")
    }) {
        return Ok(existing.id);
    }
    let persona_id = slack_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Slack".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("slack"))?;
    Ok(conversation.id)
}

fn slack_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

fn slack_files_from_event(event: &Value) -> Vec<Value> {
    event
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| {
            let id = file
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let name = file
                .get("name")
                .or_else(|| file.get("title"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("attachment");
            let mime = file
                .get("mimetype")
                .or_else(|| file.get("mimeType"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| guess_content_type(name));
            let url = file
                .get("url_private_download")
                .or_else(|| file.get("url_private"))
                .or_else(|| file.get("urlPrivateDownload"))
                .or_else(|| file.get("urlPrivate"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(json!({
                "id": id,
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": slack_media_kind(mime),
                "url": url,
                "size": file.get("size").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect()
}

async fn slack_enrich_inbound_files(
    store: &AppStore,
    settings: &SlackSettings,
    mut inbound: Value,
) -> AppResult<Value> {
    let files = inbound
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        return Ok(inbound);
    }

    let client = slack_client(settings)?;
    let cache_dir = store.data_dir().join("attachments").join("slack");
    fs::create_dir_all(&cache_dir)?;
    let mut attachments = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    let mut skipped_files = Vec::new();

    for file in files {
        match slack_download_inbound_file(&client, settings, &cache_dir, &file).await {
            Ok(attachment) => {
                if let Some(path) = attachment.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                attachments.push(attachment);
            }
            Err(error) => skipped_files.push(json!({
                "id": file.get("id").and_then(Value::as_str).unwrap_or("attachment"),
                "error": error.to_string(),
            })),
        }
    }

    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        if inbound.get("messageType").and_then(Value::as_str) == Some("text")
            || inbound.get("message_type").and_then(Value::as_str) == Some("text")
        {
            let message_type = slack_message_type_from_media(&inbound["mediaTypes"]);
            inbound["messageType"] = json!(message_type);
            inbound["message_type"] = json!(message_type);
        }
    }
    if !skipped_files.is_empty() {
        inbound["skippedFiles"] = json!(skipped_files);
        inbound["skipped_files"] = inbound["skippedFiles"].clone();
    }
    Ok(inbound)
}

async fn slack_download_inbound_file(
    client: &reqwest::Client,
    settings: &SlackSettings,
    cache_dir: &Path,
    file: &Value,
) -> AppResult<Value> {
    let file_id = file
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("empty Slack file id".into()))?;
    let url = file
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Slack file has no private download URL".into()))?;
    let download_response = client
        .get(url)
        .bearer_auth(&settings.bot_token)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Slack file download failed: {error}")))?;
    let status = download_response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Slack file download failed ({})",
            status.as_u16()
        )));
    }
    let bytes = download_response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Slack file download: {error}"))
    })?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    let mime = file
        .get("mimeType")
        .or_else(|| file.get("mime_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| guess_content_type(name));
    let safe_name = mattermost_safe_file_name(name);
    let path = cache_dir.join(format!("{file_id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    Ok(json!({
        "id": file_id,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": slack_media_kind(mime),
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    }))
}

fn slack_message_type_from_media(media_types: &Value) -> &'static str {
    let media_types = media_types.as_array().cloned().unwrap_or_default();
    if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
    }) {
        "photo"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("audio/"))
            .unwrap_or(false)
    }) {
        "voice"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("video/"))
            .unwrap_or(false)
    }) {
        "video"
    } else {
        "document"
    }
}

fn slack_media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "document"
    }
}

#[derive(Clone, Debug)]
pub(super) struct MattermostSettings {
    pub(super) url: String,
    pub(super) token: String,
    pub(super) reply_mode: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn mattermost_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = mattermost_settings(&store.config()?.mattermost)?;
    let client = mattermost_client(&settings)?;
    let channel_id = required_string_arg(
        payload,
        &["channel_id", "channelId", "chat_id", "chatId"],
        "send_message mattermost",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let media_files = discord_media_file_paths(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Mattermost requires message text or media_files".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "Mattermost message text cannot exceed 4000 characters in one send_message chunk"
                .into(),
        ));
    }
    let root_id = string_arg(
        payload,
        &[
            "root_id",
            "rootId",
            "reply_to",
            "replyTo",
            "message_id",
            "messageId",
        ],
    )
    .filter(|_| settings.reply_mode == "thread");
    let root_id = if let Some(root_id) = root_id.as_deref() {
        Some(mattermost_resolve_root_id(&client, &settings, root_id).await?)
    } else {
        None
    };
    let mut file_ids = Vec::new();
    let mut skipped_files = Vec::new();
    for file_path in &media_files {
        match mattermost_upload_file(&client, &settings, &channel_id, file_path).await {
            Ok(file_id) => file_ids.push(file_id),
            Err(error) if error.to_string().contains("file not found") => {
                skipped_files.push(file_path.clone());
            }
            Err(error) => return Err(error),
        }
    }
    if message.trim().is_empty() && file_ids.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Mattermost has no deliverable text or existing media files".into(),
        ));
    }
    let results = mattermost_create_posts(
        &client,
        &settings,
        &channel_id,
        &message,
        root_id.as_deref(),
        &file_ids,
    )
    .await?;
    let message_id = results
        .last()
        .and_then(|result| result.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "mattermost",
        "chat_id": channel_id,
        "message_id": message_id,
        "file_count": file_ids.len(),
        "skipped_files": skipped_files,
        "results": results,
    }))?)
}

pub(super) fn mattermost_settings(config: &Value) -> AppResult<MattermostSettings> {
    let url = string_arg(
        config,
        &["url", "baseUrl", "base_url", "serverUrl", "server_url"],
    )
    .or_else(|| std::env::var("MATTERMOST_URL").ok())
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        AppError::BadRequest(
            "Mattermost send_message requires settings.mattermost.url or MATTERMOST_URL".into(),
        )
    })?;
    reqwest::Url::parse(&url)
        .map_err(|error| AppError::BadRequest(format!("invalid Mattermost URL: {error}")))?;
    let token = string_arg(
        config,
        &[
            "token",
            "botToken",
            "bot_token",
            "accessToken",
            "access_token",
        ],
    )
    .or_else(|| std::env::var("MATTERMOST_TOKEN").ok())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        AppError::BadRequest(
            "Mattermost send_message requires settings.mattermost.token or MATTERMOST_TOKEN".into(),
        )
    })?;
    let reply_mode = string_arg(config, &["replyMode", "reply_mode"])
        .or_else(|| std::env::var("MATTERMOST_REPLY_MODE").ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "off".into());
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(MattermostSettings {
        url,
        token,
        reply_mode,
        timeout_seconds,
    })
}

pub(super) fn mattermost_configured(config: &Value) -> bool {
    mattermost_settings(config).is_ok()
}

pub(super) fn mattermost_client(settings: &MattermostSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Mattermost client: {error}"))
        })
}

pub(super) fn mattermost_api_url(
    settings: &MattermostSettings,
    path: &str,
) -> AppResult<reqwest::Url> {
    if !path.starts_with('/') || path.contains('\\') {
        return Err(AppError::BadRequest(format!(
            "invalid Mattermost API path: {path}"
        )));
    }
    reqwest::Url::parse(&format!("{}/api/v4{path}", settings.url)).map_err(|error| {
        AppError::BadRequest(format!("invalid Mattermost API URL for {path}: {error}"))
    })
}

async fn mattermost_create_posts(
    client: &reqwest::Client,
    settings: &MattermostSettings,
    channel_id: &str,
    message: &str,
    root_id: Option<&str>,
    file_ids: &[String],
) -> AppResult<Vec<Value>> {
    let mut results = Vec::new();
    if file_ids.is_empty() {
        results.push(
            mattermost_create_post(client, settings, channel_id, message, root_id, &[]).await?,
        );
        return Ok(results);
    }
    for (index, chunk) in file_ids.chunks(5).enumerate() {
        let chunk_message = if index == 0 { message } else { "" };
        results.push(
            mattermost_create_post(client, settings, channel_id, chunk_message, root_id, chunk)
                .await?,
        );
    }
    Ok(results)
}

async fn mattermost_create_post(
    client: &reqwest::Client,
    settings: &MattermostSettings,
    channel_id: &str,
    message: &str,
    root_id: Option<&str>,
    file_ids: &[String],
) -> AppResult<Value> {
    let mut body = json!({
        "channel_id": channel_id,
        "message": mattermost_format_message(message),
    });
    if let Some(root_id) = root_id.map(str::trim).filter(|value| !value.is_empty()) {
        body["root_id"] = json!(root_id);
    }
    if !file_ids.is_empty() {
        body["file_ids"] = json!(file_ids);
    }
    let response = client
        .post(mattermost_api_url(settings, "/posts")?)
        .bearer_auth(&settings.token)
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Mattermost send failed: {error}")))?;
    mattermost_response_json(response, "Mattermost post").await
}

async fn mattermost_resolve_root_id(
    client: &reqwest::Client,
    settings: &MattermostSettings,
    post_id: &str,
) -> AppResult<String> {
    let post_id = post_id.trim();
    if post_id.is_empty() {
        return Ok(String::new());
    }
    let response = client
        .get(mattermost_api_url(
            settings,
            &format!("/posts/{}", percent_encode_path_segment(post_id)),
        )?)
        .bearer_auth(&settings.token)
        .send()
        .await
        .map_err(|_| AppError::BadRequest("Mattermost post lookup failed".into()));
    let Ok(response) = response else {
        return Ok(post_id.to_string());
    };
    if !response.status().is_success() {
        return Ok(post_id.to_string());
    }
    let value = mattermost_response_json(response, "Mattermost post lookup")
        .await
        .unwrap_or_else(|_| json!({}));
    Ok(value
        .get("root_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(post_id)
        .to_string())
}

async fn mattermost_upload_file(
    client: &reqwest::Client,
    settings: &MattermostSettings,
    channel_id: &str,
    file_path: &str,
) -> AppResult<String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(AppError::BadRequest(format!(
            "Mattermost file not found: {file_path}"
        )));
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment")
        .to_string();
    let bytes = fs::read(path)
        .map_err(|error| AppError::BadRequest(format!("failed to read {file_path}: {error}")))?;
    let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name);
    let form = reqwest::multipart::Form::new()
        .text("channel_id", channel_id.to_string())
        .part("files", part);
    let response = client
        .post(mattermost_api_url(settings, "/files")?)
        .bearer_auth(&settings.token)
        .multipart(form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Mattermost upload failed: {error}")))?;
    let value = mattermost_response_json(response, "Mattermost file upload").await?;
    value
        .get("file_infos")
        .and_then(Value::as_array)
        .and_then(|infos| infos.first())
        .and_then(|info| info.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("Mattermost file upload returned no file id".into()))
}

async fn mattermost_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} failed ({}): {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    Ok(value)
}

pub(super) async fn mattermost_channel_directory(config: &Value) -> AppResult<Value> {
    let settings = mattermost_settings(config)?;
    let client = mattermost_client(&settings)?;
    let teams_response = client
        .get(mattermost_api_url(&settings, "/users/me/teams")?)
        .bearer_auth(&settings.token)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Mattermost team directory fetch failed: {error}"))
        })?;
    let teams_value = mattermost_response_json(teams_response, "Mattermost teams").await?;
    let teams = teams_value
        .as_array()
        .ok_or_else(|| AppError::BadRequest("Mattermost teams response must be an array".into()))?
        .clone();
    let mut channels_by_team = Vec::new();
    for team in &teams {
        let Some(team_id) = team
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let channels_response = client
            .get(mattermost_api_url(
                &settings,
                &format!(
                    "/users/me/teams/{}/channels",
                    percent_encode_path_segment(team_id)
                ),
            )?)
            .bearer_auth(&settings.token)
            .send()
            .await
            .map_err(|error| {
                AppError::BadRequest(format!(
                    "Mattermost channel directory fetch failed for team {team_id}: {error}"
                ))
            })?;
        let channels_value =
            mattermost_response_json(channels_response, "Mattermost team channels").await?;
        channels_by_team.push((team_id.to_string(), channels_value));
    }
    mattermost_channel_directory_from_api(&teams, &channels_by_team)
}

pub(super) fn mattermost_channel_directory_from_api(
    teams: &[Value],
    channels_by_team: &[(String, Value)],
) -> AppResult<Value> {
    let mut team_names = HashMap::new();
    let mut team_display_names = HashMap::new();
    for team in teams {
        let Some(team_id) = team
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let name = team
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(team_id)
            .to_string();
        let display_name = team
            .get("display_name")
            .or_else(|| team.get("displayName"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&name)
            .to_string();
        team_names.insert(team_id.to_string(), name);
        team_display_names.insert(team_id.to_string(), display_name);
    }

    let mut channels = Vec::new();
    let mut seen = HashSet::new();
    for (fallback_team_id, channels_value) in channels_by_team {
        let Some(items) = channels_value.as_array() else {
            return Err(AppError::BadRequest(format!(
                "Mattermost channels response for team {fallback_team_id} must be an array"
            )));
        };
        for channel in items {
            let Some(channel_id) = channel
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !seen.insert(channel_id.to_string()) {
                continue;
            }
            let team_id = channel
                .get("team_id")
                .or_else(|| channel.get("teamId"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(fallback_team_id.as_str());
            let team_name = team_names
                .get(team_id)
                .map(String::as_str)
                .unwrap_or(team_id);
            let team_display_name = team_display_names
                .get(team_id)
                .map(String::as_str)
                .unwrap_or(team_name);
            let name = channel
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(channel_id);
            let display_name = channel
                .get("display_name")
                .or_else(|| channel.get("displayName"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(name);
            let mut aliases = vec![format!("{team_name}/{name}")];
            if display_name != name {
                aliases.push(display_name.to_string());
                aliases.push(format!("{team_name}/{display_name}"));
            }
            if team_display_name != team_name {
                aliases.push(format!("{team_display_name}/{name}"));
                if display_name != name {
                    aliases.push(format!("{team_display_name}/{display_name}"));
                }
            }
            aliases.sort();
            aliases.dedup();
            channels.push(json!({
                "id": channel_id,
                "name": name,
                "display_name": display_name,
                "team": team_name,
                "team_id": team_id,
                "team_display_name": team_display_name,
                "type": mattermost_directory_channel_type(channel.get("type").and_then(Value::as_str).unwrap_or("O")),
                "aliases": aliases,
            }));
        }
    }
    Ok(json!({
        "updated_at": now_iso(),
        "platforms": {
            "mattermost": channels,
        }
    }))
}

fn mattermost_directory_channel_type(raw: &str) -> &'static str {
    match raw {
        "D" => "dm",
        "G" => "group",
        "P" => "private",
        _ => "channel",
    }
}

pub(crate) async fn start_mattermost_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.mattermost;
    let settings = mattermost_settings(&config)?;
    let client = mattermost_client(&settings)?;
    let me = mattermost_current_user(&client, &settings).await?;
    let bot_user_id = me
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let bot_username = me
        .get("username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if bot_user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Mattermost /users/me returned no bot user id".into(),
        ));
    }

    let state = store.update_mattermost_adapter_state(Some("starting"), None, None, 0, 0)?;
    emit_mattermost_adapter_event(&app, "starting", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        mattermost_adapter_loop(app, store_for_task, settings, bot_user_id, bot_username).await;
    });
    store.register_mattermost_adapter_task(task.abort_handle())?;
    store.mattermost_adapter_state()
}

#[derive(Debug, Clone)]
struct WebhookSettings {
    host: String,
    port: u16,
    path: String,
    secret: Option<String>,
    timeout_seconds: u64,
}

pub(crate) async fn start_webhook_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.webhook;
    let settings = webhook_settings(&config)?;
    let address = format!("{}:{}", settings.host, settings.port);
    let listener = TcpListener::bind(&address).await.map_err(|error| {
        AppError::BadRequest(format!("Webhook bind failed on {address}: {error}"))
    })?;
    let listen_url = format!(
        "http://{}:{}{}",
        settings.host, settings.port, settings.path
    );
    let state = store.update_webhook_adapter_state(
        Some("starting"),
        Some(json!({"type": "binding", "listenUrl": listen_url})),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "webhook", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        webhook_adapter_loop(app, store_for_task, settings, listener, listen_url).await;
    });
    store.register_webhook_adapter_task(task.abort_handle())?;
    store.webhook_adapter_state()
}

pub(crate) fn stop_webhook_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_webhook_adapter_task()
}

pub(crate) async fn start_feishu_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.feishu;
    let settings = feishu_webhook_settings(&config)?;
    let address = format!("{}:{}", settings.host, settings.port);
    let listener = TcpListener::bind(&address).await.map_err(|error| {
        AppError::BadRequest(format!("Feishu webhook bind failed on {address}: {error}"))
    })?;
    let listen_url = format!(
        "http://{}:{}{}",
        settings.host, settings.port, settings.path
    );
    let state = store.update_feishu_adapter_state(
        Some("starting"),
        Some(json!({"type": "binding", "listenUrl": listen_url})),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "feishu", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        feishu_webhook_adapter_loop(app, store_for_task, settings, listener, listen_url).await;
    });
    store.register_feishu_adapter_task(task.abort_handle())?;
    store.feishu_adapter_state()
}

pub(crate) fn stop_feishu_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_feishu_adapter_task()
}

pub(crate) async fn start_dingtalk_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.dingtalk;
    let settings = dingtalk_webhook_settings(&config)?;
    let address = format!("{}:{}", settings.host, settings.port);
    let listener = TcpListener::bind(&address).await.map_err(|error| {
        AppError::BadRequest(format!(
            "DingTalk webhook bind failed on {address}: {error}"
        ))
    })?;
    let listen_url = format!(
        "http://{}:{}{}",
        settings.host, settings.port, settings.path
    );
    let state = store.update_dingtalk_adapter_state(
        Some("starting"),
        Some(json!({"type": "binding", "listenUrl": listen_url})),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "dingtalk", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        dingtalk_webhook_adapter_loop(app, store_for_task, settings, listener, listen_url).await;
    });
    store.register_dingtalk_adapter_task(task.abort_handle())?;
    store.dingtalk_adapter_state()
}

pub(crate) fn stop_dingtalk_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_dingtalk_adapter_task()
}

pub(crate) async fn start_email_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let settings = email_settings(&store.config()?.email)?;
    if settings.imap_host.is_none() {
        return Err(AppError::BadRequest(
            "Email runtime requires settings.email.imapHost or EMAIL_IMAP_HOST".into(),
        ));
    }
    let state = store.update_email_adapter_state(
        Some("starting"),
        Some(json!({"type": "poll_start", "mailbox": "INBOX"})),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "email", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        email_adapter_loop(app, store_for_task, settings).await;
    });
    store.register_email_adapter_task(task.abort_handle())?;
    store.email_adapter_state()
}

pub(crate) fn stop_email_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_email_adapter_task()
}

#[derive(Debug, Clone)]
pub(super) struct MessagingGatewayReceiveSettings {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) path: String,
    pub(super) secret: Option<String>,
    pub(super) platforms: HashSet<String>,
    pub(super) timeout_seconds: u64,
    pub(super) weixin_cdn_base_url: String,
}

pub(crate) async fn start_messaging_gateway_adapter(
    store: &AppStore,
    app: AppHandle,
) -> AppResult<Value> {
    let settings = messaging_gateway_receive_settings(&store.config()?.messaging_gateway)?;
    let address = format!("{}:{}", settings.host, settings.port);
    let listener = TcpListener::bind(&address).await.map_err(|error| {
        AppError::BadRequest(format!(
            "Messaging gateway webhook bind failed on {address}: {error}"
        ))
    })?;
    let listen_url = format!(
        "http://{}:{}{}",
        settings.host, settings.port, settings.path
    );
    let state = store.update_messaging_gateway_adapter_state(
        Some("starting"),
        Some(json!({"type": "binding", "listenUrl": listen_url})),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "messaging_gateway", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        messaging_gateway_adapter_loop(app, store_for_task, settings, listener, listen_url).await;
    });
    store.register_messaging_gateway_adapter_task(task.abort_handle())?;
    store.messaging_gateway_adapter_state()
}

pub(crate) fn stop_messaging_gateway_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_messaging_gateway_adapter_task()
}

fn messaging_gateway_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("HERMES_MESSAGING_GATEWAY_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("HERMES_MESSAGING_GATEWAY_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && messaging_gateway_receive_configured(config)
}

fn email_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("EMAIL_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("EMAIL_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && email_runtime_configured(config)
}

fn dingtalk_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DINGTALK_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DINGTALK_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && dingtalk_webhook_configured(config)
}

fn feishu_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("FEISHU_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("FEISHU_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && feishu_webhook_configured(config)
}

pub(crate) async fn start_signal_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let settings = signal_settings(&store.config()?.signal)?;
    let events_url = signal_events_url(&settings)?.to_string();
    let state = store.update_signal_adapter_state(
        Some("starting"),
        Some(json!({
            "type": "connect",
            "eventsUrl": events_url,
            "events_url": events_url,
        })),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(&app, "starting", "signal", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        signal_adapter_loop(app, store_for_task, settings).await;
    });
    store.register_signal_adapter_task(task.abort_handle())?;
    store.signal_adapter_state()
}

pub(crate) fn stop_signal_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_signal_adapter_task()
}

fn signal_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SIGNAL_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SIGNAL_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && signal_configured(config)
}

fn webhook_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("WEBHOOK_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("WEBHOOK_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && webhook_configured(config)
}

fn webhook_configured(config: &Value) -> bool {
    webhook_settings(config).is_ok()
}

fn webhook_settings(config: &Value) -> AppResult<WebhookSettings> {
    let host = string_arg(config, &["host", "bindHost", "bind_host"])
        .or_else(|| std::env::var("WEBHOOK_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".into())
        .trim()
        .to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest("Webhook host cannot be empty".into()));
    }
    let port = config
        .get("port")
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("WEBHOOK_PORT")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(8787)
        .clamp(1, u16::MAX as u64) as u16;
    let mut path = string_arg(config, &["path", "webhookPath", "webhook_path"])
        .or_else(|| std::env::var("WEBHOOK_PATH").ok())
        .unwrap_or_else(|| "/webhooks/synthchat".into())
        .trim()
        .to_string();
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    let secret = string_arg(config, &["secret", "webhookSecret", "webhook_secret"])
        .or_else(|| std::env::var("WEBHOOK_SECRET").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 300);
    Ok(WebhookSettings {
        host,
        port,
        path,
        secret,
        timeout_seconds,
    })
}

async fn webhook_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: WebhookSettings,
    listener: TcpListener,
    listen_url: String,
) {
    if let Ok(state) = store.update_webhook_adapter_state(
        Some("running"),
        Some(json!({"type": "listening", "listenUrl": listen_url})),
        None,
        0,
        0,
    ) {
        emit_platform_adapter_event(&app, "connected", "webhook", &state);
    }
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let app_for_task = app.clone();
                let store_for_task = store.clone();
                let settings_for_task = settings.clone();
                tokio::spawn(async move {
                    let _ = webhook_handle_connection(
                        app_for_task,
                        store_for_task,
                        settings_for_task,
                        stream,
                    )
                    .await;
                });
            }
            Err(error) => {
                if let Ok(state) = store.update_webhook_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(format!("Webhook accept failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "webhook", &state);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn webhook_handle_connection(
    app: AppHandle,
    store: AppStore,
    settings: WebhookSettings,
    mut stream: TcpStream,
) -> AppResult<()> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let read = tokio::time::timeout(
        Duration::from_secs(settings.timeout_seconds),
        stream.read(&mut buffer),
    )
    .await
    .map_err(|_| AppError::BadRequest("Webhook request read timed out".into()))?
    .map_err(|error| AppError::BadRequest(format!("Webhook request read failed: {error}")))?;
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let parsed = webhook_parse_request(&request);
    let status = match parsed {
        Ok(parsed) => webhook_process_request(&app, &store, &settings, parsed).await,
        Err(error) => Err(error),
    };
    let (code, body) = match status {
        Ok(value) => (200, value),
        Err(error) => (400, json!({"ok": false, "error": error.to_string()})),
    };
    let body_text = body.to_string();
    let response = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if code == 200 { "OK" } else { "Bad Request" },
        body_text.as_bytes().len(),
        body_text
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| AppError::BadRequest(format!("Webhook response write failed: {error}")))?;
    Ok(())
}

struct ParsedWebhookRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

fn webhook_parse_request(request: &str) -> AppResult<ParsedWebhookRequest> {
    let (head, body) = request
        .split_once("\r\n\r\n")
        .ok_or_else(|| AppError::BadRequest("Malformed webhook HTTP request".into()))?;
    let mut lines = head.lines();
    let start = lines
        .next()
        .ok_or_else(|| AppError::BadRequest("Webhook request missing start line".into()))?;
    let mut parts = start.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Ok(ParsedWebhookRequest {
        method,
        path,
        headers,
        body: body.to_string(),
    })
}

async fn webhook_process_request(
    app: &AppHandle,
    store: &AppStore,
    settings: &WebhookSettings,
    request: ParsedWebhookRequest,
) -> AppResult<Value> {
    if request.method != "POST" {
        return Err(AppError::BadRequest("Webhook only accepts POST".into()));
    }
    let request_path = request.path.split('?').next().unwrap_or(&request.path);
    if request_path != settings.path {
        return Err(AppError::BadRequest(format!(
            "Webhook path mismatch: expected {}, got {}",
            settings.path, request_path
        )));
    }
    if let Some(secret) = settings.secret.as_deref() {
        let provided = request
            .headers
            .get("x-synthchat-webhook-secret")
            .or_else(|| request.headers.get("x-hermes-webhook-secret"))
            .or_else(|| request.headers.get("x-webhook-secret"))
            .map(String::as_str)
            .unwrap_or_default();
        if provided != secret {
            return Err(AppError::BadRequest("Webhook secret mismatch".into()));
        }
    }
    let payload = serde_json::from_str::<Value>(&request.body)
        .unwrap_or_else(|_| json!({"body": request.body}));
    let config = store.config()?.webhook;
    let conversation_id = webhook_inbound_conversation_id(store, &config)?;
    let persona_id = webhook_inbound_persona_id(store, &config)?;
    let event_id = new_id("webhook");
    let inbound = json!({
        "platform": "webhook",
        "eventId": event_id,
        "event_id": event_id,
        "path": request_path,
        "payload": payload,
        "source": {
            "platform": "webhook",
            "chatId": request_path,
            "chat_id": request_path,
            "userId": "webhook",
            "user_id": "webhook",
            "chatType": "webhook",
            "chat_type": "webhook",
        }
    });
    let prompt = webhook_inbound_prompt(&inbound);
    let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, "webhook", &inbound, prompt).await
    else {
        let state =
            store.update_webhook_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
        emit_platform_adapter_event(app, "inbound_ignored", "webhook", &state);
        return Ok(json!({"ok": true, "eventId": event_id, "skipped": true}));
    };
    let state = store.update_webhook_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
    emit_platform_adapter_event(app, "inbound_triggered", "webhook", &state);
    spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    Ok(json!({"ok": true, "eventId": event_id}))
}

async fn apply_pre_gateway_dispatch_hooks(
    store: &AppStore,
    platform: &str,
    inbound: &Value,
    prompt: String,
) -> Option<String> {
    match run_pre_gateway_dispatch_hooks(store, platform, inbound, &prompt).await {
        PreGatewayDispatchDecision::Allow => Some(prompt),
        PreGatewayDispatchDecision::Rewrite(text) => Some(text),
        PreGatewayDispatchDecision::Skip(_) => None,
    }
}

fn webhook_inbound_prompt(inbound: &Value) -> String {
    let path = inbound
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let event_id = inbound
        .get("eventId")
        .or_else(|| inbound.get("event_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let payload = inbound.get("payload").cloned().unwrap_or(Value::Null);
    let payload_text =
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    format!("Webhook inbound event\npath: {path}\nevent_id: {event_id}\n\n{payload_text}")
}

fn webhook_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("webhook")
    }) {
        return Ok(existing.id);
    }
    let persona_id = webhook_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Webhook".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("webhook"))?;
    Ok(conversation.id)
}

fn webhook_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

async fn messaging_gateway_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: MessagingGatewayReceiveSettings,
    listener: TcpListener,
    listen_url: String,
) {
    if let Ok(state) = store.update_messaging_gateway_adapter_state(
        Some("running"),
        Some(json!({"type": "listening", "listenUrl": listen_url})),
        None,
        0,
        0,
    ) {
        emit_platform_adapter_event(&app, "connected", "messaging_gateway", &state);
    }
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let app_for_task = app.clone();
                let store_for_task = store.clone();
                let settings_for_task = settings.clone();
                tokio::spawn(async move {
                    let _ = messaging_gateway_handle_connection(
                        app_for_task,
                        store_for_task,
                        settings_for_task,
                        stream,
                    )
                    .await;
                });
            }
            Err(error) => {
                if let Ok(state) = store.update_messaging_gateway_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(format!("Messaging gateway webhook accept failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "messaging_gateway", &state);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn messaging_gateway_handle_connection(
    app: AppHandle,
    store: AppStore,
    settings: MessagingGatewayReceiveSettings,
    mut stream: TcpStream,
) -> AppResult<()> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    let read = tokio::time::timeout(
        Duration::from_secs(settings.timeout_seconds),
        stream.read(&mut buffer),
    )
    .await
    .map_err(|_| AppError::BadRequest("Messaging gateway request read timed out".into()))?
    .map_err(|error| {
        AppError::BadRequest(format!("Messaging gateway request read failed: {error}"))
    })?;
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let parsed = webhook_parse_request(&request);
    let status = match parsed {
        Ok(parsed) => messaging_gateway_process_request(&app, &store, &settings, parsed).await,
        Err(error) => Err(error),
    };
    let (code, body) = match status {
        Ok(value) => (200, value),
        Err(error) => (400, json!({"ok": false, "error": error.to_string()})),
    };
    let body_text = body.to_string();
    let response = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if code == 200 { "OK" } else { "Bad Request" },
        body_text.len(),
        body_text
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Messaging gateway response write failed: {error}"))
        })?;
    Ok(())
}

async fn messaging_gateway_process_request(
    app: &AppHandle,
    store: &AppStore,
    settings: &MessagingGatewayReceiveSettings,
    request: ParsedWebhookRequest,
) -> AppResult<Value> {
    if request.method != "POST" {
        return Err(AppError::BadRequest(
            "Messaging gateway webhook only accepts POST".into(),
        ));
    }
    let request_path = request.path.split('?').next().unwrap_or(&request.path);
    if request_path != settings.path {
        return Err(AppError::BadRequest(format!(
            "Messaging gateway path mismatch: expected {}, got {}",
            settings.path, request_path
        )));
    }
    if let Some(secret) = settings.secret.as_deref() {
        let provided = request
            .headers
            .get("x-messaging-gateway-secret")
            .or_else(|| request.headers.get("x-synthchat-webhook-secret"))
            .or_else(|| request.headers.get("x-hermes-webhook-secret"))
            .or_else(|| request.headers.get("x-webhook-secret"))
            .map(String::as_str)
            .unwrap_or_default();
        if provided != secret {
            return Err(AppError::BadRequest(
                "Messaging gateway secret mismatch".into(),
            ));
        }
    }
    let payload = messaging_gateway_parse_inbound_body(&request)?;
    let config = store.config()?.messaging_gateway;
    let Some(inbound) = messaging_gateway_inbound_event_from_payload(&payload, &config, settings)
    else {
        let state = store.update_messaging_gateway_adapter_state(
            Some("running"),
            Some(json!({"type": "ignored_message"})),
            None,
            1,
            0,
        )?;
        emit_platform_adapter_event(app, "inbound_ignored", "messaging_gateway", &state);
        return Ok(json!({"ok": true, "ignored": "message"}));
    };
    let platform = inbound
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("messaging_gateway")
        .to_string();
    let inbound_fallback = inbound.clone();
    let inbound = messaging_gateway_enrich_inbound_files(store, inbound)
        .await
        .unwrap_or_else(|error| {
            let mut fallback = inbound_fallback;
            fallback["fileDownloadError"] = json!(error.to_string());
            fallback["file_download_error"] = json!(error.to_string());
            fallback
        });
    let prompt = messaging_gateway_inbound_prompt(&inbound);
    let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, &platform, &inbound, prompt).await
    else {
        let state = store.update_messaging_gateway_adapter_state(
            Some("running"),
            Some(inbound),
            None,
            1,
            0,
        )?;
        emit_platform_adapter_event(app, "inbound_ignored", &platform, &state);
        return Ok(json!({"ok": true, "platform": platform, "skipped": true}));
    };
    let conversation_id = messaging_gateway_inbound_conversation_id(store, &config, &platform)?;
    let persona_id = messaging_gateway_inbound_persona_id(store, &config)?;
    let state =
        store.update_messaging_gateway_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
    emit_platform_adapter_event(app, "inbound_triggered", &platform, &state);
    spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    Ok(json!({"ok": true, "platform": platform}))
}

pub(super) fn messaging_gateway_inbound_event_from_payload(
    payload: &Value,
    config: &Value,
    settings: &MessagingGatewayReceiveSettings,
) -> Option<Value> {
    let message = payload
        .get("data")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("event"))
        .or_else(|| payload.get("body"))
        .unwrap_or(payload);
    let platform = string_arg(message, &["platform"])
        .or_else(|| string_arg(payload, &["platform"]))
        .or_else(|| messaging_gateway_platform_from_raw_payload(payload))?
        .trim()
        .to_ascii_lowercase();
    let message = messaging_gateway_message_from_payload(payload, message, &platform);
    if !matches!(
        platform.as_str(),
        "wecom"
            | "weixin"
            | "yuanbao"
            | "whatsapp"
            | "qqbot"
            | "bluebubbles"
            | "sms"
            | "homeassistant"
            | "msgraph_webhook"
    ) || !settings.platforms.contains(&platform)
    {
        return None;
    }
    if platform == "bluebubbles" && messaging_gateway_bluebubbles_ignored(message, payload) {
        return None;
    }
    let chat_id = string_arg(
        message,
        &[
            "chatId",
            "chat_id",
            "chatGuid",
            "chat_guid",
            "chatIdentifier",
            "chat_identifier",
            "conversationId",
            "conversation_id",
            "groupCode",
            "group_code",
            "roomId",
            "room_id",
            "chatid",
            "roomid",
            "chat_room_id",
        ],
    )
    .or_else(|| messaging_gateway_weixin_chat_id(message, config))
    .or_else(|| messaging_gateway_qqbot_chat_id(message, payload))
    .or_else(|| messaging_gateway_sms_chat_id(message))
    .or_else(|| messaging_gateway_homeassistant_chat_id(message, payload))
    .or_else(|| messaging_gateway_msgraph_chat_id(message))
    .or_else(|| messaging_gateway_bluebubbles_chat_id(message, payload))
    .or_else(|| string_arg(payload, &["chatId", "chat_id", "chatid"]))?;
    if platform == "whatsapp" && messaging_gateway_whatsapp_is_broadcast_chat(&chat_id) {
        return None;
    }
    if !messaging_gateway_allowed(
        config,
        &["allowedChats", "allowed_chats"],
        "HERMES_MESSAGING_GATEWAY_ALLOWED_CHATS",
        &chat_id,
    ) {
        return None;
    }
    let user_id = string_arg(
        message,
        &[
            "userId",
            "user_id",
            "senderId",
            "sender_id",
            "from_user_id",
            "from",
            "sender",
            "address",
            "openid",
        ],
    )
    .or_else(|| {
        message
            .get("handle")
            .and_then(|handle| string_arg(handle, &["address", "id"]))
    })
    .or_else(|| {
        message
            .get("from")
            .and_then(|from| string_arg(from, &["userid", "userId", "user_id", "id"]))
    })
    .or_else(|| {
        message
            .get("sender")
            .and_then(|sender| string_arg(sender, &["userid", "userId", "user_id", "id"]))
    })
    .or_else(|| messaging_gateway_qqbot_user_id(message, payload))
    .or_else(|| messaging_gateway_sms_user_id(message))
    .or_else(|| messaging_gateway_homeassistant_user_id(message, payload))
    .or_else(|| messaging_gateway_msgraph_user_id(message))
    .or_else(|| string_arg(payload, &["userId", "user_id", "senderId", "sender_id"]))
    .unwrap_or_else(|| "unknown".into());
    if !messaging_gateway_allowed(
        config,
        &["allowedUsers", "allowed_users"],
        "HERMES_MESSAGING_GATEWAY_ALLOWED_USERS",
        &user_id,
    ) {
        return None;
    }
    let chat_type = string_arg(message, &["chatType", "chat_type", "conversationType"])
        .or_else(|| messaging_gateway_weixin_chat_type(message, config))
        .or_else(|| messaging_gateway_qqbot_chat_type(message, payload))
        .or_else(|| messaging_gateway_sms_chat_type(message))
        .or_else(|| messaging_gateway_homeassistant_chat_type(message, payload))
        .or_else(|| messaging_gateway_msgraph_chat_type(message))
        .or_else(|| messaging_gateway_bluebubbles_chat_type(message))
        .unwrap_or_else(|| {
            if platform == "yuanbao"
                || message.get("isGroup").and_then(Value::as_bool) == Some(true)
                || message.get("is_group").and_then(Value::as_bool) == Some(true)
                || message.get("chatid").is_some()
                || message.get("room_id").is_some()
                || message.get("chat_room_id").is_some()
            {
                "group".into()
            } else {
                "dm".into()
            }
        });
    let is_group = matches!(
        chat_type.to_ascii_lowercase().as_str(),
        "group" | "room" | "chat"
    );
    let mut text = if platform == "qqbot" {
        messaging_gateway_qqbot_extract_text(message, payload)
    } else if platform == "sms" {
        messaging_gateway_sms_extract_text(message)
    } else if platform == "homeassistant" {
        messaging_gateway_homeassistant_extract_text(message, payload)
    } else if platform == "msgraph_webhook" {
        messaging_gateway_msgraph_extract_text(message, config)
    } else {
        messaging_gateway_extract_text(message)
    };
    if is_group
        && !messaging_gateway_qqbot_event_is_group_at(payload)
        && messaging_gateway_require_mention(config, message, &chat_id, &text)
    {
        return None;
    }
    let attachments = messaging_gateway_attachment_metadata(message, settings);
    if platform == "bluebubbles" && text.trim().is_empty() && !attachments.is_empty() {
        text = "(attachment)".into();
    }
    if text.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    let message_id = string_arg(
        message,
        &[
            "messageId",
            "message_id",
            "messageGuid",
            "message_guid",
            "MessageSid",
            "SmsSid",
            "msgId",
            "msg_id",
            "msgid",
            "guid",
            "id",
        ],
    )
    .unwrap_or_else(|| new_id("messaging-gateway-message"));
    let message_type = string_arg(message, &["messageType", "message_type", "msgtype", "type"])
        .unwrap_or_else(|| {
            if attachments.is_empty() {
                "text".into()
            } else {
                "document".into()
            }
        });
    let user_name = string_arg(
        message,
        &[
            "userName",
            "user_name",
            "senderName",
            "sender_name",
            "name",
            "nickname",
        ],
    )
    .unwrap_or_else(|| user_id.clone());
    let source_chat_type = if is_group {
        "group"
    } else if chat_type.eq_ignore_ascii_case("channel") {
        "channel"
    } else if chat_type.eq_ignore_ascii_case("webhook") {
        "webhook"
    } else {
        "dm"
    };
    let mut inbound = json!({
        "platform": platform,
        "messageId": message_id,
        "message_id": message_id,
        "text": text,
        "messageType": message_type,
        "message_type": message_type,
        "source": {
            "platform": platform,
            "chatId": chat_id,
            "chat_id": chat_id,
            "chatType": source_chat_type,
            "chat_type": source_chat_type,
            "chatTitle": string_arg(message, &["chatTitle", "chat_title", "roomName", "room_name"]).map(Value::String).unwrap_or(Value::Null),
            "chat_title": string_arg(message, &["chatTitle", "chat_title", "roomName", "room_name"]).map(Value::String).unwrap_or(Value::Null),
            "userId": user_id,
            "user_id": user_id,
            "userName": user_name,
            "user_name": user_name,
        },
        "raw": payload,
    });
    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["skippedAttachments"] = inbound["attachments"].clone();
        inbound["skipped_attachments"] = inbound["attachments"].clone();
    }
    if let Some(context_token) = string_arg(message, &["contextToken", "context_token"]) {
        inbound["contextToken"] = json!(context_token);
        inbound["context_token"] = json!(context_token);
    }
    Some(inbound)
}

fn messaging_gateway_allowed(config: &Value, keys: &[&str], env_key: &str, value: &str) -> bool {
    let allowed = telegram_string_set(config, keys, env_key);
    allowed.is_empty() || allowed.contains("*") || allowed.contains(value)
}

fn messaging_gateway_parse_inbound_body(request: &ParsedWebhookRequest) -> AppResult<Value> {
    let content_type = request
        .headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if content_type.contains("application/x-www-form-urlencoded") {
        return Ok(messaging_gateway_parse_form_urlencoded(&request.body));
    }
    serde_json::from_str::<Value>(&request.body).map_err(|error| {
        AppError::BadRequest(format!("invalid messaging gateway webhook JSON: {error}"))
    })
}

fn messaging_gateway_parse_form_urlencoded(body: &str) -> Value {
    let mut map = serde_json::Map::new();
    for pair in body.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = messaging_gateway_url_decode(key);
        if key.is_empty() {
            continue;
        }
        map.insert(key, Value::String(messaging_gateway_url_decode(value)));
    }
    Value::Object(map)
}

fn messaging_gateway_url_decode(value: &str) -> String {
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

fn messaging_gateway_platform_from_raw_payload(payload: &Value) -> Option<String> {
    if let Some(cmd) =
        string_arg(payload, &["cmd", "command"]).map(|value| value.to_ascii_lowercase())
    {
        if matches!(
            cmd.as_str(),
            "aibot_msg_callback" | "aibot_callback" | "aibot_event_callback"
        ) {
            return Some("wecom".into());
        }
        if cmd.contains("weixin") || cmd.contains("wechat") || cmd.contains("ilink") {
            return Some("weixin".into());
        }
        if cmd.contains("whatsapp") {
            return Some("whatsapp".into());
        }
        if cmd.contains("bluebubbles") || cmd.contains("blue_bubbles") {
            return Some("bluebubbles".into());
        }
        if cmd.contains("qqbot") || cmd.contains("qq_bot") {
            return Some("qqbot".into());
        }
        if cmd.contains("sms") || cmd.contains("twilio") {
            return Some("sms".into());
        }
        if cmd.contains("homeassistant") || cmd.contains("home_assistant") || cmd.contains("hass") {
            return Some("homeassistant".into());
        }
        if cmd.contains("msgraph") || cmd.contains("microsoft_graph") {
            return Some("msgraph_webhook".into());
        }
    }
    if messaging_gateway_qqbot_event_type(payload).is_some() {
        return Some("qqbot".into());
    }
    if messaging_gateway_sms_payload_like(payload) {
        return Some("sms".into());
    }
    if messaging_gateway_homeassistant_payload_like(payload) {
        return Some("homeassistant".into());
    }
    if messaging_gateway_msgraph_payload_like(payload) {
        return Some("msgraph_webhook".into());
    }
    let candidates = [
        Some(payload),
        payload.get("data"),
        payload.get("message"),
        payload.get("body"),
    ];
    if candidates.into_iter().flatten().any(|value| {
        value.get("msgs").and_then(Value::as_array).is_some()
            || value.get("get_updates_buf").is_some()
            || (value.get("from_user_id").is_some() && value.get("item_list").is_some())
    }) {
        return Some("weixin".into());
    }
    let candidates = [
        Some(payload),
        payload.get("data"),
        payload.get("message"),
        payload.get("body"),
    ];
    if candidates.into_iter().flatten().any(|value| {
        value.get("chatId").is_some()
            && (value.get("senderId").is_some()
                || value.get("body").is_some()
                || value.get("mediaUrls").is_some())
    }) {
        return Some("whatsapp".into());
    }
    let candidates = [
        Some(payload),
        payload.get("data"),
        payload.get("message"),
        payload.get("body"),
    ];
    if candidates.into_iter().flatten().any(|value| {
        value.get("chatGuid").is_some()
            || value.get("chatIdentifier").is_some()
            || value.get("handle").is_some()
            || value.get("attachments").is_some()
    }) {
        return Some("bluebubbles".into());
    }
    None
}

fn messaging_gateway_message_from_payload<'a>(
    payload: &'a Value,
    default_message: &'a Value,
    platform: &str,
) -> &'a Value {
    if platform == "bluebubbles" {
        return messaging_gateway_bluebubbles_message_from_payload(payload, default_message);
    }
    if platform == "qqbot" {
        return messaging_gateway_qqbot_message_from_payload(payload, default_message);
    }
    if platform == "msgraph_webhook" {
        return messaging_gateway_msgraph_message_from_payload(payload, default_message);
    }
    if !default_message.is_object() {
        return payload;
    }
    if platform != "weixin" {
        return default_message;
    }
    let containers = [
        Some(default_message),
        Some(payload),
        payload.get("data"),
        payload.get("body"),
    ];
    for value in containers.into_iter().flatten() {
        if let Some(first) = value
            .get("msgs")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
        {
            return first;
        }
        if value.get("from_user_id").is_some() && value.get("item_list").is_some() {
            return value;
        }
    }
    default_message
}

fn messaging_gateway_qqbot_event_type(payload: &Value) -> Option<String> {
    let event_type = string_arg(payload, &["t", "eventType", "event_type", "type"])?;
    let event_type = event_type.trim().to_ascii_uppercase();
    if matches!(
        event_type.as_str(),
        "C2C_MESSAGE_CREATE"
            | "GROUP_AT_MESSAGE_CREATE"
            | "GUILD_MESSAGE_CREATE"
            | "GUILD_AT_MESSAGE_CREATE"
            | "DIRECT_MESSAGE_CREATE"
    ) {
        let op = payload.get("op").and_then(Value::as_i64);
        if op.is_none() || op == Some(0) {
            return Some(event_type);
        }
    }
    None
}

fn messaging_gateway_qqbot_message_from_payload<'a>(
    payload: &'a Value,
    default_message: &'a Value,
) -> &'a Value {
    if let Some(data) = payload.get("d").filter(|value| value.is_object()) {
        return data;
    }
    if let Some(data) = payload
        .get("data")
        .and_then(|data| data.get("d"))
        .filter(|value| value.is_object())
    {
        return data;
    }
    if default_message.is_object() {
        default_message
    } else {
        payload
    }
}

fn messaging_gateway_qqbot_chat_id(message: &Value, payload: &Value) -> Option<String> {
    match messaging_gateway_qqbot_event_type(payload)?.as_str() {
        "C2C_MESSAGE_CREATE" => message
            .get("author")
            .and_then(|author| string_arg(author, &["user_openid", "openid", "id"])),
        "GROUP_AT_MESSAGE_CREATE" => string_arg(message, &["group_openid", "groupOpenid"]),
        "GUILD_MESSAGE_CREATE" | "GUILD_AT_MESSAGE_CREATE" => {
            string_arg(message, &["channel_id", "channelId"])
        }
        "DIRECT_MESSAGE_CREATE" => string_arg(message, &["guild_id", "guildId"]),
        _ => None,
    }
}

fn messaging_gateway_qqbot_user_id(message: &Value, payload: &Value) -> Option<String> {
    let author = message.get("author")?;
    match messaging_gateway_qqbot_event_type(payload)?.as_str() {
        "C2C_MESSAGE_CREATE" => string_arg(author, &["user_openid", "openid", "id"]),
        "GROUP_AT_MESSAGE_CREATE" => string_arg(author, &["member_openid", "openid", "id"]),
        "GUILD_MESSAGE_CREATE" | "GUILD_AT_MESSAGE_CREATE" | "DIRECT_MESSAGE_CREATE" => {
            string_arg(author, &["id", "user_openid", "openid"])
        }
        _ => None,
    }
}

fn messaging_gateway_qqbot_chat_type(message: &Value, payload: &Value) -> Option<String> {
    match messaging_gateway_qqbot_event_type(payload)?.as_str() {
        "C2C_MESSAGE_CREATE" | "DIRECT_MESSAGE_CREATE" => Some("dm".into()),
        "GROUP_AT_MESSAGE_CREATE" | "GUILD_MESSAGE_CREATE" | "GUILD_AT_MESSAGE_CREATE" => {
            Some("group".into())
        }
        _ => string_arg(message, &["chatType", "chat_type"]),
    }
}

fn messaging_gateway_qqbot_extract_text(message: &Value, payload: &Value) -> String {
    let text = messaging_gateway_extract_text(message);
    if messaging_gateway_qqbot_event_type(payload).as_deref() == Some("GROUP_AT_MESSAGE_CREATE") {
        return messaging_gateway_qqbot_strip_at_mention(&text);
    }
    text
}

fn messaging_gateway_qqbot_strip_at_mention(text: &str) -> String {
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix('@') else {
        return text.trim().to_string();
    };
    rest.split_once(char::is_whitespace)
        .map(|(_, remainder)| remainder.trim().to_string())
        .unwrap_or_default()
}

fn messaging_gateway_qqbot_event_is_group_at(payload: &Value) -> bool {
    messaging_gateway_qqbot_event_type(payload).as_deref() == Some("GROUP_AT_MESSAGE_CREATE")
}

fn messaging_gateway_sms_payload_like(payload: &Value) -> bool {
    string_arg(payload, &["From", "from"]).is_some()
        && string_arg(payload, &["Body", "body", "text"]).is_some()
        && (string_arg(
            payload,
            &["MessageSid", "SmsSid", "message_sid", "messageId"],
        )
        .is_some()
            || string_arg(payload, &["To", "to"]).is_some())
}

fn messaging_gateway_sms_chat_id(message: &Value) -> Option<String> {
    string_arg(
        message,
        &["From", "from", "sender", "phone", "chat_id", "chatId"],
    )
}

fn messaging_gateway_sms_user_id(message: &Value) -> Option<String> {
    messaging_gateway_sms_chat_id(message)
}

fn messaging_gateway_sms_chat_type(message: &Value) -> Option<String> {
    if messaging_gateway_sms_chat_id(message).is_some() {
        Some("dm".into())
    } else {
        None
    }
}

fn messaging_gateway_sms_extract_text(message: &Value) -> String {
    string_arg(message, &["Body", "body", "text", "message", "content"]).unwrap_or_default()
}

fn messaging_gateway_homeassistant_payload_like(payload: &Value) -> bool {
    let candidates = [
        Some(payload),
        payload.get("event"),
        payload.get("data"),
        payload.get("message"),
    ];
    candidates.into_iter().flatten().any(|value| {
        string_arg(value, &["event_type", "eventType", "type"])
            .map(|event_type| event_type == "state_changed")
            .unwrap_or(false)
            || value.get("entity_id").is_some()
            || value
                .get("data")
                .and_then(|data| data.get("entity_id"))
                .is_some()
    })
}

fn messaging_gateway_homeassistant_event_data<'a>(
    message: &'a Value,
    payload: &'a Value,
) -> &'a Value {
    message
        .get("data")
        .filter(|value| value.get("entity_id").is_some())
        .or_else(|| {
            payload
                .get("data")
                .filter(|value| value.get("entity_id").is_some())
        })
        .or_else(|| {
            payload
                .get("event")
                .and_then(|event| event.get("data"))
                .filter(|value| value.get("entity_id").is_some())
        })
        .unwrap_or(message)
}

fn messaging_gateway_homeassistant_chat_id(message: &Value, payload: &Value) -> Option<String> {
    if messaging_gateway_homeassistant_payload_like(payload)
        || messaging_gateway_homeassistant_payload_like(message)
    {
        Some("ha_events".into())
    } else {
        None
    }
}

fn messaging_gateway_homeassistant_user_id(message: &Value, payload: &Value) -> Option<String> {
    messaging_gateway_homeassistant_chat_id(message, payload).map(|_| "homeassistant".into())
}

fn messaging_gateway_homeassistant_chat_type(message: &Value, payload: &Value) -> Option<String> {
    messaging_gateway_homeassistant_chat_id(message, payload).map(|_| "channel".into())
}

fn messaging_gateway_homeassistant_extract_text(message: &Value, payload: &Value) -> String {
    let data = messaging_gateway_homeassistant_event_data(message, payload);
    let Some(entity_id) = string_arg(data, &["entity_id", "entityId"]) else {
        return messaging_gateway_extract_text(message);
    };
    let old_state = data.get("old_state").unwrap_or(&Value::Null);
    let new_state = data.get("new_state").unwrap_or(data);
    let old_value = string_arg(old_state, &["state"]).unwrap_or_else(|| "unknown".into());
    let new_value = string_arg(new_state, &["state"]).unwrap_or_else(|| "unknown".into());
    if old_value == new_value {
        return String::new();
    }
    let friendly_name = new_state
        .get("attributes")
        .and_then(|attributes| string_arg(attributes, &["friendly_name", "friendlyName"]))
        .unwrap_or_else(|| entity_id.clone());
    let domain = entity_id.split('.').next().unwrap_or_default();
    match domain {
        "climate" => {
            let attrs = new_state.get("attributes").unwrap_or(&Value::Null);
            let current = string_arg(attrs, &["current_temperature"])
                .or_else(|| attrs.get("current_temperature").map(Value::to_string))
                .unwrap_or_else(|| "?".into());
            let target = string_arg(attrs, &["temperature"])
                .or_else(|| attrs.get("temperature").map(Value::to_string))
                .unwrap_or_else(|| "?".into());
            format!(
                "[Home Assistant] {friendly_name}: HVAC mode changed from '{old_value}' to '{new_value}' (current: {current}, target: {target})"
            )
        }
        "sensor" => {
            let unit = new_state
                .get("attributes")
                .and_then(|attributes| string_arg(attributes, &["unit_of_measurement"]))
                .unwrap_or_default();
            format!("[Home Assistant] {friendly_name}: changed from {old_value}{unit} to {new_value}{unit}")
        }
        "binary_sensor" => format!(
            "[Home Assistant] {friendly_name}: {} (was {})",
            if new_value == "on" { "triggered" } else { "cleared" },
            if old_value == "on" { "triggered" } else { "cleared" }
        ),
        "light" | "switch" | "fan" => format!(
            "[Home Assistant] {friendly_name}: turned {}",
            if new_value == "on" { "on" } else { "off" }
        ),
        "alarm_control_panel" => format!(
            "[Home Assistant] {friendly_name}: alarm state changed from '{old_value}' to '{new_value}'"
        ),
        _ => format!(
            "[Home Assistant] {friendly_name} ({entity_id}): changed from '{old_value}' to '{new_value}'"
        ),
    }
}

fn messaging_gateway_weixin_chat_id(message: &Value, config: &Value) -> Option<String> {
    string_arg(message, &["room_id", "chat_room_id"])
        .or_else(|| {
            let to_user_id = string_arg(message, &["to_user_id"])?;
            let account_id = string_arg(config, &["accountId", "account_id"]).unwrap_or_default();
            if !account_id.is_empty()
                && to_user_id != account_id
                && message.get("msg_type").and_then(Value::as_i64) == Some(1)
            {
                Some(to_user_id)
            } else {
                None
            }
        })
        .or_else(|| string_arg(message, &["from_user_id"]))
}

fn messaging_gateway_weixin_chat_type(message: &Value, config: &Value) -> Option<String> {
    if message.get("room_id").is_some()
        || message.get("chat_room_id").is_some()
        || message.get("is_group").and_then(Value::as_bool) == Some(true)
        || message.get("isGroup").and_then(Value::as_bool) == Some(true)
    {
        return Some("group".into());
    }
    let to_user_id = string_arg(message, &["to_user_id"]).unwrap_or_default();
    let account_id = string_arg(config, &["accountId", "account_id"]).unwrap_or_default();
    if !to_user_id.is_empty()
        && !account_id.is_empty()
        && to_user_id != account_id
        && message.get("msg_type").and_then(Value::as_i64) == Some(1)
    {
        return Some("group".into());
    }
    if message.get("from_user_id").is_some() || message.get("item_list").is_some() {
        return Some("dm".into());
    }
    None
}

fn messaging_gateway_bluebubbles_message_from_payload<'a>(
    payload: &'a Value,
    default_message: &'a Value,
) -> &'a Value {
    if let Some(data) = payload.get("data") {
        if data.is_object() {
            return data;
        }
        if let Some(first) = data
            .as_array()
            .and_then(|items| items.iter().find(|item| item.is_object()))
        {
            return first;
        }
    }
    if let Some(message) = payload.get("message").filter(|value| value.is_object()) {
        return message;
    }
    if default_message.is_object() {
        default_message
    } else {
        payload
    }
}

fn messaging_gateway_bluebubbles_ignored(message: &Value, payload: &Value) -> bool {
    let event_type = string_arg(payload, &["type", "event"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !event_type.is_empty()
        && !matches!(
            event_type.as_str(),
            "new-message" | "message" | "updated-message"
        )
    {
        return true;
    }
    if message
        .get("isFromMe")
        .or_else(|| message.get("fromMe"))
        .or_else(|| message.get("is_from_me"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    matches!(
        message.get("associatedMessageType").and_then(Value::as_i64),
        Some(2000 | 2001 | 2002 | 2003 | 2004 | 2005 | 3000 | 3001 | 3002 | 3003 | 3004 | 3005)
    )
}

fn messaging_gateway_bluebubbles_chat_id(message: &Value, payload: &Value) -> Option<String> {
    string_arg(
        message,
        &[
            "chatGuid",
            "chat_guid",
            "chatIdentifier",
            "chat_identifier",
            "identifier",
        ],
    )
    .or_else(|| {
        message
            .get("chats")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|chat| string_arg(chat, &["guid", "chatGuid", "chat_guid"]))
    })
    .or_else(|| {
        string_arg(
            payload,
            &["chatGuid", "chat_guid", "chatIdentifier", "identifier"],
        )
    })
    .or_else(|| {
        message
            .get("handle")
            .and_then(|handle| string_arg(handle, &["address", "id"]))
    })
    .or_else(|| string_arg(message, &["sender", "from", "address"]))
}

fn messaging_gateway_bluebubbles_chat_type(message: &Value) -> Option<String> {
    if message.get("isGroup").and_then(Value::as_bool) == Some(true)
        || message.get("is_group").and_then(Value::as_bool) == Some(true)
        || string_arg(message, &["chatGuid", "chat_guid"])
            .map(|value| value.contains(";+;"))
            .unwrap_or(false)
    {
        Some("group".into())
    } else if message.get("chatGuid").is_some()
        || message.get("chatIdentifier").is_some()
        || message.get("handle").is_some()
    {
        Some("dm".into())
    } else {
        None
    }
}

fn messaging_gateway_msgraph_payload_like(payload: &Value) -> bool {
    payload
        .get("value")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(messaging_gateway_msgraph_notification_like)
        })
        .unwrap_or(false)
        || messaging_gateway_msgraph_notification_like(payload)
}

fn messaging_gateway_msgraph_notification_like(value: &Value) -> bool {
    value.get("subscriptionId").is_some()
        && value.get("changeType").is_some()
        && value.get("resource").is_some()
}

fn messaging_gateway_msgraph_message_from_payload<'a>(
    payload: &'a Value,
    default_message: &'a Value,
) -> &'a Value {
    if messaging_gateway_msgraph_notification_like(default_message) {
        return default_message;
    }
    if let Some(notification) = payload
        .get("value")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| messaging_gateway_msgraph_notification_like(item))
        })
    {
        return notification;
    }
    payload
}

fn messaging_gateway_msgraph_chat_id(message: &Value) -> Option<String> {
    if messaging_gateway_msgraph_notification_like(message) {
        Some(format!(
            "msgraph:{}",
            string_arg(message, &["subscriptionId", "subscription_id"])
                .unwrap_or_else(|| "unknown".into())
        ))
    } else {
        None
    }
}

fn messaging_gateway_msgraph_user_id(message: &Value) -> Option<String> {
    messaging_gateway_msgraph_chat_id(message).map(|_| "msgraph".into())
}

fn messaging_gateway_msgraph_chat_type(message: &Value) -> Option<String> {
    messaging_gateway_msgraph_chat_id(message).map(|_| "webhook".into())
}

fn messaging_gateway_msgraph_extract_text(message: &Value, config: &Value) -> String {
    if !messaging_gateway_msgraph_client_state_allowed(message, config)
        || !messaging_gateway_msgraph_resource_allowed(message, config)
    {
        return String::new();
    }
    if let Some(template) = string_arg(config, &["msgraphPrompt", "msgraph_prompt", "prompt"])
        .filter(|value| !value.trim().is_empty())
    {
        return messaging_gateway_msgraph_render_template(&template, message);
    }
    let rendered = serde_json::to_string_pretty(message).unwrap_or_else(|_| message.to_string());
    format!(
        "Microsoft Graph change notification:\n\n```json\n{}\n```",
        truncate_output(&rendered, 4000)
    )
}

fn messaging_gateway_msgraph_client_state_allowed(message: &Value, config: &Value) -> bool {
    let expected = string_arg(
        config,
        &[
            "msgraphClientState",
            "msgraph_client_state",
            "clientState",
            "client_state",
        ],
    );
    let Some(expected) = expected.filter(|value| !value.is_empty()) else {
        return true;
    };
    string_arg(message, &["clientState", "client_state"])
        .map(|provided| provided == expected)
        .unwrap_or(false)
}

fn messaging_gateway_msgraph_resource_allowed(message: &Value, config: &Value) -> bool {
    let allowed = telegram_string_set(
        config,
        &[
            "acceptedResources",
            "accepted_resources",
            "msgraphAcceptedResources",
        ],
        "HERMES_MESSAGING_GATEWAY_MSGRAPH_ACCEPTED_RESOURCES",
    );
    if allowed.is_empty() || allowed.contains("*") {
        return true;
    }
    let resource = string_arg(message, &["resource"]).unwrap_or_default();
    let normalized = resource.trim().trim_matches('/');
    allowed.iter().any(|pattern| {
        let pattern = pattern.trim().trim_matches('/');
        if pattern.is_empty() {
            return false;
        }
        if let Some(prefix) = pattern
            .strip_suffix('*')
            .map(|value| value.trim_matches('/'))
        {
            normalized == prefix || normalized.starts_with(&format!("{prefix}/"))
        } else {
            normalized == pattern || normalized.starts_with(&format!("{pattern}/"))
        }
    })
}

fn messaging_gateway_msgraph_render_template(template: &str, message: &Value) -> String {
    let replacements = [
        (
            "{resource}",
            string_arg(message, &["resource"]).unwrap_or_default(),
        ),
        (
            "{change_type}",
            string_arg(message, &["changeType", "change_type"]).unwrap_or_default(),
        ),
        (
            "{subscription_id}",
            string_arg(message, &["subscriptionId", "subscription_id"]).unwrap_or_default(),
        ),
        (
            "{notification}",
            serde_json::to_string(message).unwrap_or_else(|_| message.to_string()),
        ),
    ];
    replacements
        .into_iter()
        .fold(template.to_string(), |acc, (key, value)| {
            acc.replace(key, &value)
        })
}

fn messaging_gateway_require_mention(
    config: &Value,
    message: &Value,
    chat_id: &str,
    text: &str,
) -> bool {
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("HERMES_MESSAGING_GATEWAY_REQUIRE_MENTION"))
        .unwrap_or(true);
    if !require_mention {
        return false;
    }
    let free_chats = telegram_string_set(
        config,
        &["freeResponseChats", "free_response_chats"],
        "HERMES_MESSAGING_GATEWAY_FREE_RESPONSE_CHATS",
    );
    if free_chats.contains(chat_id) {
        return false;
    }
    if message
        .get("mentioned")
        .or_else(|| message.get("isMentioned"))
        .or_else(|| message.get("is_mentioned"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    if messaging_gateway_whatsapp_mentions_bot(message, text) {
        return false;
    }
    let patterns = telegram_string_set(
        config,
        &["mentionPatterns", "mention_patterns"],
        "HERMES_MESSAGING_GATEWAY_MENTION_PATTERNS",
    );
    if patterns
        .iter()
        .any(|pattern| !pattern.is_empty() && text.contains(pattern))
    {
        return false;
    }
    !text.trim_start().starts_with('/')
}

fn messaging_gateway_whatsapp_is_broadcast_chat(chat_id: &str) -> bool {
    let chat_id = chat_id.trim().to_ascii_lowercase();
    chat_id == "status@broadcast"
        || chat_id.ends_with("@broadcast")
        || chat_id.ends_with("@newsletter")
}

fn messaging_gateway_whatsapp_mentions_bot(message: &Value, text: &str) -> bool {
    let bot_ids = messaging_gateway_string_array(message, &["botIds", "bot_ids"]);
    if bot_ids.is_empty() {
        return false;
    }
    let normalized_bot_ids = bot_ids
        .into_iter()
        .map(|value| messaging_gateway_normalize_whatsapp_id(&value))
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    if normalized_bot_ids.is_empty() {
        return false;
    }
    if let Some(quoted) = string_arg(message, &["quotedParticipant", "quoted_participant"]) {
        if normalized_bot_ids.contains(&messaging_gateway_normalize_whatsapp_id(&quoted)) {
            return true;
        }
    }
    let mentioned_ids = messaging_gateway_string_array(message, &["mentionedIds", "mentioned_ids"]);
    if mentioned_ids
        .into_iter()
        .any(|value| normalized_bot_ids.contains(&messaging_gateway_normalize_whatsapp_id(&value)))
    {
        return true;
    }
    let lower_text = text.to_ascii_lowercase();
    normalized_bot_ids.iter().any(|bot_id| {
        let bare = bot_id.split('@').next().unwrap_or_default();
        !bare.is_empty() && (lower_text.contains(&format!("@{bare}")) || lower_text.contains(bare))
    })
}

fn messaging_gateway_normalize_whatsapp_id(value: &str) -> String {
    let mut normalized = value.trim().to_ascii_lowercase();
    if normalized.contains(':') && normalized.contains('@') {
        normalized = normalized.replacen(':', "@", 1);
    }
    normalized
}

fn messaging_gateway_string_array(message: &Value, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .find_map(|key| message.get(*key).and_then(Value::as_array))
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn messaging_gateway_extract_text(message: &Value) -> String {
    if let Some(text) = messaging_gateway_weixin_extract_text(message) {
        return text;
    }
    if let Some(text) = string_arg(message, &["text", "content", "message", "body"]) {
        return text;
    }
    if let Some(text) = message
        .get("text")
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
    {
        return text.to_string();
    }
    let msgtype = string_arg(message, &["msgtype", "msgType", "messageType"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if msgtype == "mixed" {
        return message
            .get("mixed")
            .and_then(|mixed| mixed.get("msg_item").or_else(|| mixed.get("msgItem")))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| messaging_gateway_nested_wecom_text(item))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
    }
    messaging_gateway_nested_wecom_text(message).unwrap_or_default()
}

fn messaging_gateway_weixin_extract_text(message: &Value) -> Option<String> {
    let items = message.get("item_list").and_then(Value::as_array)?;
    for item in items {
        if messaging_gateway_weixin_item_type(item) == Some(1) {
            let text = item
                .get("text_item")
                .and_then(|text_item| string_arg(text_item, &["text"]))
                .unwrap_or_default();
            let ref_item = item
                .get("ref_msg")
                .and_then(|ref_msg| ref_msg.get("message_item"));
            if let Some(ref_item) = ref_item {
                let ref_type = messaging_gateway_weixin_item_type(ref_item);
                if matches!(ref_type, Some(2 | 3 | 4 | 5)) {
                    let title = item
                        .get("ref_msg")
                        .and_then(|ref_msg| string_arg(ref_msg, &["title"]))
                        .unwrap_or_default();
                    let prefix = if title.is_empty() {
                        "[quoted media]\n".to_string()
                    } else {
                        format!("[quoted media: {title}]\n")
                    };
                    return Some(format!("{prefix}{text}").trim().to_string());
                }
                if ref_item.is_object() {
                    let mut parts = Vec::new();
                    if let Some(title) = item
                        .get("ref_msg")
                        .and_then(|ref_msg| string_arg(ref_msg, &["title"]))
                    {
                        parts.push(title);
                    }
                    if let Some(ref_text) = messaging_gateway_weixin_extract_text(&json!({
                        "item_list": [ref_item.clone()]
                    })) {
                        if !ref_text.trim().is_empty() {
                            parts.push(ref_text);
                        }
                    }
                    if !parts.is_empty() {
                        return Some(
                            format!("[quoted: {}]\n{text}", parts.join(" | "))
                                .trim()
                                .to_string(),
                        );
                    }
                }
            }
            return Some(text);
        }
    }
    for item in items {
        if messaging_gateway_weixin_item_type(item) == Some(3) {
            if let Some(text) = item
                .get("voice_item")
                .and_then(|voice_item| string_arg(voice_item, &["text"]))
                .filter(|text| !text.trim().is_empty())
            {
                return Some(text);
            }
        }
    }
    None
}

fn messaging_gateway_weixin_item_type(item: &Value) -> Option<i64> {
    item.get("type")
        .and_then(Value::as_i64)
        .or_else(|| string_arg(item, &["type"]).and_then(|value| value.parse::<i64>().ok()))
}

fn messaging_gateway_nested_wecom_text(message: &Value) -> Option<String> {
    if let Some(text) = message
        .get("text")
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(text) = message
        .get("voice")
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(title) = message
        .get("appmsg")
        .and_then(|value| value.get("title"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(title.to_string());
    }
    None
}

fn messaging_gateway_attachment_metadata(
    message: &Value,
    settings: &MessagingGatewayReceiveSettings,
) -> Vec<Value> {
    let mut attachments = if messaging_gateway_bluebubbles_record_like(message) {
        Vec::new()
    } else {
        message
            .get("attachments")
            .or_else(|| message.get("files"))
            .or_else(|| message.get("media"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let name = string_arg(&item, &["name", "fileName", "file_name", "filename"])
                    .unwrap_or_else(|| format!("attachment-{index}"));
                let mime =
                    string_arg(&item, &["mimeType", "mime_type", "contentType", "content_type"])
                        .unwrap_or_else(|| guess_content_type(&name).into());
                json!({
                    "id": string_arg(&item, &["id", "fileId", "file_id", "mediaId", "media_id"]).unwrap_or_else(|| name.clone()),
                    "name": name,
                    "mimeType": mime,
                    "mime_type": mime,
                    "type": mattermost_media_kind(&mime),
                    "size": item.get("size").cloned().unwrap_or(Value::Null),
                    "url": item.get("url").cloned().unwrap_or(Value::Null),
                    "base64": item.get("base64").cloned().unwrap_or(Value::Null),
                    "downloadStatus": "skipped",
                    "download_status": "skipped",
                    "reason": "messaging gateway receive runtime records bridge attachment metadata only",
                })
            })
            .collect::<Vec<_>>()
    };
    attachments.extend(messaging_gateway_wecom_attachment_metadata(message));
    attachments.extend(messaging_gateway_weixin_attachment_metadata(
        message,
        &settings.weixin_cdn_base_url,
    ));
    attachments.extend(messaging_gateway_whatsapp_attachment_metadata(message));
    attachments.extend(messaging_gateway_bluebubbles_attachment_metadata(message));
    attachments
}

pub(super) async fn messaging_gateway_enrich_inbound_files(
    store: &AppStore,
    mut inbound: Value,
) -> AppResult<Value> {
    let attachments = inbound
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if attachments.is_empty() {
        return Ok(inbound);
    }
    let platform = inbound
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("messaging_gateway");
    let cache_dir = store
        .data_dir()
        .join("attachments")
        .join(mattermost_safe_file_name(platform));
    fs::create_dir_all(&cache_dir)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!(
                "failed to build messaging gateway download client: {error}"
            ))
        })?;
    let mut enriched = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    for attachment in attachments {
        match messaging_gateway_cache_base64_attachment(&cache_dir, &attachment) {
            Ok(Some(cached)) => {
                if let Some(path) = cached.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = cached
                    .get("mimeType")
                    .or_else(|| cached.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                enriched.push(cached);
            }
            Ok(None) => {
                match messaging_gateway_download_url_attachment(&client, &cache_dir, &attachment)
                    .await
                {
                    Ok(Some(cached)) => {
                        if let Some(path) = cached.get("path").and_then(Value::as_str) {
                            media_urls.push(Value::String(path.to_string()));
                        }
                        if let Some(mime) = cached
                            .get("mimeType")
                            .or_else(|| cached.get("mime_type"))
                            .and_then(Value::as_str)
                        {
                            media_types.push(Value::String(mime.to_string()));
                        }
                        enriched.push(cached);
                    }
                    Ok(None) => enriched.push(attachment),
                    Err(error) => {
                        let mut failed = attachment;
                        failed["downloadStatus"] = json!("failed");
                        failed["download_status"] = json!("failed");
                        failed["error"] = json!(error.to_string());
                        enriched.push(failed);
                    }
                }
            }
            Err(error) => {
                let mut failed = attachment;
                failed["downloadStatus"] = json!("failed");
                failed["download_status"] = json!("failed");
                failed["error"] = json!(error.to_string());
                enriched.push(failed);
            }
        }
    }
    inbound["attachments"] = json!(enriched);
    if !media_urls.is_empty() {
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        let message_type = mattermost_message_type_from_media(&inbound["mediaTypes"]);
        inbound["messageType"] = json!(message_type);
        inbound["message_type"] = json!(message_type);
    }
    Ok(inbound)
}

fn messaging_gateway_cache_base64_attachment(
    cache_dir: &Path,
    attachment: &Value,
) -> AppResult<Option<Value>> {
    let Some(encoded) = attachment.get("base64").and_then(Value::as_str) else {
        return Ok(None);
    };
    let bytes = messaging_gateway_decode_base64_bytes(encoded)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let name = string_arg(
        attachment,
        &["name", "fileName", "file_name", "filename", "id"],
    )
    .unwrap_or_else(|| new_id("wecom-attachment"));
    let mime = string_arg(attachment, &["mimeType", "mime_type", "contentType"])
        .unwrap_or_else(|| guess_content_type(&name).into());
    let safe_name = mattermost_safe_file_name(&name);
    let path = cache_dir.join(format!("{}-{safe_name}", new_id("gateway-attachment")));
    fs::write(&path, &bytes)?;
    let mut cached = attachment.clone();
    cached["path"] = json!(path.to_string_lossy().to_string());
    cached["mimeType"] = json!(mime);
    cached["mime_type"] = cached["mimeType"].clone();
    cached["type"] = json!(mattermost_media_kind(
        cached
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
    ));
    cached["size"] = json!(bytes.len());
    cached["downloadStatus"] = json!("cached");
    cached["download_status"] = json!("cached");
    cached["reason"] = Value::Null;
    cached["base64"] = Value::Null;
    Ok(Some(cached))
}

async fn messaging_gateway_download_url_attachment(
    client: &reqwest::Client,
    cache_dir: &Path,
    attachment: &Value,
) -> AppResult<Option<Value>> {
    let Some(url) = attachment
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
    else {
        return Ok(None);
    };
    const MAX_BYTES: u64 = 20 * 1024 * 1024;
    let response =
        client.get(url).send().await.map_err(|error| {
            AppError::BadRequest(format!("WeCom media download failed: {error}"))
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "WeCom media download failed ({})",
            status.as_u16()
        )));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_BYTES {
            return Err(AppError::BadRequest(format!(
                "WeCom media exceeds download limit: {length} bytes > {MAX_BYTES} bytes"
            )));
        }
    }
    let name = string_arg(
        attachment,
        &["name", "fileName", "file_name", "filename", "id"],
    )
    .unwrap_or_else(|| "wecom-attachment".into());
    let header_mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut bytes = response
        .bytes()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("failed to read WeCom media download: {error}"))
        })?
        .to_vec();
    if bytes.len() as u64 > MAX_BYTES {
        return Err(AppError::BadRequest(format!(
            "WeCom media exceeds download limit while reading: {} bytes > {MAX_BYTES} bytes",
            bytes.len()
        )));
    }
    if let Some(aes_key) = string_arg(attachment, &["aesKey", "aes_key", "aeskey"]) {
        let encryption = string_arg(attachment, &["encryption", "crypto"])
            .unwrap_or_else(|| "wecom-aes-256-cbc".into())
            .to_ascii_lowercase();
        if encryption == "weixin-aes-128-ecb" {
            bytes = messaging_gateway_decrypt_weixin_media(&bytes, &aes_key)?;
        } else {
            bytes = messaging_gateway_decrypt_wecom_media(&bytes, &aes_key)?;
        }
    }
    let mime = header_mime
        .or_else(|| string_arg(attachment, &["mimeType", "mime_type", "contentType"]))
        .unwrap_or_else(|| guess_content_type(&name).into());
    let safe_name = mattermost_safe_file_name(&name);
    let path = cache_dir.join(format!("{}-{safe_name}", new_id("gateway-attachment")));
    fs::write(&path, &bytes)?;
    let mut cached = attachment.clone();
    cached["path"] = json!(path.to_string_lossy().to_string());
    cached["mimeType"] = json!(mime);
    cached["mime_type"] = cached["mimeType"].clone();
    cached["type"] = json!(mattermost_media_kind(
        cached
            .get("mimeType")
            .and_then(Value::as_str)
            .unwrap_or("application/octet-stream")
    ));
    cached["size"] = json!(bytes.len());
    cached["downloadStatus"] = json!("cached");
    cached["download_status"] = json!("cached");
    cached["reason"] = Value::Null;
    Ok(Some(cached))
}

pub(super) fn messaging_gateway_decrypt_wecom_media(
    encrypted_data: &[u8],
    aes_key: &str,
) -> AppResult<Vec<u8>> {
    use aes::Aes256;
    use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};

    if encrypted_data.is_empty() {
        return Err(AppError::BadRequest(
            "WeCom encrypted media is empty".into(),
        ));
    }
    let mut padded_key = aes_key.trim().to_string();
    let remainder = padded_key.len() % 4;
    if remainder != 0 {
        padded_key.push_str(&"=".repeat(4 - remainder));
    }
    let key = messaging_gateway_decode_base64_bytes(&padded_key)?;
    if key.len() != 32 {
        return Err(AppError::BadRequest(format!(
            "Invalid WeCom AES key length: expected 32 bytes, got {}",
            key.len()
        )));
    }
    let mut buffer = encrypted_data.to_vec();
    let decrypted = cbc::Decryptor::<Aes256>::new_from_slices(&key, &key[..16])
        .map_err(|error| AppError::BadRequest(format!("WeCom AES init failed: {error}")))?
        .decrypt_padded_mut::<Pkcs7>(&mut buffer)
        .map_err(|error| AppError::BadRequest(format!("WeCom AES decrypt failed: {error}")))?;
    Ok(decrypted.to_vec())
}

pub(super) fn messaging_gateway_decrypt_weixin_media(
    encrypted_data: &[u8],
    aes_key: &str,
) -> AppResult<Vec<u8>> {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
    use aes::Aes128;

    if encrypted_data.is_empty() {
        return Err(AppError::BadRequest(
            "Weixin encrypted media is empty".into(),
        ));
    }
    if encrypted_data.len() % 16 != 0 {
        return Err(AppError::BadRequest(format!(
            "Invalid Weixin encrypted media length: {} is not a multiple of 16",
            encrypted_data.len()
        )));
    }
    let key = messaging_gateway_parse_weixin_aes_key(aes_key)?;
    let cipher = Aes128::new(GenericArray::from_slice(&key));
    let mut decrypted = encrypted_data.to_vec();
    for chunk in decrypted.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    if let Some(&pad_len) = decrypted.last() {
        let pad_len = pad_len as usize;
        if (1..=16).contains(&pad_len)
            && decrypted.len() >= pad_len
            && decrypted[decrypted.len() - pad_len..]
                .iter()
                .all(|byte| *byte as usize == pad_len)
        {
            decrypted.truncate(decrypted.len() - pad_len);
        }
    }
    Ok(decrypted)
}

fn messaging_gateway_parse_weixin_aes_key(value: &str) -> AppResult<[u8; 16]> {
    let decoded = messaging_gateway_decode_base64_bytes(value)?;
    let bytes = if decoded.len() == 16 {
        decoded
    } else if decoded.len() == 32
        && decoded
            .iter()
            .all(|byte| (*byte as char).is_ascii_hexdigit())
    {
        let text = String::from_utf8_lossy(&decoded);
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| AppError::BadRequest(format!("invalid Weixin hex AES key: {error}")))?
    } else {
        return Err(AppError::BadRequest(format!(
            "Invalid Weixin AES key length: expected 16 raw bytes or 32 hex bytes, got {}",
            decoded.len()
        )));
    };
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        AppError::BadRequest(format!(
            "Invalid Weixin AES key length after parsing: expected 16 bytes, got {}",
            bytes.len()
        ))
    })
}

pub(super) fn messaging_gateway_decode_base64_bytes(value: &str) -> AppResult<Vec<u8>> {
    use base64::Engine;
    let payload = value.split_once(',').map(|(_, data)| data).unwrap_or(value);
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid messaging gateway base64: {error}")))
}

fn messaging_gateway_wecom_attachment_metadata(message: &Value) -> Vec<Value> {
    let mut refs = Vec::new();
    let msgtype = string_arg(message, &["msgtype", "msgType", "messageType"])
        .unwrap_or_default()
        .to_ascii_lowercase();
    if msgtype == "mixed" {
        if let Some(items) = message
            .get("mixed")
            .and_then(|mixed| mixed.get("msg_item").or_else(|| mixed.get("msgItem")))
            .and_then(Value::as_array)
        {
            for item in items {
                let item_type = string_arg(item, &["msgtype", "msgType"])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if item_type == "image" {
                    if let Some(image) = item.get("image") {
                        refs.push(("image", image.clone()));
                    }
                }
            }
        }
    } else {
        if let Some(image) = message.get("image") {
            refs.push(("image", image.clone()));
        }
        if let Some(file) = message.get("file") {
            refs.push(("file", file.clone()));
        }
        if let Some(appmsg) = message.get("appmsg") {
            if let Some(file) = appmsg.get("file") {
                refs.push(("file", file.clone()));
            } else if let Some(image) = appmsg.get("image") {
                refs.push(("image", image.clone()));
            }
        }
    }
    if let Some(quote) = message.get("quote") {
        if let Some(image) = quote.get("image") {
            refs.push(("image", image.clone()));
        }
        if let Some(file) = quote.get("file") {
            refs.push(("file", file.clone()));
        }
    }
    refs.into_iter()
        .enumerate()
        .map(|(index, (kind, item))| {
            let name = string_arg(
                &item,
                &["filename", "fileName", "file_name", "name", "title"],
            )
            .unwrap_or_else(|| format!("wecom-{kind}-{index}"));
            let mime = string_arg(&item, &["mimeType", "mime_type", "contentType"])
                .unwrap_or_else(|| {
                    if kind == "image" {
                        "image/jpeg".into()
                    } else {
                        guess_content_type(&name).into()
                    }
                });
            json!({
                "id": string_arg(&item, &["id", "mediaId", "media_id", "fileId", "file_id", "url"]).unwrap_or_else(|| name.clone()),
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": kind,
                "size": item.get("size").cloned().unwrap_or(Value::Null),
                "url": item.get("url").cloned().unwrap_or(Value::Null),
                "base64": item.get("base64").cloned().unwrap_or(Value::Null),
                "aesKey": string_arg(&item, &["aeskey", "aesKey", "aes_key"]).map(Value::String).unwrap_or(Value::Null),
                "aes_key": string_arg(&item, &["aeskey", "aesKey", "aes_key"]).map(Value::String).unwrap_or(Value::Null),
                "downloadStatus": "skipped",
                "download_status": "skipped",
                "reason": "WeCom raw callback media download/decryption is not implemented in SynthChat yet",
            })
        })
        .collect()
}

fn messaging_gateway_weixin_attachment_metadata(message: &Value, cdn_base_url: &str) -> Vec<Value> {
    let mut refs = Vec::new();
    if let Some(items) = message.get("item_list").and_then(Value::as_array) {
        for item in items {
            messaging_gateway_collect_weixin_attachment_refs(item, &mut refs);
            if let Some(ref_item) = item
                .get("ref_msg")
                .and_then(|ref_msg| ref_msg.get("message_item"))
            {
                messaging_gateway_collect_weixin_attachment_refs(ref_item, &mut refs);
            }
        }
    }
    refs.into_iter()
        .enumerate()
        .map(|(index, (kind, item, media))| {
            let name = match kind {
                "image" => format!("weixin-image-{index}.jpg"),
                "video" => format!("weixin-video-{index}.mp4"),
                "voice" => format!("weixin-voice-{index}.silk"),
                _ => string_arg(&item, &["file_name", "filename", "fileName", "name"])
                    .unwrap_or_else(|| format!("weixin-file-{index}")),
            };
            let mime = match kind {
                "image" => "image/jpeg".to_string(),
                "video" => "video/mp4".to_string(),
                "voice" => "audio/silk".to_string(),
                _ => guess_content_type(&name).into(),
            };
            let aes_key = if kind == "image" {
                string_arg(&item, &["aeskey"])
                    .and_then(|value| messaging_gateway_weixin_hex_aes_key_to_base64(&value))
                    .or_else(|| string_arg(&media, &["aes_key", "aesKey"]))
            } else {
                string_arg(&media, &["aes_key", "aesKey"])
            };
            let full_url = string_arg(&media, &["full_url", "fullUrl"]);
            let encrypted_query_param =
                string_arg(&media, &["encrypt_query_param", "encryptQueryParam"]);
            let url = full_url
                .clone()
                .or_else(|| encrypted_query_param.as_deref().map(|param| {
                    messaging_gateway_weixin_cdn_download_url(cdn_base_url, param)
                }));
            json!({
                "id": string_arg(&media, &["media_id", "mediaId", "file_id", "fileId", "full_url", "fullUrl", "encrypt_query_param", "encryptQueryParam"]).unwrap_or_else(|| name.clone()),
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": kind,
                "size": item.get("size").or_else(|| media.get("size")).cloned().unwrap_or(Value::Null),
                "url": url.map(Value::String).unwrap_or(Value::Null),
                "fullUrl": full_url.clone().map(Value::String).unwrap_or(Value::Null),
                "full_url": full_url.map(Value::String).unwrap_or(Value::Null),
                "encryptQueryParam": encrypted_query_param.clone().map(Value::String).unwrap_or(Value::Null),
                "encrypt_query_param": encrypted_query_param.map(Value::String).unwrap_or(Value::Null),
                "aesKey": aes_key.clone().map(Value::String).unwrap_or(Value::Null),
                "aes_key": aes_key.map(Value::String).unwrap_or(Value::Null),
                "encryption": "weixin-aes-128-ecb",
                "downloadStatus": "skipped",
                "download_status": "skipped",
                "reason": "Weixin raw iLink media metadata recorded; direct media URLs are cached when reachable",
            })
        })
        .collect()
}

fn messaging_gateway_collect_weixin_attachment_refs(
    item: &Value,
    refs: &mut Vec<(&'static str, Value, Value)>,
) {
    match messaging_gateway_weixin_item_type(item) {
        Some(2) => {
            if let Some(image_item) = item.get("image_item") {
                refs.push((
                    "image",
                    image_item.clone(),
                    image_item
                        .get("media")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ));
            }
        }
        Some(3) => {
            if let Some(voice_item) = item.get("voice_item") {
                refs.push((
                    "voice",
                    voice_item.clone(),
                    voice_item
                        .get("media")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ));
            }
        }
        Some(4) => {
            if let Some(file_item) = item.get("file_item") {
                refs.push((
                    "file",
                    file_item.clone(),
                    file_item.get("media").cloned().unwrap_or_else(|| json!({})),
                ));
            }
        }
        Some(5) => {
            if let Some(video_item) = item.get("video_item") {
                refs.push((
                    "video",
                    video_item.clone(),
                    video_item
                        .get("media")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                ));
            }
        }
        _ => {}
    }
}

fn messaging_gateway_weixin_hex_aes_key_to_base64(value: &str) -> Option<String> {
    use base64::Engine;

    let trimmed = value.trim();
    if trimmed.len() != 32 || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    let mut bytes = Vec::with_capacity(16);
    for index in (0..trimmed.len()).step_by(2) {
        let byte = u8::from_str_radix(&trimmed[index..index + 2], 16).ok()?;
        bytes.push(byte);
    }
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn messaging_gateway_weixin_cdn_download_url(
    cdn_base_url: &str,
    encrypted_query_param: &str,
) -> String {
    format!(
        "{}/download?encrypted_query_param={}",
        cdn_base_url.trim_end_matches('/'),
        percent_encode_path_segment(encrypted_query_param)
    )
}

fn messaging_gateway_whatsapp_attachment_metadata(message: &Value) -> Vec<Value> {
    let urls = message
        .get("mediaUrls")
        .or_else(|| message.get("media_urls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if urls.is_empty() {
        return Vec::new();
    }
    let media_type = string_arg(message, &["mediaType", "media_type"])
        .unwrap_or_else(|| "application/octet-stream".into());
    let mime = messaging_gateway_whatsapp_media_mime(&media_type);
    let kind = mattermost_media_kind(&mime);
    urls.into_iter()
        .filter_map(|url| url.as_str().map(str::to_string))
        .enumerate()
        .map(|(index, url)| {
            let name = string_arg(message, &["fileName", "file_name", "filename", "name"])
                .unwrap_or_else(|| messaging_gateway_whatsapp_media_name(&url, &mime, index));
            json!({
                "id": string_arg(message, &["mediaId", "media_id", "messageId", "message_id", "id"]).unwrap_or_else(|| format!("whatsapp-media-{index}")),
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": kind,
                "url": url,
                "size": Value::Null,
                "downloadStatus": "skipped",
                "download_status": "skipped",
                "reason": "WhatsApp bridge media metadata recorded; remote URLs are cached when reachable",
            })
        })
        .collect()
}

fn messaging_gateway_whatsapp_media_name(url: &str, mime: &str, index: usize) -> String {
    let from_url = url
        .split('?')
        .next()
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.contains('.'))
        .map(str::to_string);
    from_url.unwrap_or_else(|| {
        let ext = if mime.starts_with("image/") {
            "jpg"
        } else if mime.starts_with("video/") {
            "mp4"
        } else if mime.starts_with("audio/") {
            "ogg"
        } else {
            "bin"
        };
        format!("whatsapp-media-{index}.{ext}")
    })
}

fn messaging_gateway_whatsapp_media_mime(media_type: &str) -> String {
    let media_type = media_type.trim().to_ascii_lowercase();
    if media_type.contains("image") {
        "image/jpeg".into()
    } else if media_type.contains("video") {
        "video/mp4".into()
    } else if media_type.contains("audio") || media_type.contains("ptt") {
        "audio/ogg".into()
    } else if media_type.contains('/') {
        media_type
    } else {
        "application/octet-stream".into()
    }
}

fn messaging_gateway_bluebubbles_attachment_metadata(message: &Value) -> Vec<Value> {
    message
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            let id = string_arg(
                &attachment,
                &["guid", "id", "attachmentGuid", "attachment_guid"],
            )?;
            let name = string_arg(
                &attachment,
                &[
                    "transferName",
                    "transfer_name",
                    "fileName",
                    "file_name",
                    "filename",
                    "name",
                ],
            )
            .unwrap_or_else(|| format!("bluebubbles-attachment-{index}"));
            let mime = string_arg(&attachment, &["mimeType", "mime_type", "contentType"])
                .unwrap_or_else(|| {
                    if attachment
                        .get("uti")
                        .and_then(Value::as_str)
                        .map(|value| value.ends_with("caf"))
                        .unwrap_or(false)
                    {
                        "audio/x-caf".into()
                    } else {
                        guess_content_type(&name).into()
                    }
                });
            Some(json!({
                "id": id,
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": mattermost_media_kind(&mime),
                "size": attachment.get("totalBytes").or_else(|| attachment.get("size")).cloned().unwrap_or(Value::Null),
                "url": Value::Null,
                "downloadStatus": "skipped",
                "download_status": "skipped",
                "reason": "BlueBubbles webhook attachment metadata recorded; server download requires BlueBubbles credentials",
            }))
        })
        .collect()
}

fn messaging_gateway_bluebubbles_record_like(message: &Value) -> bool {
    message
        .get("attachments")
        .and_then(Value::as_array)
        .is_some()
        && (message.get("chatGuid").is_some()
            || message.get("chat_guid").is_some()
            || message.get("chatIdentifier").is_some()
            || message.get("handle").is_some())
}

fn messaging_gateway_inbound_prompt(inbound: &Value) -> String {
    let platform = inbound
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("messaging_gateway");
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt =
        format!("{platform} inbound message\nchat_id: {chat_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}");
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nSkipped bridge attachments:");
            for attachment in attachments {
                let id = attachment
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                prompt.push_str(&format!("\n- {id} ({mime})"));
            }
        }
    }
    prompt
}

fn messaging_gateway_inbound_conversation_id(
    store: &AppStore,
    config: &Value,
    platform: &str,
) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some(platform)
    }) {
        return Ok(existing.id);
    }
    let persona_id = messaging_gateway_inbound_persona_id(store, config)?;
    let title = match platform {
        "wecom" => "WeCom",
        "weixin" => "Weixin",
        "yuanbao" => "Yuanbao",
        _ => "Messaging Gateway",
    };
    let conversation = store.create_conversation(Some(title.into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!(platform))?;
    Ok(conversation.id)
}

fn messaging_gateway_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

async fn dingtalk_webhook_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: DingTalkWebhookSettings,
    listener: TcpListener,
    listen_url: String,
) {
    if let Ok(state) = store.update_dingtalk_adapter_state(
        Some("running"),
        Some(json!({"type": "listening", "listenUrl": listen_url})),
        None,
        0,
        0,
    ) {
        emit_platform_adapter_event(&app, "connected", "dingtalk", &state);
    }
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let app_for_task = app.clone();
                let store_for_task = store.clone();
                let settings_for_task = settings.clone();
                tokio::spawn(async move {
                    let _ = dingtalk_webhook_handle_connection(
                        app_for_task,
                        store_for_task,
                        settings_for_task,
                        stream,
                    )
                    .await;
                });
            }
            Err(error) => {
                if let Ok(state) = store.update_dingtalk_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(format!("DingTalk webhook accept failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "dingtalk", &state);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn dingtalk_webhook_handle_connection(
    app: AppHandle,
    store: AppStore,
    settings: DingTalkWebhookSettings,
    mut stream: TcpStream,
) -> AppResult<()> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    let read = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut buffer))
        .await
        .map_err(|_| AppError::BadRequest("DingTalk webhook request read timed out".into()))?
        .map_err(|error| {
            AppError::BadRequest(format!("DingTalk webhook request read failed: {error}"))
        })?;
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let parsed = webhook_parse_request(&request);
    let status = match parsed {
        Ok(parsed) => dingtalk_webhook_process_request(&app, &store, &settings, parsed).await,
        Err(error) => Err(error),
    };
    let (code, body) = match status {
        Ok(value) => (200, value),
        Err(error) => (400, json!({"ok": false, "error": error.to_string()})),
    };
    let body_text = body.to_string();
    let response = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if code == 200 { "OK" } else { "Bad Request" },
        body_text.len(),
        body_text
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("DingTalk webhook response write failed: {error}"))
        })?;
    Ok(())
}

async fn dingtalk_webhook_process_request(
    app: &AppHandle,
    store: &AppStore,
    settings: &DingTalkWebhookSettings,
    request: ParsedWebhookRequest,
) -> AppResult<Value> {
    if request.method != "POST" {
        return Err(AppError::BadRequest(
            "DingTalk webhook only accepts POST".into(),
        ));
    }
    let request_path = request.path.split('?').next().unwrap_or(&request.path);
    if request_path != settings.path {
        return Err(AppError::BadRequest(format!(
            "DingTalk webhook path mismatch: expected {}, got {}",
            settings.path, request_path
        )));
    }
    if let Some(secret) = settings.secret.as_deref() {
        let provided = request
            .headers
            .get("x-dingtalk-webhook-secret")
            .or_else(|| request.headers.get("x-synthchat-webhook-secret"))
            .or_else(|| request.headers.get("x-webhook-secret"))
            .map(String::as_str)
            .unwrap_or_default();
        if provided != secret {
            return Err(AppError::BadRequest(
                "DingTalk webhook secret mismatch".into(),
            ));
        }
    }
    let payload = serde_json::from_str::<Value>(&request.body)
        .map_err(|error| AppError::BadRequest(format!("invalid DingTalk webhook JSON: {error}")))?;
    let config = store.config()?.dingtalk;
    let Some(inbound) = dingtalk_inbound_event_from_payload(&payload, &config, settings) else {
        let state = store.update_dingtalk_adapter_state(
            Some("running"),
            Some(json!({"type": "ignored_message"})),
            None,
            1,
            0,
        )?;
        emit_platform_adapter_event(app, "inbound_ignored", "dingtalk", &state);
        return Ok(json!({"ok": true, "ignored": "message"}));
    };
    let prompt = dingtalk_inbound_prompt(&inbound);
    let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, "dingtalk", &inbound, prompt).await
    else {
        let state =
            store.update_dingtalk_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
        emit_platform_adapter_event(app, "inbound_ignored", "dingtalk", &state);
        return Ok(json!({"ok": true, "skipped": true}));
    };
    let conversation_id = dingtalk_inbound_conversation_id(store, &config)?;
    let persona_id = dingtalk_inbound_persona_id(store, &config)?;
    let state = store.update_dingtalk_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
    emit_platform_adapter_event(app, "inbound_triggered", "dingtalk", &state);
    spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    Ok(json!({"ok": true}))
}

fn dingtalk_inbound_event_from_payload(
    payload: &Value,
    config: &Value,
    settings: &DingTalkWebhookSettings,
) -> Option<Value> {
    let message = payload
        .get("data")
        .or_else(|| payload.get("message"))
        .unwrap_or(payload);
    let message_id = string_arg(message, &["messageId", "message_id", "msgId", "msg_id"])
        .unwrap_or_else(|| new_id("dingtalk-message"));
    let sender_id = string_arg(message, &["senderId", "sender_id", "sender"])
        .or_else(|| string_arg(payload, &["senderId", "sender_id"]))?;
    let sender_staff_id = string_arg(message, &["senderStaffId", "sender_staff_id"])
        .or_else(|| string_arg(payload, &["senderStaffId", "sender_staff_id"]))
        .unwrap_or_default();
    if !dingtalk_user_allowed(config, &sender_id, &sender_staff_id) {
        return None;
    }
    let conversation_id = string_arg(
        message,
        &["conversationId", "conversation_id", "chatId", "chat_id"],
    )
    .unwrap_or_else(|| sender_id.clone());
    if !dingtalk_allowed(
        config,
        &["allowedChats", "allowed_chats"],
        "DINGTALK_ALLOWED_CHATS",
        &conversation_id,
    ) {
        return None;
    }
    let conversation_type = string_arg(message, &["conversationType", "conversation_type"])
        .unwrap_or_else(|| "1".into());
    let is_group = conversation_type == "2"
        || conversation_type.eq_ignore_ascii_case("group")
        || message
            .get("isGroup")
            .or_else(|| message.get("is_group"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let text = dingtalk_extract_text(message);
    if is_group && settings.require_mention && !dingtalk_mentions_or_matches(message, config, &text)
    {
        return None;
    }
    let files = dingtalk_file_metadata(message);
    if text.trim().is_empty() && files.is_empty() {
        return None;
    }
    let message_type = dingtalk_message_type(message, &files, &text);
    let sender_name = string_arg(
        message,
        &["senderNick", "sender_nick", "senderName", "sender_name"],
    )
    .unwrap_or_else(|| sender_id.clone());
    let mut inbound = json!({
        "platform": "dingtalk",
        "messageId": message_id,
        "message_id": message_id,
        "text": text,
        "messageType": message_type,
        "message_type": message_type,
        "source": {
            "platform": "dingtalk",
            "chatId": conversation_id,
            "chat_id": conversation_id,
            "chatType": if is_group { "group" } else { "dm" },
            "chat_type": if is_group { "group" } else { "dm" },
            "chatTitle": string_arg(message, &["conversationTitle", "conversation_title"]).map(Value::String).unwrap_or(Value::Null),
            "chat_title": string_arg(message, &["conversationTitle", "conversation_title"]).map(Value::String).unwrap_or(Value::Null),
            "userId": sender_id,
            "user_id": sender_id,
            "userIdAlt": if sender_staff_id.is_empty() { Value::Null } else { json!(sender_staff_id) },
            "user_id_alt": if sender_staff_id.is_empty() { Value::Null } else { json!(sender_staff_id) },
            "userName": sender_name,
            "user_name": sender_name,
        },
        "raw": payload,
    });
    if !files.is_empty() {
        inbound["files"] = json!(files);
        inbound["skippedFiles"] = inbound["files"].clone();
        inbound["skipped_files"] = inbound["files"].clone();
    }
    if let Some(webhook) = string_arg(message, &["sessionWebhook", "session_webhook"]) {
        inbound["sessionWebhook"] = json!(webhook);
        inbound["session_webhook"] = inbound["sessionWebhook"].clone();
    }
    Some(inbound)
}

fn dingtalk_user_allowed(config: &Value, sender_id: &str, sender_staff_id: &str) -> bool {
    let allowed = telegram_string_set(
        config,
        &["allowedUsers", "allowed_users"],
        "DINGTALK_ALLOWED_USERS",
    );
    if allowed.is_empty() || allowed.contains("*") {
        return true;
    }
    allowed.contains(sender_id) || !sender_staff_id.is_empty() && allowed.contains(sender_staff_id)
}

fn dingtalk_allowed(config: &Value, keys: &[&str], env_key: &str, value: &str) -> bool {
    let allowed = telegram_string_set(config, keys, env_key);
    allowed.is_empty() || allowed.contains("*") || allowed.contains(value)
}

fn dingtalk_mentions_or_matches(message: &Value, config: &Value, text: &str) -> bool {
    if message
        .get("isInAtList")
        .or_else(|| message.get("is_in_at_list"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let free_chats = telegram_string_set(
        config,
        &["freeResponseChats", "free_response_chats"],
        "DINGTALK_FREE_RESPONSE_CHATS",
    );
    if let Some(chat_id) = string_arg(
        message,
        &["conversationId", "conversation_id", "chatId", "chat_id"],
    ) {
        if free_chats.contains(&chat_id) {
            return true;
        }
    }
    let patterns = telegram_string_set(
        config,
        &["mentionPatterns", "mention_patterns"],
        "DINGTALK_MENTION_PATTERNS",
    );
    patterns
        .iter()
        .any(|pattern| !pattern.is_empty() && text.contains(pattern))
}

fn dingtalk_extract_text(message: &Value) -> String {
    if let Some(text) = string_arg(message, &["text"]) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            return string_arg(&value, &["content", "text"]).unwrap_or(text);
        }
        return text;
    }
    if let Some(content) = message
        .get("text")
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
    {
        return content.to_string();
    }
    if let Some(content) = string_arg(message, &["content", "markdown", "richText"]) {
        return content;
    }
    if let Some(items) = message
        .get("richTextContent")
        .or_else(|| message.get("rich_text_content"))
        .and_then(|value| {
            value
                .get("richTextList")
                .or_else(|| value.get("rich_text_list"))
        })
        .and_then(Value::as_array)
    {
        return items
            .iter()
            .filter_map(|item| string_arg(item, &["text", "content"]))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

fn dingtalk_file_metadata(message: &Value) -> Vec<Value> {
    let mut files = Vec::new();
    for key in [
        "imageContent",
        "image_content",
        "audioContent",
        "audio_content",
        "videoContent",
        "video_content",
        "fileContent",
        "file_content",
    ] {
        let Some(content) = message.get(key) else {
            continue;
        };
        let id = string_arg(
            content,
            &[
                "downloadCode",
                "download_code",
                "mediaId",
                "media_id",
                "fileId",
                "file_id",
            ],
        )
        .unwrap_or_else(|| key.to_string());
        let name = string_arg(content, &["fileName", "file_name", "name"])
            .unwrap_or_else(|| key.to_string());
        let mime = string_arg(content, &["mimeType", "mime_type"])
            .unwrap_or_else(|| guess_content_type(&name).into());
        files.push(json!({
            "id": id,
            "name": name,
            "mimeType": mime,
            "mime_type": mime,
            "type": mattermost_media_kind(&mime),
            "downloadStatus": "skipped",
            "download_status": "skipped",
            "reason": "DingTalk bridge runtime does not implement robot media download yet",
        }));
    }
    files
}

fn dingtalk_message_type(message: &Value, files: &[Value], text: &str) -> &'static str {
    let raw_type = string_arg(
        message,
        &["messageType", "message_type", "msgtype", "msgType"],
    )
    .unwrap_or_default()
    .to_ascii_lowercase();
    if raw_type.contains("picture") || raw_type.contains("image") {
        return "photo";
    }
    if raw_type.contains("voice") || raw_type.contains("audio") {
        return "voice";
    }
    if raw_type.contains("video") {
        return "video";
    }
    if !files.is_empty() {
        return mattermost_message_type_from_media(&json!(files
            .iter()
            .filter_map(|file| file.get("mimeType").and_then(Value::as_str))
            .collect::<Vec<_>>()));
    }
    if text.trim_start().starts_with('/') {
        "command"
    } else {
        "text"
    }
}

fn dingtalk_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt =
        format!("DingTalk inbound message\nchat_id: {chat_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}");
    if let Some(files) = inbound.get("files").and_then(Value::as_array) {
        if !files.is_empty() {
            prompt.push_str("\n\nSkipped DingTalk files:");
            for file in files {
                let id = file.get("id").and_then(Value::as_str).unwrap_or("file");
                let mime = file
                    .get("mimeType")
                    .or_else(|| file.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                prompt.push_str(&format!("\n- {id} ({mime})"));
            }
        }
    }
    prompt
}

fn dingtalk_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("dingtalk")
    }) {
        return Ok(existing.id);
    }
    let persona_id = dingtalk_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("DingTalk".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("dingtalk"))?;
    Ok(conversation.id)
}

fn dingtalk_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

async fn email_adapter_loop(app: AppHandle, store: AppStore, settings: EmailSettings) {
    if let Ok(state) = store.update_email_adapter_state(
        Some("running"),
        Some(json!({"type": "polling", "mailbox": "INBOX"})),
        None,
        0,
        0,
    ) {
        emit_platform_adapter_event(&app, "connected", "email", &state);
    }
    loop {
        match email_poll_once(&app, &store, &settings).await {
            Ok(count) => {
                if let Ok(state) = store.update_email_adapter_state(
                    Some("running"),
                    Some(json!({"type": "poll", "count": count, "mailbox": "INBOX"})),
                    None,
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "poll", "email", &state);
                }
            }
            Err(error) => {
                if let Ok(state) = store.update_email_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(error.to_string()),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "email", &state);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(settings.poll_interval_seconds)).await;
    }
}

async fn email_poll_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &EmailSettings,
) -> AppResult<usize> {
    let cache_dir = store.data_dir().join("attachments").join("email");
    let settings_for_task = settings.clone();
    let messages = tokio::task::spawn_blocking(move || {
        email_fetch_unseen_messages(&settings_for_task, &cache_dir)
    })
    .await
    .map_err(|error| AppError::BadRequest(format!("Email IMAP task failed: {error}")))??;
    let config = store.config()?.email;
    let mut processed = 0usize;
    for inbound in messages {
        let prompt = email_inbound_prompt(&inbound);
        let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, "email", &inbound, prompt).await
        else {
            let state =
                store.update_email_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
            emit_platform_adapter_event(app, "inbound_ignored", "email", &state);
            continue;
        };
        let conversation_id = email_inbound_conversation_id(store, &config)?;
        let persona_id = email_inbound_persona_id(store, &config)?;
        let state = store.update_email_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
        emit_platform_adapter_event(app, "inbound_triggered", "email", &state);
        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
        processed += 1;
    }
    Ok(processed)
}

fn email_fetch_unseen_messages(
    settings: &EmailSettings,
    cache_dir: &Path,
) -> AppResult<Vec<Value>> {
    use mailparse::MailHeaderMap;

    let imap_host = settings
        .imap_host
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("Email IMAP host is not configured".into()))?;
    fs::create_dir_all(cache_dir)?;
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|error| AppError::BadRequest(format!("Email TLS init failed: {error}")))?;
    let client = imap::connect((imap_host, settings.imap_port), imap_host, &tls)
        .map_err(|error| AppError::BadRequest(format!("Email IMAP connect failed: {error}")))?;
    let mut session = client
        .login(&settings.address, &settings.password)
        .map_err(|(error, _client)| {
            AppError::BadRequest(format!("Email IMAP login failed: {error}"))
        })?;
    session
        .select("INBOX")
        .map_err(|error| AppError::BadRequest(format!("Email IMAP select failed: {error}")))?;
    let unseen = session
        .search("UNSEEN")
        .map_err(|error| AppError::BadRequest(format!("Email IMAP search failed: {error}")))?;
    let mut messages = Vec::new();
    for sequence in unseen {
        let fetches = session
            .fetch(sequence.to_string(), "RFC822")
            .map_err(|error| AppError::BadRequest(format!("Email IMAP fetch failed: {error}")))?;
        for fetch in fetches.iter() {
            let Some(body) = fetch.body() else {
                continue;
            };
            let parsed = mailparse::parse_mail(body)
                .map_err(|error| AppError::BadRequest(format!("Email parse failed: {error}")))?;
            let from_raw = parsed.headers.get_first_value("From").unwrap_or_default();
            let from = email_extract_address(&from_raw);
            if from.is_empty()
                || from.eq_ignore_ascii_case(&settings.address)
                || email_is_automated_sender(&from, &parsed)
                || !email_sender_allowed(settings, &from)
            {
                continue;
            }
            let subject = parsed
                .headers
                .get_first_value("Subject")
                .unwrap_or_else(|| "(no subject)".into());
            let message_id = parsed
                .headers
                .get_first_value("Message-ID")
                .unwrap_or_else(|| new_id("email-message"));
            let in_reply_to = parsed.headers.get_first_value("In-Reply-To");
            let sender_name = email_sender_name(&from_raw, &from);
            let body_text = email_extract_text_body(&parsed);
            let attachments = if settings.skip_attachments {
                Vec::new()
            } else {
                email_extract_attachments(&parsed, cache_dir)?
            };
            let mut text = body_text.trim().to_string();
            if !subject.trim().is_empty() && !subject.to_ascii_lowercase().starts_with("re:") {
                text = format!("[Subject: {subject}]\n\n{text}");
            }
            if text.trim().is_empty() && attachments.is_empty() {
                text = "(empty email)".into();
            }
            let message_type = email_message_type(&attachments);
            let mut inbound = json!({
                "platform": "email",
                "messageId": message_id,
                "message_id": message_id,
                "subject": subject,
                "text": text,
                "messageType": message_type,
                "message_type": message_type,
                "source": {
                    "platform": "email",
                    "chatId": from,
                    "chat_id": from,
                    "chatType": "dm",
                    "chat_type": "dm",
                    "userId": from,
                    "user_id": from,
                    "userName": sender_name,
                    "user_name": sender_name,
                },
            });
            if let Some(in_reply_to) = in_reply_to {
                inbound["replyToMessageId"] = json!(in_reply_to);
                inbound["reply_to_message_id"] = inbound["replyToMessageId"].clone();
            }
            if !attachments.is_empty() {
                inbound["attachments"] = json!(attachments);
                inbound["mediaUrls"] = json!(inbound["attachments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|attachment| attachment.get("path").and_then(Value::as_str))
                    .collect::<Vec<_>>());
                inbound["media_urls"] = inbound["mediaUrls"].clone();
            }
            messages.push(inbound);
        }
        let _ = session.store(sequence.to_string(), "+FLAGS (\\Seen)");
    }
    let _ = session.logout();
    Ok(messages)
}

fn email_extract_address(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw[start + 1..].find('>') {
            return raw[start + 1..start + 1 + end].trim().to_ascii_lowercase();
        }
    }
    raw.trim().to_ascii_lowercase()
}

fn email_sender_name(raw: &str, address: &str) -> String {
    let name = raw
        .split('<')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_matches('"');
    if name.is_empty() {
        address.to_string()
    } else {
        name.to_string()
    }
}

fn email_sender_allowed(settings: &EmailSettings, from: &str) -> bool {
    settings.allowed_users.is_empty()
        || settings.allowed_users.contains("*")
        || settings.allowed_users.contains(&from.to_ascii_lowercase())
}

fn email_is_automated_sender(message_from: &str, parsed: &mailparse::ParsedMail<'_>) -> bool {
    use mailparse::MailHeaderMap;
    let lower = message_from.to_ascii_lowercase();
    let automated_patterns = [
        "noreply",
        "no-reply",
        "no_reply",
        "donotreply",
        "do-not-reply",
        "mailer-daemon",
        "postmaster",
        "bounce",
        "notifications@",
        "automated@",
    ];
    if automated_patterns
        .iter()
        .any(|pattern| lower.contains(pattern))
    {
        return true;
    }
    for header in [
        "Auto-Submitted",
        "Precedence",
        "X-Auto-Response-Suppress",
        "List-Unsubscribe",
    ] {
        if parsed.headers.get_first_value(header).is_some() {
            return true;
        }
    }
    false
}

fn email_extract_text_body(parsed: &mailparse::ParsedMail<'_>) -> String {
    if parsed.subparts.is_empty() {
        if parsed.ctype.mimetype.eq_ignore_ascii_case("text/html") {
            return email_strip_html(&parsed.get_body().unwrap_or_default());
        }
        return parsed.get_body().unwrap_or_default();
    }
    for part in &parsed.subparts {
        if part.ctype.mimetype.eq_ignore_ascii_case("text/plain") {
            if let Ok(body) = part.get_body() {
                return body;
            }
        }
    }
    for part in &parsed.subparts {
        if part.ctype.mimetype.eq_ignore_ascii_case("text/html") {
            if let Ok(body) = part.get_body() {
                return email_strip_html(&body);
            }
        }
    }
    parsed
        .subparts
        .iter()
        .map(email_extract_text_body)
        .find(|body| !body.trim().is_empty())
        .unwrap_or_default()
}

fn email_strip_html(html: &str) -> String {
    let mut text = html
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    while let Some(start) = text.find('<') {
        let Some(end) = text[start..].find('>') else {
            break;
        };
        text.replace_range(start..start + end + 1, "");
    }
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

fn email_extract_attachments(
    parsed: &mailparse::ParsedMail<'_>,
    cache_dir: &Path,
) -> AppResult<Vec<Value>> {
    let mut attachments = Vec::new();
    email_collect_attachments(parsed, cache_dir, &mut attachments)?;
    Ok(attachments)
}

fn email_collect_attachments(
    parsed: &mailparse::ParsedMail<'_>,
    cache_dir: &Path,
    attachments: &mut Vec<Value>,
) -> AppResult<()> {
    use mailparse::MailHeaderMap;
    let disposition = parsed
        .headers
        .get_first_value("Content-Disposition")
        .unwrap_or_default();
    let lower_disposition = disposition.to_ascii_lowercase();
    let is_attachment = lower_disposition.contains("attachment")
        || lower_disposition.contains("inline") && !parsed.ctype.mimetype.starts_with("text/");
    if is_attachment {
        let name = email_attachment_filename(&disposition)
            .unwrap_or_else(|| format!("attachment.{}", parsed.ctype.mimetype.replace('/', ".")));
        let bytes = parsed.get_body_raw().map_err(|error| {
            AppError::BadRequest(format!("Email attachment decode failed: {error}"))
        })?;
        let safe_name = mattermost_safe_file_name(&name);
        let path = cache_dir.join(format!("{}-{safe_name}", new_id("email-attachment")));
        fs::write(&path, &bytes)?;
        let mime = parsed.ctype.mimetype.clone();
        attachments.push(json!({
            "id": new_id("email-attachment"),
            "name": name,
            "mimeType": mime,
            "mime_type": mime,
            "type": mattermost_media_kind(&mime),
            "size": bytes.len(),
            "path": path.to_string_lossy(),
        }));
    }
    for part in &parsed.subparts {
        email_collect_attachments(part, cache_dir, attachments)?;
    }
    Ok(())
}

fn email_attachment_filename(disposition: &str) -> Option<String> {
    for segment in disposition.split(';') {
        if let Some((key, value)) = segment.split_once('=') {
            if key.trim().eq_ignore_ascii_case("filename") {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn email_message_type(attachments: &[Value]) -> &'static str {
    if attachments.iter().any(|attachment| {
        attachment
            .get("mimeType")
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.starts_with("image/"))
    }) {
        "photo"
    } else if attachments.is_empty() {
        "text"
    } else {
        "document"
    }
}

fn email_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let from = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let subject = inbound
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!("Email inbound message\nfrom: {from}\nsubject: {subject}\n\n{text}");
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    prompt
}

fn email_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("email")
    }) {
        return Ok(existing.id);
    }
    let persona_id = email_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Email".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("email"))?;
    Ok(conversation.id)
}

fn email_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

async fn feishu_webhook_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: FeishuWebhookSettings,
    listener: TcpListener,
    listen_url: String,
) {
    if let Ok(state) = store.update_feishu_adapter_state(
        Some("running"),
        Some(json!({"type": "listening", "listenUrl": listen_url})),
        None,
        0,
        0,
    ) {
        emit_platform_adapter_event(&app, "connected", "feishu", &state);
    }
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let app_for_task = app.clone();
                let store_for_task = store.clone();
                let settings_for_task = settings.clone();
                tokio::spawn(async move {
                    let _ = feishu_webhook_handle_connection(
                        app_for_task,
                        store_for_task,
                        settings_for_task,
                        stream,
                    )
                    .await;
                });
            }
            Err(error) => {
                if let Ok(state) = store.update_feishu_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(format!("Feishu webhook accept failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "feishu", &state);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn feishu_webhook_handle_connection(
    app: AppHandle,
    store: AppStore,
    settings: FeishuWebhookSettings,
    mut stream: TcpStream,
) -> AppResult<()> {
    let mut buffer = vec![0_u8; 1024 * 1024];
    let read = tokio::time::timeout(Duration::from_secs(30), stream.read(&mut buffer))
        .await
        .map_err(|_| AppError::BadRequest("Feishu webhook request read timed out".into()))?
        .map_err(|error| {
            AppError::BadRequest(format!("Feishu webhook request read failed: {error}"))
        })?;
    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
    let parsed = webhook_parse_request(&request);
    let status = match parsed {
        Ok(parsed) => feishu_webhook_process_request(&app, &store, &settings, parsed).await,
        Err(error) => Err(error),
    };
    let (code, body) = match status {
        Ok(value) => (200, value),
        Err(error) => (400, json!({"ok": false, "error": error.to_string()})),
    };
    let body_text = body.to_string();
    let response = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        if code == 200 { "OK" } else { "Bad Request" },
        body_text.len(),
        body_text
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Feishu webhook response write failed: {error}"))
        })?;
    Ok(())
}

async fn feishu_webhook_process_request(
    app: &AppHandle,
    store: &AppStore,
    settings: &FeishuWebhookSettings,
    request: ParsedWebhookRequest,
) -> AppResult<Value> {
    if request.method != "POST" {
        return Err(AppError::BadRequest(
            "Feishu webhook only accepts POST".into(),
        ));
    }
    let request_path = request.path.split('?').next().unwrap_or(&request.path);
    if request_path != settings.path {
        return Err(AppError::BadRequest(format!(
            "Feishu webhook path mismatch: expected {}, got {}",
            settings.path, request_path
        )));
    }
    let payload = serde_json::from_str::<Value>(&request.body)
        .map_err(|error| AppError::BadRequest(format!("invalid Feishu webhook JSON: {error}")))?;
    feishu_verify_webhook_token(settings, &payload)?;
    if let Some(challenge) = payload.get("challenge").and_then(Value::as_str) {
        return Ok(json!({"challenge": challenge}));
    }
    if payload.get("encrypt").is_some() {
        return Err(AppError::BadRequest(
            "encrypted Feishu webhook payloads are not supported yet".into(),
        ));
    }
    let event_type = payload
        .pointer("/header/event_type")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(
        event_type,
        "im.message.receive_v1" | "drive.notice.comment_add_v1"
    ) {
        let state = store.update_feishu_adapter_state(
            Some("running"),
            Some(
                json!({"type": "ignored_event", "eventType": event_type, "event_type": event_type}),
            ),
            None,
            1,
            0,
        )?;
        emit_platform_adapter_event(app, "inbound_ignored", "feishu", &state);
        return Ok(json!({"ok": true, "ignored": event_type}));
    }
    let config = store.config()?.feishu;
    let Some(inbound) = feishu_inbound_event_from_payload(&payload, &config, settings) else {
        let state = store.update_feishu_adapter_state(
            Some("running"),
            Some(json!({"type": "ignored_message"})),
            None,
            1,
            0,
        )?;
        emit_platform_adapter_event(app, "inbound_ignored", "feishu", &state);
        return Ok(json!({"ok": true, "ignored": "message"}));
    };
    let prompt = feishu_inbound_prompt(&inbound);
    let Some(prompt) = apply_pre_gateway_dispatch_hooks(store, "feishu", &inbound, prompt).await
    else {
        let state =
            store.update_feishu_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
        emit_platform_adapter_event(app, "inbound_ignored", "feishu", &state);
        return Ok(json!({"ok": true, "skipped": true}));
    };
    let conversation_id = feishu_inbound_conversation_id(store, &config)?;
    let persona_id = feishu_inbound_persona_id(store, &config)?;
    let state = store.update_feishu_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
    emit_platform_adapter_event(app, "inbound_triggered", "feishu", &state);
    spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    Ok(json!({"ok": true}))
}

fn feishu_verify_webhook_token(settings: &FeishuWebhookSettings, payload: &Value) -> AppResult<()> {
    let Some(expected) = settings.verification_token.as_deref() else {
        return Ok(());
    };
    let provided = payload
        .get("token")
        .or_else(|| payload.pointer("/header/token"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if provided != expected {
        return Err(AppError::BadRequest(
            "Feishu webhook verification token mismatch".into(),
        ));
    }
    Ok(())
}

pub(super) fn feishu_inbound_event_from_payload(
    payload: &Value,
    config: &Value,
    settings: &FeishuWebhookSettings,
) -> Option<Value> {
    let event_type = payload
        .pointer("/header/event_type")
        .or_else(|| payload.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if event_type == "drive.notice.comment_add_v1" {
        return feishu_comment_inbound_event_from_payload(payload, config, settings);
    }
    let event = payload.get("event")?;
    let message = event.get("message")?;
    let sender = event.get("sender").cloned().unwrap_or_else(|| json!({}));
    let sender_type = sender
        .get("sender_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sender_type == "bot" || sender_type == "app" {
        return None;
    }
    let sender_id = sender
        .get("sender_id")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let user_id = feishu_first_string(
        &sender_id,
        &[
            "open_id", "openId", "user_id", "userId", "union_id", "unionId",
        ],
    )
    .unwrap_or_default();
    if user_id.is_empty() {
        return None;
    }
    let chat_id = string_arg(message, &["chat_id", "chatId", "open_chat_id"])?;
    let chat_type = string_arg(message, &["chat_type", "chatType"]).unwrap_or_else(|| "p2p".into());
    let is_group = chat_type != "p2p";
    if !feishu_allowed(
        config,
        &["allowedUsers", "allowed_users"],
        "FEISHU_ALLOWED_USERS",
        &user_id,
    ) {
        return None;
    }
    if !feishu_allowed(
        config,
        &["allowedChats", "allowed_chats"],
        "FEISHU_ALLOWED_CHATS",
        &chat_id,
    ) {
        return None;
    }
    if is_group && settings.require_mention && !feishu_message_mentions_bot(message, settings) {
        return None;
    }
    let message_id = string_arg(message, &["message_id", "messageId"])
        .unwrap_or_else(|| new_id("feishu-message"));
    let raw_type = string_arg(message, &["message_type", "messageType", "msg_type"])
        .unwrap_or_else(|| "text".into());
    let content = feishu_message_content(message);
    let normalized = feishu_normalize_message_content(&raw_type, &content, message);
    if normalized
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .is_empty()
        && normalized
            .get("files")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return None;
    }
    let sender_name = string_arg(&sender, &["sender_name", "senderName", "name"])
        .unwrap_or_else(|| user_id.clone());
    let message_type = normalized
        .get("messageType")
        .and_then(Value::as_str)
        .unwrap_or("text");
    let mut inbound = json!({
        "platform": "feishu",
        "eventId": payload.pointer("/header/event_id").cloned().unwrap_or(Value::Null),
        "event_id": payload.pointer("/header/event_id").cloned().unwrap_or(Value::Null),
        "messageId": message_id,
        "message_id": message_id,
        "text": normalized.get("text").cloned().unwrap_or(Value::String(String::new())),
        "messageType": message_type,
        "message_type": message_type,
        "source": {
            "platform": "feishu",
            "chatId": chat_id,
            "chat_id": chat_id,
            "chatType": chat_type,
            "chat_type": chat_type,
            "userId": user_id,
            "user_id": user_id,
            "userIdAlt": feishu_first_string(&sender_id, &["union_id", "unionId", "user_id", "userId"]).map(Value::String).unwrap_or(Value::Null),
            "user_id_alt": feishu_first_string(&sender_id, &["union_id", "unionId", "user_id", "userId"]).map(Value::String).unwrap_or(Value::Null),
            "userName": sender_name,
            "user_name": sender_name,
        },
        "raw": payload,
    });
    if let Some(files) = normalized.get("files") {
        inbound["files"] = files.clone();
        inbound["skippedFiles"] = files.clone();
        inbound["skipped_files"] = files.clone();
    }
    if let Some(parent_id) = string_arg(message, &["parent_id", "parentId"]) {
        inbound["replyToMessageId"] = json!(parent_id);
        inbound["reply_to_message_id"] = inbound["replyToMessageId"].clone();
    }
    Some(inbound)
}

fn feishu_comment_inbound_event_from_payload(
    payload: &Value,
    config: &Value,
    settings: &FeishuWebhookSettings,
) -> Option<Value> {
    let event = payload.get("event")?;
    let notice_meta = event
        .get("notice_meta")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let from_user = notice_meta
        .get("from_user_id")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let user_id = feishu_first_string(
        &from_user,
        &[
            "open_id", "openId", "user_id", "userId", "union_id", "unionId",
        ],
    )?;
    if user_id.is_empty() {
        return None;
    }
    if !feishu_allowed(
        config,
        &["allowedUsers", "allowed_users"],
        "FEISHU_ALLOWED_USERS",
        &user_id,
    ) {
        return None;
    }
    let file_token = string_arg(&notice_meta, &["file_token", "fileToken"])?;
    let file_type = string_arg(&notice_meta, &["file_type", "fileType"]).unwrap_or_default();
    let comment_id = string_arg(event, &["comment_id", "commentId"])?;
    let reply_id = string_arg(event, &["reply_id", "replyId"]).unwrap_or_default();
    let notice_type = string_arg(&notice_meta, &["notice_type", "noticeType"]).unwrap_or_default();
    if !matches!(notice_type.as_str(), "add_comment" | "add_reply" | "") {
        return None;
    }
    let chat_id = format!("comment-doc:{file_type}:{file_token}");
    if !feishu_allowed(
        config,
        &["allowedChats", "allowed_chats"],
        "FEISHU_ALLOWED_CHATS",
        &chat_id,
    ) {
        return None;
    }
    let is_mentioned = event
        .get("is_mentioned")
        .or_else(|| event.get("isMentioned"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if settings.require_mention && !is_mentioned {
        return None;
    }
    let event_id = string_arg(event, &["event_id", "eventId"])
        .or_else(|| {
            payload
                .pointer("/header/event_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| new_id("feishu-comment"));
    let text = feishu_comment_inbound_text(
        &file_type,
        &file_token,
        &comment_id,
        &reply_id,
        &notice_type,
        is_mentioned,
    );
    let sender_name = string_arg(&from_user, &["name"]).unwrap_or_else(|| user_id.clone());
    Some(json!({
        "platform": "feishu",
        "eventId": event_id,
        "event_id": event_id,
        "messageId": event_id,
        "message_id": event_id,
        "text": text,
        "messageType": "comment",
        "message_type": "comment",
        "comment": {
            "fileToken": file_token,
            "file_token": file_token,
            "fileType": file_type,
            "file_type": file_type,
            "commentId": comment_id,
            "comment_id": comment_id,
            "replyId": reply_id,
            "reply_id": reply_id,
            "noticeType": notice_type,
            "notice_type": notice_type,
            "isMentioned": is_mentioned,
            "is_mentioned": is_mentioned,
        },
        "source": {
            "platform": "feishu",
            "chatId": chat_id,
            "chat_id": chat_id,
            "chatType": "comment",
            "chat_type": "comment",
            "userId": user_id,
            "user_id": user_id,
            "userName": sender_name,
            "user_name": sender_name,
        },
        "raw": payload,
    }))
}

fn feishu_comment_inbound_text(
    file_type: &str,
    file_token: &str,
    comment_id: &str,
    reply_id: &str,
    notice_type: &str,
    is_mentioned: bool,
) -> String {
    let mut lines = vec![
        "Feishu document comment event".to_string(),
        "This is a Feishu document comment thread, not an IM chat.".to_string(),
        "Use the Feishu Drive comment tools to inspect the document/comment context before replying.".to_string(),
        "Do not call feishu_drive_add_comment or feishu_drive_reply_comment unless the user explicitly asks you to post a reply.".to_string(),
        format!("file_type: {file_type}"),
        format!("file_token: {file_token}"),
        format!("comment_id: {comment_id}"),
        format!("notice_type: {notice_type}"),
        format!("is_mentioned: {is_mentioned}"),
    ];
    if !reply_id.is_empty() {
        lines.push(format!("reply_id: {reply_id}"));
    }
    lines.join("\n")
}

fn feishu_first_string(value: &Value, keys: &[&str]) -> Option<String> {
    string_arg(value, keys)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn feishu_allowed(config: &Value, keys: &[&str], env_key: &str, value: &str) -> bool {
    let allowed = telegram_string_set(config, keys, env_key);
    allowed.is_empty() || allowed.contains("*") || allowed.contains(value)
}

fn feishu_message_mentions_bot(message: &Value, settings: &FeishuWebhookSettings) -> bool {
    if let Some(mentions) = message.get("mentions").and_then(Value::as_array) {
        for mention in mentions {
            let id = mention.get("id").cloned().unwrap_or_else(|| json!({}));
            if settings.bot_open_id.as_deref().is_some_and(|bot| {
                feishu_first_string(&id, &["open_id", "openId"]).as_deref() == Some(bot)
            }) {
                return true;
            }
            if settings.bot_user_id.as_deref().is_some_and(|bot| {
                feishu_first_string(&id, &["user_id", "userId"]).as_deref() == Some(bot)
            }) {
                return true;
            }
            if settings.bot_name.as_deref().is_some_and(|bot| {
                feishu_first_string(mention, &["name", "key"]).as_deref() == Some(bot)
            }) {
                return true;
            }
        }
    }
    let text = feishu_normalize_message_content(
        &string_arg(message, &["message_type", "messageType"]).unwrap_or_default(),
        &feishu_message_content(message),
        message,
    )
    .get("text")
    .and_then(Value::as_str)
    .unwrap_or_default()
    .to_string();
    settings
        .bot_name
        .as_deref()
        .is_some_and(|name| text.contains(&format!("@{name}")))
}

fn feishu_message_content(message: &Value) -> Value {
    let Some(content) = string_arg(message, &["content"]) else {
        return json!({});
    };
    serde_json::from_str::<Value>(&content).unwrap_or_else(|_| json!({"text": content}))
}

fn feishu_normalize_message_content(raw_type: &str, content: &Value, message: &Value) -> Value {
    let message_type = raw_type.trim().to_ascii_lowercase();
    match message_type.as_str() {
        "text" => {
            let text = string_arg(content, &["text"]).unwrap_or_default();
            json!({"text": text, "messageType": if text.trim_start().starts_with('/') { "command" } else { "text" }})
        }
        "post" => json!({"text": feishu_post_text(content), "messageType": "text"}),
        "image" => {
            let key = string_arg(content, &["image_key", "imageKey"]).unwrap_or_default();
            json!({"text": "[Image]", "messageType": "photo", "files": [feishu_file_metadata(&key, "image", "image/jpeg")]})
        }
        "audio" => {
            let key = string_arg(content, &["file_key", "fileKey"]).unwrap_or_default();
            json!({"text": "[Audio]", "messageType": "voice", "files": [feishu_file_metadata(&key, "audio", "audio/ogg")]})
        }
        "media" => {
            let key = string_arg(content, &["file_key", "fileKey"]).unwrap_or_default();
            json!({"text": "[Video]", "messageType": "video", "files": [feishu_file_metadata(&key, "video", "video/mp4")]})
        }
        "file" => {
            let key = string_arg(content, &["file_key", "fileKey"]).unwrap_or_default();
            let name =
                string_arg(content, &["file_name", "fileName"]).unwrap_or_else(|| "file".into());
            json!({"text": format!("[File] {name}"), "messageType": "document", "files": [feishu_file_metadata(&key, &name, guess_content_type(&name))]})
        }
        "interactive" => json!({"text": feishu_card_text(content), "messageType": "command"}),
        _ => {
            json!({"text": string_arg(content, &["text", "content"]).unwrap_or_else(|| format!("[{} message]", message_type)), "messageType": if message.get("parent_id").is_some() { "text" } else { "text" }})
        }
    }
}

fn feishu_file_metadata(key: &str, name: &str, mime: &str) -> Value {
    json!({
        "id": key,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": mattermost_media_kind(mime),
        "downloadStatus": "skipped",
        "download_status": "skipped",
        "reason": "Feishu message resource download is not implemented in this webhook runtime yet",
    })
}

fn feishu_post_text(content: &Value) -> String {
    let post = content.get("post").unwrap_or(content);
    for locale in ["zh_cn", "en_us"] {
        if let Some(text) = feishu_post_locale_text(post.get(locale).unwrap_or(&Value::Null)) {
            return text;
        }
    }
    feishu_post_locale_text(post).unwrap_or_else(|| "[Rich text message]".into())
}

fn feishu_post_locale_text(value: &Value) -> Option<String> {
    let mut lines = Vec::new();
    if let Some(title) = value.get("title").and_then(Value::as_str) {
        if !title.trim().is_empty() {
            lines.push(title.trim().to_string());
        }
    }
    for row in value.get("content").and_then(Value::as_array)? {
        let mut parts = Vec::new();
        for item in row.as_array().cloned().unwrap_or_default() {
            if let Some(text) = string_arg(&item, &["text", "un_escape_text", "name"]) {
                parts.push(text);
            } else if item.get("image_key").is_some() || item.get("imageKey").is_some() {
                parts.push("[Image]".into());
            }
        }
        let line = parts.join("");
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    Some(lines.join("\n").trim().to_string()).filter(|value| !value.is_empty())
}

fn feishu_card_text(content: &Value) -> String {
    if let Some(text) = string_arg(content, &["text", "content", "title"]) {
        return text;
    }
    "[Interactive message]".into()
}

fn feishu_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt =
        format!("Feishu inbound message\nchat_id: {chat_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}");
    if let Some(files) = inbound.get("files").and_then(Value::as_array) {
        if !files.is_empty() {
            prompt.push_str("\n\nSkipped Feishu files:");
            for file in files {
                let id = file.get("id").and_then(Value::as_str).unwrap_or("file");
                let mime = file
                    .get("mimeType")
                    .or_else(|| file.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                prompt.push_str(&format!("\n- {id} ({mime})"));
            }
        }
    }
    prompt
}

fn feishu_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("feishu")
    }) {
        return Ok(existing.id);
    }
    let persona_id = feishu_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Feishu".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("feishu"))?;
    Ok(conversation.id)
}

fn feishu_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

async fn signal_adapter_loop(app: AppHandle, store: AppStore, settings: SignalSettings) {
    let mut backoff = 2u64;
    loop {
        match signal_adapter_connect_once(&app, &store, &settings).await {
            Ok(()) => {
                if let Ok(state) =
                    store.update_signal_adapter_state(Some("stopped"), None, None, 0, 0)
                {
                    emit_platform_adapter_event(&app, "stopped", "signal", &state);
                }
                break;
            }
            Err(error) => {
                if let Ok(state) = store.update_signal_adapter_state(
                    Some("reconnecting"),
                    None,
                    Some(error.to_string()),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app, "reconnecting", "signal", &state);
                }
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(60);
            }
        }
    }
}

async fn signal_adapter_connect_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &SignalSettings,
) -> AppResult<()> {
    let client = signal_stream_client()?;
    let url = signal_events_url(settings)?;
    let response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Signal SSE connect failed: {error}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Signal SSE connect failed ({})",
            status.as_u16()
        )));
    }
    let state = store.update_signal_adapter_state(
        Some("running"),
        Some(json!({
            "type": "connected",
            "eventsUrl": url.to_string(),
            "events_url": url.to_string(),
        })),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(app, "connected", "signal", &state);

    let config = store.config()?.signal;
    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk
            .map_err(|error| AppError::BadRequest(format!("Signal SSE read failed: {error}")))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(index) = buffer.find('\n') {
            let line = buffer[..index].trim().to_string();
            buffer = buffer[index + 1..].to_string();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data.is_empty() {
                continue;
            }
            let envelope = serde_json::from_str::<Value>(data).map_err(|error| {
                AppError::BadRequest(format!("invalid Signal SSE JSON: {error}"))
            })?;
            if let Some(inbound) = signal_inbound_event_from_envelope(&envelope, &config, settings)
            {
                let prompt = signal_inbound_prompt(&inbound);
                let Some(prompt) =
                    apply_pre_gateway_dispatch_hooks(store, "signal", &inbound, prompt).await
                else {
                    let state = store.update_signal_adapter_state(
                        Some("running"),
                        Some(inbound),
                        None,
                        1,
                        0,
                    )?;
                    emit_platform_adapter_event(app, "inbound_ignored", "signal", &state);
                    continue;
                };
                let conversation_id = signal_inbound_conversation_id(store, &config)?;
                let persona_id = signal_inbound_persona_id(store, &config)?;
                let state = store.update_signal_adapter_state(
                    Some("running"),
                    Some(inbound),
                    None,
                    1,
                    1,
                )?;
                emit_platform_adapter_event(app, "inbound_triggered", "signal", &state);
                spawn_background_chat_turn_for_job(
                    app.clone(),
                    conversation_id,
                    persona_id,
                    prompt,
                    None,
                );
            } else {
                let state = store.update_signal_adapter_state(
                    Some("running"),
                    Some(json!({"type": "ignored_envelope"})),
                    None,
                    1,
                    0,
                )?;
                emit_platform_adapter_event(app, "inbound_ignored", "signal", &state);
            }
        }
    }
    Err(AppError::BadRequest(
        "Signal SSE stream ended unexpectedly".into(),
    ))
}

fn signal_inbound_event_from_envelope(
    envelope: &Value,
    config: &Value,
    settings: &SignalSettings,
) -> Option<Value> {
    let mut envelope_data = envelope
        .get("envelope")
        .cloned()
        .unwrap_or_else(|| envelope.clone());
    let mut is_note_to_self = false;
    if let Some(sent_message) = envelope_data
        .get("syncMessage")
        .and_then(|value| value.get("sentMessage"))
        .cloned()
    {
        let destination = string_arg(&sent_message, &["destinationNumber", "destination"]);
        let group_id = sent_message
            .get("groupInfo")
            .and_then(|value| value.get("groupId"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if destination.as_deref() == Some(settings.account.as_str()) || group_id.is_some() {
            if let Some(object) = envelope_data.as_object_mut() {
                object.insert("dataMessage".into(), sent_message);
            }
            is_note_to_self = true;
        } else {
            return None;
        }
    }
    if envelope_data.get("storyMessage").is_some()
        && config
            .get("ignoreStories")
            .or_else(|| config.get("ignore_stories"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    {
        return None;
    }
    let sender = string_arg(&envelope_data, &["sourceNumber", "sourceUuid", "source"])?;
    if sender == settings.account && !is_note_to_self {
        return None;
    }
    let sender_name = string_arg(&envelope_data, &["sourceName"]).unwrap_or_default();
    let sender_uuid = string_arg(&envelope_data, &["sourceUuid"]).unwrap_or_default();
    let data_message = envelope_data
        .get("dataMessage")
        .or_else(|| envelope_data.pointer("/editMessage/dataMessage"))?;
    let group_info = data_message
        .get("groupInfo")
        .cloned()
        .unwrap_or(Value::Null);
    let group_id = group_info
        .get("groupId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_group = group_id.is_some();
    if is_group {
        let allowed = telegram_string_set(
            config,
            &[
                "groupAllowedUsers",
                "group_allowed_users",
                "allowedGroups",
                "allowed_groups",
            ],
            "SIGNAL_GROUP_ALLOWED_USERS",
        );
        if allowed.is_empty()
            || !allowed.contains("*")
                && !group_id
                    .as_deref()
                    .is_some_and(|value| allowed.contains(value))
        {
            return None;
        }
    } else {
        let allowed = telegram_string_set(
            config,
            &[
                "allowedUsers",
                "allowed_users",
                "allowedChats",
                "allowed_chats",
            ],
            "SIGNAL_ALLOWED_USERS",
        );
        if !allowed.is_empty() && !allowed.contains("*") && !allowed.contains(&sender) {
            return None;
        }
    }
    let raw_text = string_arg(data_message, &["message"]).unwrap_or_default();
    let text = signal_render_mentions(&raw_text, data_message.get("mentions"));
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("SIGNAL_REQUIRE_MENTION"))
        .unwrap_or(false);
    if is_group && require_mention {
        let mentioned_in_text = !settings.account.is_empty()
            && text
                .to_ascii_lowercase()
                .contains(&format!("@{}", settings.account).to_ascii_lowercase());
        let mentioned_in_metadata = data_message
            .get("mentions")
            .and_then(Value::as_array)
            .is_some_and(|mentions| {
                mentions.iter().any(|mention| {
                    string_arg(mention, &["number", "uuid"])
                        .is_some_and(|value| value.eq_ignore_ascii_case(&settings.account))
                })
            });
        if !mentioned_in_text && !mentioned_in_metadata {
            return None;
        }
    }
    let attachments = signal_attachment_metadata(data_message);
    if text.trim().is_empty() && attachments.is_empty() {
        return None;
    }
    let chat_id = if let Some(group_id) = group_id.clone() {
        format!("group:{group_id}")
    } else {
        sender.clone()
    };
    let message_id = envelope_data
        .get("timestamp")
        .or_else(|| data_message.get("timestamp"))
        .map(value_to_string)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| new_id("signal-message"));
    let message_type = signal_message_type(&attachments);
    let user_name = if sender_name.is_empty() {
        sender.clone()
    } else {
        sender_name
    };
    let mut inbound = json!({
        "platform": "signal",
        "messageId": message_id,
        "message_id": message_id,
        "text": text,
        "messageType": message_type,
        "message_type": message_type,
        "source": {
            "platform": "signal",
            "chatId": chat_id,
            "chat_id": chat_id,
            "chatType": if is_group { "group" } else { "dm" },
            "chat_type": if is_group { "group" } else { "dm" },
            "chatTitle": group_info.get("groupName").cloned().unwrap_or(Value::Null),
            "chat_title": group_info.get("groupName").cloned().unwrap_or(Value::Null),
            "userId": sender,
            "user_id": sender,
            "userName": user_name,
            "user_name": user_name,
            "userIdAlt": if sender_uuid.is_empty() { Value::Null } else { json!(sender_uuid) },
            "user_id_alt": if sender_uuid.is_empty() { Value::Null } else { json!(sender_uuid) },
        },
        "raw": envelope_data,
    });
    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["skippedAttachments"] = inbound["attachments"].clone();
        inbound["skipped_attachments"] = inbound["attachments"].clone();
    }
    if let Some(quote) = data_message.get("quote") {
        inbound["replyToMessageId"] = quote
            .get("id")
            .map(value_to_string)
            .map_or(Value::Null, Value::String);
        inbound["reply_to_message_id"] = inbound["replyToMessageId"].clone();
        inbound["replyToText"] = quote.get("text").cloned().unwrap_or(Value::Null);
        inbound["reply_to_text"] = inbound["replyToText"].clone();
    }
    Some(inbound)
}

fn signal_render_mentions(text: &str, mentions: Option<&Value>) -> String {
    let mut output = text.to_string();
    let Some(mentions) = mentions.and_then(Value::as_array) else {
        return output;
    };
    for mention in mentions.iter().rev() {
        let Some(start) = mention
            .get("start")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            continue;
        };
        let length = mention
            .get("length")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(1);
        if start > output.len() || start + length > output.len() {
            continue;
        }
        let replacement = string_arg(mention, &["number", "uuid"])
            .map(|value| format!("@{value}"))
            .unwrap_or_else(|| "@user".into());
        output.replace_range(start..start + length, &replacement);
    }
    output
}

fn signal_attachment_metadata(data_message: &Value) -> Vec<Value> {
    data_message
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|attachment| {
            let id = string_arg(&attachment, &["id"])?;
            let mime = string_arg(&attachment, &["contentType", "content_type"])
                .unwrap_or_else(|| "application/octet-stream".into());
            Some(json!({
                "id": id,
                "name": string_arg(&attachment, &["filename", "fileName", "file_name"]).unwrap_or_else(|| "attachment".into()),
                "mimeType": mime,
                "mime_type": mime,
                "type": mattermost_media_kind(&mime),
                "size": attachment.get("size").cloned().unwrap_or(Value::Null),
                "downloadStatus": "skipped",
                "download_status": "skipped",
                "reason": "signal-cli HTTP attachment download is not configured in SynthChat yet",
            }))
        })
        .collect()
}

fn signal_message_type(attachments: &[Value]) -> &'static str {
    if attachments.iter().any(|attachment| {
        attachment
            .get("mimeType")
            .or_else(|| attachment.get("mime_type"))
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.starts_with("image/"))
    }) {
        "photo"
    } else if attachments.iter().any(|attachment| {
        attachment
            .get("mimeType")
            .or_else(|| attachment.get("mime_type"))
            .and_then(Value::as_str)
            .is_some_and(|mime| mime.starts_with("audio/"))
    }) {
        "voice"
    } else if attachments.is_empty() {
        "text"
    } else {
        "document"
    }
}

fn signal_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!(
        "Signal inbound message\nchat_id: {chat_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}"
    );
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nSkipped Signal attachments:");
            for attachment in attachments {
                let id = attachment
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                prompt.push_str(&format!("\n- {id} ({mime})"));
            }
        }
    }
    prompt
}

fn signal_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("signal")
    }) {
        return Ok(existing.id);
    }
    let persona_id = signal_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Signal".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("signal"))?;
    Ok(conversation.id)
}

fn signal_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

pub(crate) fn start_configured_platform_adapters(
    store: &AppStore,
    app: AppHandle,
) -> AppResult<Vec<String>> {
    let config = store.config()?;
    let mut started = Vec::new();
    if feishu_adapter_autostart_enabled(&config.feishu) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_feishu_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_feishu_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Feishu autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "feishu",
                        &state,
                    );
                }
            }
        });
        let state = store.update_feishu_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "feishu", &state);
        started.push("feishu".into());
    }
    if dingtalk_adapter_autostart_enabled(&config.dingtalk) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_dingtalk_adapter(&store_for_task, app_for_task.clone()).await
            {
                if let Ok(state) = store_for_task.update_dingtalk_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("DingTalk autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "dingtalk",
                        &state,
                    );
                }
            }
        });
        let state = store.update_dingtalk_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "dingtalk", &state);
        started.push("dingtalk".into());
    }
    if email_adapter_autostart_enabled(&config.email) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_email_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_email_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Email autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app_for_task, "autostart_failed", "email", &state);
                }
            }
        });
        let state = store.update_email_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "email", &state);
        started.push("email".into());
    }
    if mattermost_adapter_autostart_enabled(&config.mattermost) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) =
                start_mattermost_adapter(&store_for_task, app_for_task.clone()).await
            {
                if let Ok(state) = store_for_task.update_mattermost_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Mattermost autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_mattermost_adapter_event(&app_for_task, "autostart_failed", &state);
                }
            }
        });
        let state = store.update_mattermost_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_mattermost_adapter_event(&app, "autostart_requested", &state);
        started.push("mattermost".into());
    }
    if telegram_adapter_autostart_enabled(&config.telegram) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_telegram_adapter(&store_for_task, app_for_task.clone()).await
            {
                if let Ok(state) = store_for_task.update_telegram_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Telegram autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "telegram",
                        &state,
                    );
                }
            }
        });
        let state = store.update_telegram_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "telegram", &state);
        started.push("telegram".into());
    }
    if matrix_adapter_autostart_enabled(&config.matrix) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_matrix_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_matrix_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Matrix autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "matrix",
                        &state,
                    );
                }
            }
        });
        let state = store.update_matrix_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "matrix", &state);
        started.push("matrix".into());
    }
    if slack_adapter_autostart_enabled(&config.slack) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_slack_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_slack_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Slack autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(&app_for_task, "autostart_failed", "slack", &state);
                }
            }
        });
        let state = store.update_slack_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "slack", &state);
        started.push("slack".into());
    }
    if discord_adapter_autostart_enabled(&config.discord) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_discord_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_discord_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Discord autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "discord",
                        &state,
                    );
                }
            }
        });
        let state = store.update_discord_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "discord", &state);
        started.push("discord".into());
    }
    if webhook_adapter_autostart_enabled(&config.webhook) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_webhook_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_webhook_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Webhook autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "webhook",
                        &state,
                    );
                }
            }
        });
        let state = store.update_webhook_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "webhook", &state);
        started.push("webhook".into());
    }
    if signal_adapter_autostart_enabled(&config.signal) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) = start_signal_adapter(&store_for_task, app_for_task.clone()).await {
                if let Ok(state) = store_for_task.update_signal_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Signal autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "signal",
                        &state,
                    );
                }
            }
        });
        let state = store.update_signal_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "signal", &state);
        started.push("signal".into());
    }
    if messaging_gateway_adapter_autostart_enabled(&config.messaging_gateway) {
        let store_for_task = store.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            if let Err(error) =
                start_messaging_gateway_adapter(&store_for_task, app_for_task.clone()).await
            {
                if let Ok(state) = store_for_task.update_messaging_gateway_adapter_state(
                    Some("stopped"),
                    None,
                    Some(format!("Messaging gateway autostart failed: {error}")),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app_for_task,
                        "autostart_failed",
                        "messaging_gateway",
                        &state,
                    );
                }
            }
        });
        let state = store.update_messaging_gateway_adapter_state(
            Some("starting"),
            Some(json!({"type": "autostart_requested"})),
            None,
            0,
            0,
        )?;
        emit_platform_adapter_event(&app, "autostart_requested", "messaging_gateway", &state);
        started.push("messaging_gateway".into());
    }
    Ok(started)
}

pub(crate) fn stop_mattermost_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_mattermost_adapter_task()
}

pub(crate) fn mattermost_adapter_status(store: &AppStore) -> AppResult<Value> {
    store.mattermost_adapter_state()
}

pub(crate) async fn start_platform_adapter(
    store: &AppStore,
    app: AppHandle,
    platform: &str,
) -> AppResult<Value> {
    match normalize_platform_name(platform).as_str() {
        "mattermost" => start_mattermost_adapter(store, app).await,
        "feishu" => start_feishu_adapter(store, app).await,
        "dingtalk" => start_dingtalk_adapter(store, app).await,
        "email" => start_email_adapter(store, app).await,
        "telegram" => start_telegram_adapter(store, app).await,
        "matrix" => start_matrix_adapter(store, app).await,
        "slack" => start_slack_adapter(store, app).await,
        "discord" => start_discord_adapter(store, app).await,
        "webhook" => start_webhook_adapter(store, app).await,
        "signal" => start_signal_adapter(store, app).await,
        "messaging_gateway" | "wecom" | "weixin" | "yuanbao" => {
            start_messaging_gateway_adapter(store, app).await
        }
        other => Err(AppError::BadRequest(format!(
            "platform adapter '{other}' does not support runtime start yet"
        ))),
    }
}

pub(crate) fn stop_platform_adapter(store: &AppStore, platform: &str) -> AppResult<Value> {
    match normalize_platform_name(platform).as_str() {
        "mattermost" => stop_mattermost_adapter(store),
        "feishu" => stop_feishu_adapter(store),
        "dingtalk" => stop_dingtalk_adapter(store),
        "email" => stop_email_adapter(store),
        "telegram" => stop_telegram_adapter(store),
        "matrix" => stop_matrix_adapter(store),
        "slack" => stop_slack_adapter(store),
        "discord" => stop_discord_adapter(store),
        "webhook" => stop_webhook_adapter(store),
        "signal" => stop_signal_adapter(store),
        "messaging_gateway" | "wecom" | "weixin" | "yuanbao" => {
            stop_messaging_gateway_adapter(store)
        }
        other => Err(AppError::BadRequest(format!(
            "platform adapter '{other}' does not support runtime stop yet"
        ))),
    }
}

pub(crate) fn platform_adapter_status(
    store: &AppStore,
    platform: Option<&str>,
) -> AppResult<Value> {
    if let Some(platform) = platform
        .map(normalize_platform_name)
        .filter(|value| !value.is_empty() && value != "all")
    {
        return platform_adapter_state(store, &platform);
    }
    let config = store.config()?;
    let mut adapters = Vec::new();
    adapters.push(enrich_runtime_adapter_state(
        store.mattermost_adapter_state()?,
        platform_config_enabled(&config.mattermost),
        mattermost_configured(&config.mattermost),
        true,
        "websocket",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.discord_adapter_state()?,
        platform_config_enabled(&config.discord),
        discord_runtime_configured(&config.discord),
        true,
        "gateway",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.feishu_adapter_state()?,
        platform_config_enabled(&config.feishu),
        feishu_webhook_configured(&config.feishu),
        true,
        "webhook_openapi",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.webhook_adapter_state()?,
        platform_config_enabled(&config.webhook),
        webhook_configured(&config.webhook),
        true,
        "http_listener",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.telegram_adapter_state()?,
        platform_config_enabled(&config.telegram),
        telegram_configured(&config.telegram),
        true,
        "bot_api",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.slack_adapter_state()?,
        platform_config_enabled(&config.slack),
        slack_runtime_configured(&config.slack),
        true,
        "socket_mode",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.matrix_adapter_state()?,
        platform_config_enabled(&config.matrix),
        matrix_configured(&config.matrix),
        true,
        "client_server_api",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.signal_adapter_state()?,
        platform_config_enabled(&config.signal),
        signal_configured(&config.signal),
        true,
        "sse_json_rpc",
    ));
    adapters.push(enrich_runtime_adapter_state(
        store.email_adapter_state()?,
        platform_config_enabled(&config.email),
        email_runtime_configured(&config.email),
        true,
        "imap_smtp",
    ));
    adapters.push(sms_adapter_state(store, &config)?);
    adapters.push(enrich_runtime_adapter_state(
        store.dingtalk_adapter_state()?,
        platform_config_enabled(&config.dingtalk),
        dingtalk_webhook_configured(&config.dingtalk),
        true,
        "bridge_webhook_robot",
    ));
    adapters.push(whatsapp_adapter_state(store, &config)?);
    adapters.push(qqbot_adapter_state(store, &config)?);
    adapters.push(homeassistant_adapter_state(store, &config)?);
    adapters.push(bluebubbles_adapter_state(store, &config)?);
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway);
    for platform in ["wecom", "weixin", "yuanbao", "msgraph_webhook"] {
        adapters.push(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                && messaging_gateway_platform_enabled(&config.messaging_gateway, platform),
            gateway_configured
                && messaging_gateway_platform_enabled(&config.messaging_gateway, platform),
            platform,
        ));
    }
    Ok(json!({
        "updatedAt": now_iso(),
        "supportedAdapters": ["mattermost", "telegram", "matrix", "slack", "discord", "webhook", "signal", "feishu", "dingtalk", "email", "sms", "whatsapp", "qqbot", "homeassistant", "bluebubbles", "wecom", "weixin", "yuanbao", "msgraph_webhook", "messaging_gateway"],
        "adapters": adapters,
    }))
}

fn platform_adapter_state(store: &AppStore, platform: &str) -> AppResult<Value> {
    let config = store.config()?;
    match platform {
        "mattermost" => Ok(enrich_runtime_adapter_state(
            store.mattermost_adapter_state()?,
            platform_config_enabled(&config.mattermost),
            mattermost_configured(&config.mattermost),
            true,
            "websocket",
        )),
        "discord" => Ok(enrich_runtime_adapter_state(
            store.discord_adapter_state()?,
            platform_config_enabled(&config.discord),
            discord_runtime_configured(&config.discord),
            true,
            "gateway",
        )),
        "feishu" => Ok(enrich_runtime_adapter_state(
            store.feishu_adapter_state()?,
            platform_config_enabled(&config.feishu),
            feishu_webhook_configured(&config.feishu),
            true,
            "webhook_openapi",
        )),
        "webhook" => Ok(enrich_runtime_adapter_state(
            store.webhook_adapter_state()?,
            platform_config_enabled(&config.webhook),
            webhook_configured(&config.webhook),
            true,
            "http_listener",
        )),
        "telegram" => Ok(enrich_runtime_adapter_state(
            store.telegram_adapter_state()?,
            platform_config_enabled(&config.telegram),
            telegram_configured(&config.telegram),
            true,
            "bot_api",
        )),
        "slack" => Ok(enrich_runtime_adapter_state(
            store.slack_adapter_state()?,
            platform_config_enabled(&config.slack),
            slack_runtime_configured(&config.slack),
            true,
            "socket_mode",
        )),
        "matrix" => Ok(enrich_runtime_adapter_state(
            store.matrix_adapter_state()?,
            platform_config_enabled(&config.matrix),
            matrix_configured(&config.matrix),
            true,
            "client_server_api",
        )),
        "signal" => Ok(enrich_runtime_adapter_state(
            store.signal_adapter_state()?,
            platform_config_enabled(&config.signal),
            signal_configured(&config.signal),
            true,
            "sse_json_rpc",
        )),
        "email" => Ok(enrich_runtime_adapter_state(
            store.email_adapter_state()?,
            platform_config_enabled(&config.email),
            email_runtime_configured(&config.email),
            true,
            "imap_smtp",
        )),
        "sms" => sms_adapter_state(store, &config),
        "dingtalk" => Ok(enrich_runtime_adapter_state(
            store.dingtalk_adapter_state()?,
            platform_config_enabled(&config.dingtalk),
            dingtalk_webhook_configured(&config.dingtalk),
            true,
            "bridge_webhook_robot",
        )),
        "whatsapp" => whatsapp_adapter_state(store, &config),
        "qqbot" => qqbot_adapter_state(store, &config),
        "homeassistant" => homeassistant_adapter_state(store, &config),
        "bluebubbles" => bluebubbles_adapter_state(store, &config),
        "wecom" | "weixin" | "yuanbao" | "msgraph_webhook" => {
            Ok(messaging_gateway_adapter_state_for_platform(
                store.messaging_gateway_adapter_state()?,
                platform_config_enabled(&config.messaging_gateway)
                    && messaging_gateway_platform_enabled(&config.messaging_gateway, platform),
                messaging_gateway_runtime_configured(&config.messaging_gateway)
                    && messaging_gateway_platform_enabled(&config.messaging_gateway, platform),
                platform,
            ))
        }
        "messaging_gateway" => Ok(enrich_runtime_adapter_state(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway),
            messaging_gateway_runtime_configured(&config.messaging_gateway),
            true,
            "bridge_webhook",
        )),
        other => Ok(json!({
            "platform": other,
            "status": "unsupported",
            "runtime": false,
            "configured": false,
            "enabled": false,
            "updatedAt": now_iso(),
        })),
    }
}

fn enrich_runtime_adapter_state(
    mut state: Value,
    enabled: bool,
    configured: bool,
    runtime: bool,
    transport: &str,
) -> Value {
    if !state.is_object() {
        state = json!({});
    }
    state["enabled"] = json!(enabled);
    state["configured"] = json!(configured);
    state["runtime"] = json!(runtime);
    state["transport"] = json!(transport);
    state["capabilities"] = json!(["send", "receive", "lifecycle", "attachments"]);
    state
}

pub(super) fn messaging_gateway_adapter_state_for_platform(
    state: Value,
    enabled: bool,
    configured: bool,
    platform: &str,
) -> Value {
    let mut state =
        enrich_runtime_adapter_state(state, enabled, configured, true, "bridge_webhook");
    state["platform"] = json!(platform);
    state["gatewayPlatform"] = json!("messaging_gateway");
    state["gateway_platform"] = json!("messaging_gateway");
    state
}

fn send_only_adapter_state(
    platform: &str,
    enabled: bool,
    configured: bool,
    transport: &str,
) -> Value {
    json!({
        "platform": platform,
        "status": if configured { "configured" } else { "unconfigured" },
        "enabled": enabled,
        "configured": configured,
        "runtime": false,
        "transport": transport,
        "capabilities": ["send"],
        "updatedAt": now_iso(),
    })
}

fn sms_adapter_state(store: &AppStore, config: &crate::models::AppConfig) -> AppResult<Value> {
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway)
        && messaging_gateway_platform_enabled(&config.messaging_gateway, "sms");
    if gateway_configured {
        Ok(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                || platform_config_enabled(&config.sms),
            true,
            "sms",
        ))
    } else {
        Ok(send_only_adapter_state(
            "sms",
            platform_config_enabled(&config.sms),
            sms_configured(&config.sms),
            "twilio",
        ))
    }
}

fn whatsapp_adapter_state(store: &AppStore, config: &crate::models::AppConfig) -> AppResult<Value> {
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway)
        && messaging_gateway_platform_enabled(&config.messaging_gateway, "whatsapp");
    if gateway_configured {
        Ok(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                || platform_config_enabled(&config.whatsapp),
            true,
            "whatsapp",
        ))
    } else {
        Ok(send_only_adapter_state(
            "whatsapp",
            platform_config_enabled(&config.whatsapp),
            whatsapp_configured(&config.whatsapp),
            "bridge",
        ))
    }
}

fn qqbot_adapter_state(store: &AppStore, config: &crate::models::AppConfig) -> AppResult<Value> {
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway)
        && messaging_gateway_platform_enabled(&config.messaging_gateway, "qqbot");
    if gateway_configured {
        Ok(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                || platform_config_enabled(&config.qqbot),
            true,
            "qqbot",
        ))
    } else {
        Ok(send_only_adapter_state(
            "qqbot",
            platform_config_enabled(&config.qqbot),
            qqbot_configured(&config.qqbot),
            "rest",
        ))
    }
}

fn homeassistant_adapter_state(
    store: &AppStore,
    config: &crate::models::AppConfig,
) -> AppResult<Value> {
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway)
        && messaging_gateway_platform_enabled(&config.messaging_gateway, "homeassistant");
    if gateway_configured {
        Ok(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                || platform_config_enabled(&config.homeassistant),
            true,
            "homeassistant",
        ))
    } else {
        Ok(send_only_adapter_state(
            "homeassistant",
            platform_config_enabled(&config.homeassistant),
            homeassistant_configured(&config.homeassistant),
            "notify_api",
        ))
    }
}

fn bluebubbles_adapter_state(
    store: &AppStore,
    config: &crate::models::AppConfig,
) -> AppResult<Value> {
    let gateway_configured = messaging_gateway_runtime_configured(&config.messaging_gateway)
        && messaging_gateway_platform_enabled(&config.messaging_gateway, "bluebubbles");
    if gateway_configured {
        Ok(messaging_gateway_adapter_state_for_platform(
            store.messaging_gateway_adapter_state()?,
            platform_config_enabled(&config.messaging_gateway)
                || platform_config_enabled(&config.bluebubbles),
            true,
            "bluebubbles",
        ))
    } else {
        Ok(send_only_adapter_state(
            "bluebubbles",
            platform_config_enabled(&config.bluebubbles),
            bluebubbles_configured(&config.bluebubbles),
            "imessage_bridge",
        ))
    }
}

fn platform_config_enabled(config: &Value) -> bool {
    config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn normalize_platform_name(platform: &str) -> String {
    platform.trim().to_ascii_lowercase().replace('-', "_")
}

pub(super) fn mattermost_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| mattermost_env_bool("MATTERMOST_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| mattermost_env_bool("MATTERMOST_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && mattermost_configured(config)
}

fn mattermost_env_bool(key: &str) -> Option<bool> {
    std::env::var(key).ok().map(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        )
    })
}

async fn mattermost_current_user(
    client: &reqwest::Client,
    settings: &MattermostSettings,
) -> AppResult<Value> {
    let response = client
        .get(mattermost_api_url(settings, "/users/me")?)
        .bearer_auth(&settings.token)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Mattermost /users/me failed: {error}")))?;
    mattermost_response_json(response, "Mattermost /users/me").await
}

async fn mattermost_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: MattermostSettings,
    bot_user_id: String,
    bot_username: String,
) {
    let mut seen_post_ids = HashSet::new();
    loop {
        let result = mattermost_adapter_connect_once(
            &app,
            &store,
            &settings,
            &bot_user_id,
            &bot_username,
            &mut seen_post_ids,
        )
        .await;
        match result {
            Ok(()) => {
                if let Ok(state) = store.update_mattermost_adapter_state(
                    Some("stopped"),
                    None,
                    Some("Mattermost WebSocket closed".into()),
                    0,
                    0,
                ) {
                    emit_mattermost_adapter_event(&app, "stopped", &state);
                }
                break;
            }
            Err(error) => {
                let message = error.to_string();
                let auth_error = message.contains("401") || message.contains("403");
                if let Ok(state) = store.update_mattermost_adapter_state(
                    Some(if auth_error {
                        "stopped"
                    } else {
                        "reconnecting"
                    }),
                    None,
                    Some(message),
                    0,
                    0,
                ) {
                    emit_mattermost_adapter_event(
                        &app,
                        if auth_error {
                            "auth_failed"
                        } else {
                            "reconnecting"
                        },
                        &state,
                    );
                }
                if auth_error {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn mattermost_adapter_connect_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &MattermostSettings,
    bot_user_id: &str,
    bot_username: &str,
    seen_post_ids: &mut HashSet<String>,
) -> AppResult<()> {
    let ws_url = mattermost_websocket_url(settings)?;
    let (ws, _) = connect_async(&ws_url).await.map_err(|error| {
        AppError::BadRequest(format!("Mattermost WebSocket connect failed: {error}"))
    })?;
    let (mut writer, mut reader) = ws.split();
    writer
        .send(WsMessage::Text(
            mattermost_auth_challenge(&settings.token)
                .to_string()
                .into(),
        ))
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Mattermost WebSocket auth send failed: {error}"))
        })?;
    let state = store.update_mattermost_adapter_state(
        Some("running"),
        Some(json!({"type": "connected", "url": ws_url})),
        None,
        0,
        0,
    )?;
    emit_mattermost_adapter_event(app, "connected", &state);

    while let Some(message) = reader.next().await {
        let message = message.map_err(|error| {
            AppError::BadRequest(format!("Mattermost WebSocket read failed: {error}"))
        })?;
        let text = match message {
            WsMessage::Text(text) => text.to_string(),
            WsMessage::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            WsMessage::Close(frame) => {
                return Err(AppError::BadRequest(format!(
                    "Mattermost WebSocket closed: {:?}",
                    frame
                )));
            }
            _ => continue,
        };
        let Ok(event) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if mattermost_auth_failed_event(&event) {
            return Err(AppError::BadRequest(format!(
                "Mattermost WebSocket authentication failed: {}",
                truncate_output(&event.to_string(), 1000)
            )));
        }
        let config = store.config()?.mattermost;
        let Some(inbound) = mattermost_inbound_event_from_ws(
            &event,
            &config,
            bot_user_id,
            bot_username,
            seen_post_ids,
        ) else {
            continue;
        };
        let inbound_fallback = inbound.clone();
        let inbound = mattermost_enrich_inbound_files(store, settings, inbound)
            .await
            .unwrap_or_else(|error| {
                let mut fallback = inbound_fallback;
                fallback["fileDownloadError"] = json!(error.to_string());
                fallback["file_download_error"] = json!(error.to_string());
                fallback
            });
        let prompt = mattermost_inbound_prompt(&inbound);
        let Some(prompt) =
            apply_pre_gateway_dispatch_hooks(store, "mattermost", &inbound, prompt).await
        else {
            let state = store.update_mattermost_adapter_state(
                Some("running"),
                Some(inbound),
                None,
                1,
                0,
            )?;
            emit_mattermost_adapter_event(app, "inbound_ignored", &state);
            continue;
        };
        let conversation_id = mattermost_inbound_conversation_id(store, &config)?;
        let persona_id = mattermost_inbound_persona_id(store, &config)?;
        let state =
            store.update_mattermost_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
        emit_mattermost_adapter_event(app, "inbound_triggered", &state);
        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    }
    Ok(())
}

fn emit_mattermost_adapter_event(app: &AppHandle, event_type: &str, state: &Value) {
    emit_platform_adapter_event(app, event_type, "mattermost", state);
}

fn emit_platform_adapter_event(app: &AppHandle, event_type: &str, platform: &str, state: &Value) {
    let _ = app.emit(
        "synthchat-platform-adapter-event",
        json!({
            "type": event_type,
            "platform": platform,
            "state": state,
            "detail": {
                "platform": platform,
                "status": state.get("status").cloned().unwrap_or(Value::Null),
                "lastError": state.get("lastError").cloned().unwrap_or(Value::Null),
                "receivedCount": state.get("receivedCount").cloned().unwrap_or(Value::Null),
                "triggeredCount": state.get("triggeredCount").cloned().unwrap_or(Value::Null),
            }
        }),
    );
}

pub(super) fn mattermost_websocket_url(settings: &MattermostSettings) -> AppResult<String> {
    let mut url = reqwest::Url::parse(&settings.url)
        .map_err(|error| AppError::BadRequest(format!("invalid Mattermost URL: {error}")))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(AppError::BadRequest(format!(
                "Mattermost WebSocket requires http(s) URL, got {other}"
            )));
        }
    };
    url.set_scheme(scheme)
        .map_err(|_| AppError::BadRequest("failed to build Mattermost WebSocket URL".into()))?;
    url.set_path("/api/v4/websocket");
    url.set_query(None);
    Ok(url.to_string())
}

pub(super) fn mattermost_auth_challenge(token: &str) -> Value {
    json!({
        "seq": 1,
        "action": "authentication_challenge",
        "data": {"token": token}
    })
}

async fn mattermost_enrich_inbound_files(
    store: &AppStore,
    settings: &MattermostSettings,
    mut inbound: Value,
) -> AppResult<Value> {
    let file_ids = inbound
        .get("fileIds")
        .or_else(|| inbound.get("file_ids"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<_>>();
    if file_ids.is_empty() {
        return Ok(inbound);
    }

    let client = mattermost_client(settings)?;
    let cache_dir = store.data_dir().join("attachments").join("mattermost");
    fs::create_dir_all(&cache_dir)?;
    let mut attachments = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    let mut skipped_files = Vec::new();

    for file_id in file_ids {
        match mattermost_download_inbound_file(&client, settings, &cache_dir, &file_id).await {
            Ok(attachment) => {
                if let Some(path) = attachment.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                attachments.push(attachment);
            }
            Err(error) => skipped_files.push(json!({
                "id": file_id,
                "error": error.to_string(),
            })),
        }
    }

    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        if inbound.get("messageType").and_then(Value::as_str) == Some("text")
            || inbound.get("message_type").and_then(Value::as_str) == Some("text")
        {
            let message_type = mattermost_message_type_from_media(&inbound["mediaTypes"]);
            inbound["messageType"] = json!(message_type);
            inbound["message_type"] = json!(message_type);
        }
    }
    if !skipped_files.is_empty() {
        inbound["skippedFiles"] = json!(skipped_files);
        inbound["skipped_files"] = inbound["skippedFiles"].clone();
    }
    Ok(inbound)
}

async fn mattermost_download_inbound_file(
    client: &reqwest::Client,
    settings: &MattermostSettings,
    cache_dir: &Path,
    file_id: &str,
) -> AppResult<Value> {
    let file_id = file_id.trim();
    if file_id.is_empty() {
        return Err(AppError::BadRequest("empty Mattermost file id".into()));
    }
    let info_response = client
        .get(mattermost_api_url(
            settings,
            &format!("/files/{}/info", percent_encode_path_segment(file_id)),
        )?)
        .bearer_auth(&settings.token)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Mattermost file info failed: {error}")))?;
    let info = mattermost_response_json(info_response, "Mattermost file info").await?;
    let name = info
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    let mime = info
        .get("mime_type")
        .or_else(|| info.get("mimeType"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    let download_response = client
        .get(mattermost_api_url(
            settings,
            &format!("/files/{}", percent_encode_path_segment(file_id)),
        )?)
        .bearer_auth(&settings.token)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("Mattermost file download failed: {error}"))
        })?;
    let status = download_response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Mattermost file download failed ({})",
            status.as_u16()
        )));
    }
    let bytes = download_response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Mattermost file download: {error}"))
    })?;
    let safe_name = mattermost_safe_file_name(name);
    let path = cache_dir.join(format!("{file_id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    let kind = mattermost_media_kind(mime);
    Ok(json!({
        "id": file_id,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": kind,
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    }))
}

pub(super) fn mattermost_message_type_from_media(media_types: &Value) -> &'static str {
    let media_types = media_types.as_array().cloned().unwrap_or_default();
    if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
    }) {
        "photo"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("audio/"))
            .unwrap_or(false)
    }) {
        "voice"
    } else {
        "document"
    }
}

fn mattermost_media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else {
        "document"
    }
}

pub(super) fn mattermost_safe_file_name(file_name: &str) -> String {
    let base = Path::new(file_name)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let safe = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .to_string();
    if safe.is_empty() {
        "attachment".into()
    } else {
        safe
    }
}

fn mattermost_auth_failed_event(event: &Value) -> bool {
    let status = event
        .get("status")
        .or_else(|| event.pointer("/data/status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let error = event
        .get("error")
        .or_else(|| event.pointer("/data/error"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    status.contains("fail")
        || status.contains("not ok")
        || error.contains("authentication")
        || error.contains("unauthorized")
}

fn mattermost_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("mattermost")
    }) {
        return Ok(existing.id);
    }
    let persona_id = mattermost_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Mattermost".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("mattermost"))?;
    Ok(conversation.id)
}

fn mattermost_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

pub(super) fn mattermost_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let chat_id = source
        .get("chatId")
        .or_else(|| source.get("chat_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_name = source
        .get("userName")
        .or_else(|| source.get("user_name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = source
        .get("threadId")
        .or_else(|| source.get("thread_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!(
        "Mattermost inbound message\nchannel_id: {chat_id}\nthread_id: {thread_id}\nmessage_id: {message_id}\nuser: {user_name}\n\n{text}"
    );
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    prompt
}

pub(super) fn mattermost_format_message(content: &str) -> String {
    let mut output = String::new();
    let chars = content.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '!' && chars.get(index + 1) == Some(&'[') {
            let mut cursor = index + 2;
            while cursor < chars.len() && chars[cursor] != ']' {
                cursor += 1;
            }
            if cursor + 1 < chars.len() && chars[cursor + 1] == '(' {
                cursor += 2;
                let url_start = cursor;
                while cursor < chars.len() && chars[cursor] != ')' {
                    cursor += 1;
                }
                if cursor < chars.len() {
                    output.extend(chars[url_start..cursor].iter());
                    index = cursor + 1;
                    continue;
                }
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

#[allow(dead_code)]
pub(super) fn mattermost_inbound_event_from_ws(
    event: &Value,
    config: &Value,
    bot_user_id: &str,
    bot_username: &str,
    seen_post_ids: &mut HashSet<String>,
) -> Option<Value> {
    if event.get("event").and_then(Value::as_str) != Some("posted") {
        return None;
    }
    let data = event.get("data")?;
    let post = data
        .get("post")
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())?;
    if post.get("user_id").and_then(Value::as_str) == Some(bot_user_id) {
        return None;
    }
    if post
        .get("type")
        .and_then(Value::as_str)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return None;
    }
    let post_id = post
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !post_id.is_empty() && !seen_post_ids.insert(post_id.clone()) {
        return None;
    }
    let channel_id = post
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let channel_type_raw = data
        .get("channel_type")
        .and_then(Value::as_str)
        .unwrap_or("O");
    let chat_type = mattermost_channel_type(channel_type_raw);
    let mut text = post
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mention_patterns = mattermost_mention_patterns(bot_user_id, bot_username);
    let has_mention = mention_patterns.iter().any(|pattern| {
        text.to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
    });
    if channel_type_raw != "D" {
        let allowed_channels = mattermost_config_string_set(
            config,
            &["allowedChannels", "allowed_channels"],
            "MATTERMOST_ALLOWED_CHANNELS",
        );
        if !allowed_channels.is_empty() && !allowed_channels.contains(&channel_id) {
            return None;
        }
        let free_channels = mattermost_config_string_set(
            config,
            &["freeResponseChannels", "free_response_channels"],
            "MATTERMOST_FREE_RESPONSE_CHANNELS",
        );
        let require_mention = mattermost_config_bool(
            config,
            &["requireMention", "require_mention"],
            "MATTERMOST_REQUIRE_MENTION",
            true,
        );
        if require_mention && !free_channels.contains(&channel_id) && !has_mention {
            return None;
        }
        if has_mention {
            text = mattermost_strip_mentions(&text, &mention_patterns);
        }
    }
    let sender_id = post
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let sender_name = data
        .get("sender_name")
        .and_then(Value::as_str)
        .map(|value| value.trim_start_matches('@'))
        .filter(|value| !value.is_empty())
        .unwrap_or(sender_id);
    let file_ids = post
        .get("file_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let message_type = if text.trim_start().starts_with('/') {
        "command"
    } else if !file_ids.is_empty() {
        "document"
    } else {
        "text"
    };
    Some(json!({
        "platform": "mattermost",
        "messageId": post_id,
        "message_id": post_id,
        "text": text,
        "messageType": message_type,
        "message_type": message_type,
        "fileIds": file_ids.clone(),
        "file_ids": file_ids,
        "source": {
            "platform": "mattermost",
            "chatId": channel_id,
            "chat_id": channel_id,
            "chatType": chat_type,
            "chat_type": chat_type,
            "userId": sender_id,
            "user_id": sender_id,
            "userName": sender_name,
            "user_name": sender_name,
            "threadId": post.get("root_id").and_then(Value::as_str),
            "thread_id": post.get("root_id").and_then(Value::as_str),
        }
    }))
}

#[allow(dead_code)]
pub(super) fn mattermost_channel_type(raw: &str) -> &'static str {
    match raw {
        "D" => "dm",
        "G" | "P" => "group",
        _ => "channel",
    }
}

#[allow(dead_code)]
fn mattermost_mention_patterns(bot_user_id: &str, bot_username: &str) -> Vec<String> {
    [bot_username, bot_user_id]
        .into_iter()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("@{}", value.trim_start_matches('@')))
        .collect()
}

#[allow(dead_code)]
fn mattermost_strip_mentions(text: &str, patterns: &[String]) -> String {
    let mut output = text.to_string();
    for pattern in patterns {
        output = replace_ascii_case_insensitive(&output, pattern, "");
    }
    output.trim().to_string()
}

#[allow(dead_code)]
fn replace_ascii_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return input.to_string();
    }
    let mut output = String::new();
    let input_lower = input.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut index = 0;
    while let Some(relative) = input_lower[index..].find(&needle_lower) {
        let start = index + relative;
        output.push_str(&input[index..start]);
        output.push_str(replacement);
        index = start + needle.len();
    }
    output.push_str(&input[index..]);
    output
}

#[allow(dead_code)]
fn mattermost_config_bool(config: &Value, keys: &[&str], env_key: &str, default: bool) -> bool {
    keys.iter()
        .find_map(|key| config.get(*key))
        .and_then(|value| {
            value.as_bool().or_else(|| {
                value.as_str().map(|text| {
                    !matches!(
                        text.trim().to_ascii_lowercase().as_str(),
                        "false" | "0" | "no" | "off"
                    )
                })
            })
        })
        .or_else(|| {
            std::env::var(env_key).ok().map(|text| {
                !matches!(
                    text.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "no" | "off"
                )
            })
        })
        .unwrap_or(default)
}

#[allow(dead_code)]
fn mattermost_config_string_set(config: &Value, keys: &[&str], env_key: &str) -> HashSet<String> {
    keys.iter()
        .find_map(|key| config.get(*key))
        .map(mattermost_string_set_from_value)
        .or_else(|| {
            std::env::var(env_key)
                .ok()
                .map(|value| mattermost_string_set_from_value(&Value::String(value)))
        })
        .unwrap_or_default()
}

#[allow(dead_code)]
fn mattermost_string_set_from_value(value: &Value) -> HashSet<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        Value::String(text) => text
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect(),
        _ => HashSet::new(),
    }
}

#[derive(Debug, Clone)]
pub(super) struct MatrixSettings {
    pub(super) homeserver: String,
    pub(super) access_token: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn matrix_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = matrix_settings(&store.config()?.matrix)?;
    let client = matrix_client(&settings)?;
    let room_id = required_string_arg(
        payload,
        &["room_id", "roomId", "chat_id", "chatId", "target"],
        "send_message matrix",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let media_files = discord_media_file_paths(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Matrix requires message text or media_files".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "Matrix message text cannot exceed 4000 characters in one send_message chunk".into(),
        ));
    }
    let mut events = Vec::new();
    if !message.trim().is_empty() {
        let txn_id = format!("synthchat_{}", uuid::Uuid::new_v4().simple());
        let body = json!({
            "msgtype": "m.text",
            "body": message,
        });
        events.push(
            matrix_request(
                &client,
                &settings,
                &room_id,
                &txn_id,
                Some(body),
                "send message",
            )
            .await?,
        );
    }
    for file_path in &media_files {
        events.push(matrix_upload_and_send_file(&client, &settings, &room_id, file_path).await?);
    }
    let message_id = events
        .last()
        .and_then(|event| event.get("event_id").and_then(Value::as_str));
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "matrix",
        "room_id": room_id,
        "message_id": message_id,
        "media_count": media_files.len(),
        "events": events,
    }))?)
}

pub(super) fn matrix_settings(config: &Value) -> AppResult<MatrixSettings> {
    let homeserver = string_arg(config, &["homeserver", "homeServer", "baseUrl", "base_url"])
        .or_else(|| std::env::var("MATRIX_HOMESERVER").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Matrix send_message requires settings.matrix.homeserver or MATRIX_HOMESERVER"
                    .into(),
            )
        })?;
    reqwest::Url::parse(&homeserver)
        .map_err(|error| AppError::BadRequest(format!("invalid Matrix homeserver: {error}")))?;
    let access_token = string_arg(config, &["accessToken", "access_token", "token"])
        .or_else(|| std::env::var("MATRIX_ACCESS_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Matrix send_message requires settings.matrix.accessToken or MATRIX_ACCESS_TOKEN"
                    .into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(MatrixSettings {
        homeserver,
        access_token,
        timeout_seconds,
    })
}

pub(super) fn matrix_configured(config: &Value) -> bool {
    let homeserver = string_arg(config, &["homeserver", "homeServer", "baseUrl", "base_url"])
        .or_else(|| std::env::var("MATRIX_HOMESERVER").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let token = string_arg(config, &["accessToken", "access_token", "token"])
        .or_else(|| std::env::var("MATRIX_ACCESS_TOKEN").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    homeserver && token
}

pub(super) fn matrix_client(settings: &MatrixSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Matrix client: {error}")))
}

async fn matrix_request(
    client: &reqwest::Client,
    settings: &MatrixSettings,
    room_id: &str,
    txn_id: &str,
    body: Option<Value>,
    label: &str,
) -> AppResult<Value> {
    let url = matrix_send_url(settings, room_id, txn_id)?;
    let mut request = client
        .put(url)
        .bearer_auth(&settings.access_token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&strip_null_json_object(body));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Matrix {label} failed: {error}")))?;
    matrix_response_json(response, label).await
}

pub(super) fn matrix_send_url(
    settings: &MatrixSettings,
    room_id: &str,
    txn_id: &str,
) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!(
        "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
        settings.homeserver,
        percent_encode_path_segment(room_id),
        percent_encode_path_segment(txn_id)
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Matrix send URL: {error}")))
}

pub(super) fn matrix_upload_url(
    settings: &MatrixSettings,
    filename: &str,
) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!(
        "{}/_matrix/media/v3/upload?filename={}",
        settings.homeserver,
        percent_encode_path_segment(filename)
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Matrix upload URL: {error}")))
}

async fn matrix_upload_and_send_file(
    client: &reqwest::Client,
    settings: &MatrixSettings,
    room_id: &str,
    file_path: &str,
) -> AppResult<Value> {
    let path = Path::new(file_path);
    let bytes = fs::read(path).map_err(|error| {
        AppError::BadRequest(format!("failed to read Matrix media file: {error}"))
    })?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("attachment");
    let content_type = guess_content_type(filename);
    let upload = client
        .post(matrix_upload_url(settings, filename)?)
        .bearer_auth(&settings.access_token)
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(bytes.clone())
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Matrix upload failed: {error}")))?;
    let upload = matrix_response_json(upload, "upload media").await?;
    let mxc_url = upload
        .get("content_uri")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("Matrix upload response missing content_uri".into()))?;
    let msgtype = matrix_msgtype_for_content_type(content_type);
    let txn_id = format!("synthchat_{}", uuid::Uuid::new_v4().simple());
    let body = json!({
        "msgtype": msgtype,
        "body": filename,
        "url": mxc_url,
        "info": {
            "mimetype": content_type,
            "size": bytes.len(),
        },
    });
    matrix_request(client, settings, room_id, &txn_id, Some(body), "send media").await
}

pub(super) fn matrix_msgtype_for_content_type(content_type: &str) -> &'static str {
    if content_type.starts_with("image/") {
        "m.image"
    } else if content_type.starts_with("video/") {
        "m.video"
    } else if content_type.starts_with("audio/") {
        "m.audio"
    } else {
        "m.file"
    }
}

pub(super) fn guess_content_type(filename: &str) -> &'static str {
    match Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("ogg") | Some("opus") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        _ => "application/octet-stream",
    }
}

async fn matrix_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Matrix {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid Matrix {label} JSON: {error}")))?;
    if status.as_u16() != 200 && status.as_u16() != 201 {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text.trim());
        return Err(AppError::BadRequest(format!(
            "Matrix {label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(crate) async fn start_matrix_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.matrix;
    let settings = matrix_settings(&config)?;
    let client = matrix_client(&settings)?;
    let whoami = matrix_get_json(&client, &settings, "/account/whoami", None, "whoami").await?;
    let user_id = whoami
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Matrix whoami returned no user_id".into(),
        ));
    }
    let state = store.update_matrix_adapter_state(Some("starting"), None, None, 0, 0)?;
    emit_platform_adapter_event(&app, "starting", "matrix", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        matrix_adapter_loop(app, store_for_task, settings, user_id).await;
    });
    store.register_matrix_adapter_task(task.abort_handle())?;
    store.matrix_adapter_state()
}

pub(crate) fn stop_matrix_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_matrix_adapter_task()
}

pub(super) fn matrix_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("MATRIX_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("MATRIX_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && matrix_configured(config)
}

async fn matrix_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: MatrixSettings,
    user_id: String,
) {
    let mut next_batch: Option<String> = None;
    loop {
        match matrix_poll_once(&app, &store, &settings, &user_id, &mut next_batch).await {
            Ok(()) => {
                if let Ok(state) =
                    store.update_matrix_adapter_state(Some("running"), None, None, 0, 0)
                {
                    emit_platform_adapter_event(&app, "poll", "matrix", &state);
                }
            }
            Err(error) => {
                let message = error.to_string();
                let auth_error = message.contains("401") || message.contains("403");
                if let Ok(state) = store.update_matrix_adapter_state(
                    Some(if auth_error {
                        "stopped"
                    } else {
                        "reconnecting"
                    }),
                    None,
                    Some(message),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app,
                        if auth_error {
                            "auth_failed"
                        } else {
                            "reconnecting"
                        },
                        "matrix",
                        &state,
                    );
                }
                if auth_error {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn matrix_poll_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &MatrixSettings,
    user_id: &str,
    next_batch: &mut Option<String>,
) -> AppResult<()> {
    let config = store.config()?.matrix;
    let client = matrix_client(settings)?;
    let timeout = config
        .get("syncTimeoutMs")
        .or_else(|| config.get("sync_timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| settings.timeout_seconds.saturating_sub(2).clamp(1, 60) * 1000);
    let mut query = vec![("timeout".to_string(), timeout.to_string())];
    if let Some(since) = next_batch.as_deref().filter(|value| !value.is_empty()) {
        query.push(("since".into(), since.to_string()));
    }
    let sync = matrix_get_json(&client, settings, "/sync", Some(&query), "sync").await?;
    if let Some(batch) = sync.get("next_batch").and_then(Value::as_str) {
        *next_batch = Some(batch.to_string());
    }
    let inbound_events = matrix_inbound_events_from_sync(&sync, &config, user_id);
    let state = store.update_matrix_adapter_state(
        Some("running"),
        Some(json!({
            "type": "sync",
            "count": inbound_events.len(),
            "nextBatch": next_batch.clone(),
            "next_batch": next_batch.clone(),
        })),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(app, "connected", "matrix", &state);
    for inbound in inbound_events {
        let inbound = matrix_enrich_inbound_media(store, settings, inbound).await?;
        let prompt = matrix_inbound_prompt(&inbound);
        let Some(prompt) =
            apply_pre_gateway_dispatch_hooks(store, "matrix", &inbound, prompt).await
        else {
            let state =
                store.update_matrix_adapter_state(Some("running"), Some(inbound), None, 1, 0)?;
            emit_platform_adapter_event(app, "inbound_ignored", "matrix", &state);
            continue;
        };
        let conversation_id = matrix_inbound_conversation_id(store, &config)?;
        let persona_id = matrix_inbound_persona_id(store, &config)?;
        let state =
            store.update_matrix_adapter_state(Some("running"), Some(inbound), None, 1, 1)?;
        emit_platform_adapter_event(app, "inbound_triggered", "matrix", &state);
        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
    }
    Ok(())
}

async fn matrix_get_json(
    client: &reqwest::Client,
    settings: &MatrixSettings,
    path: &str,
    query: Option<&[(String, String)]>,
    label: &str,
) -> AppResult<Value> {
    let mut url = matrix_client_url(settings, path)?;
    if let Some(query) = query {
        {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
    }
    let response = client
        .get(url)
        .bearer_auth(&settings.access_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Matrix {label} failed: {error}")))?;
    matrix_response_json(response, label).await
}

fn matrix_client_url(settings: &MatrixSettings, path: &str) -> AppResult<reqwest::Url> {
    if !path.starts_with('/') || path.contains('\\') {
        return Err(AppError::BadRequest(format!(
            "invalid Matrix client path: {path}"
        )));
    }
    reqwest::Url::parse(&format!("{}/_matrix/client/v3{path}", settings.homeserver))
        .map_err(|error| AppError::BadRequest(format!("invalid Matrix client URL: {error}")))
}

fn matrix_inbound_events_from_sync(sync: &Value, config: &Value, bot_user_id: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let Some(joined) = sync.pointer("/rooms/join").and_then(Value::as_object) else {
        return events;
    };
    for (room_id, room) in joined {
        let timeline = room
            .pointer("/timeline/events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for event in timeline {
            if let Some(inbound) =
                matrix_inbound_event_from_room_event(room_id, &event, config, bot_user_id)
            {
                events.push(inbound);
            }
        }
    }
    events
}

fn matrix_inbound_event_from_room_event(
    room_id: &str,
    event: &Value,
    config: &Value,
    bot_user_id: &str,
) -> Option<Value> {
    if event.get("type").and_then(Value::as_str) != Some("m.room.message") {
        return None;
    }
    let sender = event
        .get("sender")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sender == bot_user_id {
        return None;
    }
    let content = event.get("content")?;
    let text = content
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let msgtype = content
        .get("msgtype")
        .and_then(Value::as_str)
        .unwrap_or("m.text");
    let media_file = matrix_media_file_from_content(content, msgtype, event);
    if text.is_empty() && media_file.is_none() {
        return None;
    }
    if !matrix_allowed(
        config,
        &["allowedRooms", "allowed_rooms"],
        "MATRIX_ALLOWED_ROOMS",
        room_id,
    ) {
        return None;
    }
    if !matrix_allowed(
        config,
        &["allowedUsers", "allowed_users"],
        "MATRIX_ALLOWED_USERS",
        sender,
    ) {
        return None;
    }
    let free_rooms = telegram_string_set(
        config,
        &["freeResponseRooms", "free_response_rooms"],
        "MATRIX_FREE_RESPONSE_ROOMS",
    );
    let free_room = free_rooms.contains("*") || free_rooms.contains(room_id);
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("MATRIX_REQUIRE_MENTION"))
        .unwrap_or(true);
    let mentioned = text.contains(bot_user_id)
        || bot_user_id
            .split(':')
            .next()
            .map(|local| text.contains(local.trim_start_matches('@')))
            .unwrap_or(false);
    let command = text.trim_start().starts_with('/');
    if require_mention && !free_room && !mentioned && !command {
        return None;
    }
    let event_id = event
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = content
        .pointer("/m.relates_to/event_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut inbound = json!({
        "platform": "matrix",
        "eventId": event_id,
        "event_id": event_id,
        "messageId": event_id,
        "message_id": event_id,
        "text": matrix_strip_mention(&text, bot_user_id),
        "messageType": matrix_message_type(msgtype),
        "message_type": matrix_message_type(msgtype),
        "source": {
            "platform": "matrix",
            "roomId": room_id,
            "room_id": room_id,
            "chatId": room_id,
            "chat_id": room_id,
            "userId": sender,
            "user_id": sender,
            "threadId": thread_id,
            "thread_id": thread_id,
        }
    });
    if let Some(file) = media_file {
        inbound["files"] = json!([file]);
    }
    Some(inbound)
}

fn matrix_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let room_id = source
        .get("roomId")
        .or_else(|| source.get("room_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_id = source
        .get("userId")
        .or_else(|| source.get("user_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thread_id = source
        .get("threadId")
        .or_else(|| source.get("thread_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!(
        "Matrix inbound message\nroom_id: {room_id}\nthread_id: {thread_id}\nmessage_id: {message_id}\nuser: {user_id}\n\n{text}"
    );
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    if let Some(skipped) = inbound
        .get("skippedFiles")
        .or_else(|| inbound.get("skipped_files"))
        .and_then(Value::as_array)
    {
        if !skipped.is_empty() {
            prompt.push_str("\n\nSkipped Matrix attachments:");
            for item in skipped {
                let id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                let error = item
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                prompt.push_str(&format!("\n- {id}: {error}"));
            }
        }
    }
    prompt
}

fn matrix_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("matrix")
    }) {
        return Ok(existing.id);
    }
    let persona_id = matrix_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Matrix".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("matrix"))?;
    Ok(conversation.id)
}

fn matrix_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

fn matrix_allowed(config: &Value, keys: &[&str], env_key: &str, value: &str) -> bool {
    let allowed = telegram_string_set(config, keys, env_key);
    allowed.is_empty() || allowed.contains("*") || allowed.contains(value)
}

fn matrix_env_bool(key: &str) -> Option<bool> {
    std::env::var(key)
        .ok()
        .and_then(|value| telegram_parse_bool(&value))
}

fn matrix_message_type(msgtype: &str) -> &'static str {
    match msgtype {
        "m.image" => "photo",
        "m.audio" => "voice",
        "m.video" => "video",
        "m.file" => "document",
        _ => "text",
    }
}

fn matrix_media_file_from_content(content: &Value, msgtype: &str, event: &Value) -> Option<Value> {
    if !matches!(msgtype, "m.image" | "m.audio" | "m.video" | "m.file") {
        return None;
    }
    let file_content = content.get("file");
    let url = content
        .get("url")
        .and_then(Value::as_str)
        .or_else(|| {
            file_content
                .and_then(|file| file.get("url"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let name = content
        .get("body")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| matrix_fallback_media_name(msgtype));
    let mime = content
        .pointer("/info/mimetype")
        .and_then(Value::as_str)
        .or_else(|| {
            file_content.and_then(|file| {
                file.pointer("/mimetype")
                    .and_then(Value::as_str)
                    .or_else(|| file.pointer("/info/mimetype").and_then(Value::as_str))
            })
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| matrix_fallback_media_mime(msgtype, name));
    let event_id = event
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some(json!({
        "id": event_id,
        "url": url,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": matrix_media_kind(mime),
        "msgtype": msgtype,
        "encrypted": file_content.and_then(Value::as_object).is_some(),
        "size": content.pointer("/info/size").cloned().unwrap_or(Value::Null),
    }))
}

async fn matrix_enrich_inbound_media(
    store: &AppStore,
    settings: &MatrixSettings,
    mut inbound: Value,
) -> AppResult<Value> {
    let files = inbound
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        return Ok(inbound);
    }

    let client = matrix_client(settings)?;
    let cache_dir = store.data_dir().join("attachments").join("matrix");
    fs::create_dir_all(&cache_dir)?;
    let mut attachments = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    let mut skipped_files = Vec::new();

    for file in files {
        match matrix_download_inbound_media(&client, settings, &cache_dir, &file).await {
            Ok(attachment) => {
                if let Some(path) = attachment.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                attachments.push(attachment);
            }
            Err(error) => skipped_files.push(json!({
                "id": file.get("id").and_then(Value::as_str).unwrap_or("attachment"),
                "url": file.get("url").cloned().unwrap_or(Value::Null),
                "error": error.to_string(),
            })),
        }
    }

    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        if inbound.get("messageType").and_then(Value::as_str) == Some("text")
            || inbound.get("message_type").and_then(Value::as_str) == Some("text")
        {
            let message_type = matrix_message_type_from_media(&inbound["mediaTypes"]);
            inbound["messageType"] = json!(message_type);
            inbound["message_type"] = json!(message_type);
        }
    }
    if !skipped_files.is_empty() {
        inbound["skippedFiles"] = json!(skipped_files);
        inbound["skipped_files"] = inbound["skippedFiles"].clone();
    }
    Ok(inbound)
}

async fn matrix_download_inbound_media(
    client: &reqwest::Client,
    settings: &MatrixSettings,
    cache_dir: &Path,
    file: &Value,
) -> AppResult<Value> {
    if file
        .get("encrypted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(AppError::BadRequest(
            "encrypted Matrix media is not supported by the runtime adapter yet".into(),
        ));
    }
    let mxc_url = file
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("empty Matrix media URL".into()))?;
    let download_response = client
        .get(matrix_mxc_download_url(settings, mxc_url)?)
        .bearer_auth(&settings.access_token)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Matrix media download failed: {error}")))?;
    let status = download_response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Matrix media download failed ({})",
            status.as_u16()
        )));
    }
    let bytes = download_response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Matrix media download: {error}"))
    })?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    let mime = file
        .get("mimeType")
        .or_else(|| file.get("mime_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| guess_content_type(name));
    let id = file
        .get("id")
        .and_then(Value::as_str)
        .map(matrix_safe_id_fragment)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let safe_name = mattermost_safe_file_name(name);
    let path = cache_dir.join(format!("{id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    Ok(json!({
        "id": file.get("id").cloned().unwrap_or(Value::Null),
        "url": mxc_url,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": matrix_media_kind(mime),
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    }))
}

fn matrix_mxc_download_url(settings: &MatrixSettings, mxc_url: &str) -> AppResult<reqwest::Url> {
    let rest = mxc_url
        .strip_prefix("mxc://")
        .ok_or_else(|| AppError::BadRequest("Matrix media URL must use mxc://".into()))?;
    let (server_name, media_id) = rest
        .split_once('/')
        .ok_or_else(|| AppError::BadRequest("invalid Matrix mxc:// media URL".into()))?;
    if server_name.trim().is_empty() || media_id.trim().is_empty() {
        return Err(AppError::BadRequest(
            "invalid Matrix mxc:// media URL".into(),
        ));
    }
    reqwest::Url::parse(&format!(
        "{}/_matrix/media/v3/download/{}/{}",
        settings.homeserver,
        percent_encode_path_segment(server_name),
        percent_encode_path_segment(media_id)
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid Matrix media URL: {error}")))
}

fn matrix_message_type_from_media(media_types: &Value) -> &'static str {
    let media_types = media_types.as_array().cloned().unwrap_or_default();
    if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
    }) {
        "photo"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("audio/"))
            .unwrap_or(false)
    }) {
        "voice"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("video/"))
            .unwrap_or(false)
    }) {
        "video"
    } else {
        "document"
    }
}

fn matrix_media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "document"
    }
}

fn matrix_fallback_media_name(msgtype: &str) -> &'static str {
    match msgtype {
        "m.image" => "image.jpg",
        "m.audio" => "audio.ogg",
        "m.video" => "video.mp4",
        _ => "attachment",
    }
}

fn matrix_fallback_media_mime(msgtype: &str, name: &str) -> &'static str {
    let guessed = guess_content_type(name);
    if guessed != "application/octet-stream" {
        return guessed;
    }
    match msgtype {
        "m.image" => "image/jpeg",
        "m.audio" => "audio/ogg",
        "m.video" => "video/mp4",
        _ => "application/octet-stream",
    }
}

fn matrix_safe_id_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn matrix_strip_mention(text: &str, bot_user_id: &str) -> String {
    let mut output = text.to_string();
    if !bot_user_id.trim().is_empty() {
        output = replace_ascii_case_insensitive(&output, bot_user_id, "");
        if let Some(local) = bot_user_id.split(':').next() {
            output = replace_ascii_case_insensitive(&output, local.trim_start_matches('@'), "");
        }
    }
    output.trim().to_string()
}

#[derive(Debug, Clone)]
pub(super) struct SignalSettings {
    pub(super) http_url: String,
    pub(super) account: String,
    pub(super) timeout_seconds: u64,
}

const SIGNAL_MAX_ATTACHMENTS_PER_MSG: usize = 32;

pub(super) async fn signal_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = signal_settings(&store.config()?.signal)?;
    let client = signal_client(&settings)?;
    let recipient = required_string_arg(
        payload,
        &[
            "recipient",
            "recipient_id",
            "recipientId",
            "chat_id",
            "chatId",
            "target",
        ],
        "send_message signal",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let media_files = discord_media_file_paths(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Signal requires message text or media_files".into(),
        ));
    }
    if message.chars().count() > 8_000 {
        return Err(AppError::BadRequest(
            "Signal message text cannot exceed 8000 characters in one send_message chunk".into(),
        ));
    }
    let (media_files, skipped_media_count) = signal_existing_media_file_paths(&media_files);
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message Signal has no deliverable text or existing media files".into(),
        ));
    }
    let attachment_batches = signal_attachment_batches(&media_files);
    let mut results = Vec::new();
    for (index, attachment_batch) in attachment_batches.iter().enumerate() {
        let mut params = json!({
            "account": settings.account,
            "message": if index == 0 { message.clone() } else { String::new() },
        });
        if let Some(group_id) = recipient.strip_prefix("group:") {
            params["groupId"] = json!(group_id);
        } else {
            params["recipient"] = json!([recipient.clone()]);
        }
        if !attachment_batch.is_empty() {
            params["attachments"] = json!(attachment_batch);
        }
        let id = format!("send_{}", uuid::Uuid::new_v4().simple());
        results.push(signal_rpc(&client, &settings, params, &id).await?);
    }
    let warnings = if skipped_media_count > 0 {
        json!([format!(
            "Skipped {skipped_media_count} Signal media file(s) that do not exist"
        )])
    } else {
        Value::Null
    };
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "signal",
        "chat_id": recipient,
        "media_count": media_files.len(),
        "skipped_media_count": skipped_media_count,
        "batches": attachment_batches.len(),
        "warnings": warnings,
        "results": results,
    }))?)
}

pub(super) fn signal_attachment_batches(media_files: &[String]) -> Vec<Vec<String>> {
    if media_files.is_empty() {
        return vec![Vec::new()];
    }
    media_files
        .chunks(SIGNAL_MAX_ATTACHMENTS_PER_MSG)
        .map(|chunk| chunk.to_vec())
        .collect()
}

fn signal_existing_media_file_paths(media_files: &[String]) -> (Vec<String>, usize) {
    let mut existing = Vec::new();
    let mut skipped = 0;
    for file_path in media_files {
        if Path::new(file_path).exists() {
            existing.push(file_path.clone());
        } else {
            skipped += 1;
        }
    }
    (existing, skipped)
}

pub(super) fn signal_settings(config: &Value) -> AppResult<SignalSettings> {
    let http_url = string_arg(config, &["httpUrl", "http_url", "baseUrl", "base_url"])
        .or_else(|| std::env::var("SIGNAL_HTTP_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8080".into())
        .trim()
        .trim_end_matches('/')
        .to_string();
    reqwest::Url::parse(&http_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Signal httpUrl: {error}")))?;
    let account = string_arg(config, &["account", "accountId", "account_id"])
        .or_else(|| std::env::var("SIGNAL_ACCOUNT").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Signal send_message requires settings.signal.account or SIGNAL_ACCOUNT".into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(SignalSettings {
        http_url,
        account,
        timeout_seconds,
    })
}

pub(super) fn signal_configured(config: &Value) -> bool {
    signal_settings(config).is_ok()
}

#[derive(Clone, Debug)]
pub(super) struct EmailSettings {
    pub(super) address: String,
    pub(super) password: String,
    pub(super) imap_host: Option<String>,
    pub(super) imap_port: u16,
    pub(super) smtp_host: String,
    pub(super) smtp_port: u16,
    pub(super) subject: String,
    pub(super) timeout_seconds: u64,
    pub(super) poll_interval_seconds: u64,
    pub(super) allowed_users: HashSet<String>,
    pub(super) skip_attachments: bool,
}

pub(super) async fn email_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = email_settings(&store.config()?.email)?;
    let to = required_string_arg(
        payload,
        &["to", "email", "address", "recipient", "chat_id", "chatId"],
        "send_message email",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message Email requires message text".into(),
        ));
    }
    if message.chars().count() > 20_000 {
        return Err(AppError::BadRequest(
            "Email message text cannot exceed 20000 characters in one send_message chunk".into(),
        ));
    }
    let subject = string_arg(payload, &["subject", "title"]).unwrap_or(settings.subject.clone());
    let settings_for_send = settings.clone();
    let to_for_send = to.clone();
    let message_for_send = message.clone();
    let subject_for_send = subject.clone();
    tokio::task::spawn_blocking(move || {
        email_send_smtp(
            &settings_for_send,
            &to_for_send,
            &subject_for_send,
            &message_for_send,
        )
    })
    .await
    .map_err(|error| AppError::BadRequest(format!("Email send task failed: {error}")))??;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "email",
        "chat_id": to,
        "subject": subject,
    }))?)
}

pub(super) fn email_settings(config: &Value) -> AppResult<EmailSettings> {
    let address = string_arg(config, &["address", "from", "email", "username"])
        .or_else(|| std::env::var("EMAIL_ADDRESS").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Email send_message requires settings.email.address or EMAIL_ADDRESS".into(),
            )
        })?;
    let password = string_arg(config, &["password", "appPassword", "app_password"])
        .or_else(|| std::env::var("EMAIL_PASSWORD").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Email send_message requires settings.email.password or EMAIL_PASSWORD".into(),
            )
        })?;
    let smtp_host = string_arg(config, &["smtpHost", "smtp_host", "host"])
        .or_else(|| std::env::var("EMAIL_SMTP_HOST").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "Email send_message requires settings.email.smtpHost or EMAIL_SMTP_HOST".into(),
            )
        })?;
    let smtp_port = config
        .get("smtpPort")
        .or_else(|| config.get("smtp_port"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("EMAIL_SMTP_PORT")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(587)
        .clamp(1, u16::MAX as u64) as u16;
    let imap_host = string_arg(config, &["imapHost", "imap_host"])
        .or_else(|| std::env::var("EMAIL_IMAP_HOST").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let imap_port = config
        .get("imapPort")
        .or_else(|| config.get("imap_port"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("EMAIL_IMAP_PORT")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(993)
        .clamp(1, u16::MAX as u64) as u16;
    let subject = string_arg(config, &["subject", "defaultSubject", "default_subject"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Hermes Agent".into());
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    let poll_interval_seconds = config
        .get("pollIntervalSeconds")
        .or_else(|| config.get("poll_interval_seconds"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("EMAIL_POLL_INTERVAL")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(15)
        .clamp(5, 3600);
    let allowed_users = telegram_string_set(
        config,
        &["allowedUsers", "allowed_users"],
        "EMAIL_ALLOWED_USERS",
    );
    let skip_attachments = config
        .get("skipAttachments")
        .or_else(|| config.get("skip_attachments"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("EMAIL_SKIP_ATTACHMENTS"))
        .unwrap_or(false);
    Ok(EmailSettings {
        address,
        password,
        imap_host,
        imap_port,
        smtp_host,
        smtp_port,
        subject,
        timeout_seconds,
        poll_interval_seconds,
        allowed_users,
        skip_attachments,
    })
}

pub(super) fn email_configured(config: &Value) -> bool {
    email_settings(config).is_ok()
}

fn email_runtime_configured(config: &Value) -> bool {
    email_settings(config)
        .map(|settings| settings.imap_host.is_some())
        .unwrap_or(false)
}

#[derive(Clone, Debug)]
pub(super) struct SmsSettings {
    pub(super) account_sid: String,
    pub(super) auth_token: String,
    pub(super) from_number: String,
    pub(super) api_base_url: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn sms_send_message_tool(store: &AppStore, payload: &Value) -> AppResult<String> {
    let settings = sms_settings(&store.config()?.sms)?;
    let client = sms_client(&settings)?;
    let to = required_string_arg(
        payload,
        &["to", "phone", "number", "recipient", "chat_id", "chatId"],
        "send_message sms",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message SMS requires message text".into(),
        ));
    }
    if message.chars().count() > 1_600 {
        return Err(AppError::BadRequest(
            "SMS message text cannot exceed 1600 characters in one send_message chunk".into(),
        ));
    }
    let sanitized = sms_strip_markdown(&message);
    let result = sms_request(&client, &settings, &to, &sanitized).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "sms",
        "chat_id": to,
        "message_id": result.get("sid").cloned().unwrap_or(Value::Null),
        "raw": result,
    }))?)
}

pub(super) fn sms_settings(config: &Value) -> AppResult<SmsSettings> {
    let account_sid = string_arg(config, &["accountSid", "account_sid"])
        .or_else(|| std::env::var("TWILIO_ACCOUNT_SID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "SMS send_message requires settings.sms.accountSid or TWILIO_ACCOUNT_SID".into(),
            )
        })?;
    let auth_token = string_arg(config, &["authToken", "auth_token", "apiKey", "api_key"])
        .or_else(|| std::env::var("TWILIO_AUTH_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "SMS send_message requires settings.sms.authToken or TWILIO_AUTH_TOKEN".into(),
            )
        })?;
    let from_number = string_arg(config, &["fromNumber", "from_number", "from"])
        .or_else(|| std::env::var("TWILIO_PHONE_NUMBER").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "SMS send_message requires settings.sms.fromNumber or TWILIO_PHONE_NUMBER".into(),
            )
        })?;
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .unwrap_or_else(|| "https://api.twilio.com".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid SMS apiBaseUrl: {error}")))?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(SmsSettings {
        account_sid,
        auth_token,
        from_number,
        api_base_url,
        timeout_seconds,
    })
}

pub(super) fn sms_configured(config: &Value) -> bool {
    sms_settings(config).is_ok()
}

pub(super) fn sms_client(settings: &SmsSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build SMS client: {error}")))
}

pub(super) fn sms_url(settings: &SmsSettings) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!(
        "{}/2010-04-01/Accounts/{}/Messages.json",
        settings.api_base_url, settings.account_sid
    ))
    .map_err(|error| AppError::BadRequest(format!("invalid SMS URL: {error}")))
}

async fn sms_request(
    client: &reqwest::Client,
    settings: &SmsSettings,
    to: &str,
    message: &str,
) -> AppResult<Value> {
    let response = client
        .post(sms_url(settings)?)
        .basic_auth(&settings.account_sid, Some(&settings.auth_token))
        .form(&[
            ("From", settings.from_number.as_str()),
            ("To", to),
            ("Body", message),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("SMS send failed: {error}")))?;
    sms_response_json(response).await
}

async fn sms_response_json(response: reqwest::Response) -> AppResult<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| AppError::BadRequest(format!("failed to read SMS response: {error}")))?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid SMS JSON: {error}")))?;
    if !status.is_success() {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(&text);
        return Err(AppError::BadRequest(format!(
            "Twilio API error ({}): {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(super) fn sms_strip_markdown(message: &str) -> String {
    let mut text = message.replace("```", "");
    text = text.replace("**", "");
    text = text.replace("__", "");
    text = text.replace('*', "");
    text = text.replace('_', "");
    let mut output = String::new();
    let mut in_inline_code = false;
    for ch in text.chars() {
        if ch == '`' {
            in_inline_code = !in_inline_code;
            continue;
        }
        output.push(ch);
    }
    let output = sms_strip_markdown_links(&output);
    let mut compact = String::new();
    let mut blank_lines = 0usize;
    for line in output.lines() {
        if line.trim().is_empty() {
            blank_lines += 1;
            if blank_lines <= 2 {
                compact.push('\n');
            }
            continue;
        }
        blank_lines = 0;
        compact.push_str(line);
        compact.push('\n');
    }
    compact
        .lines()
        .map(|line| line.trim_start_matches('#').trim_start())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn sms_strip_markdown_links(message: &str) -> String {
    let chars = message.chars().collect::<Vec<_>>();
    let mut output = String::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '[' {
            if let Some(close) = chars[index + 1..].iter().position(|ch| *ch == ']') {
                let close = index + 1 + close;
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = chars[close + 2..].iter().position(|ch| *ch == ')') {
                        output.extend(chars[index + 1..close].iter());
                        index = close + 2 + end + 1;
                        continue;
                    }
                }
            }
        }
        output.push(chars[index]);
        index += 1;
    }
    output
}

#[derive(Clone, Debug)]
pub(super) struct DingTalkSettings {
    pub(super) webhook_url: String,
    pub(super) timeout_seconds: u64,
}

#[derive(Clone, Debug)]
struct DingTalkWebhookSettings {
    host: String,
    port: u16,
    path: String,
    secret: Option<String>,
    require_mention: bool,
}

pub(super) async fn dingtalk_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = dingtalk_settings(&store.config()?.dingtalk)?;
    let client = dingtalk_client(&settings)?;
    let target =
        string_arg(payload, &["target", "chat_id", "chatId"]).unwrap_or_else(|| "webhook".into());
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message DingTalk requires message text".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "DingTalk message text cannot exceed 4000 characters in one send_message chunk".into(),
        ));
    }
    let result = dingtalk_request(&client, &settings, &message).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "dingtalk",
        "chat_id": target,
        "raw": result,
    }))?)
}

pub(super) fn dingtalk_settings(config: &Value) -> AppResult<DingTalkSettings> {
    let webhook_url = string_arg(config, &["webhookUrl", "webhook_url", "url"])
        .or_else(|| std::env::var("DINGTALK_WEBHOOK_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "DingTalk send_message requires settings.dingtalk.webhookUrl or DINGTALK_WEBHOOK_URL"
                    .into(),
            )
        })?;
    reqwest::Url::parse(&webhook_url)
        .map_err(|error| AppError::BadRequest(format!("invalid DingTalk webhookUrl: {error}")))?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(DingTalkSettings {
        webhook_url,
        timeout_seconds,
    })
}

pub(super) fn dingtalk_configured(config: &Value) -> bool {
    dingtalk_settings(config).is_ok()
}

fn dingtalk_webhook_configured(config: &Value) -> bool {
    dingtalk_webhook_settings(config).is_ok()
}

fn dingtalk_webhook_settings(config: &Value) -> AppResult<DingTalkWebhookSettings> {
    let host = string_arg(config, &["webhookHost", "webhook_host", "host", "bindHost"])
        .or_else(|| std::env::var("DINGTALK_WEBHOOK_HOST").ok())
        .unwrap_or_else(|| "127.0.0.1".into())
        .trim()
        .to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest(
            "DingTalk webhook host cannot be empty".into(),
        ));
    }
    let port = config
        .get("webhookPort")
        .or_else(|| config.get("webhook_port"))
        .or_else(|| config.get("port"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("DINGTALK_WEBHOOK_PORT")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(8766)
        .clamp(1, u16::MAX as u64) as u16;
    let mut path = string_arg(config, &["webhookPath", "webhook_path", "path"])
        .or_else(|| std::env::var("DINGTALK_WEBHOOK_PATH").ok())
        .unwrap_or_else(|| "/dingtalk/webhook".into())
        .trim()
        .to_string();
    if path.is_empty() {
        path = "/dingtalk/webhook".into();
    }
    if !path.starts_with('/') {
        path = format!("/{path}");
    }
    let secret = string_arg(config, &["secret", "webhookSecret", "webhook_secret"])
        .or_else(|| std::env::var("DINGTALK_WEBHOOK_SECRET").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DINGTALK_REQUIRE_MENTION"))
        .unwrap_or(false);
    Ok(DingTalkWebhookSettings {
        host,
        port,
        path,
        secret,
        require_mention,
    })
}

pub(super) fn dingtalk_client(settings: &DingTalkSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build DingTalk client: {error}")))
}

async fn dingtalk_request(
    client: &reqwest::Client,
    settings: &DingTalkSettings,
    message: &str,
) -> AppResult<Value> {
    let response = client
        .post(&settings.webhook_url)
        .json(&json!({
            "msgtype": "text",
            "text": { "content": message },
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("DingTalk send failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read DingTalk response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid DingTalk JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "DingTalk API error ({}): {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    if value.get("errcode").and_then(Value::as_i64).unwrap_or(0) != 0 {
        let errmsg = value
            .get("errmsg")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(AppError::BadRequest(format!(
            "DingTalk API error: {}",
            truncate_output(errmsg, 2000)
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug)]
pub(super) struct WhatsAppSettings {
    pub(super) bridge_url: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn whatsapp_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = whatsapp_settings(&store.config()?.whatsapp)?;
    let client = whatsapp_client(&settings)?;
    let chat_id = required_string_arg(
        payload,
        &["chat_id", "chatId", "to", "recipient"],
        "send_message whatsapp",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message WhatsApp requires message text".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "WhatsApp message text cannot exceed 4000 characters in one send_message chunk".into(),
        ));
    }
    let result = whatsapp_request(&client, &settings, &chat_id, &message).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "whatsapp",
        "chat_id": chat_id,
        "message_id": result.get("messageId").cloned().unwrap_or(Value::Null),
        "raw": result,
    }))?)
}

pub(super) fn whatsapp_settings(config: &Value) -> AppResult<WhatsAppSettings> {
    let bridge_url = string_arg(
        config,
        &["bridgeUrl", "bridge_url", "url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("WHATSAPP_BRIDGE_URL").ok())
    .or_else(|| {
        string_arg(config, &["bridgePort", "bridge_port"])
            .or_else(|| std::env::var("WHATSAPP_BRIDGE_PORT").ok())
            .or_else(|| {
                config
                    .get("bridgePort")
                    .or_else(|| config.get("bridge_port"))
                    .and_then(Value::as_u64)
                    .map(|port| port.to_string())
            })
            .map(|port| format!("http://localhost:{}", port.trim()))
    })
    .unwrap_or_else(|| "http://localhost:3000".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&bridge_url)
        .map_err(|error| AppError::BadRequest(format!("invalid WhatsApp bridgeUrl: {error}")))?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(WhatsAppSettings {
        bridge_url,
        timeout_seconds,
    })
}

pub(super) fn whatsapp_configured(config: &Value) -> bool {
    let explicitly_enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || string_arg(
            config,
            &["bridgeUrl", "bridge_url", "url", "baseUrl", "base_url"],
        )
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        || config
            .get("bridgePort")
            .or_else(|| config.get("bridge_port"))
            .is_some()
        || std::env::var("WHATSAPP_BRIDGE_URL")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        || std::env::var("WHATSAPP_BRIDGE_PORT")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    explicitly_enabled && whatsapp_settings(config).is_ok()
}

pub(super) fn whatsapp_client(settings: &WhatsAppSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build WhatsApp client: {error}")))
}

pub(super) fn whatsapp_send_url(settings: &WhatsAppSettings) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!("{}/send", settings.bridge_url))
        .map_err(|error| AppError::BadRequest(format!("invalid WhatsApp send URL: {error}")))
}

async fn whatsapp_request(
    client: &reqwest::Client,
    settings: &WhatsAppSettings,
    chat_id: &str,
    message: &str,
) -> AppResult<Value> {
    let response = client
        .post(whatsapp_send_url(settings)?)
        .json(&json!({
            "chatId": chat_id,
            "message": message,
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("WhatsApp send failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read WhatsApp response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid WhatsApp JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "WhatsApp bridge error ({}): {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug)]
pub(super) struct QqBotSettings {
    pub(super) app_id: String,
    pub(super) client_secret: String,
    pub(super) api_base_url: String,
    pub(super) token_url: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn qqbot_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = qqbot_settings(&store.config()?.qqbot)?;
    let client = qqbot_client(&settings)?;
    let chat_id = required_string_arg(
        payload,
        &[
            "chat_id",
            "chatId",
            "channel_id",
            "channelId",
            "to",
            "recipient",
        ],
        "send_message qqbot",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty() {
        return Err(AppError::BadRequest(
            "send_message QQBot requires message text".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "QQBot message text cannot exceed 4000 characters in one send_message chunk".into(),
        ));
    }
    let access_token = qqbot_access_token(&client, &settings).await?;
    let result =
        qqbot_send_to_any_endpoint(&client, &settings, &access_token, &chat_id, &message).await?;
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "qqbot",
        "chat_id": chat_id,
        "message_id": result.get("id").cloned().unwrap_or(Value::Null),
        "raw": result,
    }))?)
}

pub(super) fn qqbot_settings(config: &Value) -> AppResult<QqBotSettings> {
    let app_id = string_arg(config, &["appId", "app_id"])
        .or_else(|| std::env::var("QQ_APP_ID").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "QQBot send_message requires settings.qqbot.appId or QQ_APP_ID".into(),
            )
        })?;
    let client_secret = string_arg(
        config,
        &["clientSecret", "client_secret", "secret", "token"],
    )
    .or_else(|| std::env::var("QQ_CLIENT_SECRET").ok())
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| {
        AppError::BadRequest(
            "QQBot send_message requires settings.qqbot.clientSecret or QQ_CLIENT_SECRET".into(),
        )
    })?;
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .unwrap_or_else(|| "https://api.sgroup.qq.com".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot apiBaseUrl: {error}")))?;
    let token_url = string_arg(config, &["tokenUrl", "token_url"])
        .unwrap_or_else(|| "https://bots.qq.com/app/getAppAccessToken".into())
        .trim()
        .to_string();
    reqwest::Url::parse(&token_url)
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot tokenUrl: {error}")))?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 600);
    Ok(QqBotSettings {
        app_id,
        client_secret,
        api_base_url,
        token_url,
        timeout_seconds,
    })
}

pub(super) fn qqbot_configured(config: &Value) -> bool {
    qqbot_settings(config).is_ok()
}

pub(super) fn qqbot_client(settings: &QqBotSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build QQBot client: {error}")))
}

async fn qqbot_access_token(
    client: &reqwest::Client,
    settings: &QqBotSettings,
) -> AppResult<String> {
    let response = client
        .post(&settings.token_url)
        .json(&json!({
            "appId": settings.app_id,
            "clientSecret": settings.client_secret,
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("QQBot token request failed: {error}")))?;
    let value = qqbot_response_json(response, "QQBot token request").await?;
    value
        .get("access_token")
        .or_else(|| value.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::BadRequest("QQBot token response missing access_token".into()))
}

async fn qqbot_send_to_any_endpoint(
    client: &reqwest::Client,
    settings: &QqBotSettings,
    access_token: &str,
    chat_id: &str,
    message: &str,
) -> AppResult<Value> {
    let payload = json!({
        "content": message,
        "msg_type": 0,
    });
    let mut failures = Vec::new();
    for kind in ["channel", "c2c", "group"] {
        let url = qqbot_message_url(settings, kind, chat_id)?;
        let response = client
            .post(url)
            .header("Authorization", format!("QQBot {access_token}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| AppError::BadRequest(format!("QQBot send failed: {error}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AppError::BadRequest(format!("failed to read QQBot response: {error}"))
        })?;
        if status.as_u16() == 200 || status.as_u16() == 201 {
            return serde_json::from_str::<Value>(&text)
                .map_err(|error| AppError::BadRequest(format!("invalid QQBot JSON: {error}")));
        }
        failures.push(format!(
            "{kind} {}: {}",
            status.as_u16(),
            truncate_output(&text, 500)
        ));
    }
    Err(AppError::BadRequest(format!(
        "QQBot send failed on all endpoints: {}",
        failures.join("; ")
    )))
}

pub(super) fn qqbot_message_url(
    settings: &QqBotSettings,
    kind: &str,
    chat_id: &str,
) -> AppResult<reqwest::Url> {
    let path = match kind {
        "channel" => format!("channels/{chat_id}/messages"),
        "c2c" => format!("v2/users/{chat_id}/messages"),
        "group" => format!("v2/groups/{chat_id}/messages"),
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported QQBot endpoint kind: {other}"
            )))
        }
    };
    reqwest::Url::parse(&format!("{}/{}", settings.api_base_url, path))
        .map_err(|error| AppError::BadRequest(format!("invalid QQBot message URL: {error}")))
}

async fn qqbot_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} failed ({}): {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug)]
pub(super) struct BlueBubblesSettings {
    pub(super) server_url: String,
    pub(super) password: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn bluebubbles_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = bluebubbles_settings(&store.config()?.bluebubbles)?;
    let client = bluebubbles_client(&settings)?;
    let chat_id = required_string_arg(
        payload,
        &["chat_id", "chatId", "to", "recipient", "address"],
        "send_message bluebubbles",
    )?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    let media_files = discord_media_file_paths(payload)?;
    if message.trim().is_empty() && media_files.is_empty() {
        return Err(AppError::BadRequest(
            "send_message BlueBubbles requires message text or media_files".into(),
        ));
    }
    if message.chars().count() > 4_000 {
        return Err(AppError::BadRequest(
            "BlueBubbles message text cannot exceed 4000 characters in one send_message chunk"
                .into(),
        ));
    }
    let guid = bluebubbles_resolve_chat_guid(&client, &settings, &chat_id).await?;
    let mut events = Vec::new();
    if !message.trim().is_empty() {
        events.push(bluebubbles_send_text(&client, &settings, &guid, &message).await?);
    }
    for file_path in &media_files {
        events.push(bluebubbles_send_attachment(&client, &settings, &guid, file_path).await?);
    }
    let message_id = events
        .last()
        .and_then(|event| event.get("data"))
        .and_then(|data| data.get("guid").or_else(|| data.get("messageGuid")))
        .cloned()
        .unwrap_or(Value::Null);
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": "bluebubbles",
        "chat_id": chat_id,
        "chat_guid": guid,
        "message_id": message_id,
        "media_count": media_files.len(),
        "events": events,
    }))?)
}

pub(super) fn bluebubbles_settings(config: &Value) -> AppResult<BlueBubblesSettings> {
    let server_url = string_arg(config, &["serverUrl", "server_url", "url", "baseUrl", "base_url"])
        .or_else(|| std::env::var("BLUEBUBBLES_SERVER_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with("http://") || value.starts_with("https://") {
                value
            } else {
                format!("http://{value}")
            }
        })
        .map(|value| value.trim_end_matches('/').to_string())
        .ok_or_else(|| {
            AppError::BadRequest(
                "BlueBubbles send_message requires settings.bluebubbles.serverUrl or BLUEBUBBLES_SERVER_URL"
                    .into(),
            )
        })?;
    reqwest::Url::parse(&server_url)
        .map_err(|error| AppError::BadRequest(format!("invalid BlueBubbles serverUrl: {error}")))?;
    let password = string_arg(config, &["password", "token"])
        .or_else(|| std::env::var("BLUEBUBBLES_PASSWORD").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(
                "BlueBubbles send_message requires settings.bluebubbles.password or BLUEBUBBLES_PASSWORD"
                    .into(),
            )
        })?;
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 600);
    Ok(BlueBubblesSettings {
        server_url,
        password,
        timeout_seconds,
    })
}

pub(super) fn bluebubbles_configured(config: &Value) -> bool {
    bluebubbles_settings(config).is_ok()
}

pub(super) fn bluebubbles_client(settings: &BlueBubblesSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build BlueBubbles client: {error}"))
        })
}

pub(super) fn bluebubbles_api_url(
    settings: &BlueBubblesSettings,
    path: &str,
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&settings.server_url)
        .map_err(|error| AppError::BadRequest(format!("invalid BlueBubbles URL: {error}")))?;
    if !path.starts_with('/') || path.contains('\\') {
        return Err(AppError::BadRequest(format!(
            "invalid BlueBubbles API path: {path}"
        )));
    }
    url.set_path(path);
    url.query_pairs_mut()
        .append_pair("password", &settings.password);
    Ok(url)
}

async fn bluebubbles_resolve_chat_guid(
    client: &reqwest::Client,
    settings: &BlueBubblesSettings,
    target: &str,
) -> AppResult<String> {
    let target = target.trim();
    if target.is_empty() {
        return Err(AppError::BadRequest(
            "BlueBubbles target chat id cannot be empty".into(),
        ));
    }
    if target.contains(';') {
        return Ok(target.to_string());
    }
    let response = client
        .post(bluebubbles_api_url(settings, "/api/v1/chat/query")?)
        .json(&json!({
            "limit": 100,
            "offset": 0,
            "with": ["participants"],
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("BlueBubbles chat query failed: {error}")))?;
    let value = bluebubbles_response_json(response, "BlueBubbles chat query").await?;
    for chat in value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let guid = chat
            .get("guid")
            .or_else(|| chat.get("chatGuid"))
            .and_then(Value::as_str);
        let identifier = chat
            .get("chatIdentifier")
            .or_else(|| chat.get("identifier"))
            .and_then(Value::as_str);
        if identifier == Some(target) {
            if let Some(guid) = guid {
                return Ok(guid.to_string());
            }
        }
        for participant in chat
            .get("participants")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if participant.get("address").and_then(Value::as_str) == Some(target) {
                if let Some(guid) = guid {
                    return Ok(guid.to_string());
                }
            }
        }
    }
    Err(AppError::BadRequest(format!(
        "BlueBubbles chat not found for target: {target}"
    )))
}

async fn bluebubbles_send_text(
    client: &reqwest::Client,
    settings: &BlueBubblesSettings,
    chat_guid: &str,
    message: &str,
) -> AppResult<Value> {
    let response = client
        .post(bluebubbles_api_url(settings, "/api/v1/message/text")?)
        .json(&json!({
            "chatGuid": chat_guid,
            "tempGuid": format!("temp-{}", uuid::Uuid::new_v4().simple()),
            "message": message,
        }))
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("BlueBubbles send failed: {error}")))?;
    bluebubbles_response_json(response, "BlueBubbles send").await
}

async fn bluebubbles_send_attachment(
    client: &reqwest::Client,
    settings: &BlueBubblesSettings,
    chat_guid: &str,
    file_path: &str,
) -> AppResult<Value> {
    let path = Path::new(file_path);
    let bytes = fs::read(path).map_err(|error| {
        AppError::BadRequest(format!("failed to read BlueBubbles media file: {error}"))
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("attachment")
        .to_string();
    let mut form = reqwest::multipart::Form::new()
        .text("chatGuid", chat_guid.to_string())
        .text("name", file_name.clone())
        .text("tempGuid", uuid::Uuid::new_v4().simple().to_string())
        .part(
            "attachment",
            reqwest::multipart::Part::bytes(bytes).file_name(file_name.clone()),
        );
    if bluebubbles_is_audio_message(&file_name) {
        form = form.text("isAudioMessage", "true");
    }
    let response = client
        .post(bluebubbles_api_url(settings, "/api/v1/message/attachment")?)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("BlueBubbles attachment send failed: {error}"))
        })?;
    bluebubbles_response_json(response, "BlueBubbles attachment send").await
}

pub(super) fn bluebubbles_is_audio_message(file_name: &str) -> bool {
    matches!(
        Path::new(file_name)
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("ogg") | Some("opus") | Some("mp3") | Some("m4a") | Some("wav") | Some("aac")
    )
}

async fn bluebubbles_response_json(response: reqwest::Response, label: &str) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "{label} failed ({}): {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug)]
pub(super) struct MessagingGatewaySettings {
    pub(super) url: String,
    pub(super) token: Option<String>,
    pub(super) send_path: String,
    pub(super) platforms: HashSet<String>,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn messaging_gateway_send_message_tool(
    store: &AppStore,
    payload: &Value,
) -> AppResult<String> {
    let settings = messaging_gateway_settings(&store.config()?.messaging_gateway)?;
    let platform = required_string_arg(payload, &["platform"], "send_message messaging gateway")?;
    let target = required_string_arg(payload, &["target"], "send_message messaging gateway")?;
    let message = string_arg(payload, &["message", "content", "text", "body"]).unwrap_or_default();
    if message.trim().is_empty()
        && payload
            .get("media_files")
            .or_else(|| payload.get("mediaFiles"))
            .and_then(Value::as_array)
            .map(|items| items.is_empty())
            .unwrap_or(true)
    {
        return Err(AppError::BadRequest(
            "send_message messaging gateway requires message text or media_files".into(),
        ));
    }
    let client = messaging_gateway_client(&settings)?;
    let mut body = json!({
        "tool": "send_message",
        "action": "send",
        "platform": platform,
        "target": target,
        "message": message,
    });
    if let Some(chat_id) = string_arg(payload, &["chat_id", "chatId"]) {
        body["chat_id"] = json!(chat_id);
    }
    if let Some(media_files) = payload
        .get("media_files")
        .or_else(|| payload.get("mediaFiles"))
    {
        body["media_files"] = media_files.clone();
    }
    let mut request = client
        .post(messaging_gateway_send_url(&settings)?)
        .json(&body);
    if let Some(token) = settings.token.as_deref() {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("messaging gateway send failed: {error}")))?;
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read messaging gateway response: {error}"
        ))
    })?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({ "raw": text }));
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "messaging gateway returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&value.to_string(), 2000)
        )));
    }
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        if !error.trim().is_empty() {
            return Err(AppError::BadRequest(format!(
                "messaging gateway send failed: {}",
                truncate_output(error, 2000)
            )));
        }
    }
    Ok(serde_json::to_string_pretty(&json!({
        "success": true,
        "platform": platform,
        "target": target,
        "gateway": settings.url,
        "response": value,
    }))?)
}

pub(super) fn messaging_gateway_settings(config: &Value) -> AppResult<MessagingGatewaySettings> {
    let explicitly_enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let url = string_arg(config, &["url", "gatewayUrl", "gateway_url", "baseUrl", "base_url"])
        .or_else(|| std::env::var("HERMES_MESSAGING_GATEWAY_URL").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.starts_with("http://") || value.starts_with("https://") {
                value
            } else {
                format!("http://{value}")
            }
        })
        .map(|value| value.trim_end_matches('/').to_string())
        .ok_or_else(|| {
            AppError::BadRequest(
                "messaging gateway requires settings.messagingGateway.url or HERMES_MESSAGING_GATEWAY_URL"
                    .into(),
            )
        })?;
    if !explicitly_enabled {
        return Err(AppError::BadRequest(
            "messaging gateway is disabled; set settings.messagingGateway.enabled=true".into(),
        ));
    }
    reqwest::Url::parse(&url)
        .map_err(|error| AppError::BadRequest(format!("invalid messaging gateway URL: {error}")))?;
    let token = string_arg(config, &["token", "apiKey", "api_key"])
        .or_else(|| std::env::var("HERMES_MESSAGING_GATEWAY_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let send_path = string_arg(config, &["sendPath", "send_path", "path"])
        .unwrap_or_else(|| "/send_message".into())
        .trim()
        .to_string();
    if !send_path.starts_with('/') || send_path.contains('\\') {
        return Err(AppError::BadRequest(format!(
            "invalid messaging gateway sendPath: {send_path}"
        )));
    }
    let platforms = config
        .get("platforms")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .collect::<HashSet<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            ["wecom", "weixin", "yuanbao"]
                .into_iter()
                .map(str::to_string)
                .collect()
        });
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 900);
    Ok(MessagingGatewaySettings {
        url,
        token,
        send_path,
        platforms,
        timeout_seconds,
    })
}

pub(super) fn messaging_gateway_receive_settings(
    config: &Value,
) -> AppResult<MessagingGatewayReceiveSettings> {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("HERMES_MESSAGING_GATEWAY_ENABLED"))
        .unwrap_or(false);
    if !enabled {
        return Err(AppError::BadRequest(
            "messaging gateway is disabled; set settings.messagingGateway.enabled=true".into(),
        ));
    }
    let host = string_arg(
        config,
        &[
            "webhookHost",
            "webhook_host",
            "bindHost",
            "bind_host",
            "host",
        ],
    )
    .or_else(|| std::env::var("HERMES_MESSAGING_GATEWAY_WEBHOOK_HOST").ok())
    .unwrap_or_else(|| "127.0.0.1".into())
    .trim()
    .to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest(
            "messaging gateway webhook host cannot be empty".into(),
        ));
    }
    let port = config
        .get("webhookPort")
        .or_else(|| config.get("webhook_port"))
        .or_else(|| config.get("listenPort"))
        .or_else(|| config.get("listen_port"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("HERMES_MESSAGING_GATEWAY_WEBHOOK_PORT")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(8767)
        .clamp(1, u16::MAX as u64) as u16;
    let mut path = string_arg(
        config,
        &["webhookPath", "webhook_path", "listenPath", "listen_path"],
    )
    .or_else(|| std::env::var("HERMES_MESSAGING_GATEWAY_WEBHOOK_PATH").ok())
    .unwrap_or_else(|| "/messaging-gateway/webhook".into())
    .trim()
    .to_string();
    if !path.starts_with('/') {
        path.insert(0, '/');
    }
    let secret = string_arg(config, &["webhookSecret", "webhook_secret", "secret"])
        .or_else(|| std::env::var("HERMES_MESSAGING_GATEWAY_WEBHOOK_SECRET").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let platforms = messaging_gateway_platform_set(config);
    let timeout_seconds = config
        .get("webhookTimeoutSeconds")
        .or_else(|| config.get("webhook_timeout_seconds"))
        .or_else(|| config.get("timeoutSeconds"))
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 300);
    let weixin_cdn_base_url = string_arg(
        config,
        &[
            "weixinCdnBaseUrl",
            "weixin_cdn_base_url",
            "cdnBaseUrl",
            "cdn_base_url",
        ],
    )
    .or_else(|| std::env::var("WEIXIN_CDN_BASE_URL").ok())
    .unwrap_or_else(|| "https://novac2c.cdn.weixin.qq.com/c2c".into())
    .trim()
    .trim_end_matches('/')
    .to_string();
    Ok(MessagingGatewayReceiveSettings {
        host,
        port,
        path,
        secret,
        platforms,
        timeout_seconds,
        weixin_cdn_base_url,
    })
}

fn messaging_gateway_platform_set(config: &Value) -> HashSet<String> {
    config
        .get("platforms")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| {
                    matches!(
                        value.as_str(),
                        "wecom"
                            | "weixin"
                            | "yuanbao"
                            | "whatsapp"
                            | "qqbot"
                            | "bluebubbles"
                            | "sms"
                            | "homeassistant"
                            | "msgraph_webhook"
                    )
                })
                .collect::<HashSet<_>>()
        })
        .filter(|items| !items.is_empty())
        .unwrap_or_else(|| {
            [
                "wecom",
                "weixin",
                "yuanbao",
                "whatsapp",
                "qqbot",
                "bluebubbles",
                "sms",
                "homeassistant",
                "msgraph_webhook",
            ]
            .into_iter()
            .map(str::to_string)
            .collect()
        })
}

pub(super) fn messaging_gateway_configured(config: &Value) -> bool {
    messaging_gateway_settings(config).is_ok()
}

pub(super) fn messaging_gateway_receive_configured(config: &Value) -> bool {
    messaging_gateway_receive_settings(config).is_ok()
}

pub(super) fn messaging_gateway_runtime_configured(config: &Value) -> bool {
    messaging_gateway_configured(config) || messaging_gateway_receive_configured(config)
}

pub(super) fn messaging_gateway_platform_enabled(config: &Value, platform: &str) -> bool {
    messaging_gateway_settings(config)
        .map(|settings| settings.platforms.contains(&platform.to_ascii_lowercase()))
        .or_else(|_| {
            messaging_gateway_receive_settings(config)
                .map(|settings| settings.platforms.contains(&platform.to_ascii_lowercase()))
        })
        .unwrap_or(false)
}

pub(super) fn messaging_gateway_client(
    settings: &MessagingGatewaySettings,
) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build messaging gateway client: {error}"))
        })
}

pub(super) fn messaging_gateway_send_url(
    settings: &MessagingGatewaySettings,
) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!("{}{}", settings.url, settings.send_path)).map_err(|error| {
        AppError::BadRequest(format!("invalid messaging gateway send URL: {error}"))
    })
}

fn email_send_smtp(settings: &EmailSettings, to: &str, subject: &str, body: &str) -> AppResult<()> {
    let from_mailbox = settings
        .address
        .parse::<Mailbox>()
        .map_err(|error| AppError::BadRequest(format!("invalid Email from address: {error}")))?;
    let to_mailbox = to.parse::<Mailbox>().map_err(|error| {
        AppError::BadRequest(format!("invalid Email recipient address: {error}"))
    })?;
    let message = Message::builder()
        .from(from_mailbox)
        .to(to_mailbox)
        .subject(subject)
        .body(body.to_string())
        .map_err(|error| AppError::BadRequest(format!("failed to build Email message: {error}")))?;
    let credentials = Credentials::new(settings.address.clone(), settings.password.clone());
    let mailer = SmtpTransport::starttls_relay(&settings.smtp_host)
        .map_err(|error| AppError::BadRequest(format!("failed to configure SMTP relay: {error}")))?
        .port(settings.smtp_port)
        .credentials(credentials)
        .timeout(Some(Duration::from_secs(settings.timeout_seconds)))
        .build();
    mailer
        .send(&message)
        .map_err(|error| AppError::BadRequest(format!("Email send failed: {error}")))?;
    Ok(())
}

pub(super) fn signal_client(settings: &SignalSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Signal client: {error}")))
}

fn signal_stream_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Signal stream client: {error}"))
        })
}

fn signal_events_url(settings: &SignalSettings) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&format!("{}/api/v1/events", settings.http_url))
        .map_err(|error| AppError::BadRequest(format!("invalid Signal events URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("account", &settings.account);
    Ok(url)
}

async fn signal_rpc(
    client: &reqwest::Client,
    settings: &SignalSettings,
    params: Value,
    id: &str,
) -> AppResult<Value> {
    let url = signal_rpc_url(settings)?;
    let body = json!({
        "jsonrpc": "2.0",
        "method": "send",
        "params": params,
        "id": id,
    });
    let response = client
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Signal send failed: {error}")))?;
    signal_response_json(response).await
}

pub(super) fn signal_rpc_url(settings: &SignalSettings) -> AppResult<reqwest::Url> {
    reqwest::Url::parse(&format!("{}/api/v1/rpc", settings.http_url))
        .map_err(|error| AppError::BadRequest(format!("invalid Signal RPC URL: {error}")))
}

async fn signal_response_json(response: reqwest::Response) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read Signal response: {error}"))
    })?;
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid Signal JSON: {error}")))?;
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Signal returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(&text, 2000)
        )));
    }
    if let Some(error) = value.get("error") {
        return Err(AppError::BadRequest(format!(
            "Signal RPC error: {}",
            truncate_output(&error.to_string(), 2000)
        )));
    }
    Ok(value)
}

pub(super) fn yuanbao_search_local_stickers(
    settings: &YuanbaoSettings,
    payload: &Value,
) -> AppResult<Option<Value>> {
    let Some(stickers) = settings
        .config
        .get("stickers")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty())
    else {
        return Ok(None);
    };
    let query = string_arg(payload, &["query", "q"]).unwrap_or_default();
    let query_lower = query.to_lowercase();
    let limit = payload
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .clamp(1, 50) as usize;
    let mut results = Vec::new();
    for sticker in stickers {
        let haystack = [
            sticker
                .get("sticker_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            sticker
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            sticker
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            sticker
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            sticker
                .get("package_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ]
        .join(" ")
        .to_lowercase();
        if query_lower.is_empty() || haystack.contains(&query_lower) {
            results.push(json!({
                "sticker_id": sticker
                    .get("sticker_id")
                    .or_else(|| sticker.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "name": sticker.get("name").and_then(Value::as_str).unwrap_or_default(),
                "description": sticker.get("description").and_then(Value::as_str).unwrap_or_default(),
                "package_id": sticker.get("package_id").and_then(Value::as_str).unwrap_or_default(),
            }));
            if results.len() >= limit {
                break;
            }
        }
    }
    Ok(Some(json!({
        "success": true,
        "query": query,
        "count": results.len(),
        "results": results,
        "source": "local-config",
    })))
}

#[derive(Debug, Clone)]
pub(super) struct SpotifySettings {
    pub(super) api_base_url: String,
    pub(super) access_token: Option<String>,
    pub(super) refresh_token: Option<String>,
    pub(super) client_id: Option<String>,
    pub(super) client_secret: Option<String>,
    pub(super) token_url: String,
    pub(super) timeout_seconds: u64,
}

pub(super) async fn spotify_tool(
    store: &AppStore,
    tool_name: &str,
    payload: &Value,
) -> AppResult<String> {
    let settings = spotify_settings(&store.config()?.spotify)?;
    let client = spotify_client(&settings)?;
    let result = match tool_name {
        "spotify_playback" => spotify_playback_tool(&client, &settings, payload).await?,
        "spotify_devices" => spotify_devices_tool(&client, &settings, payload).await?,
        "spotify_queue" => spotify_queue_tool(&client, &settings, payload).await?,
        "spotify_search" => spotify_search_tool(&client, &settings, payload).await?,
        "spotify_playlists" => spotify_playlists_tool(&client, &settings, payload).await?,
        "spotify_albums" => spotify_albums_tool(&client, &settings, payload).await?,
        "spotify_library" => spotify_library_tool(&client, &settings, payload).await?,
        other => {
            return Err(AppError::BadRequest(format!(
                "unsupported Spotify tool: {other}"
            )));
        }
    };
    Ok(serde_json::to_string_pretty(&json!({
        "tool": tool_name,
        "success": true,
        "result": result,
    }))?)
}

pub(super) async fn spotify_playback_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let action = spotify_action(payload).unwrap_or_else(|| "get_state".into());
    match action.as_str() {
        "get_state" => {
            spotify_request(
                client,
                settings,
                "GET",
                "/me/player",
                &spotify_query(payload, &[("market", &["market"])]),
                None,
                "Spotify playback state",
            )
            .await
        }
        "get_currently_playing" => {
            spotify_request(
                client,
                settings,
                "GET",
                "/me/player/currently-playing",
                &spotify_query(payload, &[("market", &["market"])]),
                None,
                "Spotify currently playing",
            )
            .await
        }
        "play" => {
            let mut body = serde_json::Map::new();
            if let Some(context_uri) = string_arg(payload, &["context_uri", "contextUri"]) {
                body.insert(
                    "context_uri".into(),
                    Value::String(normalize_spotify_uri(&context_uri, None)?),
                );
            }
            if let Some(uris) = spotify_string_list(payload, &["uris", "items"]) {
                body.insert(
                    "uris".into(),
                    json!(normalize_spotify_uris(&uris, Some("track"))?),
                );
            }
            if let Some(offset) = payload.get("offset").filter(|value| value.is_object()) {
                body.insert("offset".into(), offset.clone());
            }
            if let Some(position_ms) = spotify_u64_arg(payload, &["position_ms", "positionMs"]) {
                body.insert("position_ms".into(), json!(position_ms));
            }
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/play",
                &spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]),
                Some(Value::Object(body)),
                "Spotify playback play",
            )
            .await
        }
        "pause" => {
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/pause",
                &spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]),
                None,
                "Spotify playback pause",
            )
            .await
        }
        "next" | "previous" => {
            spotify_request(
                client,
                settings,
                "POST",
                &format!("/me/player/{action}"),
                &spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]),
                None,
                "Spotify playback skip",
            )
            .await
        }
        "seek" => {
            let position_ms =
                spotify_u64_arg(payload, &["position_ms", "positionMs"]).ok_or_else(|| {
                    AppError::BadRequest("spotify_playback seek requires position_ms".into())
                })?;
            let mut query = spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]);
            query.push(("position_ms".into(), position_ms.to_string()));
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/seek",
                &query,
                None,
                "Spotify playback seek",
            )
            .await
        }
        "set_repeat" => {
            let state =
                required_string_arg(payload, &["state"], "spotify_playback")?.to_lowercase();
            if !matches!(state.as_str(), "track" | "context" | "off") {
                return Err(AppError::BadRequest(
                    "spotify_playback set_repeat state must be track, context, or off".into(),
                ));
            }
            let mut query = spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]);
            query.push(("state".into(), state));
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/repeat",
                &query,
                None,
                "Spotify repeat",
            )
            .await
        }
        "set_shuffle" => {
            let state = spotify_bool_arg(payload, &["state"]).unwrap_or(false);
            let mut query = spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]);
            query.push(("state".into(), state.to_string()));
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/shuffle",
                &query,
                None,
                "Spotify shuffle",
            )
            .await
        }
        "set_volume" => {
            let volume = spotify_u64_arg(payload, &["volume_percent", "volumePercent"])
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "spotify_playback set_volume requires volume_percent".into(),
                    )
                })?
                .min(100);
            let mut query = spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]);
            query.push(("volume_percent".into(), volume.to_string()));
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player/volume",
                &query,
                None,
                "Spotify volume",
            )
            .await
        }
        "recently_played" => {
            if spotify_u64_arg(payload, &["after"]).is_some()
                && spotify_u64_arg(payload, &["before"]).is_some()
            {
                return Err(AppError::BadRequest(
                    "spotify_playback recently_played accepts only one of after or before".into(),
                ));
            }
            let mut query = vec![(
                "limit".into(),
                spotify_limit(payload, 20, 1, 50).to_string(),
            )];
            for (target, keys) in [("after", &["after"][..]), ("before", &["before"][..])] {
                if let Some(value) = spotify_u64_arg(payload, keys) {
                    query.push((target.into(), value.to_string()));
                }
            }
            spotify_request(
                client,
                settings,
                "GET",
                "/me/player/recently-played",
                &query,
                None,
                "Spotify recently played",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_playback action: {other}"
        ))),
    }
}

pub(super) async fn spotify_devices_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let action = spotify_action(payload).unwrap_or_else(|| "list".into());
    match action.as_str() {
        "list" => {
            spotify_request(
                client,
                settings,
                "GET",
                "/me/player/devices",
                &[],
                None,
                "Spotify devices",
            )
            .await
        }
        "transfer" => {
            let device_id =
                required_string_arg(payload, &["device_id", "deviceId"], "spotify_devices")?;
            let play = spotify_bool_arg(payload, &["play"]).unwrap_or(false);
            spotify_request(
                client,
                settings,
                "PUT",
                "/me/player",
                &[],
                Some(json!({"device_ids": [device_id], "play": play})),
                "Spotify transfer playback",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_devices action: {other}"
        ))),
    }
}

pub(super) async fn spotify_queue_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let action = spotify_action(payload).unwrap_or_else(|| "get".into());
    match action.as_str() {
        "get" => {
            spotify_request(
                client,
                settings,
                "GET",
                "/me/player/queue",
                &[],
                None,
                "Spotify queue",
            )
            .await
        }
        "add" => {
            let uri = required_string_arg(payload, &["uri"], "spotify_queue")?;
            let mut query = spotify_query(payload, &[("device_id", &["device_id", "deviceId"])]);
            query.push(("uri".into(), normalize_spotify_uri(&uri, None)?));
            spotify_request(
                client,
                settings,
                "POST",
                "/me/player/queue",
                &query,
                None,
                "Spotify add queue item",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_queue action: {other}"
        ))),
    }
}

pub(super) async fn spotify_search_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let query_text = required_string_arg(payload, &["query", "q"], "spotify_search")?;
    let raw_types =
        spotify_string_list(payload, &["types", "type"]).unwrap_or_else(|| vec!["track".into()]);
    let types = raw_types
        .into_iter()
        .map(|value| value.to_lowercase())
        .filter(|value| {
            matches!(
                value.as_str(),
                "album" | "artist" | "playlist" | "track" | "show" | "episode" | "audiobook"
            )
        })
        .collect::<Vec<_>>();
    if types.is_empty() {
        return Err(AppError::BadRequest(
            "spotify_search types must include album, artist, playlist, track, show, episode, or audiobook".into(),
        ));
    }
    let mut query = vec![
        ("q".into(), query_text),
        ("type".into(), types.join(",")),
        (
            "limit".into(),
            spotify_limit(payload, 10, 1, 50).to_string(),
        ),
        (
            "offset".into(),
            spotify_u64_arg(payload, &["offset"])
                .unwrap_or(0)
                .to_string(),
        ),
    ];
    query.extend(spotify_query(
        payload,
        &[
            ("market", &["market"]),
            ("include_external", &["include_external", "includeExternal"]),
        ],
    ));
    spotify_request(
        client,
        settings,
        "GET",
        "/search",
        &query,
        None,
        "Spotify search",
    )
    .await
}

pub(super) async fn spotify_playlists_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let action = spotify_action(payload).unwrap_or_else(|| "list".into());
    match action.as_str() {
        "list" => {
            let query = vec![
                (
                    "limit".into(),
                    spotify_limit(payload, 20, 1, 50).to_string(),
                ),
                (
                    "offset".into(),
                    spotify_u64_arg(payload, &["offset"])
                        .unwrap_or(0)
                        .to_string(),
                ),
            ];
            spotify_request(
                client,
                settings,
                "GET",
                "/me/playlists",
                &query,
                None,
                "Spotify playlists",
            )
            .await
        }
        "get" => {
            let playlist_id = spotify_playlist_id(payload)?;
            spotify_request(
                client,
                settings,
                "GET",
                &format!("/playlists/{}", percent_encode_path_segment(&playlist_id)),
                &spotify_query(payload, &[("market", &["market"])]),
                None,
                "Spotify playlist",
            )
            .await
        }
        "create" => {
            let name = required_string_arg(payload, &["name"], "spotify_playlists")?;
            let body = strip_null_json_object(json!({
                "name": name,
                "public": spotify_bool_arg(payload, &["public"]).unwrap_or(false),
                "collaborative": spotify_bool_arg(payload, &["collaborative"]).unwrap_or(false),
                "description": string_arg(payload, &["description"]),
            }));
            spotify_request(
                client,
                settings,
                "POST",
                "/me/playlists",
                &[],
                Some(body),
                "Spotify create playlist",
            )
            .await
        }
        "add_items" => {
            let playlist_id = spotify_playlist_id(payload)?;
            let uris = spotify_string_list(payload, &["uris", "items"]).ok_or_else(|| {
                AppError::BadRequest("spotify_playlists add_items requires uris".into())
            })?;
            let body = strip_null_json_object(json!({
                "uris": normalize_spotify_uris(&uris, None)?,
                "position": spotify_u64_arg(payload, &["position"]),
            }));
            spotify_request(
                client,
                settings,
                "POST",
                &format!(
                    "/playlists/{}/items",
                    percent_encode_path_segment(&playlist_id)
                ),
                &[],
                Some(body),
                "Spotify add playlist items",
            )
            .await
        }
        "remove_items" => {
            let playlist_id = spotify_playlist_id(payload)?;
            let uris = spotify_string_list(payload, &["uris", "items"]).ok_or_else(|| {
                AppError::BadRequest("spotify_playlists remove_items requires uris".into())
            })?;
            let items = normalize_spotify_uris(&uris, None)?
                .into_iter()
                .map(|uri| json!({"uri": uri}))
                .collect::<Vec<_>>();
            let body = strip_null_json_object(json!({
                "items": items,
                "snapshot_id": string_arg(payload, &["snapshot_id", "snapshotId"]),
            }));
            spotify_request(
                client,
                settings,
                "DELETE",
                &format!(
                    "/playlists/{}/items",
                    percent_encode_path_segment(&playlist_id)
                ),
                &[],
                Some(body),
                "Spotify remove playlist items",
            )
            .await
        }
        "update_details" => {
            let playlist_id = spotify_playlist_id(payload)?;
            let body = strip_null_json_object(json!({
                "name": string_arg(payload, &["name"]),
                "public": spotify_bool_arg(payload, &["public"]),
                "collaborative": spotify_bool_arg(payload, &["collaborative"]),
                "description": string_arg(payload, &["description"]),
            }));
            spotify_request(
                client,
                settings,
                "PUT",
                &format!("/playlists/{}", percent_encode_path_segment(&playlist_id)),
                &[],
                Some(body),
                "Spotify update playlist",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_playlists action: {other}"
        ))),
    }
}

pub(super) async fn spotify_albums_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let action = spotify_action(payload).unwrap_or_else(|| "get".into());
    let album_id = required_string_arg(payload, &["album_id", "albumId", "id"], "spotify_albums")
        .and_then(|value| normalize_spotify_id(&value, Some("album")))?;
    match action.as_str() {
        "get" => {
            spotify_request(
                client,
                settings,
                "GET",
                &format!("/albums/{}", percent_encode_path_segment(&album_id)),
                &spotify_query(payload, &[("market", &["market"])]),
                None,
                "Spotify album",
            )
            .await
        }
        "tracks" => {
            let mut query = vec![
                (
                    "limit".into(),
                    spotify_limit(payload, 20, 1, 50).to_string(),
                ),
                (
                    "offset".into(),
                    spotify_u64_arg(payload, &["offset"])
                        .unwrap_or(0)
                        .to_string(),
                ),
            ];
            query.extend(spotify_query(payload, &[("market", &["market"])]));
            spotify_request(
                client,
                settings,
                "GET",
                &format!("/albums/{}/tracks", percent_encode_path_segment(&album_id)),
                &query,
                None,
                "Spotify album tracks",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_albums action: {other}"
        ))),
    }
}

pub(super) async fn spotify_library_tool(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    payload: &Value,
) -> AppResult<Value> {
    let kind = required_string_arg(payload, &["kind"], "spotify_library")?.to_lowercase();
    let item_type = match kind.as_str() {
        "tracks" => "track",
        "albums" => "album",
        _ => {
            return Err(AppError::BadRequest(
                "spotify_library kind must be tracks or albums".into(),
            ));
        }
    };
    let action = spotify_action(payload).unwrap_or_else(|| "list".into());
    let collection_path = if kind == "tracks" {
        "/me/tracks"
    } else {
        "/me/albums"
    };
    match action.as_str() {
        "list" => {
            let mut query = vec![
                (
                    "limit".into(),
                    spotify_limit(payload, 20, 1, 50).to_string(),
                ),
                (
                    "offset".into(),
                    spotify_u64_arg(payload, &["offset"])
                        .unwrap_or(0)
                        .to_string(),
                ),
            ];
            query.extend(spotify_query(payload, &[("market", &["market"])]));
            spotify_request(
                client,
                settings,
                "GET",
                collection_path,
                &query,
                None,
                "Spotify library list",
            )
            .await
        }
        "save" => {
            let items =
                spotify_string_list(payload, &["uris", "ids", "items"]).ok_or_else(|| {
                    AppError::BadRequest("spotify_library save requires uris, ids, or items".into())
                })?;
            let ids = normalize_spotify_ids(&items, Some(item_type))?;
            spotify_request(
                client,
                settings,
                "PUT",
                collection_path,
                &[("ids".into(), ids.join(","))],
                None,
                "Spotify save library items",
            )
            .await
        }
        "remove" => {
            let items =
                spotify_string_list(payload, &["ids", "items", "uris"]).ok_or_else(|| {
                    AppError::BadRequest(
                        "spotify_library remove requires ids, items, or uris".into(),
                    )
                })?;
            let ids = normalize_spotify_ids(&items, Some(item_type))?;
            spotify_request(
                client,
                settings,
                "DELETE",
                collection_path,
                &[("ids".into(), ids.join(","))],
                None,
                "Spotify remove library items",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported spotify_library action: {other}"
        ))),
    }
}

pub(super) fn spotify_settings(config: &Value) -> AppResult<SpotifySettings> {
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("SPOTIFY_API_BASE_URL").ok())
    .unwrap_or_else(|| "https://api.spotify.com/v1".into())
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Spotify apiBaseUrl: {error}")))?;
    let token_url = string_arg(config, &["tokenUrl", "token_url"])
        .or_else(|| std::env::var("SPOTIFY_TOKEN_URL").ok())
        .unwrap_or_else(|| "https://accounts.spotify.com/api/token".into());
    reqwest::Url::parse(&token_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Spotify tokenUrl: {error}")))?;
    let access_token = string_arg(config, &["accessToken", "access_token", "token"])
        .or_else(|| std::env::var("SPOTIFY_ACCESS_TOKEN").ok());
    let refresh_token = string_arg(config, &["refreshToken", "refresh_token"])
        .or_else(|| std::env::var("SPOTIFY_REFRESH_TOKEN").ok());
    let client_id = string_arg(config, &["clientId", "client_id"])
        .or_else(|| std::env::var("SPOTIFY_CLIENT_ID").ok());
    let client_secret = string_arg(config, &["clientSecret", "client_secret"])
        .or_else(|| std::env::var("SPOTIFY_CLIENT_SECRET").ok());
    if access_token.is_none()
        && (refresh_token.is_none() || client_id.is_none() || client_secret.is_none())
    {
        return Err(AppError::BadRequest(
            "Spotify tools require settings.spotify.accessToken/SPOTIFY_ACCESS_TOKEN, or refreshToken/clientId/clientSecret".into(),
        ));
    }
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(SpotifySettings {
        api_base_url,
        access_token,
        refresh_token,
        client_id,
        client_secret,
        token_url,
        timeout_seconds,
    })
}

pub(super) fn spotify_client(settings: &SpotifySettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Spotify client: {error}")))
}

pub(super) async fn spotify_access_token(
    client: &reqwest::Client,
    settings: &SpotifySettings,
) -> AppResult<String> {
    if let Some(token) = settings.access_token.as_deref() {
        return Ok(token.to_string());
    }
    let refresh_token = settings.refresh_token.as_deref().ok_or_else(|| {
        AppError::BadRequest("Spotify refreshToken is required to refresh access token".into())
    })?;
    let client_id = settings.client_id.as_deref().ok_or_else(|| {
        AppError::BadRequest("Spotify clientId is required to refresh access token".into())
    })?;
    let client_secret = settings.client_secret.as_deref().ok_or_else(|| {
        AppError::BadRequest("Spotify clientSecret is required to refresh access token".into())
    })?;
    let response = client
        .post(&settings.token_url)
        .basic_auth(client_id, Some(client_secret))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("Spotify token refresh failed: {error}")))?;
    let value = spotify_response_json(response, "Spotify token refresh").await?;
    value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::BadRequest("Spotify token response missing access_token".into()))
}

pub(super) async fn spotify_request(
    client: &reqwest::Client,
    settings: &SpotifySettings,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: Option<Value>,
    label: &str,
) -> AppResult<Value> {
    let token = spotify_access_token(client, settings).await?;
    let url = spotify_url(settings, path, query)?;
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| {
        AppError::BadRequest(format!("unsupported Spotify HTTP method {method}: {error}"))
    })?;
    let mut request = client
        .request(method, url)
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&strip_null_json_object(body));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    spotify_response_json(response, label).await
}

pub(super) fn spotify_url(
    settings: &SpotifySettings,
    path: &str,
    query: &[(String, String)],
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&settings.api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Spotify apiBaseUrl: {error}")))?;
    let mut full_path = url.path().trim_end_matches('/').to_string();
    full_path.push('/');
    full_path.push_str(path.trim_start_matches('/'));
    url.set_path(&full_path);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            if !value.trim().is_empty() {
                pairs.append_pair(key, value);
            }
        }
    }
    Ok(url)
}

pub(super) async fn spotify_response_json(
    response: reqwest::Response,
    label: &str,
) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if status.as_u16() == 204 || text.trim().is_empty() {
        return Ok(json!({"success": true, "status_code": status.as_u16(), "empty": true}));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if !status.is_success() {
        let message = value
            .get("error")
            .and_then(|error| error.get("message").or_else(|| error.get("reason")))
            .and_then(Value::as_str)
            .or_else(|| value.get("error").and_then(Value::as_str))
            .unwrap_or_else(|| text.trim());
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(super) fn spotify_action(payload: &Value) -> Option<String> {
    string_arg(payload, &["action"]).map(|value| value.to_lowercase())
}

pub(super) fn spotify_playlist_id(payload: &Value) -> AppResult<String> {
    let value = required_string_arg(
        payload,
        &["playlist_id", "playlistId", "id"],
        "spotify_playlists",
    )?;
    normalize_spotify_id(&value, Some("playlist"))
}

pub(super) fn spotify_query(
    payload: &Value,
    mappings: &[(&str, &[&str])],
) -> Vec<(String, String)> {
    mappings
        .iter()
        .filter_map(|(target, keys)| {
            string_arg(payload, keys).map(|value| ((*target).into(), value))
        })
        .collect()
}

pub(super) fn spotify_u64_arg(payload: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        payload.get(*key).and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str()?.trim().parse().ok())
        })
    })
}

pub(super) fn spotify_bool_arg(payload: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        let value = payload.get(*key)?;
        if let Some(value) = value.as_bool() {
            return Some(value);
        }
        let text = value.as_str()?.trim().to_lowercase();
        match text.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

pub(super) fn spotify_limit(payload: &Value, default: u64, minimum: u64, maximum: u64) -> u64 {
    spotify_u64_arg(payload, &["limit"])
        .unwrap_or(default)
        .clamp(minimum, maximum)
}

pub(super) fn spotify_string_list(payload: &Value, keys: &[&str]) -> Option<Vec<String>> {
    for key in keys {
        let Some(value) = payload.get(*key) else {
            continue;
        };
        let items = if let Some(array) = value.as_array() {
            array
                .iter()
                .filter_map(|item| item.as_str().or_else(|| item.as_i64().map(|_| "")))
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        } else if let Some(text) = value.as_str() {
            text.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if !items.is_empty() {
            return Some(items);
        }
    }
    None
}

pub(super) fn normalize_spotify_id(value: &str, expected_type: Option<&str>) -> AppResult<String> {
    let cleaned = value.trim();
    if cleaned.is_empty() {
        return Err(AppError::BadRequest(
            "Spotify id/uri/url is required".into(),
        ));
    }
    if let Some(rest) = cleaned.strip_prefix("spotify:") {
        let parts = rest.split(':').collect::<Vec<_>>();
        if parts.len() >= 2 {
            if let Some(expected) = expected_type {
                if parts[0] != expected {
                    return Err(AppError::BadRequest(format!(
                        "expected Spotify {expected}, got {}",
                        parts[0]
                    )));
                }
            }
            return Ok(parts[1].to_string());
        }
    }
    if cleaned.contains("open.spotify.com") {
        let url = reqwest::Url::parse(cleaned)
            .map_err(|error| AppError::BadRequest(format!("invalid Spotify URL: {error}")))?;
        let parts = url
            .path_segments()
            .map(|segments| segments.collect::<Vec<_>>())
            .unwrap_or_default();
        if parts.len() >= 2 {
            if let Some(expected) = expected_type {
                if parts[0] != expected {
                    return Err(AppError::BadRequest(format!(
                        "expected Spotify {expected}, got {}",
                        parts[0]
                    )));
                }
            }
            return Ok(parts[1].to_string());
        }
    }
    Ok(cleaned.to_string())
}

pub(super) fn normalize_spotify_uri(value: &str, expected_type: Option<&str>) -> AppResult<String> {
    let cleaned = value.trim();
    if cleaned.is_empty() {
        return Err(AppError::BadRequest(
            "Spotify URI/url/id is required".into(),
        ));
    }
    if cleaned.starts_with("spotify:") {
        if let Some(expected) = expected_type {
            let parts = cleaned.split(':').collect::<Vec<_>>();
            if parts.len() >= 3 && parts[1] != expected {
                return Err(AppError::BadRequest(format!(
                    "expected Spotify {expected}, got {}",
                    parts[1]
                )));
            }
        }
        return Ok(cleaned.to_string());
    }
    let item_id = normalize_spotify_id(cleaned, expected_type)?;
    if let Some(expected) = expected_type {
        Ok(format!("spotify:{expected}:{item_id}"))
    } else {
        Ok(item_id)
    }
}

pub(super) fn normalize_spotify_uris(
    values: &[String],
    expected_type: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut output = Vec::new();
    for value in values {
        let uri = normalize_spotify_uri(value, expected_type)?;
        if !output.contains(&uri) {
            output.push(uri);
        }
    }
    if output.is_empty() {
        return Err(AppError::BadRequest(
            "at least one Spotify item is required".into(),
        ));
    }
    Ok(output)
}

pub(super) fn normalize_spotify_ids(
    values: &[String],
    expected_type: Option<&str>,
) -> AppResult<Vec<String>> {
    let mut output = Vec::new();
    for value in values {
        let id = normalize_spotify_id(value, expected_type)?;
        if !output.contains(&id) {
            output.push(id);
        }
    }
    if output.is_empty() {
        return Err(AppError::BadRequest(
            "at least one Spotify item is required".into(),
        ));
    }
    Ok(output)
}

pub(super) fn strip_null_json_object(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter_map(|(key, value)| {
                    if value.is_null() {
                        None
                    } else {
                        Some((key, strip_null_json_object(value)))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(strip_null_json_object).collect())
        }
        other => other,
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiscordSettings {
    pub(super) api_base_url: String,
    pub(super) bot_token: Option<String>,
    pub(super) gateway_url: Option<String>,
    pub(super) timeout_seconds: u64,
    pub(super) config: Value,
}

pub(super) async fn discord_tool(
    store: &AppStore,
    tool_name: &str,
    payload: &Value,
) -> AppResult<String> {
    let settings = discord_settings(&store.config()?.discord)?;
    let action = discord_action(payload)
        .ok_or_else(|| AppError::BadRequest(format!("{tool_name} requires payload.action")))?;
    ensure_discord_action_allowed(tool_name, &action)?;
    let result = if settings.bot_token.is_some() {
        let client = discord_client(&settings)?;
        discord_rest_action(&client, &settings, tool_name, &action, payload).await?
    } else {
        discord_bridge_request(&settings, tool_name, payload).await?
    };
    Ok(serde_json::to_string_pretty(&json!({
        "tool": tool_name,
        "success": result.get("success").and_then(Value::as_bool).unwrap_or(true),
        "result": result,
    }))?)
}

pub(super) fn discord_settings(config: &Value) -> AppResult<DiscordSettings> {
    let api_base_url = string_arg(
        config,
        &["apiBaseUrl", "api_base_url", "baseUrl", "base_url"],
    )
    .or_else(|| std::env::var("DISCORD_API_BASE_URL").ok())
    .unwrap_or_else(|| "https://discord.com/api/v10".into())
    .trim_end_matches('/')
    .to_string();
    reqwest::Url::parse(&api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Discord apiBaseUrl: {error}")))?;
    let bot_token = string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("DISCORD_BOT_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let gateway_url = string_arg(
        config,
        &["gatewayUrl", "gateway_url", "bridgeUrl", "bridge_url"],
    )
    .or_else(|| std::env::var("DISCORD_GATEWAY_URL").ok())
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty());
    if let Some(url) = gateway_url.as_deref() {
        reqwest::Url::parse(url).map_err(|error| {
            AppError::BadRequest(format!("invalid Discord gatewayUrl: {error}"))
        })?;
    }
    if bot_token.is_none() && gateway_url.is_none() {
        return Err(AppError::BadRequest(
            "Discord tools require settings.discord.botToken/DISCORD_BOT_TOKEN or settings.discord.gatewayUrl/DISCORD_GATEWAY_URL".into(),
        ));
    }
    let timeout_seconds = config
        .get("timeoutSeconds")
        .or_else(|| config.get("timeout_seconds"))
        .and_then(Value::as_u64)
        .unwrap_or(15)
        .clamp(1, 120);
    Ok(DiscordSettings {
        api_base_url,
        bot_token,
        gateway_url,
        timeout_seconds,
        config: config.clone(),
    })
}

pub(super) fn discord_client(settings: &DiscordSettings) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| AppError::BadRequest(format!("failed to build Discord client: {error}")))
}

pub(super) async fn discord_rest_action(
    client: &reqwest::Client,
    settings: &DiscordSettings,
    tool_name: &str,
    action: &str,
    payload: &Value,
) -> AppResult<Value> {
    match action {
        "list_guilds" => {
            let guilds = discord_request(
                client,
                settings,
                "GET",
                "/users/@me/guilds",
                &[],
                None,
                "Discord list guilds",
            )
            .await?;
            Ok(json!({"guilds": guilds, "count": guilds.as_array().map(Vec::len).unwrap_or(0)}))
        }
        "server_info" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            discord_request(
                client,
                settings,
                "GET",
                &format!("/guilds/{}", percent_encode_path_segment(&guild_id)),
                &[("with_counts".into(), "true".into())],
                None,
                "Discord server info",
            )
            .await
        }
        "list_channels" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            let channels = discord_request(
                client,
                settings,
                "GET",
                &format!(
                    "/guilds/{}/channels",
                    percent_encode_path_segment(&guild_id)
                ),
                &[],
                None,
                "Discord list channels",
            )
            .await?;
            Ok(
                json!({"channels": channels, "count": channels.as_array().map(Vec::len).unwrap_or(0)}),
            )
        }
        "channel_info" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            discord_request(
                client,
                settings,
                "GET",
                &format!("/channels/{}", percent_encode_path_segment(&channel_id)),
                &[],
                None,
                "Discord channel info",
            )
            .await
        }
        "list_roles" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            let roles = discord_request(
                client,
                settings,
                "GET",
                &format!("/guilds/{}/roles", percent_encode_path_segment(&guild_id)),
                &[],
                None,
                "Discord list roles",
            )
            .await?;
            Ok(json!({"roles": roles, "count": roles.as_array().map(Vec::len).unwrap_or(0)}))
        }
        "member_info" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            let user_id = discord_required_id(payload, &["user_id", "userId"], "user_id")?;
            discord_request(
                client,
                settings,
                "GET",
                &format!(
                    "/guilds/{}/members/{}",
                    percent_encode_path_segment(&guild_id),
                    percent_encode_path_segment(&user_id)
                ),
                &[],
                None,
                "Discord member info",
            )
            .await
        }
        "search_members" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            let query = required_string_arg(payload, &["query"], tool_name)?;
            let limit = spotify_limit(payload, 20, 1, 100);
            let members = discord_request(
                client,
                settings,
                "GET",
                &format!(
                    "/guilds/{}/members/search",
                    percent_encode_path_segment(&guild_id)
                ),
                &[("query".into(), query), ("limit".into(), limit.to_string())],
                None,
                "Discord search members",
            )
            .await?;
            Ok(json!({"members": members, "count": members.as_array().map(Vec::len).unwrap_or(0)}))
        }
        "fetch_messages" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let mut query = vec![(
                "limit".into(),
                spotify_limit(payload, 50, 1, 100).to_string(),
            )];
            query.extend(discord_optional_query(
                payload,
                &[("before", &["before"]), ("after", &["after"])],
            ));
            let messages = discord_request(
                client,
                settings,
                "GET",
                &format!(
                    "/channels/{}/messages",
                    percent_encode_path_segment(&channel_id)
                ),
                &query,
                None,
                "Discord fetch messages",
            )
            .await?;
            Ok(
                json!({"messages": messages, "count": messages.as_array().map(Vec::len).unwrap_or(0)}),
            )
        }
        "send_message" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let content =
                required_string_arg(payload, &["content", "message", "text", "body"], "discord")?;
            if content.chars().count() > 2_000 {
                return Err(AppError::BadRequest(
                    "Discord message content cannot exceed 2000 characters".into(),
                ));
            }
            let mut body = json!({
                "content": content,
                "tts": payload.get("tts").and_then(Value::as_bool).unwrap_or(false),
            });
            if let Some(message_id) =
                string_arg(payload, &["message_id", "messageId", "reply_to", "replyTo"])
            {
                discord_required_id(
                    &json!({"message_id": message_id}),
                    &["message_id"],
                    "message_id",
                )?;
                body["message_reference"] = json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "fail_if_not_exists": payload
                        .get("fail_if_not_exists")
                        .or_else(|| payload.get("failIfNotExists"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            let path = format!(
                "/channels/{}/messages",
                percent_encode_path_segment(&channel_id)
            );
            let media_files = discord_media_file_paths(payload)?;
            if media_files.is_empty() {
                discord_request(
                    client,
                    settings,
                    "POST",
                    &path,
                    &[],
                    Some(body),
                    "Discord send message",
                )
                .await
            } else {
                discord_send_message_multipart(
                    client,
                    settings,
                    &path,
                    body,
                    &media_files,
                    "Discord send message",
                )
                .await
            }
        }
        "list_pins" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let messages = discord_request(
                client,
                settings,
                "GET",
                &format!(
                    "/channels/{}/pins",
                    percent_encode_path_segment(&channel_id)
                ),
                &[],
                None,
                "Discord list pins",
            )
            .await?;
            Ok(
                json!({"messages": messages, "count": messages.as_array().map(Vec::len).unwrap_or(0)}),
            )
        }
        "pin_message" | "unpin_message" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let message_id =
                discord_required_id(payload, &["message_id", "messageId"], "message_id")?;
            let method = if action == "pin_message" {
                "PUT"
            } else {
                "DELETE"
            };
            discord_request(
                client,
                settings,
                method,
                &format!(
                    "/channels/{}/pins/{}",
                    percent_encode_path_segment(&channel_id),
                    percent_encode_path_segment(&message_id)
                ),
                &[],
                None,
                "Discord pin state",
            )
            .await
        }
        "delete_message" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let message_id =
                discord_required_id(payload, &["message_id", "messageId"], "message_id")?;
            discord_request(
                client,
                settings,
                "DELETE",
                &format!(
                    "/channels/{}/messages/{}",
                    percent_encode_path_segment(&channel_id),
                    percent_encode_path_segment(&message_id)
                ),
                &[],
                None,
                "Discord delete message",
            )
            .await
        }
        "create_thread" => {
            let channel_id =
                discord_required_id(payload, &["channel_id", "channelId"], "channel_id")?;
            let name = required_string_arg(payload, &["name"], tool_name)?;
            let archive =
                spotify_u64_arg(payload, &["auto_archive_duration", "autoArchiveDuration"])
                    .unwrap_or(1440);
            let (path, body) =
                if let Some(message_id) = string_arg(payload, &["message_id", "messageId"]) {
                    (
                        format!(
                            "/channels/{}/messages/{}/threads",
                            percent_encode_path_segment(&channel_id),
                            percent_encode_path_segment(&message_id)
                        ),
                        json!({"name": name, "auto_archive_duration": archive}),
                    )
                } else {
                    (
                        format!(
                            "/channels/{}/threads",
                            percent_encode_path_segment(&channel_id)
                        ),
                        json!({"name": name, "auto_archive_duration": archive, "type": 11}),
                    )
                };
            discord_request(
                client,
                settings,
                "POST",
                &path,
                &[],
                Some(body),
                "Discord create thread",
            )
            .await
        }
        "add_role" | "remove_role" => {
            let guild_id = discord_required_id(payload, &["guild_id", "guildId"], "guild_id")?;
            let user_id = discord_required_id(payload, &["user_id", "userId"], "user_id")?;
            let role_id = discord_required_id(payload, &["role_id", "roleId"], "role_id")?;
            let method = if action == "add_role" {
                "PUT"
            } else {
                "DELETE"
            };
            discord_request(
                client,
                settings,
                method,
                &format!(
                    "/guilds/{}/members/{}/roles/{}",
                    percent_encode_path_segment(&guild_id),
                    percent_encode_path_segment(&user_id),
                    percent_encode_path_segment(&role_id)
                ),
                &[],
                None,
                "Discord role update",
            )
            .await
        }
        other => Err(AppError::BadRequest(format!(
            "unsupported {tool_name} action: {other}"
        ))),
    }
}

pub(super) async fn discord_request(
    client: &reqwest::Client,
    settings: &DiscordSettings,
    method: &str,
    path: &str,
    query: &[(String, String)],
    body: Option<Value>,
    label: &str,
) -> AppResult<Value> {
    let token = settings.bot_token.as_deref().ok_or_else(|| {
        AppError::BadRequest("Discord bot token is required for REST requests".into())
    })?;
    let url = discord_url(settings, path, query)?;
    let method = reqwest::Method::from_bytes(method.as_bytes()).map_err(|error| {
        AppError::BadRequest(format!("unsupported Discord HTTP method {method}: {error}"))
    })?;
    let mut request = client
        .request(method, url)
        .header("Authorization", format!("Bot {token}"))
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request.json(&strip_null_json_object(body));
    }
    let response = request
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    discord_response_json(response, label).await
}

async fn discord_send_message_multipart(
    client: &reqwest::Client,
    settings: &DiscordSettings,
    path: &str,
    body: Value,
    media_files: &[String],
    label: &str,
) -> AppResult<Value> {
    let token = settings.bot_token.as_deref().ok_or_else(|| {
        AppError::BadRequest("Discord bot token is required for REST requests".into())
    })?;
    let url = discord_url(settings, path, &[])?;
    let payload_json = serde_json::to_string(&strip_null_json_object(body)).map_err(|error| {
        AppError::BadRequest(format!("failed to encode Discord payload: {error}"))
    })?;
    let mut form = reqwest::multipart::Form::new().text("payload_json", payload_json);
    for (index, file_path) in media_files.iter().enumerate() {
        let bytes = fs::read(file_path).map_err(|error| {
            AppError::BadRequest(format!(
                "failed to read Discord media file {file_path}: {error}"
            ))
        })?;
        let file_name = Path::new(file_path)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("attachment");
        let part = reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string());
        form = form.part(format!("files[{index}]"), part);
    }
    let response = client
        .post(url)
        .header("Authorization", format!("Bot {token}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .multipart(form)
        .send()
        .await
        .map_err(|error| AppError::BadRequest(format!("{label} failed: {error}")))?;
    discord_response_json(response, label).await
}

fn discord_media_file_paths(payload: &Value) -> AppResult<Vec<String>> {
    let Some(files) = json_array_arg(payload, &["media_files", "mediaFiles"]) else {
        return Ok(Vec::new());
    };
    files
        .into_iter()
        .map(|file| {
            if let Some(path) = file.as_str() {
                return Ok(path.trim().to_string());
            }
            string_arg(&file, &["path", "file", "file_path", "filePath"]).ok_or_else(|| {
                AppError::BadRequest(
                    "Discord media_files entries must be strings or objects with path".into(),
                )
            })
        })
        .filter(|result| result.as_ref().map(|path| !path.is_empty()).unwrap_or(true))
        .collect()
}

fn json_array_arg(payload: &Value, keys: &[&str]) -> Option<Vec<Value>> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_array)
            .map(|values| values.to_vec())
    })
}

pub(super) fn discord_url(
    settings: &DiscordSettings,
    path: &str,
    query: &[(String, String)],
) -> AppResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(&settings.api_base_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Discord apiBaseUrl: {error}")))?;
    let mut full_path = url.path().trim_end_matches('/').to_string();
    full_path.push('/');
    full_path.push_str(path.trim_start_matches('/'));
    url.set_path(&full_path);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            if !value.trim().is_empty() {
                pairs.append_pair(key, value);
            }
        }
    }
    Ok(url)
}

pub(super) async fn discord_response_json(
    response: reqwest::Response,
    label: &str,
) -> AppResult<Value> {
    let status = response.status();
    let text = response.text().await.map_err(|error| {
        AppError::BadRequest(format!("failed to read {label} response: {error}"))
    })?;
    if status.as_u16() == 204 || text.trim().is_empty() {
        return Ok(json!({"success": true, "status_code": status.as_u16(), "empty": true}));
    }
    let value = serde_json::from_str::<Value>(&text)
        .map_err(|error| AppError::BadRequest(format!("invalid {label} JSON: {error}")))?;
    if !status.is_success() {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| text.trim());
        return Err(AppError::BadRequest(format!(
            "{label} returned HTTP {}: {}",
            status.as_u16(),
            truncate_output(message, 2000)
        )));
    }
    Ok(value)
}

pub(crate) async fn start_discord_adapter(store: &AppStore, app: AppHandle) -> AppResult<Value> {
    let config = store.config()?.discord;
    let settings = discord_settings(&config)?;
    if settings.bot_token.as_deref().unwrap_or_default().is_empty() {
        return Err(AppError::BadRequest(
            "Discord runtime requires settings.discord.botToken or DISCORD_BOT_TOKEN".into(),
        ));
    }
    let client = discord_client(&settings)?;
    let me = discord_request(
        &client,
        &settings,
        "GET",
        "/users/@me",
        &[],
        None,
        "Discord current user",
    )
    .await?;
    let bot_user_id = me
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if bot_user_id.is_empty() {
        return Err(AppError::BadRequest(
            "Discord /users/@me returned no bot user id".into(),
        ));
    }
    let state = store.update_discord_adapter_state(Some("starting"), None, None, 0, 0)?;
    emit_platform_adapter_event(&app, "starting", "discord", &state);
    let store_for_task = store.clone();
    let task = tokio::spawn(async move {
        discord_adapter_loop(app, store_for_task, settings, bot_user_id).await;
    });
    store.register_discord_adapter_task(task.abort_handle())?;
    store.discord_adapter_state()
}

pub(crate) fn stop_discord_adapter(store: &AppStore) -> AppResult<Value> {
    store.stop_discord_adapter_task()
}

pub(super) fn discord_adapter_autostart_enabled(config: &Value) -> bool {
    let enabled = config
        .get("enabled")
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DISCORD_ENABLED"))
        .unwrap_or(false);
    let autostart = config
        .get("autoStart")
        .or_else(|| config.get("auto_start"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DISCORD_AUTO_START"))
        .unwrap_or(true);
    enabled && autostart && discord_runtime_configured(config)
}

fn discord_runtime_configured(config: &Value) -> bool {
    string_arg(config, &["botToken", "bot_token", "token"])
        .or_else(|| std::env::var("DISCORD_BOT_TOKEN").ok())
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

async fn discord_adapter_loop(
    app: AppHandle,
    store: AppStore,
    settings: DiscordSettings,
    bot_user_id: String,
) {
    loop {
        match discord_adapter_connect_once(&app, &store, &settings, &bot_user_id).await {
            Ok(()) => {
                if let Ok(state) =
                    store.update_discord_adapter_state(Some("reconnecting"), None, None, 0, 0)
                {
                    emit_platform_adapter_event(&app, "reconnecting", "discord", &state);
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(error) => {
                let message = error.to_string();
                let auth_error = message.contains("401") || message.contains("403");
                if let Ok(state) = store.update_discord_adapter_state(
                    Some(if auth_error {
                        "stopped"
                    } else {
                        "reconnecting"
                    }),
                    None,
                    Some(message),
                    0,
                    0,
                ) {
                    emit_platform_adapter_event(
                        &app,
                        if auth_error {
                            "auth_failed"
                        } else {
                            "reconnecting"
                        },
                        "discord",
                        &state,
                    );
                }
                if auth_error {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn discord_adapter_connect_once(
    app: &AppHandle,
    store: &AppStore,
    settings: &DiscordSettings,
    bot_user_id: &str,
) -> AppResult<()> {
    let client = discord_client(settings)?;
    let gateway_url = discord_gateway_websocket_url(&client, settings).await?;
    let (ws, _) = connect_async(&gateway_url).await.map_err(|error| {
        AppError::BadRequest(format!("Discord Gateway connect failed: {error}"))
    })?;
    let (mut writer, mut reader) = ws.split();
    let mut sequence: Option<i64> = None;
    let heartbeat_ms = loop {
        let message = reader
            .next()
            .await
            .ok_or_else(|| AppError::BadRequest("Discord Gateway closed before hello".into()))?
            .map_err(|error| {
                AppError::BadRequest(format!("Discord Gateway read failed: {error}"))
            })?;
        let text = discord_ws_message_text(message)?;
        let envelope = serde_json::from_str::<Value>(&text).map_err(|error| {
            AppError::BadRequest(format!("invalid Discord Gateway JSON: {error}"))
        })?;
        if let Some(seq) = envelope.get("s").and_then(Value::as_i64) {
            sequence = Some(seq);
        }
        if envelope.get("op").and_then(Value::as_i64) == Some(10) {
            break envelope
                .pointer("/d/heartbeat_interval")
                .and_then(Value::as_u64)
                .unwrap_or(45_000);
        }
    };
    writer
        .send(WsMessage::Text(
            discord_identify_payload(settings).to_string().into(),
        ))
        .await
        .map_err(|error| AppError::BadRequest(format!("Discord identify send failed: {error}")))?;
    let state = store.update_discord_adapter_state(
        Some("running"),
        Some(json!({
            "type": "connected",
            "gatewayUrl": gateway_url,
            "heartbeatMs": heartbeat_ms,
        })),
        None,
        0,
        0,
    )?;
    emit_platform_adapter_event(app, "connected", "discord", &state);
    let mut heartbeat = tokio::time::interval(Duration::from_millis(heartbeat_ms.max(1)));
    heartbeat.tick().await;

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                writer
                    .send(WsMessage::Text(json!({"op": 1, "d": sequence}).to_string().into()))
                    .await
                    .map_err(|error| AppError::BadRequest(format!("Discord heartbeat send failed: {error}")))?;
            }
            message = reader.next() => {
                let message = message
                    .ok_or_else(|| AppError::BadRequest("Discord Gateway closed".into()))?
                    .map_err(|error| AppError::BadRequest(format!("Discord Gateway read failed: {error}")))?;
                let text = discord_ws_message_text(message)?;
                let Ok(envelope) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(seq) = envelope.get("s").and_then(Value::as_i64) {
                    sequence = Some(seq);
                }
                match envelope.get("op").and_then(Value::as_i64).unwrap_or(-1) {
                    0 => {
                        let event_type = envelope.get("t").and_then(Value::as_str).unwrap_or_default();
                        if event_type != "MESSAGE_CREATE" {
                            continue;
                        }
                        let config = store.config()?.discord;
                        let Some(inbound) = discord_inbound_event_from_gateway(&envelope, &config, bot_user_id) else {
                            continue;
                        };
                        let inbound_fallback = inbound.clone();
                        let inbound = discord_enrich_inbound_files(store, settings, inbound)
                            .await
                            .unwrap_or_else(|error| {
                                let mut fallback = inbound_fallback;
                                fallback["fileDownloadError"] = json!(error.to_string());
                                fallback["file_download_error"] = json!(error.to_string());
                                fallback
                            });
                        let prompt = discord_inbound_prompt(&inbound);
                        let Some(prompt) =
                            apply_pre_gateway_dispatch_hooks(store, "discord", &inbound, prompt).await
                        else {
                            let state = store.update_discord_adapter_state(
                                Some("running"),
                                Some({
                                    let mut event = inbound;
                                    event["sequence"] = json!(sequence);
                                    event
                                }),
                                None,
                                1,
                                0,
                            )?;
                            emit_platform_adapter_event(app, "inbound_ignored", "discord", &state);
                            continue;
                        };
                        let conversation_id = discord_inbound_conversation_id(store, &config)?;
                        let persona_id = discord_inbound_persona_id(store, &config)?;
                        let state = store.update_discord_adapter_state(
                            Some("running"),
                            Some({
                                let mut event = inbound;
                                event["sequence"] = json!(sequence);
                                event
                            }),
                            None,
                            1,
                            1,
                        )?;
                        emit_platform_adapter_event(app, "inbound_triggered", "discord", &state);
                        spawn_background_chat_turn_for_job(app.clone(), conversation_id, persona_id, prompt, None);
                    }
                    7 => return Ok(()),
                    9 => return Err(AppError::BadRequest("Discord Gateway invalid session".into())),
                    10 | 11 => {}
                    _ => {}
                }
            }
        }
    }
}

fn discord_ws_message_text(message: WsMessage) -> AppResult<String> {
    match message {
        WsMessage::Text(text) => Ok(text.to_string()),
        WsMessage::Binary(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
        WsMessage::Close(frame) => Err(AppError::BadRequest(format!(
            "Discord Gateway closed: {:?}",
            frame
        ))),
        _ => Ok(String::new()),
    }
}

async fn discord_gateway_websocket_url(
    client: &reqwest::Client,
    settings: &DiscordSettings,
) -> AppResult<String> {
    let raw = string_arg(
        &settings.config,
        &[
            "gatewayWebsocketUrl",
            "gateway_websocket_url",
            "websocketUrl",
            "websocket_url",
        ],
    )
    .or_else(|| std::env::var("DISCORD_GATEWAY_WEBSOCKET_URL").ok());
    let raw = if let Some(raw) = raw {
        raw
    } else {
        discord_request(
            client,
            settings,
            "GET",
            "/gateway/bot",
            &[],
            None,
            "Discord gateway",
        )
        .await?
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("wss://gateway.discord.gg")
        .to_string()
    };
    let mut url = reqwest::Url::parse(raw.trim())
        .map_err(|error| AppError::BadRequest(format!("invalid Discord Gateway URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("v", "10")
        .append_pair("encoding", "json");
    Ok(url.to_string())
}

fn discord_identify_payload(settings: &DiscordSettings) -> Value {
    let intents = settings
        .config
        .get("gatewayIntents")
        .or_else(|| settings.config.get("gateway_intents"))
        .and_then(Value::as_u64)
        .or_else(|| {
            std::env::var("DISCORD_GATEWAY_INTENTS")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
        })
        .unwrap_or(37_376);
    json!({
        "op": 2,
        "d": {
            "token": settings.bot_token.as_deref().unwrap_or_default(),
            "intents": intents,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "SynthChat",
                "device": "SynthChat",
            },
        }
    })
}

fn discord_inbound_event_from_gateway(
    envelope: &Value,
    config: &Value,
    bot_user_id: &str,
) -> Option<Value> {
    let event = envelope.get("d")?;
    let author = event.get("author").unwrap_or(&Value::Null);
    let author_id = author.get("id").and_then(Value::as_str).unwrap_or_default();
    if author_id == bot_user_id || author.get("bot").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let channel_id = event
        .get("channel_id")
        .or_else(|| event.get("channelId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if channel_id.is_empty()
        || !matrix_allowed(
            config,
            &["allowedChannels", "allowed_channels"],
            "DISCORD_ALLOWED_CHANNELS",
            channel_id,
        )
    {
        return None;
    }
    let guild_id = event
        .get("guild_id")
        .or_else(|| event.get("guildId"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !guild_id.is_empty()
        && !matrix_allowed(
            config,
            &["allowedGuilds", "allowed_guilds"],
            "DISCORD_ALLOWED_GUILDS",
            guild_id,
        )
    {
        return None;
    }
    let text = event
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let files = discord_files_from_gateway_event(event);
    if text.is_empty() && files.is_empty() {
        return None;
    }
    let is_dm = guild_id.is_empty();
    let free_channels = telegram_string_set(
        config,
        &["freeResponseChannels", "free_response_channels"],
        "DISCORD_FREE_RESPONSE_CHANNELS",
    );
    let free_channel = free_channels.contains("*") || free_channels.contains(channel_id);
    let require_mention = config
        .get("requireMention")
        .or_else(|| config.get("require_mention"))
        .and_then(Value::as_bool)
        .or_else(|| matrix_env_bool("DISCORD_REQUIRE_MENTION"))
        .unwrap_or(true);
    let mentioned = discord_event_mentions_bot(event, bot_user_id);
    let command = text.trim_start().starts_with('/');
    if !is_dm && require_mention && !free_channel && !mentioned && !command {
        return None;
    }
    let message_id = event.get("id").and_then(Value::as_str).unwrap_or_default();
    let mention_a = format!("<@{bot_user_id}>");
    let mention_b = format!("<@!{bot_user_id}>");
    let cleaned = replace_ascii_case_insensitive(
        &replace_ascii_case_insensitive(&text, &mention_a, ""),
        &mention_b,
        "",
    )
    .trim()
    .to_string();
    let mut inbound = json!({
        "platform": "discord",
        "eventId": message_id,
        "event_id": message_id,
        "messageId": message_id,
        "message_id": message_id,
        "text": cleaned,
        "messageType": if command { "command" } else { "text" },
        "message_type": if command { "command" } else { "text" },
        "source": {
            "platform": "discord",
            "channelId": channel_id,
            "channel_id": channel_id,
            "chatId": channel_id,
            "chat_id": channel_id,
            "guildId": guild_id,
            "guild_id": guild_id,
            "userId": author_id,
            "user_id": author_id,
            "userName": author.get("username").and_then(Value::as_str).unwrap_or_default(),
            "user_name": author.get("username").and_then(Value::as_str).unwrap_or_default(),
            "chatType": if is_dm { "dm" } else { "channel" },
            "chat_type": if is_dm { "dm" } else { "channel" },
        },
        "raw": event,
    });
    if !files.is_empty() {
        inbound["files"] = json!(files);
    }
    Some(inbound)
}

fn discord_event_mentions_bot(event: &Value, bot_user_id: &str) -> bool {
    if event
        .get("content")
        .and_then(Value::as_str)
        .map(|text| {
            text.contains(&format!("<@{bot_user_id}>"))
                || text.contains(&format!("<@!{bot_user_id}>"))
        })
        .unwrap_or(false)
    {
        return true;
    }
    event
        .get("mentions")
        .and_then(Value::as_array)
        .map(|mentions| {
            mentions.iter().any(|mention| {
                mention
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| id == bot_user_id)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn discord_files_from_gateway_event(event: &Value) -> Vec<Value> {
    event
        .get("attachments")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| {
            let id = file
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let name = file
                .get("filename")
                .or_else(|| file.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("attachment");
            let mime = file
                .get("content_type")
                .or_else(|| file.get("contentType"))
                .or_else(|| file.get("mimeType"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| guess_content_type(name));
            let url = file
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            Some(json!({
                "id": id,
                "name": name,
                "mimeType": mime,
                "mime_type": mime,
                "type": discord_media_kind(mime),
                "url": url,
                "size": file.get("size").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect()
}

async fn discord_enrich_inbound_files(
    store: &AppStore,
    settings: &DiscordSettings,
    mut inbound: Value,
) -> AppResult<Value> {
    let files = inbound
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        return Ok(inbound);
    }

    let client = discord_client(settings)?;
    let cache_dir = store.data_dir().join("attachments").join("discord");
    fs::create_dir_all(&cache_dir)?;
    let mut attachments = Vec::new();
    let mut media_urls = Vec::new();
    let mut media_types = Vec::new();
    let mut skipped_files = Vec::new();

    for file in files {
        match discord_download_inbound_file(&client, settings, &cache_dir, &file).await {
            Ok(attachment) => {
                if let Some(path) = attachment.get("path").and_then(Value::as_str) {
                    media_urls.push(Value::String(path.to_string()));
                }
                if let Some(mime) = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                {
                    media_types.push(Value::String(mime.to_string()));
                }
                attachments.push(attachment);
            }
            Err(error) => skipped_files.push(json!({
                "id": file.get("id").and_then(Value::as_str).unwrap_or("attachment"),
                "error": error.to_string(),
            })),
        }
    }

    if !attachments.is_empty() {
        inbound["attachments"] = json!(attachments);
        inbound["mediaUrls"] = json!(media_urls);
        inbound["media_urls"] = inbound["mediaUrls"].clone();
        inbound["mediaTypes"] = json!(media_types);
        inbound["media_types"] = inbound["mediaTypes"].clone();
        if inbound.get("messageType").and_then(Value::as_str) == Some("text")
            || inbound.get("message_type").and_then(Value::as_str) == Some("text")
        {
            let message_type = discord_message_type_from_media(&inbound["mediaTypes"]);
            inbound["messageType"] = json!(message_type);
            inbound["message_type"] = json!(message_type);
        }
    }
    if !skipped_files.is_empty() {
        inbound["skippedFiles"] = json!(skipped_files);
        inbound["skipped_files"] = inbound["skippedFiles"].clone();
    }
    Ok(inbound)
}

async fn discord_download_inbound_file(
    client: &reqwest::Client,
    settings: &DiscordSettings,
    cache_dir: &Path,
    file: &Value,
) -> AppResult<Value> {
    let file_id = file
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("empty Discord attachment id".into()))?;
    let url = file
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("Discord attachment has no URL".into()))?;
    let mut request = client.get(url);
    if let Some(token) = settings.bot_token.as_deref() {
        request = request.bearer_auth(token);
    }
    let download_response = request.send().await.map_err(|error| {
        AppError::BadRequest(format!("Discord attachment download failed: {error}"))
    })?;
    let status = download_response.status();
    if !status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Discord attachment download failed ({})",
            status.as_u16()
        )));
    }
    let bytes = download_response.bytes().await.map_err(|error| {
        AppError::BadRequest(format!(
            "failed to read Discord attachment download: {error}"
        ))
    })?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("attachment");
    let mime = file
        .get("mimeType")
        .or_else(|| file.get("mime_type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| guess_content_type(name));
    let safe_name = mattermost_safe_file_name(name);
    let path = cache_dir.join(format!("{file_id}-{safe_name}"));
    fs::write(&path, &bytes)?;
    Ok(json!({
        "id": file_id,
        "name": name,
        "mimeType": mime,
        "mime_type": mime,
        "type": discord_media_kind(mime),
        "size": bytes.len(),
        "path": path.to_string_lossy(),
    }))
}

fn discord_inbound_prompt(inbound: &Value) -> String {
    let text = inbound
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let source = inbound.get("source").cloned().unwrap_or_else(|| json!({}));
    let channel_id = source
        .get("channelId")
        .or_else(|| source.get("channel_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let guild_id = source
        .get("guildId")
        .or_else(|| source.get("guild_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let user_id = source
        .get("userId")
        .or_else(|| source.get("user_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message_id = inbound
        .get("messageId")
        .or_else(|| inbound.get("message_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut prompt = format!(
        "Discord inbound message\nguild_id: {guild_id}\nchannel_id: {channel_id}\nmessage_id: {message_id}\nuser: {user_id}\n\n{text}"
    );
    if let Some(attachments) = inbound.get("attachments").and_then(Value::as_array) {
        if !attachments.is_empty() {
            prompt.push_str("\n\nAttachments:");
            for attachment in attachments {
                let path = attachment
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mime = attachment
                    .get("mimeType")
                    .or_else(|| attachment.get("mime_type"))
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                let name = attachment
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("attachment");
                prompt.push_str(&format!("\n- {name} ({mime}): {path}"));
            }
        }
    }
    prompt
}

fn discord_inbound_conversation_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(
        config,
        &[
            "inboundConversationId",
            "inbound_conversation_id",
            "conversationId",
            "conversation_id",
        ],
    ) {
        return Ok(id);
    }
    if let Some(existing) = store.conversations()?.into_iter().find(|conversation| {
        conversation
            .metadata
            .get("platform")
            .and_then(Value::as_str)
            == Some("discord")
    }) {
        return Ok(existing.id);
    }
    let persona_id = discord_inbound_persona_id(store, config)?;
    let conversation = store.create_conversation(Some("Discord".into()), Some(persona_id))?;
    store.set_conversation_metadata_value(&conversation.id, "platform", json!("discord"))?;
    Ok(conversation.id)
}

fn discord_inbound_persona_id(store: &AppStore, config: &Value) -> AppResult<String> {
    if let Some(id) = string_arg(config, &["personaId", "persona_id", "inboundPersonaId"]) {
        return Ok(id);
    }
    Ok(store
        .personas()?
        .first()
        .map(|persona| persona.id.clone())
        .ok_or_else(|| AppError::NotFound("persona".into()))?)
}

fn discord_message_type_from_media(media_types: &Value) -> &'static str {
    let media_types = media_types.as_array().cloned().unwrap_or_default();
    if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
    }) {
        "photo"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("audio/"))
            .unwrap_or(false)
    }) {
        "voice"
    } else if media_types.iter().any(|value| {
        value
            .as_str()
            .map(|mime| mime.starts_with("video/"))
            .unwrap_or(false)
    }) {
        "video"
    } else {
        "document"
    }
}

fn discord_media_kind(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("audio/") {
        "audio"
    } else if mime.starts_with("video/") {
        "video"
    } else {
        "document"
    }
}

pub(super) async fn discord_bridge_request(
    settings: &DiscordSettings,
    tool_name: &str,
    payload: &Value,
) -> AppResult<Value> {
    let gateway_url = settings.gateway_url.as_deref().ok_or_else(|| {
        AppError::BadRequest(
            "Discord tools require settings.discord.gatewayUrl when botToken is not configured"
                .into(),
        )
    })?;
    let path = discord_bridge_path(settings, tool_name);
    let mut url = reqwest::Url::parse(gateway_url)
        .map_err(|error| AppError::BadRequest(format!("invalid Discord gatewayUrl: {error}")))?;
    let mut full_path = url.path().trim_end_matches('/').to_string();
    full_path.push('/');
    full_path.push_str(path.trim_start_matches('/'));
    url.set_path(&full_path);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(settings.timeout_seconds))
        .user_agent("SynthChat-agent/1.0")
        .build()
        .map_err(|error| {
            AppError::BadRequest(format!("failed to build Discord bridge client: {error}"))
        })?;
    let response = client
        .post(url)
        .json(payload)
        .send()
        .await
        .map_err(|error| {
            AppError::BadRequest(format!("{tool_name} bridge request failed: {error}"))
        })?;
    discord_response_json(response, &format!("{tool_name} bridge")).await
}

pub(super) fn discord_bridge_path(settings: &DiscordSettings, tool_name: &str) -> String {
    settings
        .config
        .get("paths")
        .and_then(Value::as_object)
        .and_then(|paths| paths.get(tool_name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("/discord/{tool_name}"))
}

pub(super) fn discord_action(payload: &Value) -> Option<String> {
    string_arg(payload, &["action"]).map(|value| value.to_lowercase())
}

pub(super) fn ensure_discord_action_allowed(tool_name: &str, action: &str) -> AppResult<()> {
    let allowed = match tool_name {
        "discord" => matches!(
            action,
            "fetch_messages" | "search_members" | "create_thread" | "send_message"
        ),
        "discord_admin" => matches!(
            action,
            "list_guilds"
                | "server_info"
                | "list_channels"
                | "channel_info"
                | "list_roles"
                | "member_info"
                | "list_pins"
                | "pin_message"
                | "unpin_message"
                | "delete_message"
                | "add_role"
                | "remove_role"
        ),
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "unsupported {tool_name} action: {action}"
        )))
    }
}

pub(super) fn discord_required_id(
    payload: &Value,
    keys: &[&str],
    label: &str,
) -> AppResult<String> {
    let value = required_string_arg(payload, keys, label)?;
    if value
        .chars()
        .all(|ch| ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        Ok(value)
    } else {
        Err(AppError::BadRequest(format!(
            "invalid Discord {label}: expected an id-like value"
        )))
    }
}

pub(super) fn discord_optional_query(
    payload: &Value,
    mappings: &[(&str, &[&str])],
) -> Vec<(String, String)> {
    mappings
        .iter()
        .filter_map(|(target, keys)| {
            string_arg(payload, keys).map(|value| ((*target).into(), value))
        })
        .collect()
}

pub(super) fn ensure_ha_entity_id(entity_id: &str) -> AppResult<()> {
    let Some((domain, object_id)) = entity_id.split_once('.') else {
        return Err(AppError::BadRequest(format!(
            "invalid Home Assistant entity_id: {entity_id}"
        )));
    };
    ensure_ha_service_name(domain, "Home Assistant entity domain")?;
    if object_id.is_empty()
        || !object_id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(AppError::BadRequest(format!(
            "invalid Home Assistant entity_id: {entity_id}"
        )));
    }
    Ok(())
}

pub(super) fn ensure_ha_service_name(value: &str, label: &str) -> AppResult<()> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::BadRequest(format!("{label} cannot be empty")));
    };
    if !first.is_ascii_lowercase() {
        return Err(AppError::BadRequest(format!("invalid {label}: {value}")));
    }
    if !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
        return Err(AppError::BadRequest(format!("invalid {label}: {value}")));
    }
    Ok(())
}
