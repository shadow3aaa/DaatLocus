use super::{ApiError, ApiResult, ServiceState, access_actor};
use crate::actors::{Attachment, StoredResource};
use axum::{
    Json,
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::stream;

pub(super) async fn upload(
    State(state): State<ServiceState>,
    Path(actor): Path<String>,
    request: Request,
) -> ApiResult<(StatusCode, Json<Attachment>)> {
    let headers = request.headers().clone();
    access_actor(&state, &headers, &actor, true).await?;
    let raw_name = headers
        .get("x-file-name")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(ApiError::invalid)?;
    let name = match headers.get("x-file-name-encoding") {
        None => raw_name.to_owned(),
        Some(encoding) if encoding == "utf-8-percent" => decode_filename(raw_name)?,
        Some(_) => return Err(ApiError::invalid()),
    };
    if name.is_empty()
        || name.len() > 255
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(ApiError::invalid());
    }
    let media_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    if media_type.is_empty()
        || media_type.len() > 128
        || !media_type
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'+' | b'.'))
    {
        return Err(ApiError::invalid());
    }
    let media_type = media_type.to_owned();
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        axum::body::to_bytes(
            request.into_body(),
            crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES,
        ),
    )
    .await
    .map_err(|_| ApiError {
        status: StatusCode::REQUEST_TIMEOUT,
        code: "upload_timeout",
        message: "Upload timed out",
    })?
    .map_err(|_| ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "payload_too_large",
        message: "Upload exceeds the permitted size",
    })?;
    if bytes.is_empty()
        || bytes.len() > crate::daemon::attachments::MAX_FILE_ATTACHMENT_BYTES
        || (media_type.starts_with("image/")
            && bytes.len() > crate::daemon::attachments::MAX_IMAGE_ATTACHMENT_BYTES)
    {
        return Err(ApiError::invalid());
    }
    access_actor(&state, &headers, &actor, true).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let base = state.system.actor_data_dir(&actor);
    let dir = base.join("uploads");
    crate::file_io::ensure_directory(&crate::sandbox::RuntimeSandboxPolicy::disabled(), &dir)
        .map_err(|_| ApiError::internal())?;
    crate::actors::permissions::protect_private_path(&dir).map_err(|_| ApiError::forbidden())?;
    let base = std::fs::canonicalize(&base).map_err(|_| ApiError::internal())?;
    let path = dir.join(&id);
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.full_disk_write = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.writable_roots = vec![crate::sandbox::WritableRoot {
        root: base.clone(),
        read_only_subpaths: vec![],
    }];
    policy.filesystem.bound_roots = vec![base.clone()];
    crate::file_io::write_bytes(&base, &policy, &path.to_string_lossy(), &bytes, None)
        .map_err(|_| ApiError::internal())?;
    let uri = format!("/v1/actors/{actor}/resources/{id}");
    let attachment = Attachment {
        id: id.clone(),
        name,
        media_type,
        size_bytes: bytes.len() as u64,
        uri: uri.clone(),
    };
    if let Err(error) = access_actor(&state, &headers, &actor, true).await {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(error);
    }
    if let Err(error) = state.system.store().register_resource(StoredResource {
        id,
        actor_id: actor,
        path: path.clone(),
        name: attachment.name.clone(),
        media_type: attachment.media_type.clone(),
        size_bytes: attachment.size_bytes,
        uri,
    }) {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(ApiError::from_report(error));
    }
    Ok((StatusCode::CREATED, Json(attachment)))
}

pub(super) fn decode_filename(encoded: &str) -> ApiResult<String> {
    if encoded.len() > 3 * 255 {
        return Err(ApiError::invalid());
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes
                .get(index + 1..index + 3)
                .ok_or_else(ApiError::invalid)?;
            let text = std::str::from_utf8(pair).map_err(|_| ApiError::invalid())?;
            decoded.push(u8::from_str_radix(text, 16).map_err(|_| ApiError::invalid())?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ApiError::invalid())
}

pub(super) async fn download(
    State(state): State<ServiceState>,
    headers: HeaderMap,
    Path((actor, resource)): Path<(String, String)>,
) -> ApiResult<Response> {
    access_actor(&state, &headers, &actor, false).await?;
    let resource = state
        .system
        .store()
        .resource(&actor, &resource)
        .map_err(ApiError::from_report)?;
    serve_file(state, headers, actor, resource).await
}

async fn serve_file(
    state: ServiceState,
    headers: HeaderMap,
    actor: String,
    resource: StoredResource,
) -> ApiResult<Response> {
    let base = std::fs::canonicalize(state.system.actor_data_dir(&actor))
        .map_err(|_| ApiError::not_found())?;
    let mut policy = crate::sandbox::RuntimeSandboxPolicy::disabled();
    policy.filesystem.full_disk_read = false;
    policy.filesystem.readable_roots = vec![base.clone()];
    policy.filesystem.bound_roots = vec![base];
    let path = resource.path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        crate::file_io::read_bytes(&path, 32 * 1024 * 1024, &policy)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(|_| ApiError::forbidden())?;
    let len = bytes.len();
    let chunks = stream::try_unfold(
        (bytes, 0usize, state, headers, actor),
        |(bytes, offset, state, headers, actor)| async move {
            access_actor(&state, &headers, &actor, false)
                .await
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Resource access revoked",
                    )
                })?;
            if offset >= bytes.len() {
                return Ok::<_, std::io::Error>(None);
            }
            let end = (offset + 32768).min(bytes.len());
            let chunk = bytes[offset..end].to_vec();
            Ok(Some((chunk, (bytes, end, state, headers, actor))))
        },
    );
    let mut response = Body::from_stream(chunks).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(
        "content-type",
        HeaderValue::from_str(&resource.media_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response_headers.insert(
        "content-length",
        HeaderValue::from_str(&len.to_string()).map_err(|_| ApiError::internal())?,
    );
    response_headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response_headers.insert(
        "cache-control",
        HeaderValue::from_static("private, no-store"),
    );
    response_headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response_headers.insert("content-security-policy", HeaderValue::from_static("sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'; frame-ancestors 'self'"));
    // SVG/HTML are active documents and are confined to a unique sandbox origin.
    if !matches!(
        resource.media_type.as_str(),
        "text/html"
            | "image/svg+xml"
            | "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
            | "application/pdf"
            | "text/plain"
    ) {
        response_headers.insert(
            "content-disposition",
            HeaderValue::from_static("attachment"),
        );
    }
    Ok(response)
}
