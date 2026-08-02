//! STT model catalog and downloader.
//!
//! Holds the hard-coded catalog of speech-to-text models (whisper.cpp ggml
//! models and sherpa-onnx transducer models, both from Hugging Face), tracks
//! which ones are installed under `data_dir/models/`, and performs
//! checksum-verified downloads. A catalog entry is one or more artifacts: a
//! whisper model is a single `.bin` file, and a model with several artifacts
//! (e.g. a sherpa-onnx transducer's encoder, decoder, joiner and tokens) is
//! installed as a directory holding all of them. Each artifact streams to
//! its own `.part` file while its sha256 is computed incrementally, and a
//! model only becomes visible at its final path once every one of its
//! artifacts has verified — a checksum mismatch or an interrupted download
//! always leaves the staging area cleaned up rather than reporting a
//! half-installed model.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::IntegrityError;

/// A speech-to-text model available for download, with its installed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub engine: String,
    pub label: String,
    pub size_mb: u32,
    pub installed: bool,
}

/// One downloadable file that makes up a catalog entry.
///
/// `name` is the file name the artifact is installed under (directly inside
/// `models/` for a single-artifact entry, or inside the entry's model
/// directory for a multi-artifact one) — it is independent of `url`, since a
/// remote file's name is not always the one it should be installed as.
#[derive(Debug, Clone, Copy)]
struct Artifact {
    url: &'static str,
    sha256: &'static str,
    name: &'static str,
    /// The artifact's exact size once fully downloaded, used by
    /// [`ModelManager::verify_installed`] to catch a truncated file before
    /// its path is ever handed to a native engine. This is a size check,
    /// not a re-hash: it catches truncation — the failure mode actually
    /// observed in practice — but not a file of the right length with
    /// corrupted bytes inside it; that class of corruption is what the
    /// download path's sha256 verification already guards against, and
    /// re-hashing hundreds of megabytes on every engine load would trade a
    /// cheap, load-time-only guard for a slow one that mostly re-checks
    /// what already passed once.
    size_bytes: u64,
}

/// Static metadata for one catalog entry.
///
/// `engine` distinguishes the STT engine a model belongs to; installation
/// itself is driven purely by artifact count: a single file directly under
/// `models/` when there is exactly one artifact (e.g. `"whisper"`), or a
/// directory named after the entry's `id` holding every artifact when there
/// is more than one (e.g. a sherpa-onnx transducer's encoder, decoder,
/// joiner and tokens).
#[derive(Debug, Clone, Copy)]
struct CatalogEntry {
    id: &'static str,
    engine: &'static str,
    label: &'static str,
    size_mb: u32,
    artifacts: &'static [Artifact],
}

