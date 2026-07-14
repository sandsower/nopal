//! Versioned native Field restore preferences.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use crate::reconcile::ExactSessionSelection;

const RESTORE_PREFERENCE_KIND: &str = "nopal.native_field_preference/v1";
const TEMP_CREATE_ATTEMPTS: usize = 32;

/// Maximum accepted size of a native restore preference document.
pub const MAX_RESTORE_PREFERENCE_BYTES: usize = 64 * 1024;

/// The versioned desktop intent read from the native preference file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreference {
    /// One exact Plot and Session pair, or no prior selection.
    pub selection: Option<ExactSessionSelection>,
}

/// A typed read that distinguishes safe absence from content that must be preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorePreferenceReadOutcome {
    /// No preference has been written yet.
    Missing,
    /// An exact v1 document was decoded.
    Ready(RestorePreference),
    /// A document exists but is not exact valid v1 JSON.
    Malformed {
        /// A diagnostic suitable for logs, not for round-tripping the source.
        message: String,
    },
    /// A document names a version this build does not understand.
    FutureVersion {
        /// The exact version string found in the file.
        version: String,
    },
    /// A document exceeds the bounded preference wire size.
    Oversized {
        /// The maximum document size accepted by this build.
        max_bytes: usize,
        /// The size reported by the file, or the bounded prefix observed.
        observed_bytes: Option<u64>,
        /// An actionable diagnostic suitable for logs.
        message: String,
    },
    /// The document exists but could not be opened, inspected, or read.
    Unreadable {
        /// An actionable diagnostic suitable for logs.
        message: String,
    },
}

/// Selection knowledge supplied by the reconciled native UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorePreferenceUpdate {
    /// Persist no selected pair as an exact JSON `null`.
    ClearSelection,
    /// Persist a pair already known to the caller.
    Select(ExactSessionSelection),
    /// Refuse persistence because the caller cannot name an exact pair.
    UnknownSelection,
}

impl RestorePreferenceUpdate {
    /// Creates an update for one exact selection intent.
    pub fn select(selection: ExactSessionSelection) -> Self {
        Self::Select(selection)
    }
}

/// Existing content intentionally left byte-for-byte unchanged by a write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreservedPreference {
    /// Existing v1-shaped content was malformed.
    Malformed,
    /// Existing content names an unsupported version.
    FutureVersion {
        /// The exact version string found in the file.
        version: String,
    },
    /// Existing content exceeded the bounded wire size.
    Oversized {
        /// The maximum document size accepted by this build.
        max_bytes: usize,
        /// The size reported by the file, or the bounded prefix observed.
        observed_bytes: Option<u64>,
        /// An actionable diagnostic suitable for logs.
        message: String,
    },
    /// Existing content could not be read safely.
    Unreadable {
        /// An actionable diagnostic suitable for logs.
        message: String,
    },
}

/// The non-I/O result of a preference update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestorePreferenceWriteOutcome {
    /// A complete new document was atomically installed.
    Written,
    /// No file was touched because the selection was not exact.
    RejectedUnknownSelection,
    /// No file was touched because the encoded document exceeded the wire limit.
    RejectedOversized {
        /// The maximum document size accepted by this build.
        max_bytes: usize,
        /// The number of bytes the requested document would require.
        encoded_bytes: usize,
    },
    /// Existing unreadable content was intentionally preserved.
    PreservedUnreadable(PreservedPreference),
}

/// Crash-safe storage for the native Field's versioned restore intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePreferenceStore {
    path: PathBuf,
}

