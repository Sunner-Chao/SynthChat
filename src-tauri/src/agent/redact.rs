use serde_json::Value;

pub(super) fn redact_sensitive_text(value: &str) -> String {
    let value = redact_private_key_blocks(value);
    let value = redact_urls_with_sensitive_query(&value);
    let value = redact_request_target_queries(&value);
    let value = redact_form_body(&value);
    let value = redact_json_like_fields(&value);
    let value = redact_env_assignments(&value);
    let value = redact_authorization_headers(&value);
    let value = redact_telegram_tokens(&value);
    redact_prefixed_tokens(&value)
}

pub(super) fn redact_json_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_sensitive_text(&text)),
        Value::Array(items) => Value::Array(items.into_iter().map(redact_json_value).collect()),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    if is_sensitive_key(&key) {
                        (key, Value::String("***".into()))
                    } else {
                        (key, redact_json_value(value))
                    }
                })
                .collect(),
        ),
        other => other,
    }
}

fn mask_secret(value: &str) -> String {
    if value.len() < 18 {
        "***".into()
    } else {
        let head = value.chars().take(6).collect::<String>();
        let tail = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        format!("{head}...{tail}")
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "access_token"
            | "refreshtoken"
            | "refresh_token"
            | "id_token"
            | "token"
            | "api_key"
            | "apikey"
            | "apikeyenv"
            | "api_key_env"
            | "client_secret"
            | "password"
            | "auth"
            | "jwt"
            | "secret"
            | "private_key"
            | "authorization"
            | "key"
            | "bearer"
            | "bot_token"
            | "bottoken"
    )
}

fn redact_prefixed_tokens(value: &str) -> String {
    map_whitespace_tokens(value, |token| {
        let trimmed = token
            .trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | ']' | '}' | '"' | '\'' | '`'));
        if looks_like_secret_token(trimmed) {
            token.replace(trimmed, &mask_secret(trimmed))
        } else {
            token.to_string()
        }
    })
}

fn looks_like_secret_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let prefixes = [
        "sk-",
        "sk_",
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "pplx-",
        "fal_",
        "fc-",
        "bb_live_",
        "gaaaa",
        "akia",
        "sk_live_",
        "sk_test_",
        "rk_live_",
        "sg.",
        "hf_",
        "r8_",
        "npm_",
        "pypi-",
        "dop_v1_",
        "doo_v1_",
        "am_",
        "tvly-",
        "exa_",
        "gsk_",
        "syt_",
        "retaindb_",
        "hsk-",
        "mem0_",
        "brv_",
        "xai-",
    ];
    (lower.starts_with("aiza") && value.len() >= 34)
        || prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix) && value.len() >= prefix.len() + 10)
        || (lower.starts_with("eyj") && value.len() >= 18 && value.contains('.'))
}

fn redact_authorization_headers(value: &str) -> String {
    let mut output = String::new();
    let mut cursor = 0usize;
    let lower = value.to_ascii_lowercase();
    while let Some(relative) = lower[cursor..].find("bearer ") {
        let bearer = cursor + relative;
        let token_start = bearer + "bearer ".len();
        let token_end = value[token_start..]
            .find(char::is_whitespace)
            .map(|offset| token_start + offset)
            .unwrap_or(value.len());
        output.push_str(&value[cursor..token_start]);
        output.push_str(&mask_secret(&value[token_start..token_end]));
        cursor = token_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn redact_env_assignments(value: &str) -> String {
    map_whitespace_tokens(value, |part| {
        if let Some((key, secret)) = part.split_once('=') {
            if is_sensitive_env_name(key) && !secret.is_empty() {
                if part.contains('&') {
                    return redact_query_pairs(part);
                }
                return format!("{key}=***");
            }
        }
        part.to_string()
    })
}

pub(super) fn is_sensitive_env_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "APIKEY",
        "API_KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|needle| upper.contains(needle))
}

fn redact_json_like_fields(value: &str) -> String {
    let mut output = value.to_string();
    for key in [
        "apiKey",
        "api_key",
        "token",
        "access_token",
        "refresh_token",
        "clientSecret",
        "client_secret",
        "password",
        "secret",
        "authorization",
        "private_key",
        "botToken",
        "bot_token",
    ] {
        output = redact_quoted_field(&output, key);
    }
    output
}

fn redact_quoted_field(value: &str, key: &str) -> String {
    let mut output = String::new();
    let lower = value.to_ascii_lowercase();
    let needle = format!("\"{}\"", key.to_ascii_lowercase());
    let mut cursor = 0usize;
    while let Some(relative) = lower[cursor..].find(&needle) {
        let key_start = cursor + relative;
        let after_key = key_start + needle.len();
        let Some(colon_relative) = value[after_key..].find(':') else {
            break;
        };
        let colon = after_key + colon_relative;
        let rest = &value[colon + 1..];
        let quote_offset = rest.find('"');
        let Some(quote_offset) = quote_offset else {
            break;
        };
        if rest[..quote_offset].trim().is_empty() {
            let value_start = colon + 1 + quote_offset + 1;
            let Some(value_end_relative) = value[value_start..].find('"') else {
                break;
            };
            let value_end = value_start + value_end_relative;
            output.push_str(&value[cursor..value_start]);
            output.push_str("***");
            cursor = value_end;
        } else {
            output.push_str(&value[cursor..after_key]);
            cursor = after_key;
        }
    }
    output.push_str(&value[cursor..]);
    output
}

fn redact_urls_with_sensitive_query(value: &str) -> String {
    map_whitespace_tokens(value, redact_url_token)
}

fn redact_url_token(token: &str) -> String {
    let trimmed = token.trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | ']' | '}'));
    let (prefix, candidate) = if let Some((key, value)) = trimmed.split_once('=') {
        if reqwest::Url::parse(value).is_ok() {
            (format!("{key}="), value)
        } else {
            (String::new(), trimmed)
        }
    } else {
        (String::new(), trimmed)
    };
    let Ok(mut url) = reqwest::Url::parse(candidate) else {
        return token.to_string();
    };
    let mut changed = false;
    if url.password().is_some() {
        let _ = url.set_password(Some("***"));
        changed = true;
    }
    if url.query().is_none() {
        if changed {
            return token.replace(trimmed, &format!("{prefix}{}", url.as_str()));
        }
        return token.to_string();
    }
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            if is_sensitive_key(&key) {
                changed = true;
                (key.to_string(), "***".to_string())
            } else {
                (key.to_string(), value.to_string())
            }
        })
        .collect::<Vec<_>>();
    if changed {
        url.set_query(None);
        {
            let mut serializer = url.query_pairs_mut();
            for (key, value) in pairs {
                serializer.append_pair(&key, &value);
            }
        }
        token.replace(trimmed, &format!("{prefix}{}", url.as_str()))
    } else {
        token.to_string()
    }
}