/// The hard-coded catalog of downloadable speech-to-text models.
///
/// Whisper sha256 and size_bytes values were read from the Hugging Face tree
/// API for `ggerganov/whisper.cpp` (`lfs.oid` and `size` per file).
/// Sherpa-onnx sha256 and size_bytes values were read from the Hugging Face
/// tree API for each model's repository at the pinned revision in its
/// artifact URLs.
const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "tiny",
        engine: "whisper",
        label: "Whisper Tiny",
        size_mb: 74,
        artifacts: &[Artifact {
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
            sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
            name: "ggml-tiny.bin",
            size_bytes: 77_691_713,
        }],
    },
    CatalogEntry {
        id: "base",
        engine: "whisper",
        label: "Whisper Base",
        size_mb: 141,
        artifacts: &[Artifact {
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
            name: "ggml-base.bin",
            size_bytes: 147_951_465,
        }],
    },
    CatalogEntry {
        id: "small",
        engine: "whisper",
        label: "Whisper Small",
        size_mb: 465,
        artifacts: &[Artifact {
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
            sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
            name: "ggml-small.bin",
            size_bytes: 487_601_967,
        }],
    },
    CatalogEntry {
        id: "medium",
        engine: "whisper",
        label: "Whisper Medium",
        size_mb: 1463,
        artifacts: &[Artifact {
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
            sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
            name: "ggml-medium.bin",
            size_bytes: 1_533_763_059,
        }],
    },
    CatalogEntry {
        id: "large-v3-turbo-q5_0",
        engine: "whisper",
        label: "Whisper Large v3 Turbo (q5_0)",
        size_mb: 547,
        artifacts: &[Artifact {
            url:
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
            sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
            name: "ggml-large-v3-turbo-q5_0.bin",
            size_bytes: 574_041_195,
        }],
    },
    CatalogEntry {
        id: "gigaam-v3-e2e-rnnt",
        engine: "sherpa",
        label: "GigaAM-v3 (Russian)",
        size_mb: 221,
        artifacts: &[
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/a6039be7cee829a9044a69ac0ebaf1c191217c97/encoder.int8.onnx",
                sha256: "369f35a71bf288d3b8e0391fabd8dba5f2314088d440bca474056b7b4b6e66bf",
                name: "encoder.int8.onnx",
                size_bytes: 224_570_820,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/a6039be7cee829a9044a69ac0ebaf1c191217c97/decoder.onnx",
                sha256: "38fc7475443ea2a26f63211ca350f73ac50fff824ab7a3876ee2bd610c53bbc4",
                name: "decoder.onnx",
                size_bytes: 4_600_132,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/a6039be7cee829a9044a69ac0ebaf1c191217c97/joiner.onnx",
                sha256: "602ff7017a93311aad34df1437c8d7f49911353c13d6eae7a6ee7b041339465c",
                name: "joiner.onnx",
                size_bytes: 2_712_896,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16/resolve/a6039be7cee829a9044a69ac0ebaf1c191217c97/tokens.txt",
                sha256: "39abae20e692998290c574e606f11a9edef2902a1995463fcff63d1490cf22b7",
                name: "tokens.txt",
                size_bytes: 13_354,
            },
        ],
    },
    CatalogEntry {
        id: "parakeet-tdt-110m-en",
        engine: "sherpa",
        label: "Parakeet TDT (English)",
        size_mb: 455,
        artifacts: &[
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000/resolve/e9bea5a06247dc3f55319ff23d34b0328f2f5ddf/encoder.onnx",
                sha256: "db260f1073c654c37dd65006885d1ee98ff16c22463b1ef992bbcabc29780a3f",
                name: "encoder.onnx",
                size_bytes: 456_050_698,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000/resolve/e9bea5a06247dc3f55319ff23d34b0328f2f5ddf/decoder.onnx",
                sha256: "3da156bde41a04c94ef783e0bd92928e9974e08645b976a22d0c3e1063510249",
                name: "decoder.onnx",
                size_bytes: 15_753_086,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000/resolve/e9bea5a06247dc3f55319ff23d34b0328f2f5ddf/joiner.onnx",
                sha256: "b603765c0724a0768c378a23326dabbeb9cfea932d260e4fcc14384fa5fd5aff",
                name: "joiner.onnx",
                size_bytes: 5_596_854,
            },
            Artifact {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-parakeet_tdt_transducer_110m-en-36000/resolve/e9bea5a06247dc3f55319ff23d34b0328f2f5ddf/tokens.txt",
                sha256: "450e56bd2f036fe5b6aa821865838cc5aa9d8b0106134ce9a9ba0664abe6cd10",
                name: "tokens.txt",
                size_bytes: 9_953,
            },
        ],
    },
];

/// Manages the local install state of the speech-to-text model catalog:
/// lists what is available, resolves installed paths, and performs
/// checksum-verified downloads and removal.
pub struct ModelManager {
    data_dir: PathBuf,
    catalog: Vec<CatalogEntry>,
}

