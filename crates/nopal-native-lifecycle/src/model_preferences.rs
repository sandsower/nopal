//! Versioned desktop-only recency preferences for Pi model choices.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use nopal_feed_client::session::{MAX_SESSION_IDENTITY_BYTES, SessionModelReference};
use serde::{Deserialize, Serialize};

const MODEL_RECENTS_KIND: &str = "nopal.native_model_recents/v1";
const TEMP_CREATE_ATTEMPTS: usize = 32;

pub const MAX_MODEL_RECENTS: usize = 32;
pub const MAX_MODEL_RECENTS_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRecentsReadOutcome {
    Missing,
    Ready(Vec<SessionModelReference>),
    Malformed { message: String },
    FutureVersion { version: String },
    Oversized { observed_bytes: Option<u64> },
    Unreadable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRecentsWriteOutcome {
    Written,
    PreservedExisting(ModelRecentsReadOutcome),
}

/// Crash-safe storage for UI ordering only.
///
/// Pi remains authoritative for both the current model and the set of models
/// that can be selected in a Session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecentsStore {
    path: PathBuf,
}

impl ModelRecentsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> io::Result<ModelRecentsReadOutcome> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ModelRecentsReadOutcome::Missing);
            }
            Err(error) => return Ok(self.unreadable("open", &error)),
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return Ok(self.unreadable("inspect", &error)),
        };
        if metadata.len() > MAX_MODEL_RECENTS_BYTES as u64 {
            return Ok(ModelRecentsReadOutcome::Oversized {
                observed_bytes: Some(metadata.len()),
            });
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(MAX_MODEL_RECENTS_BYTES + 1)
                .min(MAX_MODEL_RECENTS_BYTES + 1),
        );
        if let Err(error) = Read::by_ref(&mut file)
            .take((MAX_MODEL_RECENTS_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
        {
            return Ok(self.unreadable("read", &error));
        }
        if bytes.len() > MAX_MODEL_RECENTS_BYTES {
            return Ok(ModelRecentsReadOutcome::Oversized {
                observed_bytes: Some(bytes.len() as u64),
            });
        }
        Ok(parse_recents(&bytes))
    }

    pub fn write(&self, recents: &[SessionModelReference]) -> io::Result<ModelRecentsWriteOutcome> {
        let recents = canonical_recents(recents);
        let mut bytes = serde_json::to_vec_pretty(&ModelRecentsDocumentRef {
            kind: MODEL_RECENTS_KIND,
            recents: &recents,
        })
        .map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_MODEL_RECENTS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "model recency preference exceeds its wire limit",
            ));
        }

        match self.read()? {
            ModelRecentsReadOutcome::Missing | ModelRecentsReadOutcome::Ready(_) => {}
            preserved => return Ok(ModelRecentsWriteOutcome::PreservedExisting(preserved)),
        }

        let parent = preference_parent(&self.path)?;
        create_private_parent(parent)?;
        let mut replacement = PrivateReplacement::create(parent, &self.path)?;
        replacement.file.write_all(&bytes)?;
        replacement.file.flush()?;
        replacement.file.sync_all()?;
        replacement.install(&self.path)?;
        sync_parent_directory(parent)?;
        Ok(ModelRecentsWriteOutcome::Written)
    }

    fn unreadable(&self, operation: &str, error: &io::Error) -> ModelRecentsReadOutcome {
        ModelRecentsReadOutcome::Unreadable {
            message: format!(
                "could not {operation} model recency preference {}: {error}",
                self.path.display()
            ),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelRecentsDocument {
    kind: String,
    recents: Vec<StoredModelReference>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredModelReference {
    provider: String,
    id: String,
}

#[derive(Serialize)]
struct ModelRecentsDocumentRef<'a> {
    kind: &'static str,
    recents: &'a [StoredModelReference],
}

fn parse_recents(bytes: &[u8]) -> ModelRecentsReadOutcome {
    let value = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            return ModelRecentsReadOutcome::Malformed {
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
            return ModelRecentsReadOutcome::Malformed {
                message: "model recency preference kind must be a string".to_owned(),
            };
        }
    };
    if version != MODEL_RECENTS_KIND {
        return ModelRecentsReadOutcome::FutureVersion {
            version: version.to_owned(),
        };
    }
    let document = match serde_json::from_value::<ModelRecentsDocument>(value) {
        Ok(document) => document,
        Err(error) => {
            return ModelRecentsReadOutcome::Malformed {
                message: error.to_string(),
            };
        }
    };
    if document.kind != MODEL_RECENTS_KIND || document.recents.len() > MAX_MODEL_RECENTS {
        return ModelRecentsReadOutcome::Malformed {
            message: "model recency preference has an invalid kind or too many entries".to_owned(),
        };
    }
    let mut seen = BTreeSet::new();
    let mut recents = Vec::with_capacity(document.recents.len());
    for model in document.recents {
        if !valid_identity(&model.provider)
            || !valid_identity(&model.id)
            || !seen.insert((model.provider.clone(), model.id.clone()))
        {
            return ModelRecentsReadOutcome::Malformed {
                message: "model recency entries must be unique, non-empty identities".to_owned(),
            };
        }
        recents.push(SessionModelReference {
            provider: model.provider,
            id: model.id,
            extra: Default::default(),
        });
    }
    ModelRecentsReadOutcome::Ready(recents)
}

fn canonical_recents(recents: &[SessionModelReference]) -> Vec<StoredModelReference> {
    let mut seen = BTreeSet::new();
    recents
        .iter()
        .filter(|model| valid_identity(&model.provider) && valid_identity(&model.id))
        .filter(|model| seen.insert((model.provider.clone(), model.id.clone())))
        .take(MAX_MODEL_RECENTS)
        .map(|model| StoredModelReference {
            provider: model.provider.clone(),
            id: model.id.clone(),
        })
        .collect()
}

fn valid_identity(identity: &str) -> bool {
    !identity.trim().is_empty() && identity.len() <= MAX_SESSION_IDENTITY_BYTES
}

fn preference_parent(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Ok(parent),
        Some(_) => Ok(Path::new(".")),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "model recency preference path must name a file",
        )),
    }
}

