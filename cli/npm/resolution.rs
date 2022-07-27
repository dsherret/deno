// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::collections::VecDeque;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::parking_lot::RwLock;

use super::registry::NpmPackageInfo;
use super::registry::NpmPackageVersionDistInfo;
use super::registry::NpmPackageVersionInfo;
use super::registry::NpmRegistryApi;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NpmPackageReference {
  pub name: String,
  pub version_req: semver::VersionReq,
}

impl NpmPackageReference {
  pub fn from_specifier(
    specifier: &ModuleSpecifier,
  ) -> Result<NpmPackageReference, AnyError> {
    Self::from_str(specifier.as_str())
  }

  pub fn from_str(specifier: &str) -> Result<NpmPackageReference, AnyError> {
    let specifier = match specifier.strip_prefix("npm:") {
      Some(s) => s,
      None => {
        bail!("not an npm specifier for '{}'", specifier);
      }
    };
    let (name, version_req) = match specifier.rsplit_once('@') {
      Some(r) => r,
      None => {
        bail!(
          "npm specifier must include a version (ex. `package@1.0.0`) for '{}'",
          specifier
        );
      }
    };
    let version_req = match semver::VersionReq::parse(version_req) {
      Ok(v) => v,
      Err(err) => bail!(
        "npm specifier must have a valid version requirement for '{}'.\n\n{}",
        specifier,
        err
      ),
    };
    Ok(NpmPackageReference {
      name: name.to_string(),
      version_req,
    })
  }
}

impl std::fmt::Display for NpmPackageReference {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}@{}", self.name, self.version_req)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NpmPackageId {
  pub name: String,
  pub version: semver::Version,
}

impl std::fmt::Display for NpmPackageId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}@{}", self.name, self.version)
  }
}

#[derive(Debug, Clone)]
pub struct NpmResolutionPackage {
  pub id: NpmPackageId,
  pub dist: NpmPackageVersionDistInfo,
  pub dependencies: HashMap<String, semver::Version>,
}

#[derive(Debug, Clone, Default)]
struct NpmResolutionSnapshot {
  package_references: HashMap<NpmPackageReference, semver::Version>,
  packages_by_name: HashMap<String, Vec<semver::Version>>,
  packages: HashMap<NpmPackageId, NpmResolutionPackage>,
}

impl NpmResolutionSnapshot {
  /// Resolve a node package from a deno module.
  pub fn resolve_package_from_deno_module(
    &self,
    reference: &NpmPackageReference,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    match self.package_references.get(reference) {
      Some(version) => Ok(
        self
          .packages
          .get(&NpmPackageId {
            name: reference.name.clone(),
            version: version.clone(),
          })
          .unwrap(),
      ),
      None => bail!("could not find package '{}'", reference),
    }
  }

  pub fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &NpmPackageId,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    match self.packages.get(referrer) {
      Some(referrer_package) => match referrer_package.dependencies.get(name) {
        Some(version) => Ok(
          self
            .packages
            .get(&NpmPackageId {
              name: name.to_string(),
              version: version.clone(),
            })
            .unwrap(),
        ),
        None => {
          bail!(
            "could not find package '{}' referenced by '{}'",
            name,
            referrer
          )
        }
      },
      None => bail!("could not find referrer package '{}'", referrer),
    }
  }

  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.packages.values().cloned().collect()
  }

  pub fn resolve_best_package_version(
    &self,
    package: &NpmPackageReference,
  ) -> Option<semver::Version> {
    let mut maybe_best_version: Option<&semver::Version> = None;
    if let Some(versions) = self.packages_by_name.get(&package.name) {
      for version in versions {
        if package.version_req.matches(version) {
          let is_best_version = maybe_best_version
            .as_ref()
            .map(|best_version| (*best_version).cmp(version).is_lt())
            .unwrap_or(true);
          if is_best_version {
            maybe_best_version = Some(version);
          }
        }
      }
    }
    maybe_best_version.cloned()
  }
}

pub struct NpmResolution {
  api: NpmRegistryApi,
  snapshot: RwLock<NpmResolutionSnapshot>,
  update_sempahore: tokio::sync::Semaphore,
}

impl NpmResolution {
  pub fn new(api: NpmRegistryApi) -> Self {
    Self {
      api,
      snapshot: Default::default(),
      update_sempahore: tokio::sync::Semaphore::new(1),
    }
  }

