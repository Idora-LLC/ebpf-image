//! The execution RunRecord and metadata sidecar the Recorder emits.
//!
//! Mirrors the pipeline's `RunRecordBase` execution variant
//! (`idora-pipeline/specs/data-types.md` §3.1-3.2) and `ExecutionMetadataSidecar`
//! (§4.2). The record is flat, `snake_case` on the wire, and every base key is
//! present (nullable where the contract allows). See `specs/run-record.md` §2.

use serde::{Deserialize, Serialize};

/// The execution operation type. The Recorder only ever emits these three;
/// verification/ingestion variants belong to WIT Core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunRecordType {
    Build,
    Test,
    Deploy,
}

impl RunRecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            RunRecordType::Build => "build",
            RunRecordType::Test => "test",
            RunRecordType::Deploy => "deploy",
        }
    }
}

/// A single entry in `inputs[]` / `outputs[]`. Exactly two fields
/// (`idora-pipeline/specs/data-types.md` §2.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactEntry {
    /// Real absolute path as observed; the pipeline relativizes against
    /// `working_directory` (`specs/run-record.md` §3.1). The Recorder does not
    /// pre-relativize.
    pub path: String,
    /// `sha256:<64 lowercase hex>` of the file content.
    pub hash: String,
}

/// Wall-clock window. Present on the wire; stripped by Normalize and surfaced on
/// the sidecar instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamps {
    pub start_time: String,
    pub end_time: String,
    pub duration_ms: u64,
}

/// The flat execution RunRecord. Field order matches `data-types.md` §3.1; the
/// pipeline canonicalizes via RFC 8785 so wire order is irrelevant to identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    #[serde(rename = "type")]
    pub run_type: RunRecordType,
    pub command: String,
    /// `null` when not observable; never defaulted to 0 (`specs/run-record.md` §2).
    pub exit_code: Option<i32>,
    pub platform: String,
    pub architecture: String,
    pub working_directory: String,
    pub repo: String,
    pub commit: String,
    pub tool_versions: Option<std::collections::BTreeMap<String, String>>,
    pub inputs: Option<Vec<ArtifactEntry>>,
    pub outputs: Option<Vec<ArtifactEntry>>,
    pub environment: Option<std::collections::BTreeMap<String, String>>,
    pub timestamps: Timestamps,
}

/// `ExecutionMetadataSidecar` (`idora-pipeline/specs/data-types.md` §4.2). All
/// fields are operational and live outside the receipt-identity hash.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionMetadataSidecar {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Capture fidelity grade. Always `file_access_tracking` on supported
    /// runners (`specs/observation.md` §7); `snapshot` is dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation_mode: Option<String>,
    /// Deploy-event identity for re-deploy counting (`specs/run-record.md` §5.1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_attempt: Option<String>,
    /// Deploy destination (`prod`/`staging`), adapter-sourced (`specs/run-record.md` §5.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_target: Option<String>,
}

/// The request body of `POST /pipeline/process`
/// (`idora-pipeline/specs/api-contract.md` §2.1).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmissionBody {
    #[serde(rename = "runRecord")]
    pub run_record: RunRecord,
    pub metadata: ExecutionMetadataSidecar,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sample() -> RunRecord {
        RunRecord {
            run_type: RunRecordType::Build,
            command: "npm run build".into(),
            exit_code: None,
            platform: "linux".into(),
            architecture: "amd64".into(),
            working_directory: "/work/repo".into(),
            repo: "owner/name".into(),
            commit: "deadbeef".into(),
            tool_versions: None,
            inputs: None,
            outputs: None,
            environment: None,
            timestamps: Timestamps {
                start_time: "2026-04-01T15:00:00.000Z".into(),
                end_time: "2026-04-01T15:02:30.000Z".into(),
                duration_ms: 150000,
            },
        }
    }

    #[test]
    fn type_is_lowercase_and_renamed() {
        let v: Value = serde_json::to_value(sample()).unwrap();
        assert_eq!(v.get("type").unwrap(), "build");
    }

    #[test]
    fn required_but_nullable_keys_are_present_as_null() {
        // The pipeline requires the keys to be present even when null
        // (idora-pipeline/specs/data-types.md §3.1).
        let v: Value = serde_json::to_value(sample()).unwrap();
        for key in ["exit_code", "inputs", "outputs", "environment", "tool_versions"] {
            assert!(v.as_object().unwrap().contains_key(key), "missing key {key}");
            assert!(v.get(key).unwrap().is_null(), "{key} should be null");
        }
    }

    #[test]
    fn all_base_fields_present_snake_case() {
        let v: Value = serde_json::to_value(sample()).unwrap();
        for key in [
            "type", "command", "exit_code", "platform", "architecture",
            "working_directory", "repo", "commit", "tool_versions", "inputs",
            "outputs", "environment", "timestamps",
        ] {
            assert!(v.as_object().unwrap().contains_key(key), "missing {key}");
        }
    }

    #[test]
    fn submission_body_wraps_run_record_camelcase() {
        let body = SubmissionBody {
            run_record: sample(),
            metadata: ExecutionMetadataSidecar {
                observation_mode: Some("file_access_tracking".into()),
                ..Default::default()
            },
        };
        let v: Value = serde_json::to_value(&body).unwrap();
        assert!(v.get("runRecord").is_some());
        assert_eq!(
            v.get("metadata").unwrap().get("observation_mode").unwrap(),
            "file_access_tracking"
        );
    }

    #[test]
    fn sidecar_omits_unset_optional_fields() {
        let v: Value = serde_json::to_value(ExecutionMetadataSidecar::default()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 0);
    }
}
