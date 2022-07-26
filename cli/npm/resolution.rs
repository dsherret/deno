// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
use deno_core::anyhow::bail;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::futures::future::BoxFuture;
use deno_core::futures::FutureExt;
use deno_core::parking_lot::Mutex;

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

#[derive(Debug)]
pub struct NpmResolutionPackage {
  pub id: NpmPackageId,
  pub dist: NpmPackageVersionDistInfo,
  pub dependencies: HashMap<String, NpmPackageId>,
}

pub struct NpmResolution {
  top_level_packages: HashMap<String, NpmPackageId>,
  packages: HashMap<NpmPackageId, NpmResolutionPackage>,
}

impl NpmResolution {
  pub fn resolve_package(
    &self,
    name: &str,
    referrer: Option<&NpmPackageId>,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    if let Some(referrer) = referrer {
      match self.packages.get(referrer) {
        Some(referrer_package) => match referrer_package.dependencies.get(name)
        {
          Some(dep_id) => Ok(self.packages.get(dep_id).unwrap()),
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
    } else {
      match self.top_level_packages.get(name) {
        Some(p) => Ok(self.packages.get(p).unwrap()),
        None => bail!("could not find package '{}'", name),
      }
    }
  }

  pub fn all_packages(&self) -> impl Iterator<Item = &NpmResolutionPackage> {
    self.packages.values()
  }
}

pub async fn resolve_packages(
  packages: Vec<NpmPackageReference>,
  api: NpmRegistryApi,
) -> Result<NpmResolution, AnyError> {
  let context = Arc::new(Mutex::new(ResolutionContext::default()));
  npm_dependency_resolution(None, packages, api, context.clone()).await?;
  let context = context.lock();
  Ok(context.as_resolved())
}

#[derive(Default)]
struct ResolutionContext {
  parent: Option<Arc<Mutex<ResolutionContext>>>,
  children: HashMap<String, ContextChild>,
}

impl ResolutionContext {
  pub fn as_resolved(&self) -> NpmResolution {
    let mut packages: HashMap<NpmPackageId, NpmResolutionPackage> =
      Default::default();

    self.populate_packages(&mut packages);
    let top_level_packages = self
      .children
      .iter()
      .map(|(package_name, child)| {
        (
          package_name.clone(),
          NpmPackageId {
            name: package_name.clone(),
            version: child.version.clone(),
          },
        )
      })
      .collect();
    NpmResolution {
      top_level_packages,
      packages,
    }
  }

  fn populate_packages(
    &self,
    packages: &mut HashMap<NpmPackageId, NpmResolutionPackage>,
  ) {
    for (package_name, child) in self.children.iter() {
      let id = NpmPackageId {
        name: package_name.clone(),
        version: child.version.clone(),
      };
      if !packages.contains_key(&id) {
        let child_context = child.context.lock();
        let dependencies = child_context
          .children
          .iter()
          .map(|(package_name, child)| {
            (
              package_name.clone(),
              NpmPackageId {
                name: package_name.clone(),
                version: child.version.clone(),
              },
            )
          })
          .collect::<HashMap<_, _>>();
        packages.insert(
          id.clone(),
          NpmResolutionPackage {
            dependencies,
            dist: child.info.dist.clone(),
            id: id,
          },
        );
        child_context.populate_packages(packages);
      }
    }
  }
}

struct ContextChild {
  version: semver::Version,
  info: NpmPackageVersionInfo,
  context: Arc<Mutex<ResolutionContext>>,
}

#[derive(Clone)]
struct VersionAndInfo {
  version: semver::Version,
  info: NpmPackageVersionInfo,
}

/// Resolves dependencies according to:
/// https://npm.github.io/npm-like-im-5/npm3/dependency-resolution.html
/// We use the result of this a little differently.
fn npm_dependency_resolution(
  parent: Option<NpmPackageId>,
  mut packages: Vec<NpmPackageReference>,
  api: NpmRegistryApi,
  context: Arc<Mutex<ResolutionContext>>,
) -> BoxFuture<'static, Result<(), AnyError>> {
  // boxed due to async recursion
  async move {
    // need to resolve alphabetically, so sort
    packages.sort_by(|a, b| a.name.cmp(&b.name));

    for package in packages {
      let original_context = context.clone();
      let mut current_context_cell = original_context.clone();

      let (maybe_ancestor_version, insert_context) = loop {
        let (maybe_parent, maybe_child_version) = {
          let current_context = current_context_cell.lock();
          (
            current_context.parent.clone(),
            current_context
              .children
              .get(&package.name)
              .map(|c| c.version.clone()),
          )
        };
        if let Some(child_version) = maybe_child_version {
          if package.version_req.matches(&child_version) {
            // re-use this dependency
            break (Some(child_version), current_context_cell);
          } else {
            // insert in the current child context
            break (None, current_context_cell);
          }
        }
        if let Some(parent) = maybe_parent {
          current_context_cell = parent;
        } else {
          // not found, insert in the root context, which is
          // the current context
          break (None, current_context_cell);
        }
      };

      // not found, so get the package info and insert
      if maybe_ancestor_version.is_none() {
        let info = api.package_info(&package.name).await?;
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
        let package_version = match maybe_best_version {
          Some(v) => v,
          None => bail!(
            "could not package '{}' matching '{}'{}",
            package.name,
            package.version_req,
            match parent {
              Some(id) => format!(" as specified in {}", id),
              None => String::new(),
            }
          ),
        };

        let child_context = Arc::new(Mutex::new(ResolutionContext {
          parent: Some(original_context.clone()),
          children: Default::default(),
        }));
        insert_context.lock().children.insert(
          package.name.clone(),
          ContextChild {
            version: package_version.version.clone(),
            info: package_version.info.clone(),
            context: child_context.clone(),
          },
        );

        // now go analyze this child package
        let id = NpmPackageId {
          name: package.name,
          version: package_version.version,
        };

        let sub_packages = package_version
          .info
          .dependencies
          .into_iter()
          .map(|(package_name, version_req)| {
            Ok(NpmPackageReference {
              name: package_name.to_string(),
              version_req: semver::VersionReq::parse(&version_req)
                .with_context(|| {
                  format!(
                    "Error parsing version requirement for {} in {}",
                    package_name, id
                  )
                })?,
            })
          })
          .collect::<Result<Vec<_>, AnyError>>()?;
        npm_dependency_resolution(
          Some(id),
          sub_packages,
          api.clone(),
          child_context,
        )
        .await?;
      }
    }

    Ok(())
  }
  .boxed()
}