fn redact_request_target_queries(value: &str) -> String {
    map_whitespace_tokens(value, |token| {
        let trimmed =
            token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']' | '}'));
        if trimmed.contains("://") {
            return token.to_string();
        }
        let Some(question) = trimmed.find('?') else {
            return token.to_string();
        };
        let query = &trimmed[question + 1..];
        if query.is_empty() || !query.contains('=') {
            return token.to_string();
        }
        let redacted_query = redact_query_pairs(query);
        if redacted_query == query {
            token.to_string()
        } else {
            let replacement = format!("{}?{}", &trimmed[..question], redacted_query);
            token.replace(trimmed, &replacement)
        }
    })
}

fn redact_form_body(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains(char::is_whitespace)
        || !trimmed.contains('&')
        || !trimmed.contains('=')
    {
        return value.to_string();
    }
    let redacted = redact_query_pairs(trimmed);
    if redacted == trimmed {
        value.to_string()
    } else {
        value.replace(trimmed, &redacted)
    }
}

fn redact_query_pairs(query: &str) -> String {
    query
        .split('&')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return part.to_string();
            };
            if is_sensitive_key(key) {
                format!("{key}=***")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn redact_private_key_blocks(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0usize;
    while let Some(relative_begin) = value[cursor..].find("-----BEGIN") {
        let begin = cursor + relative_begin;
        let begin_line_end = value[begin..]
            .find("-----")
            .and_then(|first| {
                value[begin + first + 5..]
                    .find("-----")
                    .map(|second| begin + first + 5 + second + 5)
            })
            .unwrap_or(begin);
        if !value[begin..begin_line_end].contains("PRIVATE KEY") {
            output.push_str(&value[cursor..begin_line_end]);
            cursor = begin_line_end;
            continue;
        }
        let Some(relative_end) = value[begin_line_end..].find("-----END") else {
            output.push_str(&value[cursor..begin]);
            output.push_str("***PRIVATE KEY***");
            cursor = value.len();
            break;
        };
        let end_begin = begin_line_end + relative_end;
        let end = value[end_begin..]
            .find("-----")
            .and_then(|first| {
                value[end_begin + first + 5..]
                    .find("-----")
                    .map(|second| end_begin + first + 5 + second + 5)
            })
            .unwrap_or(value.len());
        output.push_str(&value[cursor..begin]);
        output.push_str("***PRIVATE KEY***");
        cursor = end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn redact_telegram_tokens(value: &str) -> String {
    map_whitespace_tokens(value, |token| {
        let trimmed = token
            .trim_matches(|ch: char| matches!(ch, ',' | ';' | ')' | ']' | '}' | '"' | '\'' | '`'));
        let candidate = trimmed.strip_prefix("bot").unwrap_or(trimmed);
        let Some((id, secret)) = candidate.split_once(':') else {
            return token.to_string();
        };
        if id.len() >= 8
            && id.chars().all(|ch| ch.is_ascii_digit())
            && secret.len() >= 30
            && secret
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            token.replace(trimmed, &mask_secret(trimmed))
        } else {
            token.to_string()
        }
    })
}

fn map_whitespace_tokens<F>(value: &str, mut f: F) -> String
where
    F: FnMut(&str) -> String,
{
    let mut output = String::with_capacity(value.len());
    let mut token_start = None;
    for (index, ch) in value.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                output.push_str(&f(&value[start..index]));
            }
            output.push(ch);
        } else if token_start.is_none() {
            token_start = Some(index);
        }
    }
    if let Some(start) = token_start {
        output.push_str(&f(&value[start..]));
    }
    output
}