impl ModelManager {
    /// Creates a manager whose models live under `data_dir/models/`.
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            catalog: CATALOG.to_vec(),
        }
    }

    /// Test-only constructor: builds a manager against a custom catalog
    /// (typically pointing at a mock server) instead of the real, hard-coded
    /// one, so tests never hit the network.
    #[cfg(test)]
    fn with_catalog(data_dir: PathBuf, catalog: Vec<CatalogEntry>) -> Self {
        Self { data_dir, catalog }
    }

    /// Lists every catalog entry, each annotated with whether it is
    /// currently installed under this manager's `data_dir`.
    pub fn catalog(&self) -> Vec<ModelInfo> {
        self.catalog
            .iter()
            .map(|entry| {
                let path = self.install_path(entry);
                ModelInfo {
                    id: entry.id.to_string(),
                    engine: entry.engine.to_string(),
                    label: entry.label.to_string(),
                    size_mb: entry.size_mb,
                    installed: self.is_installed(entry, &path),
                }
            })
            .collect()
    }

    /// Returns the installed path for `id` (a file for single-artifact
    /// models, a directory for multi-artifact models), or `None` if
    /// it is unknown, or not installed. A multi-artifact model is only
    /// reported as installed once every one of its artifacts is present —
    /// a partially downloaded model must never report as ready.
    pub fn path_for(&self, id: &str) -> Option<PathBuf> {
        let entry = self.find(id)?;
        let path = self.install_path(entry);
        self.is_installed(entry, &path).then_some(path)
    }

    /// Downloads and installs the model identified by `id`.
    ///
    /// Each artifact's response body streams into a `.part` file inside a
    /// fresh staging area while its sha256 is computed incrementally;
    /// `progress(done, total)` is called after every chunk of every artifact,
    /// with `total` the catalog's grand total across every artifact in the
    /// entry (known upfront from each artifact's `size_bytes`, independent of
    /// whatever the server reports as `Content-Length`) and `done` running
    /// cumulatively across artifacts, never resetting to zero partway through
    /// a multi-artifact model. An artifact's digest is checked against its
    /// catalog sha256 as soon as it finishes downloading: on the first
    /// mismatch, or if any body is interrupted, the whole staging area is
    /// removed and an error returned, so no half-downloaded model is ever
    /// left where [`Self::path_for`] would find it.
    ///
    /// Only once every artifact has verified does the model become visible
    /// at its final path: a single-artifact model (e.g. whisper) is renamed
    /// directly into place as one file, and a multi-artifact model (e.g. a
    /// sherpa-onnx entry) has its whole staging directory renamed into place
    /// at once.
    pub fn download(&self, id: &str, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf> {
        let entry = *self
            .find(id)
            .ok_or_else(|| anyhow!("unknown model id: {id}"))?;

        let models_dir = self.models_dir();
        fs::create_dir_all(&models_dir)
            .with_context(|| format!("failed to create {}", models_dir.display()))?;

        self.download_artifacts(id, &entry, &models_dir, progress)
    }

    /// Downloads and verifies every artifact of `entry` into a staging
    /// directory, then puts it in its final place: a single file when there
    /// is exactly one artifact, or the whole directory when there are
    /// several. See [`Self::download`] for the staging/verification
    /// contract.
    fn download_artifacts(
        &self,
        id: &str,
        entry: &CatalogEntry,
        models_dir: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<PathBuf> {
        let Some((first, _)) = entry.artifacts.split_first() else {
            bail!("model '{id}' has no artifacts defined");
        };

        let staging_dir = models_dir.join(format!("{}.staging", entry.id));
        let _ = fs::remove_dir_all(&staging_dir);
        fs::create_dir_all(&staging_dir)
            .with_context(|| format!("failed to create {}", staging_dir.display()))?;

        // The catalog already knows every artifact's exact size, so the
        // grand total is fixed up front and does not depend on the server's
        // `Content-Length`. Reporting `completed + done` against that fixed
        // total (rather than passing each artifact's own `stream_to_part`
        // progress straight through) keeps the sequence handed to `progress`
        // monotonic across the whole model, instead of resetting to zero at
        // the start of every artifact.
        let grand_total: u64 = entry.artifacts.iter().map(|a| a.size_bytes).sum();
        let mut completed: u64 = 0;
        for artifact in entry.artifacts {
            let mut aggregate = |done: u64, _total: u64| progress(completed + done, grand_total);
            if let Err(err) = stage_one_artifact(id, artifact, &staging_dir, &mut aggregate) {
                let _ = fs::remove_dir_all(&staging_dir);
                return Err(err);
            }
            completed += artifact.size_bytes;
        }

        let final_path = self.install_path(entry);
        if entry.artifacts.len() > 1 {
            if final_path.exists() {
                fs::remove_dir_all(&final_path).with_context(|| {
                    format!(
                        "failed to remove previous install at {}",
                        final_path.display()
                    )
                })?;
            }
            fs::rename(&staging_dir, &final_path).with_context(|| {
                format!(
                    "failed to move {} into {}",
                    staging_dir.display(),
                    final_path.display()
                )
            })?;
        } else {
            let staged_file = staging_dir.join(first.name);
            let renamed = fs::rename(&staged_file, &final_path).with_context(|| {
                format!(
                    "failed to move {} into {}",
                    staged_file.display(),
                    final_path.display()
                )
            });
            let _ = fs::remove_dir_all(&staging_dir);
            renamed?;
        }

        Ok(final_path)
    }

    /// Removes the installed model identified by `id`, if present. A no-op
    /// (not an error) if the model is unknown or not installed.
    pub fn remove(&self, id: &str) -> Result<()> {
        let Some(entry) = self.find(id) else {
            return Ok(());
        };

        let path = self.install_path(entry);
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to remove directory {}", path.display()))?;
        } else if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove file {}", path.display()))?;
        }

        Ok(())
    }

    fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    fn find(&self, id: &str) -> Option<&CatalogEntry> {
        self.catalog.iter().find(|entry| entry.id == id)
    }

    /// The final on-disk path a catalog entry installs to: a directory named
    /// after the entry's `id` for models with more than one artifact, or a
    /// single file (the artifact's `name`) otherwise.
    fn install_path(&self, entry: &CatalogEntry) -> PathBuf {
        if entry.artifacts.len() > 1 {
            self.models_dir().join(entry.id)
        } else {
            let name = entry.artifacts.first().map_or(entry.id, |a| a.name);
            self.models_dir().join(name)
        }
    }

    /// Whether every artifact of `entry` is present at `path`, the value
    /// returned by [`Self::install_path`] for it.
    fn is_installed(&self, entry: &CatalogEntry, path: &Path) -> bool {
        entry
            .artifacts
            .iter()
            .all(|a| artifact_path(entry, path, a).is_file())
    }

    /// Verifies that every artifact of the installed model `id` is present
    /// and has exactly the byte length recorded in the catalog, returning
    /// its installed path (the same one [`Self::path_for`] would) only if
    /// every check passes.
    ///
    /// This exists because a corrupt model file does not fail cleanly once
    /// handed to the native speech engine: sherpa-onnx's token-table parser
    /// calls a compiled-in `exit()` on a malformed `tokens.txt`, and a
    /// malformed `.onnx` file makes onnxruntime throw a C++ exception that
    /// unwinds across the FFI boundary uncaught. Neither is catchable in
    /// Rust — both abort the whole process with nothing logged and no
    /// notice shown. Verifying before the path is ever handed to the native
    /// layer is the only point at which this can be prevented rather than
    /// merely reported.
    ///
    /// The check is file size, not a checksum: hashing every artifact (up
    /// to several hundred megabytes) on every engine load would add real
    /// latency to every app start and every language switch, to guard
    /// against corruption that the download path already checksums
    /// against. A size mismatch is the failure actually observed in
    /// practice — an interrupted download leaving a file of the right name
    /// and the wrong length — but this is not a complete integrity check:
    /// a file of the correct size with corrupted bytes inside it still
    /// passes.
    pub fn verify_installed(&self, id: &str) -> Result<PathBuf, IntegrityError> {
        let entry = self
            .find(id)
            .ok_or_else(|| IntegrityError::UnknownModel(id.to_string()))?;
        let path = self.install_path(entry);
        if !self.is_installed(entry, &path) {
            return Err(IntegrityError::NotInstalled(id.to_string()));
        }

        for artifact in entry.artifacts {
            let artifact_path = artifact_path(entry, &path, artifact);
            let metadata = fs::metadata(&artifact_path).map_err(|source| IntegrityError::Io {
                model: id.to_string(),
                artifact: artifact.name.to_string(),
                path: artifact_path.clone(),
                source,
            })?;
            let actual = metadata.len();
            if actual != artifact.size_bytes {
                return Err(IntegrityError::SizeMismatch {
                    model: id.to_string(),
                    artifact: artifact.name.to_string(),
                    path: artifact_path,
                    expected: artifact.size_bytes,
                    actual,
                });
            }
        }

        Ok(path)
    }
}

