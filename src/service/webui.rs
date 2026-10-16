//! Exact embedded assets and a small set of SPA routes; no runtime filesystem lookup.
use super::ApiError;
use axum::{
    body::Body,
    http::{HeaderValue, Method, Uri},
    response::{IntoResponse, Response},
};

// Backend-only builds generate an empty asset table.
#[allow(dead_code)]
enum AssetContent {
    Plain(&'static [u8]),
    Zstd { bytes: &'static [u8], size: usize },
}

impl AssetContent {
    fn len(&self) -> usize {
        match self {
            Self::Plain(bytes) => bytes.len(),
            Self::Zstd { size, .. } => *size,
        }
    }

    fn body(&self) -> std::io::Result<Body> {
        match self {
            Self::Plain(bytes) => Ok(Body::from(*bytes)),
            Self::Zstd { bytes, size } => {
                let decoded = zstd::bulk::decompress(bytes, *size)?;
                if decoded.len() != *size {
                    return Err(std::io::Error::other("Embedded asset length mismatch"));
                }
                Ok(Body::from(decoded))
            }
        }
    }
}

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
    let Some((_, mime, content)) = WEBUI_ASSETS.iter().find(|(key, _, _)| *key == asset_path)
    else {
        return if spa {
            (axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({"code":"webui_not_built", "message":"WebUI bundle is not included in this build"}))).into_response()
        } else {
            ApiError::not_found().into_response()
        };
    };
    asset_response(method, path, mime, content)
}

fn asset_response(
    method: Method,
    path: &str,
    mime: &'static str,
    content: &AssetContent,
) -> Response {
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        match content.body() {
            Ok(body) => body,
            Err(error) => {
                tracing::error!(%error, path, "Cannot decode embedded WebUI asset");
                return ApiError::internal().into_response();
            }
        }
    };
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert("content-type", HeaderValue::from_static(mime));
    response.headers_mut().insert(
        "content-length",
        HeaderValue::from_str(&content.len().to_string()).unwrap(),
    );
    if path.starts_with("/assets/") {
        response.headers_mut().insert(
            "cache-control",
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if mime == "text/html; charset=utf-8" {
        response.headers_mut().insert("content-security-policy", HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; connect-src 'self'; frame-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
        ));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compressed_assets_keep_original_bytes_headers_and_head_semantics() {
        // Independent zstd vector, produced by Bun rather than our build helper.
        let bytes: &[u8] = &[
            40, 181, 47, 253, 96, 0, 0, 85, 1, 0, 20, 2, 0, 99, 111, 110, 115, 116, 32, 103, 114,
            101, 101, 116, 105, 110, 103, 32, 61, 32, 34, 228, 189, 160, 229, 165, 189, 240, 159,
            153, 130, 34, 59, 10, 0, 1, 0, 225, 14, 74, 61, 1,
        ];
        let original = "\0const greeting = \"你好🙂\";\n".repeat(8);
        let content = AssetContent::Zstd {
            bytes,
            size: original.len(),
        };
        for method in [Method::GET, Method::HEAD] {
            let response = asset_response(
                method.clone(),
                "/assets/test.js",
                "text/javascript; charset=utf-8",
                &content,
            );
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            assert_eq!(
                response.headers()["content-type"],
                "text/javascript; charset=utf-8"
            );
            assert_eq!(
                response.headers()["content-length"],
                original.len().to_string()
            );
            assert!(response.headers().contains_key("cache-control"));
            assert!(!response.headers().contains_key("content-encoding"));
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            assert_eq!(
                body.as_ref(),
                if method == Method::HEAD {
                    b""
                } else {
                    original.as_bytes()
                }
            );
        }
    }

    #[tokio::test]
    async fn corrupt_embedded_assets_fail_without_returning_a_successful_payload() {
        let content = AssetContent::Zstd {
            bytes: b"broken",
            size: 100,
        };
        assert_eq!(
            asset_response(
                Method::GET,
                "/index.html",
                "text/html; charset=utf-8",
                &content
            )
            .status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let marker = AssetContent::Plain(br#"{"format":"DaatLocus.EmbeddedWebUI.v1"}"#);
        let response = asset_response(
            Method::GET,
            "/.well-known/daat-locus-build.json",
            "application/json",
            &marker,
        );
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["format"],
            "DaatLocus.EmbeddedWebUI.v1"
        );
    }

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