impl RestorePreferenceStore {
    /// Targets one preference file.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the preference path without interpreting it as Core authority.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the file without changing malformed or unsupported content.
    pub fn read(&self) -> io::Result<RestorePreferenceReadOutcome> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(RestorePreferenceReadOutcome::Missing);
            }
            Err(error) => return Ok(self.unreadable("open", &error)),
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return Ok(self.unreadable("inspect", &error)),
        };
        if metadata.len() > MAX_RESTORE_PREFERENCE_BYTES as u64 {
            return Ok(self.oversized(Some(metadata.len())));
        }
        let initial_capacity = usize::try_from(metadata.len())
            .unwrap_or(MAX_RESTORE_PREFERENCE_BYTES + 1)
            .min(MAX_RESTORE_PREFERENCE_BYTES + 1);
        let mut bytes = Vec::with_capacity(initial_capacity);
        if let Err(error) = Read::by_ref(&mut file)
            .take((MAX_RESTORE_PREFERENCE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
        {
            return Ok(self.unreadable("read", &error));
        }
        if bytes.len() > MAX_RESTORE_PREFERENCE_BYTES {
            return Ok(self.oversized(Some(bytes.len() as u64)));
        }
        Ok(parse_preference(&bytes))
    }

    fn oversized(&self, observed_bytes: Option<u64>) -> RestorePreferenceReadOutcome {
        let observed = observed_bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "more than the configured limit".to_owned());
        RestorePreferenceReadOutcome::Oversized {
            max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
            observed_bytes,
            message: format!(
                "restore preference {} is oversized: observed {observed} bytes, maximum is {} bytes; move or inspect the file before retrying",
                self.path.display(),
                MAX_RESTORE_PREFERENCE_BYTES
            ),
        }
    }

    fn unreadable(&self, operation: &str, error: &io::Error) -> RestorePreferenceReadOutcome {
        RestorePreferenceReadOutcome::Unreadable {
            message: format!(
                "could not {operation} restore preference {}: {error}; fix its ownership, permissions, or file type before retrying",
                self.path.display()
            ),
        }
    }

    /// Writes an exact update, preserving malformed and unsupported existing files.
    pub fn write(
        &self,
        update: &RestorePreferenceUpdate,
    ) -> io::Result<RestorePreferenceWriteOutcome> {
        self.write_with_before_replace(update, |_| Ok(()))
    }

    fn write_with_before_replace<F>(
        &self,
        update: &RestorePreferenceUpdate,
        before_replace: F,
    ) -> io::Result<RestorePreferenceWriteOutcome>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        let selection = match update {
            RestorePreferenceUpdate::ClearSelection => None,
            RestorePreferenceUpdate::Select(selection) => {
                if !valid_identity(selection.plot_id()) || !valid_identity(selection.session_id()) {
                    return Ok(RestorePreferenceWriteOutcome::RejectedUnknownSelection);
                }
                Some(selection)
            }
            RestorePreferenceUpdate::UnknownSelection => {
                return Ok(RestorePreferenceWriteOutcome::RejectedUnknownSelection);
            }
        };
        let bytes = encode_preference(selection)?;
        if bytes.len() > MAX_RESTORE_PREFERENCE_BYTES {
            return Ok(RestorePreferenceWriteOutcome::RejectedOversized {
                max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                encoded_bytes: bytes.len(),
            });
        }

        match self.read()? {
            RestorePreferenceReadOutcome::Malformed { .. } => {
                return Ok(RestorePreferenceWriteOutcome::PreservedUnreadable(
                    PreservedPreference::Malformed,
                ));
            }
            RestorePreferenceReadOutcome::FutureVersion { version } => {
                return Ok(RestorePreferenceWriteOutcome::PreservedUnreadable(
                    PreservedPreference::FutureVersion { version },
                ));
            }
            RestorePreferenceReadOutcome::Oversized {
                max_bytes,
                observed_bytes,
                message,
            } => {
                return Ok(RestorePreferenceWriteOutcome::PreservedUnreadable(
                    PreservedPreference::Oversized {
                        max_bytes,
                        observed_bytes,
                        message,
                    },
                ));
            }
            RestorePreferenceReadOutcome::Unreadable { message } => {
                return Ok(RestorePreferenceWriteOutcome::PreservedUnreadable(
                    PreservedPreference::Unreadable { message },
                ));
            }
            RestorePreferenceReadOutcome::Missing | RestorePreferenceReadOutcome::Ready(_) => {}
        }

        let parent = preference_parent(&self.path)?;
        create_private_parent(parent)?;
        let mut replacement = PrivateReplacement::create(parent, &self.path)?;
        replacement.file.write_all(&bytes)?;
        replacement.file.flush()?;
        replacement.file.sync_all()?;
        before_replace(&replacement.path)?;
        replacement.install(&self.path)?;
        sync_parent_directory(parent)?;
        Ok(RestorePreferenceWriteOutcome::Written)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceDocument {
    kind: String,
    selection: Option<PreferenceSelection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreferenceSelection {
    plot_id: String,
    session_id: String,
}

#[derive(Serialize)]
struct PreferenceDocumentRef<'a> {
    kind: &'static str,
    selection: Option<PreferenceSelectionRef<'a>>,
}

