//! Operaciones de sistema de archivos.
//!
//! Lectura, escritura y gestión de archivos de grabación.

use std::path::Path;
use tokio::fs;
use crate::error::VigilanteError;

pub struct FileManager;

impl FileManager {
    pub async fn list_files(dir: &Path) -> Result<Vec<String>, VigilanteError> {
        let mut entries = fs::read_dir(dir).await.map_err(VigilanteError::Io)?;
        let mut files = Vec::new();

        while let Some(entry) = entries.next_entry().await.map_err(VigilanteError::Io)? {
            if let Some(name) = entry.file_name().to_str() {
                files.push(name.to_string());
            }
        }

        Ok(files)
    }

    pub async fn delete_file(path: &Path) -> Result<(), VigilanteError> {
        fs::remove_file(path).await.map_err(VigilanteError::Io)
    }

    pub async fn get_file_size(path: &Path) -> Result<u64, VigilanteError> {
        let metadata = fs::metadata(path).await.map_err(VigilanteError::Io)?;
        Ok(metadata.len())
    }
}