  pub async fn add_package_references(
    &self,
    mut packages: Vec<NpmPackageReference>,
  ) -> Result<(), AnyError> {
    // multiple packages are resolved on alphabetical order
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    // only allow one thread in here at a time
    let _permit = self.update_sempahore.acquire().await.unwrap();
    let mut current_resolution = self.snapshot.read().clone();
    let mut pending_dependencies = VecDeque::new();

    // go over the top level packages first
    for package_ref in packages {
      if current_resolution
        .package_references
        .contains_key(&package_ref)
      {
        // skip analyzing this package, as there's already a matching top level package
        continue;
      }
      // inspect the list of current packages
      if let Some(version) =
        current_resolution.resolve_best_package_version(&package_ref)
      {
        current_resolution
          .package_references
          .insert(package_ref, version);
        continue; // done, no need to continue
      }

      // no existing best version, so resolve the current packages
      let info = self.api.package_info(&package_ref.name).await?;
      let version_and_info =
        get_resolved_package_version_and_info(&package_ref, info, None)?;
      let dependencies = version_and_info
        .info
        .dependencies_as_references()
        .with_context(|| {
          format!("Package: {}@{}", package_ref.name, version_and_info.version)
        })?;

      let id = NpmPackageId {
        name: package_ref.name.clone(),
        version: version_and_info.version.clone(),
      };
      pending_dependencies.push_back((id.clone(), dependencies));
      current_resolution.packages.insert(
        id.clone(),
        NpmResolutionPackage {
          id,
          dist: version_and_info.info.dist,
          dependencies: Default::default(),
        },
      );
      current_resolution
        .packages_by_name
        .entry(package_ref.name.clone())
        .or_default()
        .push(version_and_info.version.clone());
      current_resolution
        .package_references
        .insert(package_ref, version_and_info.version);
    }

    // now go down through the dependencies by tree depth
    while let Some((parent_package_id, mut deps)) =
      pending_dependencies.pop_front()
    {
      // sort the dependencies alphabetically
      deps.sort_by(|a, b| a.name.cmp(&b.name));

      // now resolve them
      for dep in deps {
        // check if an existing dependency matches this
        let version = if let Some(version) =
          current_resolution.resolve_best_package_version(&dep)
        {
          version
        } else {
          // get the information
          let info = self.api.package_info(&dep.name).await?;
          let version_and_info =
            get_resolved_package_version_and_info(&dep, info, None)?;
          let dependencies = version_and_info
            .info
            .dependencies_as_references()
            .with_context(|| {
              format!("Package: {}@{}", dep.name, version_and_info.version)
            })?;

          let id = NpmPackageId {
            name: dep.name.clone(),
            version: version_and_info.version.clone(),
          };
          pending_dependencies.push_back((id.clone(), dependencies));
          current_resolution.packages.insert(
            id.clone(),
            NpmResolutionPackage {
              id,
              dist: version_and_info.info.dist,
              dependencies: Default::default(),
            },
          );
          current_resolution
            .packages_by_name
            .entry(dep.name.clone())
            .or_default()
            .push(version_and_info.version.clone());

          version_and_info.version
        };

        // add this version as a dependency of the package
        current_resolution
          .packages
          .get_mut(&parent_package_id)
          .unwrap()
          .dependencies
          .insert(dep.name.clone(), version);
      }
    }

    *self.snapshot.write() = current_resolution;
    Ok(())
  }

  pub fn resolve_package_from_package(
    &self,
    name: &str,
    referrer: &NpmPackageId,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    self
      .snapshot
      .read()
      .resolve_package_from_package(name, referrer)
  }

  /// Resolve a node package from a deno module.
  pub fn resolve_package_from_deno_module(
    &self,
    reference: &NpmPackageReference,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    self
      .snapshot
      .read()
      .resolve_package_from_deno_module(reference)
  }

  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.snapshot.read().all_packages()
  }
}

#[derive(Clone)]
struct VersionAndInfo {
  version: semver::Version,
  info: NpmPackageVersionInfo,
}

fn get_resolved_package_version_and_info(
  package: &NpmPackageReference,
  info: NpmPackageInfo,
  parent: Option<&NpmPackageId>,
) -> Result<VersionAndInfo, AnyError> {
  let mut maybe_best_version: Option<VersionAndInfo> = None;
  for (_, version_info) in info.versions.into_iter() {
    let version = semver::Version::parse(&version_info.version)?;
    if package.version_req.matches(&version) {
      let is_best_version = maybe_best_version
        .as_ref()
        .map(|best_version| best_version.version.cmp(&version).is_lt())
        .unwrap_or(true);
      if is_best_version {
        maybe_best_version = Some(VersionAndInfo {
          version,
          info: version_info,
        });
      }
    }
  }

  match maybe_best_version {
    Some(v) => Ok(v),
    None => bail!(
      "could not package '{}' matching '{}'{}",
      package.name,
      package.version_req,
      match parent {
        Some(id) => format!(" as specified in {}", id),
        None => String::new(),
      }
    ),
  }
}