/// Resolves the on-disk path of one artifact of `entry`, given the entry's
/// `install_path`: the artifact itself for a single-artifact entry (where
/// `install_path` is already a file), or `install_path` joined with the
/// artifact's name for a multi-artifact entry (where `install_path` is a
/// directory).
fn artifact_path(entry: &CatalogEntry, install_path: &Path, artifact: &Artifact) -> PathBuf {
    if entry.artifacts.len() > 1 {
        install_path.join(artifact.name)
    } else {
        install_path.to_path_buf()
    }
}

/// Downloads and verifies one artifact into `staging_dir`, leaving it at
/// `staging_dir/<artifact.name>` on success. The `.part` suffix is only used
/// while the body is in flight and the checksum is unconfirmed.
fn stage_one_artifact(
    id: &str,
    artifact: &Artifact,
    staging_dir: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let part_path = staging_dir.join(format!("{}.part", artifact.name));
    let digest = stream_to_part(artifact.url, &part_path, progress)?;
    if digest != artifact.sha256 {
        bail!(
            "checksum mismatch for model '{id}' artifact '{}': expected {}, got {digest}",
            artifact.name,
            artifact.sha256
        );
    }

    let final_in_staging = staging_dir.join(artifact.name);
    fs::rename(&part_path, &final_in_staging).with_context(|| {
        format!(
            "failed to move {} into {}",
            part_path.display(),
            final_in_staging.display()
        )
    })
}

