//! Base de datos para metadatos de grabaciones.
//!
//! Maneja operaciones CRUD para días, archivos y estadísticas.

use crate::error::VigilanteError;

pub struct StorageDb {
}

impl StorageDb {
    pub fn new(_path: &str) -> Result<Self, VigilanteError> {
        // TODO: Implementar conexión a base de datos cuando sea necesario
        Ok(Self {})
    }

    pub fn save_day_meta(&self, _day: &str, _meta: &str) -> Result<(), VigilanteError> {
        // Lógica para guardar metadatos
        Ok(())
    }

    pub fn load_day_meta(&self, _day: &str) -> Result<String, VigilanteError> {
        // Lógica para cargar metadatos
        Ok("{}".to_string())
    }
}