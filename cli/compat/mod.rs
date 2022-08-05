// Copyright 2018-2022 the Deno authors. All rights reserved. MIT license.

mod errors;
mod esm_resolver;
mod package_json;

use std::path::Path;
use std::path::PathBuf;

use crate::file_fetcher::FileFetcher;
use crate::npm::NpmPackageResolver;
use deno_ast::MediaType;
use deno_core::error::AnyError;
use deno_core::located_script_name;
use deno_core::serde_json::Map;
use deno_core::serde_json::Value;
use deno_core::url::Url;
use deno_core::JsRuntime;
use deno_core::ModuleSpecifier;
use once_cell::sync::Lazy;
use path_clean::PathClean;

pub use esm_resolver::check_if_should_use_esm_loader;
pub use esm_resolver::node_resolve_binary_export;
pub use esm_resolver::node_resolve_new;
pub use esm_resolver::node_resolve_npm_reference_new;
pub use esm_resolver::resolve_typescript_types;
pub use esm_resolver::NodeEsmResolver;

use package_json::PackageJson;

// WARNING: Ensure this is the only deno_std version reference as this
// is automatically updated by the version bump workflow.
pub(crate) static STD_URL_STR: &str = "https://deno.land/std@0.151.0/";

static SUPPORTED_MODULES: &[&str] = &[
  "assert",
  "assert/strict",
  "async_hooks",
  "buffer",
  "child_process",
  "cluster",
  "console",
  "constants",
  "crypto",
  "dgram",
  "dns",
  "domain",
  "events",
  "fs",
  "fs/promises",
  "http",
  "https",
  "module",
  "net",
  "os",
  "path",
  "path/posix",
  "path/win32",
  "perf_hooks",
  "process",
  "querystring",
  "readline",
  "stream",
  "stream/promises",
  "stream/web",
  "string_decoder",
  "sys",
  "timers",
  "timers/promises",
  "tls",
  "tty",
  "url",
  "util",
  "util/types",
  "v8",
  "vm",
  "worker_threads",
  "zlib",
];

static NODE_COMPAT_URL: Lazy<String> = Lazy::new(|| {
  std::env::var("DENO_NODE_COMPAT_URL")
    .map(String::into)
    .ok()
    .unwrap_or_else(|| STD_URL_STR.to_string())
});

static GLOBAL_URL_STR: Lazy<String> =
  Lazy::new(|| format!("{}node/global.ts", NODE_COMPAT_URL.as_str()));

pub static GLOBAL_URL: Lazy<Url> =
  Lazy::new(|| Url::parse(&GLOBAL_URL_STR).unwrap());

static MODULE_URL_STR: Lazy<String> =
  Lazy::new(|| format!("{}node/module.ts", NODE_COMPAT_URL.as_str()));

pub static MODULE_URL: Lazy<Url> =
  Lazy::new(|| Url::parse(&MODULE_URL_STR).unwrap());

static COMPAT_IMPORT_URL: Lazy<Url> =
  Lazy::new(|| Url::parse("flags:compat").unwrap());

/// Provide imports into a module graph when the compat flag is true.
pub fn get_node_imports() -> Vec<(Url, Vec<String>)> {
  vec![(COMPAT_IMPORT_URL.clone(), vec![GLOBAL_URL_STR.clone()])]
}

fn try_resolve_builtin_module(specifier: &str) -> Option<Url> {
  if SUPPORTED_MODULES.contains(&specifier) {
    let ext = match specifier {
      "stream/promises" => "mjs",
      _ => "ts",
    };
    let module_url =
      format!("{}node/{}.{}", NODE_COMPAT_URL.as_str(), specifier, ext);
    Some(Url::parse(&module_url).unwrap())
  } else {
    None
  }
}

pub fn all_supported_builtin_module_urls() -> Vec<Url> {
  SUPPORTED_MODULES
    .iter()
    .map(|specifier| try_resolve_builtin_module(specifier).unwrap())
    .collect()
}

pub fn load_cjs_module(
  js_runtime: &mut JsRuntime,
  module: &str,
  main: bool,
) -> Result<(), AnyError> {
  let source_code = &format!(
    r#"(async function loadCjsModule(module) {{
      const Module = await import("{module_loader}");
      Module.default._load(module, null, {main});
    }})('{module}');"#,
    module_loader = MODULE_URL_STR.as_str(),
    main = main,
    module = escape_for_single_quote_string(module),
  );

  js_runtime.execute_script(&located_script_name!(), source_code)?;
  Ok(())
}

