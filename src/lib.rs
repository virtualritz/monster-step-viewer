//! Hash containers default to `ahash` rather than std's SipHash: the loader
//! builds large per-entity maps while parsing and meshing, and the keys are
//! never attacker-controlled, so the DoS-resistant default only costs speed.
//!
//! These alias std's containers with `ahash`'s hasher rather than ahash's
//! `AHashMap`/`AHashSet` newtypes, so every std/rayon/serde trait impl still
//! applies (notably `FromParallelIterator`, which the newtypes lack). The
//! trade-off is that `new()` is unavailable on a custom hasher — use
//! `default()`.
pub type HashMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;
pub type HashSet<T> = std::collections::HashSet<T, ahash::RandomState>;

pub mod step_loader;

pub use step_loader::{
    CompressedShellData, HeaderEntry, LoadMessage, LoadPhase, LoadProgress,
    MeshingConfig, Parameter, StepBoundaryLoop, StepBounds, StepEdge, StepFace,
    StepMetadata, StepScene, StepShell, StepTopology, Transform,
    load_step_file, load_step_file_streaming, load_step_file_with_progress,
    load_step_from_string_streaming, retessellate_face,
    retessellate_scene_streaming,
};
