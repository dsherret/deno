// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod tarball;

use std::path::PathBuf;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
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

use self::cache::ReadonlyNpmCache;
use self::resolution::NpmResolutionSnapshot;

pub struct LocalNpmPackageInfo {
  pub folder_path: PathBuf,
  pub id: NpmPackageId,
}

pub trait NpmPackageResolver {
  fn resolve_package_from_deno_module(
    &self,
    pkg_req: &NpmPackageReq,
  ) -> Result<LocalNpmPackageInfo, AnyError>;

  fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError>;

  /// Resolve the root folder of the package the provided specifier is in.
  ///
  /// This will erorr when the provided specifier is not in an npm package.
  fn resolve_package_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError>;
}

#[derive(Clone, Debug)]
pub struct GlobalNpmPackageResolver {
  cache: NpmCache,
  resolution: Arc<NpmResolution>,
}

impl GlobalNpmPackageResolver {
  pub fn new(root_cache_dir: PathBuf, reload: bool) -> Self {
    Self::from_cache(NpmCache::new(root_cache_dir), reload)
  }

  pub fn from_deno_dir(dir: &DenoDir, reload: bool) -> Self {
    Self::from_cache(NpmCache::from_deno_dir(dir), reload)
  }

  fn from_cache(cache: NpmCache, reload: bool) -> Self {
    let api = NpmRegistryApi::new(cache.clone(), reload);
    let resolution = Arc::new(NpmResolution::new(api));

    Self { cache, resolution }
  }

  /// If the resolver has resolved any npm packages.
  pub fn has_packages(&self) -> bool {
    self.resolution.has_packages()
  }

  /// Gets all the packages.
  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.resolution.all_packages()
  }

  /// Adds a package requirement to the resolver.
  pub async fn add_package_reqs(
    &self,
    packages: Vec<NpmPackageReq>,
  ) -> Result<(), AnyError> {
    self.resolution.add_package_reqs(packages).await
  }

  /// Caches all the packages.
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

  fn local_package_info(&self, id: &NpmPackageId) -> LocalNpmPackageInfo {
    LocalNpmPackageInfo {
      folder_path: self.package_folder(&id),
      id: id.clone(),
    }
  }

  fn package_folder(&self, package: &NpmPackageId) -> PathBuf {
    self.cache.package_folder(package).join("package")
  }

  /// Creates an inner clone.
  pub fn snapshot(&self) -> NpmPackageResolverSnapshot {
    NpmPackageResolverSnapshot {
      cache: self.cache.as_readonly(),
      snapshot: self.resolution.snapshot(),
    }
  }
}

impl NpmPackageResolver for GlobalNpmPackageResolver {
  fn resolve_package_from_deno_module(
    &self,
    pkg_req: &NpmPackageReq,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let pkg = self.resolution.resolve_package_from_deno_module(pkg_req)?;
    Ok(self.local_package_info(&pkg.id))
  }

  fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let referrer_pkg_id =
      self.cache.resolve_package_id_from_specifier(&referrer)?;
    let pkg = self
      .resolution
      .resolve_package_from_package(name, &referrer_pkg_id)?;
    Ok(self.local_package_info(&pkg.id))
  }

  fn resolve_package_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let pkg_id = self.cache.resolve_package_id_from_specifier(specifier)?;
    Ok(self.local_package_info(&pkg_id))
  }
}

#[derive(Clone, Debug)]
pub struct NpmPackageResolverSnapshot {
  cache: ReadonlyNpmCache,
  snapshot: NpmResolutionSnapshot,
}

impl Default for NpmPackageResolverSnapshot {
  fn default() -> Self {
    Self {
      cache: Default::default(),
      snapshot: Default::default(),
    }
  }
}

impl NpmPackageResolverSnapshot {
  fn local_package_info(&self, id: &NpmPackageId) -> LocalNpmPackageInfo {
    LocalNpmPackageInfo {
      folder_path: self.package_folder(&id),
      id: id.clone(),
    }
  }

  fn package_folder(&self, package: &NpmPackageId) -> PathBuf {
    self.cache.package_folder(package).join("package")
  }
}

impl NpmPackageResolver for NpmPackageResolverSnapshot {
  fn resolve_package_from_deno_module(
    &self,
    pkg_req: &NpmPackageReq,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let pkg = self.snapshot.resolve_package_from_deno_module(pkg_req)?;
    Ok(self.local_package_info(&pkg.id))
  }

  fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let referrer_pkg_id =
      self.cache.resolve_package_id_from_specifier(&referrer)?;
    let pkg = self
      .snapshot
      .resolve_package_from_package(name, &referrer_pkg_id)?;
    Ok(self.local_package_info(&pkg.id))
  }

  fn resolve_package_from_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<LocalNpmPackageInfo, AnyError> {
    let pkg_id = self.cache.resolve_package_id_from_specifier(specifier)?;
    Ok(self.local_package_info(&pkg_id))
  }
}
