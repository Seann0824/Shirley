//! Infrastructure with no agent semantics: global configuration, on-disk state
//! layout, the user's remote cloud connection, and prompt templating.
//!
//! These modules sit *below* the domain and capabilities layers: they know how
//! to read/write settings and files, but nothing about sessions, agents, or
//! tools. (Two cycles with the layers above are known debt — see `lib.rs`.)

pub mod artifacts;
pub mod prompt_template;
pub mod remote;
pub mod settings;
pub mod storage;
