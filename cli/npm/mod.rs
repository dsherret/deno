// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod cache;
mod registry;
mod resolution;
mod setup;
mod tarball;

pub use registry::NpmPackageInfo;
pub use registry::NpmPackageVersionDistInfo;
pub use registry::NpmPackageVersionInfo;
pub use registry::NpmRegistryApi;
pub use resolution::resolve_packages;
pub use resolution::NpmPackageId;
pub use resolution::NpmPackageReference;
pub use resolution::NpmPackageResolution;