fn create_private_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
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
            "could not create a unique model recency preference replacement",
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
    use super::*;
    use tempfile::tempdir;

    fn model(provider: &str, id: &str) -> SessionModelReference {
        SessionModelReference {
            provider: provider.to_owned(),
            id: id.to_owned(),
            extra: Default::default(),
        }
    }

    #[test]
    fn missing_round_trip_and_bounded_deduplication() {
        let directory = tempdir().expect("create preference sandbox");
        let store = ModelRecentsStore::new(directory.path().join("model-recents.json"));
        assert_eq!(store.read().unwrap(), ModelRecentsReadOutcome::Missing);

        let mut input = vec![model("pi", "new"), model("pi", "new")];
        input.extend((0..40).map(|index| model("pi", &format!("model-{index}"))));
        assert_eq!(
            store.write(&input).unwrap(),
            ModelRecentsWriteOutcome::Written
        );
        let ModelRecentsReadOutcome::Ready(recents) = store.read().unwrap() else {
            panic!("written preference should decode");
        };
        assert_eq!(recents.len(), MAX_MODEL_RECENTS);
        assert_eq!(recents[0], model("pi", "new"));
        assert_eq!(recents[1], model("pi", "model-0"));
    }

    #[test]
    fn malformed_future_and_oversized_sources_are_preserved() {
        let directory = tempdir().expect("create preference sandbox");
        for (name, bytes) in [
            ("malformed.json", b"not json".to_vec()),
            (
                "future.json",
                br#"{"kind":"nopal.native_model_recents/v2","recents":[]}"#.to_vec(),
            ),
            ("oversized.json", vec![b'x'; MAX_MODEL_RECENTS_BYTES + 1]),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, &bytes).unwrap();
            let store = ModelRecentsStore::new(&path);
            assert!(matches!(
                store.write(&[model("pi", "safe")]).unwrap(),
                ModelRecentsWriteOutcome::PreservedExisting(_)
            ));
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
}
