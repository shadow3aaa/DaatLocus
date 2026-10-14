//! Exact embedded assets and a small set of SPA routes; no runtime filesystem lookup.
use super::ApiError;
use axum::{
    body::Body,
    http::{HeaderValue, Method, Uri},
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/webui-assets.rs"));

fn conversation_route(path: &str) -> bool {
    if let Some(rest) = path.strip_prefix("/stages/")
        && let Some((stage, actor)) = rest.split_once("/actors/")
    {
        return uuid::Uuid::parse_str(stage).is_ok()
            && !actor.is_empty()
            && actor.len() <= 128
            && actor
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    }
    path == "/"
        || ["/actors/"].into_iter().any(|prefix| {
            path.strip_prefix(prefix).is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            })
        })
}

pub(super) async fn serve(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if !matches!(method, Method::GET | Method::HEAD)
        || path == "/v1"
        || path.starts_with("/v1/")
        || path.contains(['%', '\\'])
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return ApiError::not_found().into_response();
    }
    let spa = conversation_route(path);
    let asset_path = if spa { "/index.html" } else { path };
    let Some((_, mime, bytes)) = WEBUI_ASSETS.iter().find(|(key, _, _)| *key == asset_path) else {
        return if spa {
            (axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({"code":"webui_not_built", "message":"WebUI bundle is not included in this build"}))).into_response()
        } else {
            ApiError::not_found().into_response()
        };
    };
    let mut response = Response::new(if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(*bytes)
    });
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(mime));
    response.headers_mut().insert(
        "content-length",
        HeaderValue::from_str(&bytes.len().to_string()).unwrap(),
    );
    if path.starts_with("/assets/") {
        response.headers_mut().insert(
            "cache-control",
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if spa || *mime == "text/html; charset=utf-8" {
        response.headers_mut().insert("content-security-policy", HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; connect-src 'self'; frame-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
        ));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spa_routes_are_explicit_locators() {
        for path in ["/", "/actors/root", "/actors/a-b_9"] {
            assert!(conversation_route(path));
        }
        for path in [
            "/v1/missing",
            "/actors/",
            "/actors/a/extra",
            "/share/../x",
            "/settings",
            "/assets/x.js",
        ] {
            assert!(!conversation_route(path));
        }
    }
}
