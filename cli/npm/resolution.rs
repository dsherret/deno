// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

use std::collections::HashMap;
use std::sync::Arc;

use deno_core::anyhow::bail;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::futures::future::BoxFuture;
use deno_core::futures::FutureExt;
use deno_core::parking_lot::Mutex;

use super::registry::NpmPackageVersionDistInfo;
use super::registry::NpmPackageVersionInfo;
use super::registry::NpmRegistryApi;

#[derive(Clone)]
pub struct NpmPackageReference {
  pub name: String,
  pub version_req: semver::VersionReq,
}

pub struct NpmPackageId {
  pub name: String,
  pub version: semver::Version,
}

impl std::fmt::Display for NpmPackageId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}@{}", self.name, self.version)
  }
}

pub struct NpmPackageResolution {
  pub id: NpmPackageId,
  pub dist: NpmPackageVersionDistInfo,
  // Any packages that need to be nested under this one
  // because this does not resolve using the root packages.
  pub sub_packages: Vec<NpmPackageResolution>,
}

pub async fn resolve_packages(
  packages: Vec<NpmPackageReference>,
  api: NpmRegistryApi,
) -> Result<Vec<NpmPackageResolution>, AnyError> {
  let context = Arc::new(Mutex::new(ResolutionContext::default()));
  resolve_packages_inner(None, packages, api, context.clone()).await?;
  let context = context.lock();
  Ok(context.as_resolved())
}

#[derive(Default)]
struct ResolutionContext {
  parent: Option<Arc<Mutex<ResolutionContext>>>,
  children: HashMap<String, ContextChild>,
}

impl ResolutionContext {
  pub fn as_resolved(&self) -> Vec<NpmPackageResolution> {
    let mut result = self
      .children
      .iter()
      .map(|(package_name, child)| NpmPackageResolution {
        dist: child.info.dist.clone(),
        id: NpmPackageId {
          name: package_name.clone(),
          version: child.version.clone(),
        },
        sub_packages: child.context.lock().as_resolved(),
      })
      .collect::<Vec<_>>();
    // create some determinism downstream and sort by name
    result.sort_by(|a, b| a.id.name.cmp(&b.id.name));
    result
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

// npm dependency resolution: https://npm.github.io/npm-like-im-5/npm3/dependency-resolution.html

fn resolve_packages_inner(
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
        for version_info in info.versions {
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
        resolve_packages_inner(
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
