use std::fs;
use std::path::Path;
use std::path::PathBuf;

use deno_ast::view::NodeTrait;
use deno_ast::MediaType;
use deno_ast::ModuleSpecifier;
use deno_ast::SourceTextInfo;
use deno_core::error::AnyError;

use super::NpmCache;
use super::NpmPackageId;
use super::NpmRegistryApi;

const NODE_TYPES_PACKAGE_NAME: &str = "@types/node";
const NODE_TYPES_VERSION: &str = "18.6.5";

pub struct NodeTypes {
  cache: NpmCache,
}

impl NodeTypes {
  pub async fn build(cache: NpmCache) -> Result<Self, AnyError> {
    let mut api = NpmRegistryApi::new(cache, false);
    let package_info = api.package_info(NODE_TYPES_PACKAGE_NAME).await?;
    let version_info =
      if let Some(info) = package_info.versions.remove(NODE_TYPES_VERSION) {
        info
      } else {
        // user had an old cache, so force reload
        api.set_reload(true);
        let package_info = api.package_info(NODE_TYPES_PACKAGE_NAME).await?;
        package_info.versions.remove(NODE_TYPES_VERSION).unwrap()
      };

    let package_id = NpmPackageId {
      name: NODE_TYPES_PACKAGE_NAME.to_string(),
      version: semver::Version::parse(&version_info.version).unwrap(),
    };
    cache
      .ensure_package(&package_id, &version_info.dist)
      .await?;
    let package_folder = cache.package_folder(&package_id);
    let declaration_file_paths = get_declaration_files(&package_folder)?;

    for path in declaration_file_paths {
      let text = fs::read_to_string(&path)?;
      let program = deno_ast::parse_program(deno_ast::ParseParams {
        capture_tokens: true,
        maybe_syntax: None,
        media_type: (&path).into(),
        scope_analysis: true,
        specifier: ModuleSpecifier::from_file_path(path).unwrap().to_string(),
        text_info: SourceTextInfo::from_string(text),
      })?;
      program.with_view(|program| for child in program.children() {});
    }

    Self { cache }
  }
}

fn get_declaration_files(
  package_folder: &Path,
) -> Result<Vec<PathBuf>, AnyError> {
  let mut result = Vec::new();
  for entry in fs::read_dir(package_folder)? {
    let entry = entry?;
    let file_type = entry.file_type()?;
    let file_path = package_folder.join(entry.file_name());
    if file_type.is_dir() {
      result.extend(get_declaration_files(&file_path));
    } else if file_path
      .to_string_lossy()
      .to_lowercase()
      .ends_with(".d.ts")
    {
      result.push(file_path);
    }
  }
  Ok(result)
}
