//! STT model catalog and downloader.
//!
//! Holds the hard-coded catalog of speech-to-text models (whisper.cpp ggml
//! models from Hugging Face, Vosk models from alphacephei.com), tracks which
//! ones are installed under `data_dir/models/`, and performs checksum-verified
//! downloads: the response body streams to a `.part` file while its sha256 is
//! computed incrementally, and the file (or, for Vosk zip archives, the
//! unpacked directory) is only put in its final place once the checksum
//! matches the catalog value. A checksum mismatch or an interrupted download
//! always leaves the `.part` file cleaned up rather than lying around.

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
    pub url: String,
    pub sha256: String,
    pub installed: bool,
}

/// Static metadata for one catalog entry.
///
/// `engine` distinguishes how a download is installed: `"whisper"` models
/// are a single `.bin` file placed directly under `models/`; `"vosk"` models
/// are a `.zip` archive whose sha256 is verified before it is unpacked into a
/// same-named directory under `models/`.
#[derive(Debug, Clone, Copy)]
struct CatalogEntry {
    id: &'static str,
    engine: &'static str,
    label: &'static str,
    size_mb: u32,
    url: &'static str,
    sha256: &'static str,
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
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    CatalogEntry {
        id: "base",
        engine: "whisper",
        label: "Whisper Base",
        size_mb: 141,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    CatalogEntry {
        id: "small",
        engine: "whisper",
        label: "Whisper Small",
        size_mb: 465,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    CatalogEntry {
        id: "medium",
        engine: "whisper",
        label: "Whisper Medium",
        size_mb: 1463,
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    CatalogEntry {
        id: "large-v3-turbo-q5_0",
        engine: "whisper",
        label: "Whisper Large v3 Turbo (q5_0)",
        size_mb: 547,
        url:
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    CatalogEntry {
        id: "vosk-model-small-en-us-0.15",
        engine: "vosk",
        label: "Vosk Small (English)",
        size_mb: 39,
        url: "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
        sha256: "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498",
    },
    CatalogEntry {
        id: "vosk-model-small-ru-0.22",
        engine: "vosk",
        label: "Vosk Small (Russian)",
        size_mb: 44,
        url: "https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip",
        sha256: "961d5ff98a17f4aa6de69864d0aa71fa5bac682301d2b5d17a3f24c5c99a46d4",
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
            .map(|entry| ModelInfo {
                id: entry.id.to_string(),
                engine: entry.engine.to_string(),
                label: entry.label.to_string(),
                size_mb: entry.size_mb,
                url: entry.url.to_string(),
                sha256: entry.sha256.to_string(),
                installed: self.install_path(entry).exists(),
            })
            .collect()
    }

    /// Returns the installed path for `id` (a file for whisper models, a
    /// directory for vosk models), or `None` if it is unknown or not yet
    /// installed.
    pub fn path_for(&self, id: &str) -> Option<PathBuf> {
        let entry = self.find(id)?;
        let path = self.install_path(entry);
        path.exists().then_some(path)
    }

    /// Downloads and installs the model identified by `id`.
    ///
    /// The response body streams into a `.part` file under `models/` while
    /// its sha256 is computed incrementally; `progress(done, total)` is
    /// called after every chunk (`total` is 0 if the server did not send a
    /// `Content-Length`). Once the body is fully received, the digest is
    /// checked against the catalog's sha256: on mismatch, or if the body is
    /// interrupted, the `.part` file is removed and an error returned, so no
    /// partial file or directory is ever left behind.
    ///
    /// On success, whisper models are atomically renamed into place;
    /// vosk models are unpacked from the verified zip into a same-named
    /// directory, and the zip is discarded.
    pub fn download(&self, id: &str, progress: &mut dyn FnMut(u64, u64)) -> Result<PathBuf> {
        let entry = *self
            .find(id)
            .ok_or_else(|| anyhow!("unknown model id: {id}"))?;

        let models_dir = self.models_dir();
        fs::create_dir_all(&models_dir)
            .with_context(|| format!("failed to create {}", models_dir.display()))?;

        let target = target_name(entry.url);
        let part_path = models_dir.join(format!("{target}.part"));

        let digest = match stream_to_part(entry.url, &part_path, progress) {
            Ok(digest) => digest,
            Err(err) => {
                let _ = fs::remove_file(&part_path);
                return Err(err);
            }
        };

        if digest != entry.sha256 {
            let _ = fs::remove_file(&part_path);
            bail!(
                "checksum mismatch for model '{id}': expected {}, got {digest}",
                entry.sha256
            );
        }

        let final_path = self.install_path(&entry);
        match entry.engine {
            "vosk" => {
                // Extract into a fresh staging directory first, rather than
                // over `final_path` directly: if extraction fails partway
                // (checksum was already verified, but e.g. disk is full or
                // the archive is otherwise unreadable), only the staging
                // directory is discarded and any prior good install at
                // `final_path` is left untouched. On success, the old
                // install (if any) is removed only after the new one has
                // been fully unpacked, then the staged directory is renamed
                // into place.
                let staging_root = models_dir.join(format!("{target}.staging"));
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
            _ => {
                fs::rename(&part_path, &final_path).with_context(|| {
                    format!(
                        "failed to move {} into {}",
                        part_path.display(),
                        final_path.display()
                    )
                })?;
                Ok(final_path)
            }
        }
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

    /// The final on-disk path a catalog entry installs to: a `.bin` file for
    /// whisper models, a directory (the zip's basename minus `.zip`) for
    /// vosk models.
    fn install_path(&self, entry: &CatalogEntry) -> PathBuf {
        let name = target_name(entry.url);
        match entry.engine {
            "vosk" => self
                .models_dir()
                .join(name.strip_suffix(".zip").unwrap_or(name)),
            _ => self.models_dir().join(name),
        }
    }
}

/// The last path segment of `url`, used as the on-disk file name for a
/// download (e.g. `ggml-tiny.bin`, `vosk-model-small-en-us-0.15.zip`).
fn target_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
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

    fn whisper_entry(url: String, sha256: String) -> CatalogEntry {
        CatalogEntry {
            id: "test-whisper",
            engine: "whisper",
            label: "Test Whisper",
            size_mb: 1,
            url: Box::leak(url.into_boxed_str()),
            sha256: Box::leak(sha256.into_boxed_str()),
        }
    }

    fn vosk_entry(url: String, sha256: String) -> CatalogEntry {
        CatalogEntry {
            id: "test-vosk",
            engine: "vosk",
            label: "Test Vosk",
            size_mb: 1,
            url: Box::leak(url.into_boxed_str()),
            sha256: Box::leak(sha256.into_boxed_str()),
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