pub fn add_global_require(
  js_runtime: &mut JsRuntime,
  main_module: &str,
) -> Result<(), AnyError> {
  let source_code = &format!(
    r#"(async function setupGlobalRequire(main) {{
      const Module = await import("{}");
      const require = Module.createRequire(main);
      globalThis.require = require;
    }})('{}');"#,
    MODULE_URL_STR.as_str(),
    escape_for_single_quote_string(main_module),
  );

  js_runtime.execute_script(&located_script_name!(), source_code)?;
  Ok(())
}

fn escape_for_single_quote_string(text: &str) -> String {
  text.replace('\\', r"\\").replace('\'', r"\'")
}

pub fn setup_builtin_modules(
  js_runtime: &mut JsRuntime,
) -> Result<(), AnyError> {
  let mut script = String::new();
  for module in SUPPORTED_MODULES {
    // skipping the modules that contains '/' as they are not available in NodeJS repl as well
    if !module.contains('/') {
      script = format!("{}const {} = require('{}');\n", script, module, module);
    }
  }

  js_runtime.execute_script("setup_node_builtins.js", &script)?;
  Ok(())
}

/// Translates given CJS module into ESM. This function will perform static
/// analysis on the file to find defined exports and reexports.
///
/// For all discovered reexports the analysis will be performed recursively.
///
/// If successful a source code for equivalent ES module is returned.
pub fn translate_cjs_to_esm(
  file_fetcher: &FileFetcher,
  specifier: &ModuleSpecifier,
  code: String,
  media_type: MediaType,
) -> Result<String, AnyError> {
  let parsed_source = deno_ast::parse_script(deno_ast::ParseParams {
    specifier: specifier.to_string(),
    text_info: deno_ast::SourceTextInfo::new(code.into()),
    media_type,
    capture_tokens: true,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let analysis = parsed_source.analyze_cjs();

  let mut source = vec![
    r#"import { createRequire } from "node:module";"#.to_string(),
    r#"const require = createRequire(import.meta.url);"#.to_string(),
  ];

  // if there are reexports, handle them first
  for (idx, reexport) in analysis.reexports.iter().enumerate() {
    // Firstly, resolve relate reexport specifier
    let resolved_reexport = node_resolver::resolve(
      reexport,
      &specifier.to_file_path().unwrap(),
      // FIXME(bartlomieju): check if these conditions are okay, probably
      // should be `deno-require`, because `deno` is already used in `esm_resolver.rs`
      &["deno", "require", "default"],
    )?;
    let reexport_specifier =
      ModuleSpecifier::from_file_path(&resolved_reexport).unwrap();
    // Secondly, read the source code from disk
    let reexport_file = file_fetcher.get_source(&reexport_specifier).unwrap();
    // Now perform analysis again
    {
      let parsed_source = deno_ast::parse_script(deno_ast::ParseParams {
        specifier: reexport_specifier.to_string(),
        text_info: deno_ast::SourceTextInfo::new(reexport_file.source),
        media_type: reexport_file.media_type,
        capture_tokens: true,
        scope_analysis: false,
        maybe_syntax: None,
      })?;
      let analysis = parsed_source.analyze_cjs();

      source.push(format!(
        "const reexport{} = require(\"{}\");",
        idx, reexport
      ));

      for export in analysis.exports.iter().filter(|e| e.as_str() != "default")
      {
        // TODO(bartlomieju): Node actually checks if a given export exists in `exports` object,
        // but it might not be necessary here since our analysis is more detailed?
        source.push(format!(
          "export const {} = reexport{}.{};",
          export, idx, export
        ));
      }
    }
  }

  source.push(format!(
    "const mod = require(\"{}\");",
    specifier
      .to_file_path()
      .unwrap()
      .to_str()
      .unwrap()
      .replace('\\', "\\\\")
      .replace('\'', "\\\'")
      .replace('\"', "\\\"")
  ));
  source.push("export default mod;".to_string());

  for export in analysis.exports.iter().filter(|e| e.as_str() != "default") {
    // TODO(bartlomieju): Node actually checks if a given export exists in `exports` object,
    // but it might not be necessary here since our analysis is more detailed?
    source.push(format!("export const {} = mod.{};", export, export));
  }

  let translated_source = source.join("\n");
  Ok(translated_source)
}

/// Translates given CJS module into ESM. This function will perform static
/// analysis on the file to find defined exports and reexports.
///
/// For all discovered reexports the analysis will be performed recursively.
///
/// If successful a source code for equivalent ES module is returned.
pub fn translate_cjs_to_esm_new(
  file_fetcher: &FileFetcher,
  specifier: &ModuleSpecifier,
  code: String,
  media_type: MediaType,
  npm_resolver: &NpmPackageResolver,
) -> Result<String, AnyError> {
  let parsed_source = deno_ast::parse_script(deno_ast::ParseParams {
    specifier: specifier.to_string(),
    text_info: deno_ast::SourceTextInfo::new(code.into()),
    media_type,
    capture_tokens: true,
    scope_analysis: false,
    maybe_syntax: None,
  })?;
  let analysis = parsed_source.analyze_cjs();

  let mut source = vec![
    r#"import { createRequire } from "node:module";"#.to_string(),
    r#"const require = createRequire(import.meta.url);"#.to_string(),
  ];

  // if there are reexports, handle them first
  for (idx, reexport) in analysis.reexports.iter().enumerate() {
    // Firstly, resolve relate reexport specifier
    // todo(dsherret): call module_resolve_new instead?
    let resolved_reexport = resolve_new(
      reexport,
      &specifier,
      // FIXME(bartlomieju): check if these conditions are okay, probably
      // should be `deno-require`, because `deno` is already used in `esm_resolver.rs`
      &["deno", "require", "default"],
      npm_resolver,
    )?;
    let reexport_specifier =
      ModuleSpecifier::from_file_path(&resolved_reexport).unwrap();
    // Secondly, read the source code from disk
    let reexport_file = file_fetcher.get_source(&reexport_specifier).unwrap();
    // Now perform analysis again
    {
      let parsed_source = deno_ast::parse_script(deno_ast::ParseParams {
        specifier: reexport_specifier.to_string(),
        text_info: deno_ast::SourceTextInfo::new(reexport_file.source),
        media_type: reexport_file.media_type,
        capture_tokens: true,
        scope_analysis: false,
        maybe_syntax: None,
      })?;
      let analysis = parsed_source.analyze_cjs();

      source.push(format!(
        "const reexport{} = require(\"{}\");",
        idx, reexport
      ));

      for export in analysis.exports.iter().filter(|e| e.as_str() != "default")
      {
        // TODO(bartlomieju): Node actually checks if a given export exists in `exports` object,
        // but it might not be necessary here since our analysis is more detailed?
        source.push(format!(
          "export const {} = reexport{}.{};",
          export, idx, export
        ));
      }
    }
  }

  source.push(format!(
    "const mod = require(\"{}\");",
    specifier
      .to_file_path()
      .unwrap()
      .to_str()
      .unwrap()
      .replace('\\', "\\\\")
      .replace('\'', "\\\'")
      .replace('\"', "\\\"")
  ));
  source.push("export default mod;".to_string());

  for export in analysis.exports.iter().filter(|e| e.as_str() != "default") {
    // TODO(bartlomieju): Node actually checks if a given export exists in `exports` object,
    // but it might not be necessary here since our analysis is more detailed?
    source.push(format!("export const {} = mod.{};", export, export));
  }

  let translated_source = source.join("\n");
  Ok(translated_source)
}

// todo(dsherret): all of the below was temporarily lifted from node_resolver crate (without unit tests)
// This should all be refactored to live here instead and when doing so, redo the unit tests.

fn resolve_new(
  specifier: &str,
  referrer: &ModuleSpecifier,
  conditions: &[&str],
  npm_resolver: &NpmPackageResolver,
) -> Result<PathBuf, AnyError> {
  if specifier.starts_with('/') {
    todo!();
  }

  let referrer_path = referrer.to_file_path().unwrap();
  if specifier.starts_with("./") || specifier.starts_with("../") {
    if let Some(parent) = referrer_path.parent() {
      return file_extension_probe(parent.join(specifier), &referrer_path);
    } else {
      todo!();
    }
  }

  // We've got a bare specifier or maybe bare_specifier/blah.js"

  let (package_name, package_subpath) = parse_specifier(specifier).unwrap();

  let referrer_package_id =
    npm_resolver.get_package_from_specifier(referrer)?;
  // todo(dsherret): use not_found error on not found here
  let package_id = npm_resolver
    .resolve_package_from_package(&package_name, &referrer_package_id)?;

  let module_dir = npm_resolver.package_folder(&package_id);
  let package_json_path = module_dir.join("package.json");
  if package_json_path.exists() {
    let package_json = PackageJson::load(package_json_path)?;

    if let Some(map) = package_json.exports_map {
      if let Some((key, subpath)) = exports_resolve(&map, &package_subpath) {
        let value = map.get(&key).unwrap();
        let s = conditions_resolve(value, conditions);

        let t = resolve_package_target_string(&s, subpath);
        return Ok(module_dir.join(t).clean());
      } else {
        todo!()
      }
    }

    // old school
    if package_subpath != "." {
      let d = module_dir.join(package_subpath);
      if let Ok(m) = d.metadata() {
        if m.is_dir() {
          return Ok(d.join("index.js").clean());
        }
      }
      return file_extension_probe(d, &referrer_path);
    } else if let Some(main) = package_json.main {
      return Ok(module_dir.join(main).clean());
    } else {
      return Ok(module_dir.join("index.js").clean());
    }
  }

  Err(not_found(specifier, &referrer_path))
}

fn resolve_package_target_string(
  target: &str,
  subpath: Option<String>,
) -> String {
  if let Some(subpath) = subpath {
    target.replace('*', &subpath)
  } else {
    target.to_string()
  }
}

fn conditions_resolve(value: &Value, conditions: &[&str]) -> String {
  match value {
    Value::String(s) => s.to_string(),
    Value::Object(map) => {
      for condition in conditions {
        if let Some(x) = map.get(&condition.to_string()) {
          if let Value::String(s) = x {
            return s.to_string();
          } else {
            todo!()
          }
        }
      }
      todo!()
    }
    _ => todo!(),
  }
}

fn parse_specifier(specifier: &str) -> Option<(String, String)> {
  let mut separator_index = specifier.find('/');
  let mut valid_package_name = true;
  // let mut is_scoped = false;
  if specifier.is_empty() {
    valid_package_name = false;
  } else if specifier.starts_with('@') {
    // is_scoped = true;
    if let Some(index) = separator_index {
      separator_index = specifier[index + 1..].find('/');
    } else {
      valid_package_name = false;
    }
  }

  let package_name = if let Some(index) = separator_index {
    specifier[0..index].to_string()
  } else {
    specifier.to_string()
  };

  // Package name cannot have leading . and cannot have percent-encoding or separators.
  for ch in package_name.chars() {
    if ch == '%' || ch == '\\' {
      valid_package_name = false;
      break;
    }
  }

  if !valid_package_name {
    return None;
  }

  let package_subpath = if let Some(index) = separator_index {
    format!(".{}", specifier.chars().skip(index).collect::<String>())
  } else {
    ".".to_string()
  };

  Some((package_name, package_subpath))
}

fn exports_resolve(
  map: &Map<String, Value>,
  subpath: &str,
) -> Option<(String, Option<String>)> {
  if map.contains_key(subpath) {
    return Some((subpath.to_string(), None));
  }

  // best match
  let mut best_match = None;
  for key in map.keys() {
    if let Some(pattern_index) = key.find('*') {
      let key_sub = &key[0..pattern_index];
      if subpath.starts_with(key_sub) {
        if subpath.ends_with('/') {
          todo!()
        }
        let pattern_trailer = &key[pattern_index + 1..];

        if subpath.len() > key.len()
          && subpath.ends_with(pattern_trailer)
          // && pattern_key_compare(best_match, key) == 1
          && key.rfind('*') == Some(pattern_index)
        {
          let rest = subpath
            [pattern_index..(subpath.len() - pattern_trailer.len())]
            .to_string();
          best_match = Some((key, rest));
        }
      }
    }
  }

  if let Some((key, subpath_)) = best_match {
    return Some((key.to_string(), Some(subpath_)));
  }

  None
}

fn file_extension_probe(
  mut p: PathBuf,
  referrer: &Path,
) -> Result<PathBuf, AnyError> {
  if p.exists() {
    Ok(p.clean())
  } else {
    p.set_extension("js");
    if p.exists() {
      Ok(p)
    } else {
      Err(not_found(&p.clean().to_string_lossy(), referrer))
    }
  }
}

fn not_found(path: &str, referrer: &Path) -> AnyError {
  let msg = format!(
    "[ERR_MODULE_NOT_FOUND] Cannot find module \"{}\" imported from \"{}\"",
    path,
    referrer.to_string_lossy()
  );
  std::io::Error::new(std::io::ErrorKind::NotFound, msg).into()
}