/// Streams the HTTP body at `url` into `part_path`, reporting `(done,
/// total)` progress as chunks arrive, and returns the hex-encoded sha256 of
/// the bytes received.
fn stream_to_part(
    url: &str,
    part_path: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<String> {
    let mut response =
        reqwest::blocking::get(url).with_context(|| format!("failed to request {url}"))?;
    if !response.status().is_success() {
        bail!("download of {url} failed with status {}", response.status());
    }
    let total = response.content_length().unwrap_or(0);

    let mut file = File::create(part_path)
        .with_context(|| format!("failed to create {}", part_path.display()))?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut buf = [0u8; 64 * 1024];

    progress(0, total);
    loop {
        let n = response
            .read(&mut buf)
            .context("failed reading response body")?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .context("failed writing to part file")?;
        done += n as u64;
        progress(done, total);
    }
    file.sync_all()
        .with_context(|| format!("failed to flush {}", part_path.display()))?;

    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Leaks a `Vec<Artifact>` into a `&'static [Artifact]`, mirroring how
    /// the real catalog's entries are `'static` data, so test entries can be
    /// built from owned `String`s (e.g. a mock server's URI).
    fn leak_artifacts(artifacts: Vec<Artifact>) -> &'static [Artifact] {
        Box::leak(artifacts.into_boxed_slice())
    }

    fn whisper_entry(url: String, sha256: String, size_bytes: u64) -> CatalogEntry {
        CatalogEntry {
            id: "test-whisper",
            engine: "whisper",
            label: "Test Whisper",
            size_mb: 1,
            artifacts: leak_artifacts(vec![Artifact {
                url: Box::leak(url.into_boxed_str()),
                sha256: Box::leak(sha256.into_boxed_str()),
                name: "ggml-test.bin",
                size_bytes,
            }]),
        }
    }

    /// A two-artifact entry (e.g. mirroring a sherpa-onnx model's encoder
    /// and tokens) whose artifacts are never actually downloaded in the
    /// tests that use it — only `path_for`'s and `verify_installed`'s
    /// "every file present" / "every size matches" logic is exercised, so
    /// the URLs are unused placeholders. `encoder.onnx` is expected to be
    /// 100 bytes and `tokens.txt` 50 once genuinely installed.
    fn two_file_entry() -> CatalogEntry {
        CatalogEntry {
            id: "two-file-model",
            engine: "sherpa",
            label: "Test Two-File Model",
            size_mb: 1,
            artifacts: &[
                Artifact {
                    url: "unused",
                    sha256: "unused",
                    name: "encoder.onnx",
                    size_bytes: 100,
                },
                Artifact {
                    url: "unused",
                    sha256: "unused",
                    name: "tokens.txt",
                    size_bytes: 50,
                },
            ],
        }
    }

    fn multi_artifact_entry(
        encoder_url: String,
        encoder_sha256: String,
        encoder_size_bytes: u64,
        tokens_url: String,
        tokens_sha256: String,
        tokens_size_bytes: u64,
    ) -> CatalogEntry {
        CatalogEntry {
            id: "test-multi",
            engine: "sherpa",
            label: "Test Multi-Artifact",
            size_mb: 1,
            artifacts: leak_artifacts(vec![
                Artifact {
                    url: Box::leak(encoder_url.into_boxed_str()),
                    sha256: Box::leak(encoder_sha256.into_boxed_str()),
                    name: "encoder.onnx",
                    size_bytes: encoder_size_bytes,
                },
                Artifact {
                    url: Box::leak(tokens_url.into_boxed_str()),
                    sha256: Box::leak(tokens_sha256.into_boxed_str()),
                    name: "tokens.txt",
                    size_bytes: tokens_size_bytes,
                },
            ]),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn download_streams_reports_progress_and_verifies_checksum() {
        let server = MockServer::start().await;
        let body = vec![0x42u8; 200_000];
        let sha256 = sha256_hex(&body);

        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry(
            format!("{}/ggml-test.bin", server.uri()),
            sha256.clone(),
            body.len() as u64,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let (result, calls) = tokio::task::spawn_blocking(move || {
            let mut calls: Vec<(u64, u64)> = Vec::new();
            let result =
                manager.download("test-whisper", &mut |done, total| calls.push((done, total)));
            (result, calls)
        })
        .await
        .expect("blocking task panicked");

        let installed_path = result.expect("download should succeed");
        assert_eq!(installed_path, dir.path().join("models/ggml-test.bin"));
        let installed_bytes = fs::read(&installed_path).expect("read installed file");
        assert_eq!(installed_bytes, body);

        // Progress must actually be observable through the public `download`
        // API, not just through the private `stream_to_part` helper.
        assert!(!calls.is_empty(), "expected at least one progress call");
        assert_eq!(calls.first(), Some(&(0, body.len() as u64)));
        assert_eq!(calls.last(), Some(&(body.len() as u64, body.len() as u64)));
        for pair in calls.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "progress must never decrease: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn progress_is_monotonically_non_decreasing_and_ends_at_total() {
        let dir = tempfile::tempdir().expect("tempdir");
        let part_path = dir.path().join("progress-test.part");

        // A raw local HTTP/1.1 server: no need for wiremock here since this
        // test only cares about the shape of the progress callback, not the
        // catalog/manager wiring.
        let (base_url, handle) = spawn_fixed_body_server(vec![7u8; 500_000], false);

        let mut calls: Vec<(u64, u64)> = Vec::new();
        let digest = stream_to_part(
            &format!("{base_url}/body"),
            &part_path,
            &mut |done, total| calls.push((done, total)),
        )
        .expect("stream_to_part should succeed");
        handle.join().expect("server thread should not panic");

        assert_eq!(digest, sha256_hex(&vec![7u8; 500_000]));
        assert!(calls.len() >= 2, "expected multiple progress callbacks");
        assert_eq!(calls.first(), Some(&(0, 500_000)));
        assert_eq!(calls.last(), Some(&(500_000, 500_000)));
        for pair in calls.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "progress must never decrease: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_checksum_errors_and_leaves_no_file_behind() {
        let server = MockServer::start().await;
        let body = vec![0x11u8; 1_000];
        let body_len = body.len() as u64;

        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry(
            format!("{}/ggml-test.bin", server.uri()),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            body_len,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let result =
            tokio::task::spawn_blocking(move || manager.download("test-whisper", &mut |_, _| {}))
                .await
                .expect("blocking task panicked");

        assert!(result.is_err(), "checksum mismatch should be an error");
        let models_dir = dir.path().join("models");
        let remaining: Vec<_> = fs::read_dir(&models_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            remaining.is_empty(),
            "expected no leftover files, found: {remaining:?}"
        );
    }

    #[test]
    fn interrupted_body_errors_and_leaves_no_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (base_url, handle) = spawn_fixed_body_server(vec![9u8; 500_000], true);
        let entry = whisper_entry(
            format!("{base_url}/body"),
            "irrelevant-because-body-is-truncated".to_string(),
            500_000,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let result = manager.download("test-whisper", &mut |_, _| {});
        let _ = handle.join();

        assert!(result.is_err(), "truncated body should be an error");
        let models_dir = dir.path().join("models");
        let remaining: Vec<_> = fs::read_dir(&models_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            remaining.is_empty(),
            "expected no leftover files, found: {remaining:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_marks_model_installed_after_download() {
        let server = MockServer::start().await;
        let body = vec![0x77u8; 10_000];
        let body_len = body.len() as u64;
        let sha256 = sha256_hex(&body);

        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry(
            format!("{}/ggml-test.bin", server.uri()),
            sha256.clone(),
            body_len,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let before = manager.catalog();
        assert_eq!(before.len(), 1);
        assert!(!before[0].installed, "should not be installed yet");

        let manager = tokio::task::spawn_blocking(move || {
            manager
                .download("test-whisper", &mut |_, _| {})
                .expect("download should succeed");
            manager
        })
        .await
        .expect("blocking task panicked");

        let after = manager.catalog();
        assert_eq!(after.len(), 1);
        assert!(after[0].installed, "should be installed after download");

        manager.remove("test-whisper").expect("remove");
        let after_remove = manager.catalog();
        assert!(
            !after_remove[0].installed,
            "should not be installed after removal"
        );
    }

    #[test]
    fn multi_artifact_model_is_installed_only_when_every_file_is_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), vec![two_file_entry()]);

        let model_dir = dir.path().join("models").join("two-file-model");
        fs::create_dir_all(&model_dir).expect("create model dir");
        fs::write(model_dir.join("encoder.onnx"), b"x").expect("write encoder");

        assert!(
            models.path_for("two-file-model").is_none(),
            "a half-downloaded model must not report as installed"
        );

        fs::write(model_dir.join("tokens.txt"), b"x").expect("write tokens");
        assert_eq!(models.path_for("two-file-model"), Some(model_dir));
    }

    #[test]
    fn a_truncated_artifact_is_reported_as_damaged_not_installed() {
        // The failure this guards against is not hypothetical: a 456 MB model
        // download stalled silently partway through during Task 3, leaving a
        // file of exactly the right name and the wrong length.
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), vec![two_file_entry()]);

        let model_dir = dir.path().join("models").join("two-file-model");
        fs::create_dir_all(&model_dir).expect("create model dir");
        fs::write(model_dir.join("encoder.onnx"), b"truncated").expect("write encoder");
        fs::write(model_dir.join("tokens.txt"), b"x").expect("write tokens");

        let err = models
            .verify_installed("two-file-model")
            .expect_err("a wrong-sized artifact must not pass verification");
        assert!(matches!(err, IntegrityError::SizeMismatch { .. }));
    }

    #[test]
    fn verify_installed_succeeds_when_every_artifact_size_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), vec![two_file_entry()]);

        let model_dir = dir.path().join("models").join("two-file-model");
        fs::create_dir_all(&model_dir).expect("create model dir");
        fs::write(model_dir.join("encoder.onnx"), vec![0u8; 100]).expect("write encoder");
        fs::write(model_dir.join("tokens.txt"), vec![0u8; 50]).expect("write tokens");

        let verified = models
            .verify_installed("two-file-model")
            .expect("correctly sized artifacts must verify");
        assert_eq!(verified, model_dir);
    }

    #[test]
    fn verify_installed_checks_the_size_of_a_single_file_model() {
        // Every other `verify_installed` test uses `two_file_entry`, so this
        // is the only one exercising `artifact_path`'s single-artifact
        // branch (whisper's shape, and the reason the function is generic).
        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry("unused".to_string(), "unused".to_string(), 100);
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);
        let models_dir = dir.path().join("models");
        fs::create_dir_all(&models_dir).expect("create models dir");
        let file = models_dir.join("ggml-test.bin");

        fs::write(&file, vec![0u8; 99]).expect("write short file");
        assert!(matches!(
            models.verify_installed("test-whisper"),
            Err(IntegrityError::SizeMismatch { .. })
        ));

        fs::write(&file, vec![0u8; 100]).expect("write correct file");
        assert_eq!(
            models.verify_installed("test-whisper").expect("verifies"),
            file
        );
    }

    #[test]
    fn verify_installed_reports_a_never_downloaded_model_as_not_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), vec![two_file_entry()]);

        let err = models
            .verify_installed("two-file-model")
            .expect_err("a never-downloaded model must not verify");
        assert!(matches!(err, IntegrityError::NotInstalled(id) if id == "two-file-model"));
    }

    #[test]
    fn verify_installed_rejects_an_unknown_model_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let models = ModelManager::with_catalog(dir.path().to_path_buf(), Vec::new());

        let err = models
            .verify_installed("does-not-exist")
            .expect_err("an unknown id must not verify");
        assert!(matches!(err, IntegrityError::UnknownModel(id) if id == "does-not-exist"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn download_installs_every_artifact_of_a_multi_artifact_model() {
        let server = MockServer::start().await;
        let encoder_body = vec![0xAAu8; 5_000];
        let tokens_body = b"token list".to_vec();
        let encoder_size_bytes = encoder_body.len() as u64;
        let tokens_size_bytes = tokens_body.len() as u64;
        let encoder_sha256 = sha256_hex(&encoder_body);
        let tokens_sha256 = sha256_hex(&tokens_body);

        Mock::given(method("GET"))
            .and(path("/encoder.onnx"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(encoder_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tokens.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tokens_body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = multi_artifact_entry(
            format!("{}/encoder.onnx", server.uri()),
            encoder_sha256,
            encoder_size_bytes,
            format!("{}/tokens.txt", server.uri()),
            tokens_sha256,
            tokens_size_bytes,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let installed_path =
            tokio::task::spawn_blocking(move || manager.download("test-multi", &mut |_, _| {}))
                .await
                .expect("blocking task panicked")
                .expect("download should succeed");

        assert_eq!(installed_path, dir.path().join("models/test-multi"));
        assert_eq!(
            fs::read(installed_path.join("encoder.onnx")).expect("read encoder"),
            encoder_body
        );
        assert_eq!(
            fs::read(installed_path.join("tokens.txt")).expect("read tokens"),
            tokens_body
        );
        assert!(!dir.path().join("models/test-multi.staging").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multi_artifact_download_reports_progress_without_resetting_between_artifacts() {
        let server = MockServer::start().await;
        let encoder_body = vec![0xCCu8; 5_000];
        let tokens_body = vec![0xDDu8; 2_000];
        let encoder_size_bytes = encoder_body.len() as u64;
        let tokens_size_bytes = tokens_body.len() as u64;
        let encoder_sha256 = sha256_hex(&encoder_body);
        let tokens_sha256 = sha256_hex(&tokens_body);

        Mock::given(method("GET"))
            .and(path("/encoder.onnx"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(encoder_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tokens.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tokens_body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = multi_artifact_entry(
            format!("{}/encoder.onnx", server.uri()),
            encoder_sha256,
            encoder_size_bytes,
            format!("{}/tokens.txt", server.uri()),
            tokens_sha256,
            tokens_size_bytes,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);
        let grand_total = encoder_size_bytes + tokens_size_bytes;

        let calls = tokio::task::spawn_blocking(move || {
            let mut calls: Vec<(u64, u64)> = Vec::new();
            manager
                .download("test-multi", &mut |done, total| calls.push((done, total)))
                .expect("download should succeed");
            calls
        })
        .await
        .expect("blocking task panicked");

        assert!(!calls.is_empty(), "expected at least one progress call");
        assert_eq!(calls.first(), Some(&(0, grand_total)));
        assert_eq!(calls.last(), Some(&(grand_total, grand_total)));
        for pair in calls.windows(2) {
            assert!(
                pair[1].0 >= pair[0].0,
                "progress must never decrease across artifacts: {:?} -> {:?}",
                pair[0],
                pair[1]
            );
            assert_eq!(
                pair[0].1, grand_total,
                "total must stay the grand total across every artifact"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn download_of_multi_artifact_model_leaves_nothing_behind_when_one_artifact_fails_checksum(
    ) {
        let server = MockServer::start().await;
        let encoder_body = vec![0xBBu8; 5_000];
        let tokens_body = b"token list".to_vec();
        let encoder_size_bytes = encoder_body.len() as u64;
        let tokens_size_bytes = tokens_body.len() as u64;
        let encoder_sha256 = sha256_hex(&encoder_body);

        Mock::given(method("GET"))
            .and(path("/encoder.onnx"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(encoder_body.clone()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/tokens.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(tokens_body.clone()))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = multi_artifact_entry(
            format!("{}/encoder.onnx", server.uri()),
            encoder_sha256,
            encoder_size_bytes,
            format!("{}/tokens.txt", server.uri()),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            tokens_size_bytes,
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let result =
            tokio::task::spawn_blocking(move || manager.download("test-multi", &mut |_, _| {}))
                .await
                .expect("blocking task panicked");

        assert!(
            result.is_err(),
            "a bad artifact checksum should fail the whole download"
        );
        let models_dir = dir.path().join("models");
        let remaining: Vec<_> = fs::read_dir(&models_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).collect())
            .unwrap_or_default();
        assert!(
            remaining.is_empty(),
            "expected no leftover files or directories, found: {remaining:?}"
        );
    }

    // `ModelInfo` (the type `ModelManager::catalog()` returns) does not carry
    // artifacts, only installed state, so this checks the real hard-coded
    // `CatalogEntry` data directly rather than going through the manager.
    #[test]
    fn catalog_entries_declare_every_artifact_they_need() {
        for entry in CATALOG {
            assert!(
                !entry.artifacts.is_empty(),
                "{} declares no artifacts",
                entry.id
            );
            for artifact in entry.artifacts {
                assert_eq!(
                    artifact.sha256.len(),
                    64,
                    "{}: {} has a malformed sha256",
                    entry.id,
                    artifact.name
                );
            }
        }
    }

    #[test]
    fn vosk_is_gone_from_the_catalog() {
        let models = ModelManager::new(PathBuf::from("/nonexistent"));
        assert!(
            models.catalog().iter().all(|m| m.engine != "vosk"),
            "vosk models must not be offered once the engine is removed"
        );
    }

    #[test]
    fn unknown_model_id_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), Vec::new());

        let result = manager.download("does-not-exist", &mut |_, _| {});
        assert!(result.is_err());
    }

    #[test]
    fn removing_unknown_or_uninstalled_model_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), Vec::new());

        assert!(manager.remove("does-not-exist").is_ok());
    }

    /// A minimal HTTP/1.1 server on a background thread that serves `body`
    /// at `GET /body` with an accurate `Content-Length`. When `truncate` is
    /// true, it writes only half of `body` and then closes the connection,
    /// simulating a network interruption mid-download.
    fn spawn_fixed_body_server(
        body: Vec<u8>,
        truncate: bool,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request_buf = [0u8; 4096];
                let _ = stream.read(&mut request_buf);

                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());

                let to_send = if truncate { body.len() / 2 } else { body.len() };
                let _ = stream.write_all(&body[..to_send]);
                let _ = stream.flush();
                // Dropping `stream` here closes the socket; when truncated,
                // the client is left expecting more bytes than were sent.
            }
        });
        (format!("http://{addr}"), handle)
    }
}
