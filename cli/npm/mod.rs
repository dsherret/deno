// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod setup;
mod tarball;

use deno_core::error::AnyError;
use std::path::PathBuf;

pub use cache::NpmCache;
pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;

use registry::NpmPackageVersionDistInfo;
use registry::NpmRegistryApi;
use resolution::resolve_packages;
use setup::setup_node_modules;

pub async fn npm_install(
  references: Vec<NpmPackageReference>,
  cache: NpmCache,
) -> Result<(), AnyError> {
  let npm_registry_api = NpmRegistryApi::default();
  let resolution =
    resolve_packages(references, npm_registry_api.clone()).await?;

  setup_node_modules(resolution, cache, PathBuf::from("node_modules")).await?;

  Ok(())
}
