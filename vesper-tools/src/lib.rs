mod backup;
mod checkpoint;
mod diff;
mod gather;
mod project;
mod repo_map;
mod sandbox;

pub use backup::{backup_file, list_backups, restore_backup, BackupEntry};
pub use checkpoint::{
    create_checkpoint, list_checkpoints, restore_checkpoint, undo_last, Checkpoint,
};
pub use diff::unified_diff;
pub use gather::{gather_codebase, CodeFileSnippet, CodebaseDigest};
pub use project::{scan_project, ProjectInfo};
pub use repo_map::build_repo_map;
pub use sandbox::{CommandOutput, Workspace};
