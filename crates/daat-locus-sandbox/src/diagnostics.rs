//! Native observations are evidence, not grants. Preserve unclassified events.
use serde::{Deserialize, Serialize};

use crate::{Access, DenialRecord, DenialSource};

pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;
pub const MAX_CAPTURE_DENIALS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeBackend {
    WindowsMxc,
    LinuxBubblewrap,
    MacosSeatbelt,
    Unrestricted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    NotRequested,
    Unavailable,
    Pending,
    Complete,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFailure {
    Setup,
    NativeCapture,
    Read,
    InvalidDocument,
    TooLarge,
}

/// The OS may add resource/access kinds without changing DL's grant model.
/// Keep their original identifiers, including `unknown`, registry and capability
/// observations. Never guess an authorization scope from them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeDenial {
    pub resource: String,
    pub resource_type: String,
    pub access_type: String,
}
impl NativeDenial {
    pub fn filesystem_denial(&self) -> Option<DenialRecord> {
        if !matches!(self.resource_type.as_str(), "file" | "directory") {
            return None;
        }
        let access = match self.access_type.as_str() {
            "read" => Access::Read,
            "write" => Access::Write,
            _ => return None,
        };
        let path = std::path::PathBuf::from(&self.resource);
        path.is_absolute().then_some(DenialRecord {
            access,
            path,
            source: DenialSource::Native,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialReport {
    pub backend: NativeBackend,
    pub status: CaptureStatus,
    pub failure: Option<CaptureFailure>,
    pub truncated: bool,
    pub observed_denials: usize,
    pub denials: Vec<NativeDenial>,
}
impl DenialReport {
    pub fn new(backend: NativeBackend, status: CaptureStatus) -> Self {
        Self {
            backend,
            status,
            failure: None,
            truncated: false,
            observed_denials: 0,
            denials: vec![],
        }
    }
    pub fn failed(backend: NativeBackend, failure: CaptureFailure) -> Self {
        Self {
            failure: Some(failure),
            ..Self::new(backend, CaptureStatus::Failed)
        }
    }
    pub fn filesystem_denials(&self) -> impl Iterator<Item = DenialRecord> + '_ {
        self.denials
            .iter()
            .filter_map(NativeDenial::filesystem_denial)
    }
}

/// Bounded import of the native capture, retaining every kind of observation.
/// Failed capture and incomplete capture must never look like a clean run.
pub fn report_from_capture_document(bytes: &[u8]) -> DenialReport {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Event {
        resource: String,
        resource_type: String,
        access_type: String,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Summary {
        denied_resources_truncated: bool,
        total_denials: usize,
    }
    #[derive(Deserialize)]
    struct Document {
        denials: Vec<Event>,
        summary: Summary,
    }
    let backend = NativeBackend::WindowsMxc;
    if bytes.len() > MAX_CAPTURE_BYTES {
        return DenialReport::failed(backend, CaptureFailure::TooLarge);
    }
    let Ok(document) = serde_json::from_slice::<Document>(bytes) else {
        return DenialReport::failed(backend, CaptureFailure::InvalidDocument);
    };
    let mut report = DenialReport::new(backend, CaptureStatus::Complete);
    report.truncated = document.summary.denied_resources_truncated;
    report.observed_denials = document.summary.total_denials.max(document.denials.len());
    let mut seen = std::collections::HashSet::new();
    for event in document.denials {
        if !seen.insert((
            event.resource.clone(),
            event.resource_type.clone(),
            event.access_type.clone(),
        )) {
            continue;
        }
        if report.denials.len() == MAX_CAPTURE_DENIALS {
            report.truncated = true;
            continue;
        }
        report.denials.push(NativeDenial {
            resource: event.resource,
            resource_type: event.resource_type,
            access_type: event.access_type,
        });
    }
    report
}

/// Retain native diagnostics across transport/setup errors without parsing text.
#[derive(Debug)]
pub struct ProcessFailure {
    pub message: String,
    pub report: DenialReport,
}
impl std::fmt::Display for ProcessFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ProcessFailure {}
impl miette::Diagnostic for ProcessFailure {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(denials: Vec<serde_json::Value>, truncated: bool) -> Vec<u8> {
        serde_json::to_vec(&json!({"summary":{"deniedResourcesTruncated":truncated,
            "totalDenials":denials.len()},"denials":denials}))
        .unwrap()
    }
    #[test]
    fn late_filesystem_and_unclassified_denials_survive_capture_import() {
        let root = std::env::temp_dir();
        let mut events = (0..32)
            .map(|n| {
                json!({"resource":root.join(format!("noise-{n}")),
            "resourceType":"file","accessType":"write"})
            })
            .collect::<Vec<_>>();
        let target = root.join("package-cache");
        events.extend([
            json!({"resource":target,"resourceType":"file","accessType":"write"}),
            json!({"resource":target,"resourceType":"file","accessType":"unknown"}),
            json!({"resource":"package-activation","resourceType":"capability","accessType":"execute"}),
            json!({"resource":"registry-key","resourceType":"other","accessType":"read"}),
            json!({"resource":"future-resource","resourceType":"future-kind","accessType":"future-access"}),
            json!({"resource":"relative/path","resourceType":"file","accessType":"write"}),
        ]);
        let report = report_from_capture_document(&document(events, false));
        assert_eq!(report.status, CaptureStatus::Complete);
        assert!(!report.truncated);
        assert_eq!(report.denials.len(), 38);
        let scopes = report.filesystem_denials().collect::<Vec<_>>();
        assert_eq!(scopes.len(), 33);
        assert_eq!(scopes.last().unwrap().path, target);
        assert_eq!(report.denials[34].resource, "package-activation");
        assert_eq!(report.denials[35].resource_type, "other");
        assert_eq!(report.denials[36].access_type, "future-access");
    }
    #[test]
    fn capture_limits_and_failures_are_explicit() {
        let events = (0..MAX_CAPTURE_DENIALS + 1)
            .map(|n| {
                json!({"resource":format!("capability-{n}"),
            "resourceType":"capability","accessType":"unknown"})
            })
            .collect();
        let report = report_from_capture_document(&document(events, false));
        assert!(report.truncated);
        assert_eq!(report.observed_denials, MAX_CAPTURE_DENIALS + 1);
        assert_eq!(report.denials.len(), MAX_CAPTURE_DENIALS);
        assert!(report_from_capture_document(&document(vec![], true)).truncated);
        for bytes in [b"not json".as_slice(), b"{}", b"{\"denials\":[]}"] {
            assert_eq!(
                report_from_capture_document(bytes).failure,
                Some(CaptureFailure::InvalidDocument)
            );
        }
        assert_eq!(
            report_from_capture_document(&vec![b' '; MAX_CAPTURE_BYTES + 1]).failure,
            Some(CaptureFailure::TooLarge)
        );
    }
}
