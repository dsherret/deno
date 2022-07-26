// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod tarball;

use std::path::Path;
use std::path::PathBuf;

use deno_core::error::AnyError;

pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;

use cache::NpmCache;
use registry::NpmPackageVersionDistInfo;
use registry::NpmRegistryApi;
use resolution::resolve_packages;
use resolution::NpmResolution;

pub struct NpmDependencyResolver {
  cache: NpmCache,
  resolution: NpmResolution,
}

impl NpmDependencyResolver {
  pub fn resolve_package(
    &self,
    name: &str,
    referrer: Option<&NpmPackageId>,
  ) -> Result<PathBuf, AnyError> {
    let package = self.resolution.resolve_package(name, referrer)?;
    Ok(self.cache.package_folder(&package.id))
  }
}

pub async fn npm_install(
  references: Vec<NpmPackageReference>,
  root_cache_dir: PathBuf,
) -> Result<NpmDependencyResolver, AnyError> {
  let cache = NpmCache::new(root_cache_dir);
  let npm_registry_api = NpmRegistryApi::default();
  let resolution = resolve_packages(references, npm_registry_api).await?;

  // todo(dsherret): parallelize
  for package in resolution.all_packages() {
    cache.ensure_package(&package.id, &package.dist).await?;
  }

  Ok(NpmDependencyResolver { cache, resolution })
}