#[derive(Serialize)]
struct PreferenceSelectionRef<'a> {
    plot_id: &'a str,
    session_id: &'a str,
}

fn parse_preference(bytes: &[u8]) -> RestorePreferenceReadOutcome {
    let value = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            return RestorePreferenceReadOutcome::Malformed {
                message: error.to_string(),
            };
        }
    };
    let version = match value
        .as_object()
        .and_then(|object| object.get("kind"))
        .and_then(serde_json::Value::as_str)
    {
        Some(version) => version,
        None => {
            return RestorePreferenceReadOutcome::Malformed {
                message: "restore preference kind must be a string".to_owned(),
            };
        }
    };
    if version != RESTORE_PREFERENCE_KIND {
        return RestorePreferenceReadOutcome::FutureVersion {
            version: version.to_owned(),
        };
    }
    if value
        .as_object()
        .is_none_or(|object| !object.contains_key("selection"))
    {
        return RestorePreferenceReadOutcome::Malformed {
            message: "restore preference selection field is required".to_owned(),
        };
    }

    let document = match serde_json::from_value::<PreferenceDocument>(value) {
        Ok(document) => document,
        Err(error) => {
            return RestorePreferenceReadOutcome::Malformed {
                message: error.to_string(),
            };
        }
    };
    if document.kind != RESTORE_PREFERENCE_KIND {
        return RestorePreferenceReadOutcome::Malformed {
            message: "restore preference kind changed while decoding".to_owned(),
        };
    }
    let selection = match document.selection {
        Some(selection)
            if valid_identity(&selection.plot_id) && valid_identity(&selection.session_id) =>
        {
            Some(ExactSessionSelection::new(
                selection.plot_id,
                selection.session_id,
            ))
        }
        Some(_) => {
            return RestorePreferenceReadOutcome::Malformed {
                message: "restore selection identities must be non-empty".to_owned(),
            };
        }
        None => None,
    };
    RestorePreferenceReadOutcome::Ready(RestorePreference { selection })
}

fn valid_identity(identity: &str) -> bool {
    !identity.trim().is_empty()
}

fn encode_preference(selection: Option<&ExactSessionSelection>) -> io::Result<Vec<u8>> {
    let document = PreferenceDocumentRef {
        kind: RESTORE_PREFERENCE_KIND,
        selection: selection.map(|selection| PreferenceSelectionRef {
            plot_id: selection.plot_id(),
            session_id: selection.session_id(),
        }),
    };
    let mut bytes = serde_json::to_vec_pretty(&document).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn preference_parent(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Ok(parent),
        Some(_) => Ok(Path::new(".")),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "restore preference path must name a file",
        )),
    }
}

fn create_private_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(parent)
    }
}

struct PrivateReplacement {
    path: PathBuf,
    file: File,
    installed: bool,
}

impl PrivateReplacement {
    fn create(parent: &Path, destination: &Path) -> io::Result<Self> {
        let destination_name = destination
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?
            .to_string_lossy();
        for _ in 0..TEMP_CREATE_ATTEMPTS {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce)
                .map_err(|error| io::Error::other(format!("generate temp nonce: {error}")))?;
            let nonce = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = parent.join(format!(".{destination_name}.{nonce}.tmp"));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;

                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        installed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique private preference replacement",
        ))
    }

    fn install(&mut self, destination: &Path) -> io::Result<()> {
        fs::rename(&self.path, destination)?;
        self.installed = true;
        Ok(())
    }
}

impl Drop for PrivateReplacement {
    fn drop(&mut self) {
        if !self.installed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Seek, SeekFrom, Write};

    use tempfile::tempdir;

    use super::*;

    fn exact_selection(plot_id: &str, session_id: &str) -> ExactSessionSelection {
        ExactSessionSelection::new(plot_id, session_id)
    }

