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
use deno_core::parking_lot::RwLock;

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
  pub dependencies: HashMap<String, NpmPackageId>,
}

struct StoredNpmResolution {
  top_level_packages: HashMap<String, NpmPackageId>,
  packages: HashMap<NpmPackageId, NpmResolutionPackage>,
}

impl StoredNpmResolution {
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

  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.packages.values().cloned().collect()
  }
}

pub struct NpmResolution {
  api: NpmRegistryApi,
  stored: RwLock<StoredNpmResolution>,
  context: tokio::sync::Mutex<ResolutionContext>,
}

impl NpmResolution {
  pub async fn add_packages(
    &self,
    mut packages: Vec<NpmPackageReference>,
  ) -> Result<(), AnyError> {
    // multiple packages are resolved on alphabetical order
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    let context = self.context.lock().await;
    for package in packages {
      context.add_package(package, self.api.clone()).await?;
    }
    *self.stored.write() = context.as_stored();
    Ok(())
  }

  pub fn resolve_package(
    &self,
    name: &str,
    referrer: Option<&NpmPackageId>,
  ) -> Result<&NpmResolutionPackage, AnyError> {
    self.stored.read().resolve_package(name, referrer)
  }

  pub fn all_packages(&self) -> Vec<NpmResolutionPackage> {
    self.stored.read().all_packages()
  }
}

#[derive(Clone)]
struct VersionAndInfo {
  version: semver::Version,
  info: NpmPackageVersionInfo,
}

#[derive(Clone)]
enum ResolutionContext {
  Root(Arc<RootContext>),
  Package(Arc<PackageContext>),
}

impl ResolutionContext {
  pub fn as_stored(&self) -> StoredNpmResolution {
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
    StoredNpmResolution {
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

  pub fn into_package_context(self) -> Option<Arc<PackageContext>> {
    match self {
      ResolutionContext::Package(c) => Some(c),
      _ => None,
    }
  }

  pub fn parent(&self) -> Option<ResolutionContext> {
    match self {
      ResolutionContext::Root(_) => None,
      ResolutionContext::Package(p) => Some(p.parent.clone()),
    }
  }

  pub fn insert_child(
    &self,
    package_name: String,
    context: Arc<PackageContext>,
  ) {
    match self {
      ResolutionContext::Root(c) => {
        c.children.lock().insert(package_name, context);
      }
      ResolutionContext::Package(c) => {
        &c.deps.lock().children.insert(package_name, context);
      }
    }
  }

  pub fn get_dependency_version(
    &self,
    package: &NpmPackageReference,
  ) -> Option<semver::Version> {
    match self {
      ResolutionContext::Root(c) => c
        .children
        .lock()
        .get(&package.name)
        .map(|c| c.version.clone()),
      ResolutionContext::Package(c) => {
        let deps = c.deps.lock();
        deps
          .children
          .get(&package.name)
          .map(|c| c.version.clone())
          .or_else(|| {
            deps.non_children_dependencies.get(&package.name).cloned()
          })
      }
    }
  }

  pub fn add_package(
    &self,
    package: NpmPackageReference,
    api: NpmRegistryApi,
  ) -> BoxFuture<'static, Result<(), AnyError>> {
    let context = self.clone();
    async move {
      match resolve(context, &package)? {
        ResolveAction::None => {
          // do nothing, it's already existing
        }
        ResolveAction::InsertNonChild(package_context, version) => {
          package_context
            .deps
            .lock()
            .non_children_dependencies
            .insert(package.name.clone(), version);
        }
        ResolveAction::InsertChild(insert_context) => {
          let info = api.package_info(&package.name).await?;
          let version_and_info =
            get_resolved_package_version_and_info(&package, info, None)?;
          let mut dependencies = version_and_info
            .info
            .dependencies_as_references()
            .with_context(|| {
              format!("Package: {}@{}", package.name, version_and_info.version)
            })?;
          let child_context = Arc::new(PackageContext {
            parent: insert_context.clone(),
            info: version_and_info.info,
            version: version_and_info.version.clone(),
            deps: Mutex::new(PackageContextDependencies {
              children: Default::default(),
              non_children_dependencies: Default::default(),
            }),
          });
          insert_context
            .insert_child(package.name.clone(), child_context.clone());

          // iterate over the dependencies in alphabetical order
          dependencies.sort_by(|a, b| a.name.cmp(&b.name));
          let child_context = ResolutionContext::Package(child_context);
          for dep in dependencies {
            child_context.add_package(dep, api.clone()).await?;
          }
        }
      }

      Ok(())
    }
    .boxed()
  }
}

struct RootContext {
  children: Mutex<HashMap<String, Arc<PackageContext>>>,
  all_: Mutex<HashMap<String, Arc<PackageContext>>>,
}

#[derive(Default)]
struct RootContextInner {
  children: HashMap<String, Arc<PackageContext>>,
  all_packages: HashMap<String, Vec<semver::Version>>,
}

struct PackageContext {
  parent: ResolutionContext,
  version: semver::Version,
  info: NpmPackageVersionInfo,
  deps: Mutex<PackageContextDependencies>,
}

struct PackageContextDependencies {
  children: HashMap<String, Arc<PackageContext>>,
  non_children_dependencies: HashMap<String, semver::Version>,
}

enum ResolveAction {
  /// Package already exists in the current context.
  None,
  /// A version was found found in an ancestor, so insert
  /// a non-child version into the current context.
  InsertNonChild(Arc<PackageContext>, semver::Version),
  /// A conflicting version was found in an ancestor.
  /// Insert into the specified context.
  InsertChild(ResolutionContext),
}

fn resolve(
  context: ResolutionContext,
  package: &NpmPackageReference,
) -> Result<ResolveAction, AnyError> {
  let mut last_parent = None;
  let mut next_context = Some(context);
  while let Some(current) = next_context {
    if let Some(dep_version) = current.get_dependency_version(&package) {
      if package.version_req.matches(&dep_version) {
        if last_parent.is_none() {
          return Ok(ResolveAction::None);
        } else {
          return Ok(ResolveAction::InsertNonChild(
            current.into_package_context().unwrap(),
            dep_version,
          ));
        }
      } else {
        if last_parent.is_none() {
          bail!(
            "cannot import {} because it's not compatible with previous resolved version {}",
            package,
            dep_version,
          );
        }
        return Ok(ResolveAction::InsertChild(current));
      }
    }
    next_context = current.parent();
    last_parent = Some(current);
  }

  // insert in the root
  let root_context = last_parent.unwrap();
  Ok(ResolveAction::InsertChild(root_context))
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
