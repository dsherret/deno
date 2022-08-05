// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod tarball;

use std::path::PathBuf;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::error::AnyError;

pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;
pub use resolution::NpmPackageReq;
pub use resolution::NpmResolutionPackage;

use cache::NpmCache;
use registry::NpmPackageVersionDistInfo;
use registry::NpmRegistryApi;
use resolution::NpmResolution;

use crate::deno_dir::DenoDir;

#[derive(Clone)]
pub struct NpmPackageResolver {
  cache: NpmCache,
  resolution: Arc<NpmResolution>,
}

impl std::fmt::Debug for NpmPackageResolver {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NpmPackageResolver")
      .field("snapshot", &self.resolution.snapshot())
      .finish()
  }
}

impl NpmPackageResolver {
  pub fn new(root_cache_dir: PathBuf, reload: bool) -> Self {
    let cache = NpmCache::new(root_cache_dir);
    let api = NpmRegistryApi::new(cache.clone(), reload);
    let resolution = Arc::new(NpmResolution::new(api));

    Self { cache, resolution }
  }

  pub fn from_deno_dir(dir: &DenoDir, reload: bool) -> Self {
    Self::new(dir.root.join("npm"), reload)
  }

  /// If the resolver has resolved any npm packages.
  pub fn has_packages(&self) -> bool {
    self.resolution.has_packages()
  }

  /// Gets all the packages.
  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.resolution.all_packages()
  }

  pub async fn add_package_reqs(
    &self,
    packages: Vec<NpmPackageReq>,
  ) -> Result<(), AnyError> {
    self.resolution.add_package_reqs(packages).await
  }

  pub async fn cache_packages(&self) -> Result<(), AnyError> {
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
    self.cache.package_folder(package).join("package")
  }

  pub fn get_package_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<NpmPackageId, AnyError> {
    match self.cache.get_package_from_specifier(specifier) {
      Some(id) => Ok(id),
      None => bail!("could not find npm package for '{}'", specifier),
    }
  }
}
