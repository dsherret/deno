// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod tarball;

use std::path::PathBuf;

use deno_ast::ModuleSpecifier;
use deno_core::error::AnyError;

pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;
pub use resolution::NpmPackageReq;

use cache::NpmCache;
use registry::NpmPackageVersionDistInfo;
use registry::NpmRegistryApi;
use resolution::NpmResolution;

pub struct NpmPackageResolver {
  cache: NpmCache,
  resolution: NpmResolution,
}

impl NpmPackageResolver {
  pub fn new(root_cache_dir: PathBuf) -> Self {
    let cache = NpmCache::new(root_cache_dir);
    let api = NpmRegistryApi::default();
    let resolution = NpmResolution::new(api);

    Self { cache, resolution }
  }

  pub async fn add_package_reqs(
    &self,
    packages: Vec<NpmPackageReq>,
  ) -> Result<(), AnyError> {
    self.resolution.add_package_reqs(packages).await?;
    // todo(dsherret): parallelize
    for package in self.resolution.all_packages() {
      self
        .cache
        .ensure_package(&package.id, &package.dist)
        .await?;
    }
    Ok(())
  }

  /// Resolve a node package from a node package.
  pub fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &NpmPackageId,
  ) -> Result<NpmPackageId, AnyError> {
    let package = self
      .resolution
      .resolve_package_from_package(name, referrer)?;
    Ok(package.id)
  }

  pub fn resolve_package_from_deno_module(
    &self,
    package: &NpmPackageReq,
  ) -> Result<NpmPackageId, AnyError> {
    let package = self.resolution.resolve_package_from_deno_module(package)?;
    Ok(package.id)
  }

  pub fn package_folder(&self, package: &NpmPackageId) -> PathBuf {
    self.cache.package_folder(package)
  }

  pub fn get_package_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Option<NpmPackageId> {
    self.cache.get_package_from_specifier(specifier)
  }
}
