pub mod cache;
pub mod coverage;
pub mod credentials;
pub mod diff;
pub mod dirs;
pub mod edit;
pub mod fileindex;
pub mod fuzzy;
pub mod git;
pub mod highlight;
pub mod idents;
pub mod index;
pub mod media;
pub mod memory;
pub mod nav;
pub mod outline;
pub mod paths;
pub mod pr;
pub mod radar;
pub mod scan;
pub mod search;
pub mod secscan;
pub mod settings;
pub mod symbols;
pub mod symindex;
pub mod testplan;
pub mod text;
pub mod trigram;
pub mod update;
pub mod watch;

pub use index::{FileEntry, FileMeta, Index, Window, WindowLine};

/// Re-exported so the server can reach the global scan/fuzzy pool (idle heap trims).
pub use rayon;
