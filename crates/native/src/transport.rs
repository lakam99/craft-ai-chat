use craft_ai_chat_core::{MAX_BODY, MAX_REPLY, validate_key};
use serde_json::Value;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

pub fn http(key: String) -> Result<impl FnMut(&Value) -> Result<Value, String>, String> {
    validate_key(&key)?;
    http_validated(key)
}

fn http_validated(key: String) -> Result<impl FnMut(&Value) -> Result<Value, String>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(Arc::new(rustls_rustcrypto::provider()))
                .build(),
        )
        // Never forward a bearer credential to a redirect target.
        .max_redirects(0)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(120)))
        .build()
        .into();
    Ok(move |body: &Value| {
        craft_ai_chat_core::bounds::value_size(body, MAX_BODY)?;
        let bytes =
            serde_json::to_vec(body).map_err(|_| "Cannot encode the AI request".to_string())?;
        if bytes.len() > MAX_BODY {
            return Err(
                "Chat context is full. Clear the chat before sending another message.".into(),
            );
        }
        let mut response = agent
            .post("https://api.openai.com/v1/responses")
            .header("Authorization", format!("Bearer {key}"))
            .header("Content-Type", "application/json")
            .send(bytes.as_slice())
            .map_err(http_error)?;
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take((MAX_REPLY + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Could not read the OpenAI response".to_string())?;
        if bytes.len() > MAX_REPLY {
            return Err("OpenAI returned a response larger than the chat limit".into());
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| "OpenAI returned an invalid JSON response".into())
    })
}

fn http_error(error: ureq::Error) -> String {
    // Server bodies and transport errors can echo credentials or request content. Show status only.
    match error {
        ureq::Error::StatusCode(401 | 403) => "OpenAI refused this key or model access. Check your API key and model.".into(),
        ureq::Error::StatusCode(429) => "OpenAI rate or quota limit reached. Check API billing or try again later.".into(),
        ureq::Error::StatusCode(400 | 404 | 422) => "OpenAI rejected the request. Check that the model supports Responses, images and function tools.".into(),
        ureq::Error::StatusCode(code) => format!("OpenAI request failed (HTTP {code}). Try again later."),
        _ => "Cannot reach OpenAI, or the request timed out. Check your connection and try again.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn errors_show_actionable_status_without_response_details() {
        assert!(http_error(ureq::Error::StatusCode(401)).contains("key"));
        assert!(http_error(ureq::Error::StatusCode(429)).contains("quota"));
        assert!(http_error(ureq::Error::StatusCode(500)).contains("HTTP 500"));
        assert!(http("bad key".into()).is_err());
    }
}
