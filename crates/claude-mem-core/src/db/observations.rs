//! `observations` table read/write surface.
//!
//! Port of `src/services/sqlite/observations/{store,get,recent,files}.ts`.

pub mod files;
pub mod get;
pub mod recent;
pub mod store;

pub use files::{get_files_for_session, SessionFilesResult};
pub use get::{
    get_observation_by_id, get_observations_by_file_path, get_observations_by_ids,
    get_observations_for_session,
};
pub use recent::{get_all_recent_observations, get_recent_observations};
pub use store::{compute_observation_content_hash, find_duplicate_observation, store_observation};
