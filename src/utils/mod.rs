pub mod fs_cleanup;

// Re-export the most useful symbols for convenience
pub use fs_cleanup::{remove_small_files_under_paths, CleanupSummary};
