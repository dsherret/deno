// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod tarball;

use deno_core::error::AnyError;

pub use cache::NpmCache;
pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;

use registry::NpmPackageVersionDistInfo;
use registry::NpmRegistryApi;
use resolution::resolve_packages;

pub async fn npm_install(
  references: Vec<NpmPackageReference>,
  cache: NpmCache,
) -> Result<(), AnyError> {
  let npm_registry_api = NpmRegistryApi::default();
  let resolution =
    resolve_packages(references, npm_registry_api.clone()).await?;

  for package in resolution.all_packages() {
    cache.ensure_package(&package.id, &package.dist).await?;
  }

  Ok(())
}
