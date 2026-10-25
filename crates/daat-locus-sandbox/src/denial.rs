//! Structured observations of a policy decision, not permission to retry an operation.

use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialSource {
    PolicyPreflight,
    Native,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenialRecord {
    pub access: Access,
    pub path: PathBuf,
    pub source: DenialSource,
}

/// A denied policy preflight. Callers may downcast a `miette::Report` to this
/// type rather than interpreting display text, stderr, or an OS error number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessDenied {
    pub record: DenialRecord,
    label: String,
}

impl AccessDenied {
    pub(crate) fn new(access: Access, path: PathBuf, label: &str) -> Self {
        Self {
            record: DenialRecord {
                access,
                path,
                source: DenialSource::PolicyPreflight,
            },
            label: label.to_owned(),
        }
    }
}

impl fmt::Display for AccessDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let access = match self.record.access {
            Access::Read => "read",
            Access::Write => "write",
        };
        write!(
            f,
            "sandbox denies {access} access to {}: {}",
            self.label,
            self.record.path.display()
        )
    }
}

impl std::error::Error for AccessDenied {}
impl miette::Diagnostic for AccessDenied {}

/// The maximum number of native denial records imported from one capture.
pub const MAX_CAPTURE_DENIALS: usize = 16;

/// Maps an MXC `captureDenials` JSON document onto typed denial records.
///
/// Only absolute file/directory read and write denials become records;
/// other resource or access kinds are ignored because they do not name a
/// filesystem scope. Records are de-duplicated in first-seen order and
/// capped at `max_records`. The returned flag reports truncation either
/// from the capture itself or from the cap.
pub fn denials_from_capture_document(
    bytes: &[u8],
    max_records: usize,
) -> (Vec<DenialRecord>, bool) {
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return (Vec::new(), false);
    };
    let mut truncated = document["summary"]["deniedResourcesTruncated"]
        .as_bool()
        .unwrap_or(false);
    let mut records: Vec<DenialRecord> = Vec::new();
    if let Some(denials) = document["denials"].as_array() {
        for denial in denials {
            let access = match denial["accessType"].as_str() {
                Some("read") => Access::Read,
                Some("write") => Access::Write,
                _ => continue,
            };
            if !matches!(denial["resourceType"].as_str(), Some("file" | "directory")) {
                continue;
            }
            let Some(resource) = denial["resource"].as_str() else {
                continue;
            };
            let path = PathBuf::from(resource);
            if !path.is_absolute() {
                continue;
            }
            let record = DenialRecord {
                access,
                path,
                source: DenialSource::Native,
            };
            if !records.contains(&record) {
                records.push(record);
            }
        }
    }
    if records.len() > max_records {
        records.truncate(max_records);
        truncated = true;
    }
    (records, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SandboxPolicy;

    #[test]
    fn policy_preflight_returns_the_denied_operation_and_normalized_target() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        let outside = temp.path().join("outside.txt");
        let spelling = temp.path().join("allowed/../outside.txt");
        let mut policy = SandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        policy.filesystem.full_disk_write = false;
        policy.filesystem.readable_roots = vec![allowed.clone()];
        // The temporary base is baseline-authorized, so a denial inside it
        // can only come from an explicit rule; explicit rules still take
        // precedence over both grants and the temporary exemption.
        policy.filesystem.deny_read_paths = vec![outside.clone()];
        policy.filesystem.deny_write_paths = vec![outside.clone()];
        assert!(policy.ensure_path_readable(&allowed, "input").is_ok());
        for (access, result) in [
            (
                Access::Read,
                policy.ensure_path_readable(&spelling, "input"),
            ),
            (
                Access::Write,
                policy.ensure_path_writable(&spelling, "output"),
            ),
        ] {
            let error = result.unwrap_err();
            let denial = error
                .downcast_ref::<AccessDenied>()
                .expect("typed policy denial");
            assert_eq!(
                denial.record,
                DenialRecord {
                    access,
                    path: outside.clone(),
                    source: DenialSource::PolicyPreflight
                }
            );
        }
    }

    #[test]
    fn typed_denial_survives_report_context_and_io_error_sources() {
        let mut policy = SandboxPolicy::disabled();
        policy.filesystem.full_disk_read = false;
        let report = policy
            .ensure_path_readable(std::path::Path::new("ungranted.txt"), "input")
            .unwrap_err()
            .wrap_err("application operation failed");
        assert!(report.downcast_ref::<AccessDenied>().is_some());
        let error = std::io::Error::other(report);
        let mut source = error
            .get_ref()
            .map(|source| source as &(dyn std::error::Error + 'static));
        while let Some(current) = source {
            if current.downcast_ref::<AccessDenied>().is_some() {
                return;
            }
            source = current.source();
        }
        panic!("I/O error conversion discarded the structured denial");
    }

    #[cfg(unix)]
    #[test]
    fn replaced_authority_root_is_not_reported_as_a_missing_permission() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("authorized");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let root = root.canonicalize().unwrap();
        let mut policy = SandboxPolicy::disabled();
        policy.filesystem.bound_roots = vec![root.clone()];
        std::fs::rename(&root, temp.path().join("original")).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        for result in [
            policy.ensure_path_readable(&root, "input"),
            policy.ensure_path_writable(&root, "output"),
        ] {
            let error = result.unwrap_err();
            assert!(
                error.downcast_ref::<AccessDenied>().is_none(),
                "root replacement must not become a scope-grant request"
            );
        }
    }

    fn json_path(path: &std::path::Path) -> String {
        path.to_string_lossy().replace('\\', "\\\\")
    }

    #[test]
    fn capture_document_maps_to_typed_native_records() {
        #[cfg(windows)]
        let (file_a, file_b, dir_a) = (
            std::path::PathBuf::from(r"C:\work\secret.txt"),
            std::path::PathBuf::from(r"C:\work\out\log.txt"),
            std::path::PathBuf::from(r"C:\work"),
        );
        #[cfg(not(windows))]
        let (file_a, file_b, dir_a) = (
            std::path::PathBuf::from("/work/secret.txt"),
            std::path::PathBuf::from("/work/out/log.txt"),
            std::path::PathBuf::from("/work"),
        );
        let document = format!(
            r#"{{"denials":[
                {{"resource":"{}","resourceType":"file","accessType":"read","pid":1,"filetime":1}},
                {{"resource":"{}","resourceType":"file","accessType":"write","pid":1,"filetime":2}},
                {{"resource":"{}","resourceType":"directory","accessType":"read","pid":1,"filetime":3}},
                {{"resource":"internetClient","resourceType":"capability","accessType":"read","pid":1,"filetime":4}},
                {{"resource":"{}","resourceType":"file","accessType":"read","pid":1,"filetime":5}},
                {{"resource":"relative/x.txt","resourceType":"file","accessType":"read","pid":1,"filetime":6}},
                {{"resource":"{}","resourceType":"file","accessType":"execute","pid":1,"filetime":7}}
            ],"summary":{{"exitCode":1,"totalDenials":7,"deniedResourcesTruncated":false}}}}"#,
            json_path(&file_a),
            json_path(&file_b),
            json_path(&dir_a),
            json_path(&file_a),
            json_path(&file_a),
        );
        let (records, truncated) =
            denials_from_capture_document(document.as_bytes(), MAX_CAPTURE_DENIALS);
        assert_eq!(
            records,
            vec![
                DenialRecord {
                    access: Access::Read,
                    path: file_a.clone(),
                    source: DenialSource::Native,
                },
                DenialRecord {
                    access: Access::Write,
                    path: file_b.clone(),
                    source: DenialSource::Native,
                },
                DenialRecord {
                    access: Access::Read,
                    path: dir_a.clone(),
                    source: DenialSource::Native,
                },
            ]
        );
        assert!(!truncated);
    }

    #[test]
    fn capture_document_caps_records_and_honors_truncation() {
        let base = std::env::temp_dir().join("dl-capture");
        let mut denials = Vec::new();
        for index in 0..(MAX_CAPTURE_DENIALS + 5) {
            denials.push(format!(
                r#"{{"resource":"{}","resourceType":"file","accessType":"write","pid":1,"filetime":{index}}}"#,
                json_path(&base.join(format!("deny{index}.txt")))
            ));
        }
        let document = format!(
            r#"{{"denials":[{}],"summary":{{"exitCode":0,"totalDenials":{},"deniedResourcesTruncated":true}}}}"#,
            denials.join(","),
            denials.len()
        );
        let (records, truncated) =
            denials_from_capture_document(document.as_bytes(), MAX_CAPTURE_DENIALS);
        assert_eq!(records.len(), MAX_CAPTURE_DENIALS);
        assert!(truncated);
        assert!(
            denials_from_capture_document(b"not json", MAX_CAPTURE_DENIALS)
                .0
                .is_empty()
        );
    }
}
