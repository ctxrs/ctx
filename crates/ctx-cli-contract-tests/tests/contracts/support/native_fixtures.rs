#![allow(unused_imports)]

#[path = "native_fixtures/appends.rs"]
mod appends;
#[path = "native_fixtures/default_installs.rs"]
mod default_installs;
#[path = "native_fixtures/installs.rs"]
mod installs;
#[path = "native_fixtures/json_tree.rs"]
mod json_tree;
#[path = "native_fixtures/sqlite.rs"]
mod sqlite;

pub(crate) use appends::*;
pub(crate) use default_installs::*;
pub(crate) use installs::*;
pub(crate) use json_tree::*;
pub(crate) use sqlite::*;
