//! Resolución y validación de rutas de almacenamiento.
//!
//! Asegura que las rutas sean seguras y válidas dentro del directorio raíz.

use std::path::{Path, PathBuf};

pub struct PathResolver {
    root: PathBuf,
}

impl PathResolver {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf, String> {
        let path = self.root.join(relative);
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err("Ruta fuera del directorio raíz".to_string())
        }
    }

    pub fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
        path.canonicalize().map_err(|e| e.to_string())
    }
}