//! STT model catalog and downloader.
//!
//! Holds the hard-coded catalog of speech-to-text models (whisper.cpp ggml
//! models from Hugging Face, Vosk models from alphacephei.com), tracks which
//! ones are installed under `data_dir/models/`, and performs checksum-verified
//! downloads. A catalog entry is one or more artifacts: a whisper model is a
//! single `.bin` file, a vosk model is a single `.zip` archive that gets
//! unpacked, and a model with several artifacts (e.g. a sherpa-onnx
//! transducer's encoder, decoder, joiner and tokens) is installed as a
//! directory holding all of them. Each artifact streams to its own `.part`
//! file while its sha256 is computed incrementally, and a model only becomes
//! visible at its final path once every one of its artifacts has verified —
//! a checksum mismatch or an interrupted download always leaves the staging
//! area cleaned up rather than reporting a half-installed model.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};

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
}

/// Static metadata for one catalog entry.
///
/// `engine` distinguishes how a download is installed: `"vosk"` models have
/// a single artifact, a `.zip` archive, whose sha256 is verified before it
/// is unpacked into a same-named directory under `models/`. Every other
/// engine installs its artifacts by their catalog `name`: a single file
/// directly under `models/` when there is exactly one artifact (e.g.
/// `"whisper"`), or a directory named after the entry's `id` holding every
/// artifact when there is more than one (e.g. a sherpa-onnx transducer's
/// encoder, decoder, joiner and tokens).
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
/// Whisper sha256 values were read from the Hugging Face tree API for
/// `ggerganov/whisper.cpp` (`lfs.oid` per file). Vosk sha256 values were
/// computed locally from the zip archives published at
/// `https://alphacephei.com/vosk/models/`, and cross-checked against the
/// md5 values listed in `https://alphacephei.com/vosk/models/model-list.json`.
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
        }],
    },
    CatalogEntry {
        id: "vosk-model-small-en-us-0.15",
        engine: "vosk",
        label: "Vosk Small (English)",
        size_mb: 39,
        artifacts: &[Artifact {
            url: "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
            sha256: "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498",
            name: "vosk-model-small-en-us-0.15.zip",
        }],
    },
    CatalogEntry {
        id: "vosk-model-small-ru-0.22",
        engine: "vosk",
        label: "Vosk Small (Russian)",
        size_mb: 44,
        artifacts: &[Artifact {
            url: "https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip",
            sha256: "961d5ff98a17f4aa6de69864d0aa71fa5bac682301d2b5d17a3f24c5c99a46d4",
            name: "vosk-model-small-ru-0.22.zip",
        }],
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
    /// models, a directory for vosk and multi-artifact models), or `None` if
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
    /// `progress(done, total)` is called after every chunk of every artifact
    /// (`total` is 0 if the server did not send a `Content-Length`, and the
    /// count restarts at zero for each new artifact). An artifact's digest
    /// is checked against its catalog sha256 as soon as it finishes
    /// downloading: on the first mismatch, or if any body is interrupted,
    /// the whole staging area is removed and an error returned, so no
    /// half-downloaded model is ever left where [`Self::path_for`] would
    /// find it.
    ///
    /// Only once every artifact has verified does the model become visible
    /// at its final path: a single-artifact model (e.g. whisper) is renamed
    /// directly into place as one file, and a multi-artifact model (e.g. a
    /// future sherpa-onnx entry) has its whole staging directory renamed
    /// into place at once. Vosk models are the remaining special case: their
    /// one artifact is a zip archive that is unpacked into a staging
    /// directory before that directory is swapped into place.
    pub fn download(&self, id: &str, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf> {
        let entry = *self
            .find(id)
            .ok_or_else(|| anyhow!("unknown model id: {id}"))?;

        let models_dir = self.models_dir();
        fs::create_dir_all(&models_dir)
            .with_context(|| format!("failed to create {}", models_dir.display()))?;

        if entry.engine == "vosk" {
            self.download_vosk(id, &entry, &models_dir, progress)
        } else {
            self.download_artifacts(id, &entry, &models_dir, progress)
        }
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

        for artifact in entry.artifacts {
            if let Err(err) = stage_one_artifact(id, artifact, &staging_dir, progress) {
                let _ = fs::remove_dir_all(&staging_dir);
                return Err(err);
            }
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

    /// Downloads the single zip artifact of a vosk `entry`, verifies its
    /// sha256, then unpacks it into a staging directory before swapping that
    /// directory into place.
    fn download_vosk(
        &self,
        id: &str,
        entry: &CatalogEntry,
        models_dir: &Path,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<PathBuf> {
        let Some(artifact) = entry.artifacts.first() else {
            bail!("model '{id}' has no artifacts defined");
        };

        let part_path = models_dir.join(format!("{}.part", artifact.name));
        let digest = match stream_to_part(artifact.url, &part_path, progress) {
            Ok(digest) => digest,
            Err(err) => {
                let _ = fs::remove_file(&part_path);
                return Err(err);
            }
        };

        if digest != artifact.sha256 {
            let _ = fs::remove_file(&part_path);
            bail!(
                "checksum mismatch for model '{id}': expected {}, got {digest}",
                artifact.sha256
            );
        }

        let final_path = self.install_path(entry);
        // Extract into a fresh staging directory first, rather than over
        // `final_path` directly: if extraction fails partway (checksum was
        // already verified, but e.g. disk is full or the archive is
        // otherwise unreadable), only the staging directory is discarded and
        // any prior good install at `final_path` is left untouched. On
        // success, the old install (if any) is removed only after the new
        // one has been fully unpacked, then the staged directory is renamed
        // into place.
        let staging_root = models_dir.join(format!("{}.staging", artifact.name));
        let _ = fs::remove_dir_all(&staging_root);

        let unpacked = unpack_zip(&part_path, &staging_root);
        let _ = fs::remove_file(&part_path);
        if let Err(err) = unpacked {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(err);
        }

        let staged_dir = final_path.file_name().map(|name| staging_root.join(name));
        let Some(staged_dir) = staged_dir.filter(|dir| dir.is_dir()) else {
            let _ = fs::remove_dir_all(&staging_root);
            bail!(
                "archive for model '{id}' did not contain the expected directory '{}'",
                final_path.display()
            );
        };

        if final_path.exists() {
            fs::remove_dir_all(&final_path).with_context(|| {
                format!(
                    "failed to remove previous install at {}",
                    final_path.display()
                )
            })?;
        }
        fs::rename(&staged_dir, &final_path).with_context(|| {
            format!(
                "failed to move {} into {}",
                staged_dir.display(),
                final_path.display()
            )
        })?;
        let _ = fs::remove_dir_all(&staging_root);

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

    /// The final on-disk path a catalog entry installs to: a directory (the
    /// zip's basename minus `.zip`) for vosk models, a directory named after
    /// the entry's `id` for models with more than one artifact, or a single
    /// file (the artifact's `name`) otherwise.
    fn install_path(&self, entry: &CatalogEntry) -> PathBuf {
        match entry.engine {
            "vosk" => {
                let name = entry.artifacts.first().map_or(entry.id, |a| a.name);
                self.models_dir()
                    .join(name.strip_suffix(".zip").unwrap_or(name))
            }
            _ if entry.artifacts.len() > 1 => self.models_dir().join(entry.id),
            _ => {
                let name = entry.artifacts.first().map_or(entry.id, |a| a.name);
                self.models_dir().join(name)
            }
        }
    }

    /// Whether every artifact of `entry` is present at `path`, the value
    /// returned by [`Self::install_path`] for it.
    fn is_installed(&self, entry: &CatalogEntry, path: &Path) -> bool {
        match entry.engine {
            "vosk" => path.is_dir(),
            _ if entry.artifacts.len() > 1 => {
                entry.artifacts.iter().all(|a| path.join(a.name).is_file())
            }
            _ => path.is_file(),
        }
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

/// Unpacks the zip archive at `zip_path` into `dest_dir`, preserving each
/// entry's relative path. Entries with an unsafe (e.g. path-traversing) name
/// are skipped rather than trusted.
fn unpack_zip(zip_path: &Path, dest_dir: &Path) -> Result<()> {
    let file = File::open(zip_path).context("failed to open downloaded archive")?;
    let mut archive = zip::ZipArchive::new(file).context("failed to read zip archive")?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .with_context(|| format!("failed to read zip entry {i}"))?;
        let Some(relative) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest_dir.join(relative);

        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("failed to create directory {}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }
            let mut out_file = File::create(&out_path)
                .with_context(|| format!("failed to create {}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out_file)
                .with_context(|| format!("failed to extract {}", out_path.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use zip::write::SimpleFileOptions;

    /// Leaks a `Vec<Artifact>` into a `&'static [Artifact]`, mirroring how
    /// the real catalog's entries are `'static` data, so test entries can be
    /// built from owned `String`s (e.g. a mock server's URI).
    fn leak_artifacts(artifacts: Vec<Artifact>) -> &'static [Artifact] {
        Box::leak(artifacts.into_boxed_slice())
    }

    fn whisper_entry(url: String, sha256: String) -> CatalogEntry {
        CatalogEntry {
            id: "test-whisper",
            engine: "whisper",
            label: "Test Whisper",
            size_mb: 1,
            artifacts: leak_artifacts(vec![Artifact {
                url: Box::leak(url.into_boxed_str()),
                sha256: Box::leak(sha256.into_boxed_str()),
                name: "ggml-test.bin",
            }]),
        }
    }

    fn vosk_entry(url: String, sha256: String) -> CatalogEntry {
        CatalogEntry {
            id: "test-vosk",
            engine: "vosk",
            label: "Test Vosk",
            size_mb: 1,
            artifacts: leak_artifacts(vec![Artifact {
                url: Box::leak(url.into_boxed_str()),
                sha256: Box::leak(sha256.into_boxed_str()),
                name: "vosk-model-test.zip",
            }]),
        }
    }

    /// A two-artifact entry (e.g. mirroring a sherpa-onnx model's encoder
    /// and tokens) whose artifacts are never actually downloaded in the
    /// tests that use it — only `path_for`'s "every file present" logic is
    /// exercised, so the URLs are unused placeholders.
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
                },
                Artifact {
                    url: "unused",
                    sha256: "unused",
                    name: "tokens.txt",
                },
            ],
        }
    }

    fn multi_artifact_entry(
        encoder_url: String,
        encoder_sha256: String,
        tokens_url: String,
        tokens_sha256: String,
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
                },
                Artifact {
                    url: Box::leak(tokens_url.into_boxed_str()),
                    sha256: Box::leak(tokens_sha256.into_boxed_str()),
                    name: "tokens.txt",
                },
            ]),
        }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    /// Builds a tiny zip archive with a single top-level directory (mirroring
    /// how real Vosk archives are laid out) containing one small file.
    fn build_test_vosk_zip(dir_name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file(format!("{dir_name}/README"), options)
                .expect("start_file");
            writer
                .write_all(b"tiny vosk model fixture")
                .expect("write fixture body");
            writer.finish().expect("finish zip");
        }
        buf
    }

    /// Builds a structurally valid zip archive (two Stored entries under
    /// `dir_name`) whose second entry's data has been corrupted in place so
    /// its CRC32 no longer matches: the archive opens fine, but extracting
    /// the second entry fails partway through. Used to test that a checksum
    /// can pass (it is computed over these exact, already-corrupt bytes)
    /// while the subsequent unpack still fails.
    fn build_corrupt_vosk_zip(dir_name: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        let second_content = b"second file content, will be corrupted";
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            writer
                .start_file(format!("{dir_name}/first"), options)
                .expect("start_file first");
            writer
                .write_all(b"first file content, extracted fine")
                .expect("write first");
            writer
                .start_file(format!("{dir_name}/second"), options)
                .expect("start_file second");
            writer.write_all(second_content).expect("write second");
            writer.finish().expect("finish zip");
        }

        // Flip one byte inside the second entry's stored (uncompressed) data.
        // Because compression is `Stored`, the entry's bytes appear verbatim
        // in the archive, so this corrupts its content without touching the
        // zip's structure or offsets; its recorded CRC32 no longer matches
        // when read back.
        let pos = buf
            .windows(second_content.len())
            .position(|window| window == second_content)
            .expect("stored bytes should be present verbatim in the archive");
        buf[pos] ^= 0xFF;

        buf
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
        let entry = whisper_entry(format!("{}/ggml-test.bin", server.uri()), sha256.clone());
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

        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry(
            format!("{}/ggml-test.bin", server.uri()),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
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
        let sha256 = sha256_hex(&body);

        Mock::given(method("GET"))
            .and(path("/ggml-test.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = whisper_entry(format!("{}/ggml-test.bin", server.uri()), sha256.clone());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn vosk_zip_is_verified_then_unpacked_into_a_directory() {
        let server = MockServer::start().await;
        let zip_bytes = build_test_vosk_zip("vosk-model-test");
        let sha256 = sha256_hex(&zip_bytes);

        Mock::given(method("GET"))
            .and(path("/vosk-model-test.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(zip_bytes))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = vosk_entry(
            format!("{}/vosk-model-test.zip", server.uri()),
            sha256.clone(),
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let installed_path =
            tokio::task::spawn_blocking(move || manager.download("test-vosk", &mut |_, _| {}))
                .await
                .expect("blocking task panicked")
                .expect("download should succeed");

        assert_eq!(installed_path, dir.path().join("models/vosk-model-test"));
        assert!(installed_path.is_dir());
        let readme = fs::read_to_string(installed_path.join("README")).expect("read README");
        assert_eq!(readme, "tiny vosk model fixture");

        // The intermediate zip must not survive as a stray artifact.
        assert!(!dir.path().join("models/vosk-model-test.zip.part").exists());
        assert!(!dir.path().join("models/vosk-model-test.zip").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vosk_redownload_replaces_stale_files_and_removes_old_content() {
        let server = MockServer::start().await;
        let zip_bytes = build_test_vosk_zip("vosk-model-test");
        let sha256 = sha256_hex(&zip_bytes);

        Mock::given(method("GET"))
            .and(path("/vosk-model-test.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(zip_bytes))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = vosk_entry(
            format!("{}/vosk-model-test.zip", server.uri()),
            sha256.clone(),
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        // Simulate a previously installed model containing a file the new
        // archive does not: a naive "unpack over the same directory" would
        // leave it behind.
        let install_dir = dir.path().join("models/vosk-model-test");
        fs::create_dir_all(&install_dir).expect("create stale install dir");
        fs::write(
            install_dir.join("stale-sentinel.txt"),
            b"leftover from an old version",
        )
        .expect("write stale sentinel");

        let installed_path =
            tokio::task::spawn_blocking(move || manager.download("test-vosk", &mut |_, _| {}))
                .await
                .expect("blocking task panicked")
                .expect("re-download should succeed");

        assert_eq!(installed_path, install_dir);
        let readme = fs::read_to_string(installed_path.join("README")).expect("read README");
        assert_eq!(readme, "tiny vosk model fixture");
        assert!(
            !installed_path.join("stale-sentinel.txt").exists(),
            "stale file from the previous install should be gone after re-download"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn vosk_failed_unpack_over_existing_install_leaves_old_install_untouched() {
        let server = MockServer::start().await;
        let corrupt_zip = build_corrupt_vosk_zip("vosk-model-test");
        // The catalog checksum is computed over these exact (already
        // corrupt) bytes, so checksum verification passes; the failure this
        // test exercises happens later, while unpacking.
        let sha256 = sha256_hex(&corrupt_zip);

        Mock::given(method("GET"))
            .and(path("/vosk-model-test.zip"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(corrupt_zip))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().expect("tempdir");
        let entry = vosk_entry(
            format!("{}/vosk-model-test.zip", server.uri()),
            sha256.clone(),
        );
        let manager = ModelManager::with_catalog(dir.path().to_path_buf(), vec![entry]);

        let install_dir = dir.path().join("models/vosk-model-test");
        fs::create_dir_all(&install_dir).expect("create existing install dir");
        fs::write(
            install_dir.join("good.txt"),
            b"the good, already-installed model",
        )
        .expect("write sentinel");

        let result =
            tokio::task::spawn_blocking(move || manager.download("test-vosk", &mut |_, _| {}))
                .await
                .expect("blocking task panicked");

        assert!(result.is_err(), "unpacking a corrupt archive should fail");

        // The prior good install must be completely untouched.
        assert_eq!(
            fs::read_to_string(install_dir.join("good.txt")).expect("read sentinel"),
            "the good, already-installed model"
        );

        // No staging directory or leftover zip artifacts beside it.
        let models_dir = dir.path().join("models");
        let leftovers: Vec<_> = fs::read_dir(&models_dir)
            .expect("read models dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name != "vosk-model-test")
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no leftover staging/zip artifacts, found: {leftovers:?}"
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

    #[tokio::test(flavor = "multi_thread")]
    async fn download_installs_every_artifact_of_a_multi_artifact_model() {
        let server = MockServer::start().await;
        let encoder_body = vec![0xAAu8; 5_000];
        let tokens_body = b"token list".to_vec();
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
            format!("{}/tokens.txt", server.uri()),
            tokens_sha256,
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
    async fn download_of_multi_artifact_model_leaves_nothing_behind_when_one_artifact_fails_checksum(
    ) {
        let server = MockServer::start().await;
        let encoder_body = vec![0xBBu8; 5_000];
        let tokens_body = b"token list".to_vec();
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
            format!("{}/tokens.txt", server.uri()),
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
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
