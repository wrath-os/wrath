//! The builder and store. See `design/store.md` for the design and the experiments behind it.

pub mod building;
pub mod cache;
pub mod chain;
pub mod content;
pub mod early;
pub mod fixup;
pub mod graph;
pub mod http;
pub mod image;
pub mod initrd;
pub mod progress;
pub mod records;
pub mod repro;
pub mod requires;
pub mod runpath;
pub mod sandbox;
pub mod seed;
pub mod seed_pack;
pub(crate) mod settle;
pub mod split;
pub mod store;
pub mod uki;
pub mod upstream;
pub mod view;