    #[test]
    fn parses_exact_v1_document_and_null_selection() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);

        fs::write(
            &path,
            r#"{"kind":"nopal.native_field_preference/v1","selection":{"plot_id":"plot-1","session_id":"session-1"}}"#,
        )
        .unwrap();
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Ready(RestorePreference {
                selection: Some(exact_selection("plot-1", "session-1")),
            })
        );

        fs::write(
            &path,
            r#"{"kind":"nopal.native_field_preference/v1","selection":null}"#,
        )
        .unwrap();
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Ready(RestorePreference { selection: None })
        );
    }

    #[test]
    fn malformed_and_future_documents_are_typed_and_preserved_on_write() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);
        let update = RestorePreferenceUpdate::select(exact_selection("plot-2", "session-2"));

        let malformed =
            br#"{"kind":"nopal.native_field_preference/v1","selection":{"plot_id":"plot-1"}}"#;
        fs::write(&path, malformed).unwrap();
        assert!(matches!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Malformed { .. }
        ));
        assert_eq!(
            store.write(&update).unwrap(),
            RestorePreferenceWriteOutcome::PreservedUnreadable(PreservedPreference::Malformed)
        );
        assert_eq!(fs::read(&path).unwrap(), malformed);

        let future =
            br#"{"kind":"nopal.native_field_preference/v2","selection":{"new_shape":true}}"#;
        fs::write(&path, future).unwrap();
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::FutureVersion {
                version: "nopal.native_field_preference/v2".to_owned(),
            }
        );
        assert_eq!(
            store.write(&update).unwrap(),
            RestorePreferenceWriteOutcome::PreservedUnreadable(
                PreservedPreference::FutureVersion {
                    version: "nopal.native_field_preference/v2".to_owned(),
                }
            )
        );
        assert_eq!(fs::read(&path).unwrap(), future);
    }

    #[test]
    fn exact_v1_parser_rejects_unknown_fields_and_empty_identities() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);

        for malformed in [
            r#"{"kind":"nopal.native_field_preference/v1"}"#,
            r#"{"kind":"nopal.native_field_preference/v1","selection":null,"title":"copy"}"#,
            r#"{"kind":"nopal.native_field_preference/v1","selection":{"plot_id":"","session_id":"session-1"}}"#,
            r#"{"kind":"nopal.native_field_preference/v1","selection":{"plot_id":"plot-1","session_id":""}}"#,
        ] {
            fs::write(&path, malformed).unwrap();
            assert!(matches!(
                store.read().unwrap(),
                RestorePreferenceReadOutcome::Malformed { .. }
            ));
        }
    }

    #[test]
    fn unknown_selection_is_rejected_without_touching_the_store() {
        let root = tempdir().unwrap();
        let path = root.path().join("missing").join("restore.json");
        let store = RestorePreferenceStore::new(&path);

        assert_eq!(
            store
                .write(&RestorePreferenceUpdate::UnknownSelection)
                .unwrap(),
            RestorePreferenceWriteOutcome::RejectedUnknownSelection
        );
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn replacement_is_complete_and_uses_a_private_temp_in_the_same_directory() {
        let root = tempdir().unwrap();
        let parent = root.path().join("private").join("native");
        let path = parent.join("restore.json");
        let store = RestorePreferenceStore::new(&path);
        store
            .write(&RestorePreferenceUpdate::select(exact_selection(
                "plot-old",
                "session-old",
            )))
            .unwrap();

        let mut observed_temp = None;
        let result = store.write_with_before_replace(
            &RestorePreferenceUpdate::select(exact_selection("plot-new", "session-new")),
            |temp| {
                assert_eq!(temp.parent(), path.parent());
                assert_ne!(temp, path);
                assert_eq!(
                    store.read().unwrap(),
                    RestorePreferenceReadOutcome::Ready(RestorePreference {
                        selection: Some(exact_selection("plot-old", "session-old")),
                    })
                );
                let staged = fs::read(temp).unwrap();
                assert!(staged.ends_with(b"\n"));
                assert!(staged.windows(b"plot-new".len()).any(|w| w == b"plot-new"));
                observed_temp = Some(temp.to_owned());
                Ok(())
            },
        );

        assert_eq!(result.unwrap(), RestorePreferenceWriteOutcome::Written);
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Ready(RestorePreference {
                selection: Some(exact_selection("plot-new", "session-new")),
            })
        );
        assert!(!observed_temp.unwrap().exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(fs::metadata(&parent).unwrap().mode() & 0o777, 0o700);
        }
    }

    #[test]
    fn interrupted_write_preserves_the_prior_complete_document() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);
        store
            .write(&RestorePreferenceUpdate::select(exact_selection(
                "plot-old",
                "session-old",
            )))
            .unwrap();
        let complete_before = fs::read(&path).unwrap();
        let mut staged_path = None;

        let error = store
            .write_with_before_replace(
                &RestorePreferenceUpdate::select(exact_selection("plot-new", "session-new")),
                |temp| {
                    staged_path = Some(temp.to_owned());
                    Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "fault injection",
                    ))
                },
            )
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(&path).unwrap(), complete_before);
        assert!(!staged_path.unwrap().exists());
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Ready(RestorePreference {
                selection: Some(exact_selection("plot-old", "session-old")),
            })
        );
    }

    #[test]
    fn clear_selection_writes_exact_null_document() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);

        assert_eq!(
            store
                .write(&RestorePreferenceUpdate::ClearSelection)
                .unwrap(),
            RestorePreferenceWriteOutcome::Written
        );
        assert_eq!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Ready(RestorePreference { selection: None })
        );
        let exact = fs::read_to_string(&path).unwrap();
        assert!(exact.contains(r#""kind": "nopal.native_field_preference/v1""#));
        assert!(!exact.contains(r#""version""#));
    }

    #[cfg(unix)]
    #[test]
    fn existing_preference_directory_is_tightened_to_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempdir().unwrap();
        let parent = root.path().join("native");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        let store = RestorePreferenceStore::new(parent.join("restore.json"));

        assert_eq!(
            store
                .write(&RestorePreferenceUpdate::ClearSelection)
                .unwrap(),
            RestorePreferenceWriteOutcome::Written
        );
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn oversized_sparse_document_is_bounded_typed_and_preserved() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);
        let oversized_len = MAX_RESTORE_PREFERENCE_BYTES as u64 + 256 * 1024 * 1024;
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(b"original-prefix").unwrap();
        file.seek(SeekFrom::Start(oversized_len - 1)).unwrap();
        file.write_all(b"z").unwrap();
        file.sync_all().unwrap();

        let before = fs::metadata(&path).unwrap();
        let read = store.read().unwrap();
        assert!(matches!(
            read,
            RestorePreferenceReadOutcome::Oversized {
                max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                observed_bytes: Some(observed),
                ref message,
            } if observed == oversized_len && message.contains("restore preference")
        ));
        assert!(matches!(
            store
                .write(&RestorePreferenceUpdate::ClearSelection)
                .unwrap(),
            RestorePreferenceWriteOutcome::PreservedUnreadable(
                PreservedPreference::Oversized {
                    max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                    observed_bytes: Some(observed),
                    ref message,
                }
            ) if observed == oversized_len && message.contains("move or inspect")
        ));
        let after = fs::metadata(&path).unwrap();
        assert_eq!(after.len(), before.len());
        let mut preserved = fs::File::open(&path).unwrap();
        let mut prefix = [0_u8; 15];
        preserved.read_exact(&mut prefix).unwrap();
        assert_eq!(prefix, *b"original-prefix");
    }

    #[test]
    fn encoded_document_over_limit_is_rejected_without_mutation() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        let store = RestorePreferenceStore::new(&path);
        let selection = exact_selection(
            &"p".repeat(MAX_RESTORE_PREFERENCE_BYTES),
            "session-oversized",
        );

        assert!(matches!(
            store
                .write(&RestorePreferenceUpdate::select(selection))
                .unwrap(),
            RestorePreferenceWriteOutcome::RejectedOversized {
                max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                encoded_bytes,
            } if encoded_bytes > MAX_RESTORE_PREFERENCE_BYTES
        ));
        assert!(!path.exists());
    }

    #[test]
    fn unreadable_document_is_typed_and_preserved() {
        let root = tempdir().unwrap();
        let path = root.path().join("restore.json");
        fs::create_dir(&path).unwrap();
        let store = RestorePreferenceStore::new(&path);

        assert!(matches!(
            store.read().unwrap(),
            RestorePreferenceReadOutcome::Unreadable { ref message }
                if message.contains("restore preference")
        ));
        assert!(matches!(
            store
                .write(&RestorePreferenceUpdate::ClearSelection)
                .unwrap(),
            RestorePreferenceWriteOutcome::PreservedUnreadable(
                PreservedPreference::Unreadable { ref message }
            ) if message.contains("ownership, permissions, or file type")
        ));
        assert!(path.is_dir());
    }
}
