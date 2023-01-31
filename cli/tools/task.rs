// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use crate::args::Flags;
use crate::args::TaskFlags;
use crate::colors;
use crate::proc_state::ProcState;
use crate::util::fs::canonicalize_path;
use deno_core::anyhow::bail;
use deno_core::anyhow::Context;
use deno_core::error::AnyError;
use deno_core::futures::future::LocalBoxFuture;
use deno_task_shell::ExecuteResult;
use deno_task_shell::ShellCommand;
use deno_task_shell::ShellCommandContext;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub async fn execute_script(
  flags: Flags,
  task_flags: TaskFlags,
) -> Result<i32, AnyError> {
  let ps = ProcState::build(flags).await?;
  let tasks_config = ps.options.resolve_tasks_config()?;
  let maybe_package_json = ps.options.get_maybe_package_json();
  let package_json_scripts = maybe_package_json
    .as_ref()
    .and_then(|p| p.scripts.clone())
    .unwrap_or_default();

  if task_flags.task.is_empty() {
    print_available_tasks(&tasks_config, &package_json_scripts);
    return Ok(1);
  }

  let task_name = task_flags.task;

  if let Some(script) = tasks_config.get(&task_name) {
    let config_file_url = ps.options.maybe_config_file_specifier().unwrap();
    let config_file_path = if config_file_url.scheme() == "file" {
      config_file_url.to_file_path().unwrap()
    } else {
      bail!("Only local configuration files are supported")
    };
    let cwd = match task_flags.cwd {
      Some(path) => canonicalize_path(&PathBuf::from(path))?,
      None => config_file_path.parent().unwrap().to_owned(),
    };
    let script = get_script_with_args(script, &ps);
    output_task(&task_name, &script);
    let seq_list = deno_task_shell::parser::parse(&script)
      .with_context(|| format!("Error parsing script '{task_name}'."))?;
    let env_vars = collect_env_vars();
    let exit_code =
      deno_task_shell::execute(seq_list, env_vars, &cwd, Default::default())
        .await;
    Ok(exit_code)
  } else if let Some(script) = package_json_scripts.get(&task_name) {
    let cwd = match task_flags.cwd {
      Some(path) => canonicalize_path(&PathBuf::from(path))?,
      None => maybe_package_json
        .as_ref()
        .unwrap()
        .path
        .parent()
        .unwrap()
        .to_owned(),
    };
    let script = get_script_with_args(script, &ps);
    output_task(&task_name, &script);
    let seq_list = deno_task_shell::parser::parse(&script)
      .with_context(|| format!("Error parsing script '{task_name}'."))?;
    let npx_commands = resolve_npm_commands(&ps)?;
    let env_vars = collect_env_vars();
    let exit_code =
      deno_task_shell::execute(seq_list, env_vars, &cwd, npx_commands).await;
    Ok(exit_code)
  } else {
    eprintln!("Task not found: {task_name}");
    print_available_tasks(&tasks_config, &package_json_scripts);
    Ok(1)
  }
}

fn get_script_with_args(script: &str, ps: &ProcState) -> String {
  let additional_args = ps
    .options
    .argv()
    .iter()
    // surround all the additional arguments in double quotes
    // and santize any command substition
    .map(|a| format!("\"{}\"", a.replace('"', "\\\"").replace('$', "\\$")))
    .collect::<Vec<_>>()
    .join(" ");
  let script = format!("{script} {additional_args}");
  script.trim().to_owned()
}

fn output_task(task_name: &str, script: &str) {
  log::info!(
    "{} {} {}",
    colors::green("Task"),
    colors::cyan(&task_name),
    script,
  );
}

fn collect_env_vars() -> HashMap<String, String> {
  // get the starting env vars (the PWD env var will be set by deno_task_shell)
  let mut env_vars = std::env::vars().collect::<HashMap<String, String>>();
  const INIT_CWD_NAME: &str = "INIT_CWD";
  if !env_vars.contains_key(INIT_CWD_NAME) {
    if let Ok(cwd) = std::env::current_dir() {
      // if not set, set an INIT_CWD env var that has the cwd
      env_vars
        .insert(INIT_CWD_NAME.to_string(), cwd.to_string_lossy().to_string());
    }
  }
  env_vars
}

fn print_available_tasks(
  tasks_config: &BTreeMap<String, String>,
  package_json_scripts: &BTreeMap<String, String>,
) {
  eprintln!("{}", colors::green("Available tasks:"));

  let mut had_task = false;
  for (key, value) in tasks_config.iter().chain(
    package_json_scripts
      .iter()
      .filter(|(key, _)| !tasks_config.contains_key(*key)),
  ) {
    eprintln!("- {}", colors::cyan(key));
    eprintln!("    {}", value);
    had_task = true;
  }
  if !had_task {
    eprintln!("  {}", colors::red("No tasks found in configuration file"));
  }
}

struct NpxCommand;

impl ShellCommand for NpxCommand {
  fn execute(
    &self,
    context: ShellCommandContext,
  ) -> LocalBoxFuture<'static, ExecuteResult> {
    if let Some(first_arg) = context.args.get(0).cloned() {
      if let Some(command) = context.state.resolve_command(&first_arg) {
        let context = ShellCommandContext {
          args: context.args.iter().skip(1).cloned().collect::<Vec<_>>(),
          ..context
        };
        return command.execute(context);
      }
    }
    let executable_command =
      deno_task_shell::ExecutableCommand::new("npx".to_string());
    executable_command.execute(context)
  }
}

#[derive(Clone)]
struct NpmPackageBinCommand {
  name: String,
  npm_package: String,
}

impl ShellCommand for NpmPackageBinCommand {
  fn execute(
    &self,
    context: ShellCommandContext,
  ) -> LocalBoxFuture<'static, ExecuteResult> {
    let mut args = vec![
      "run".to_string(),
      "-A".to_string(),
      format!("npm:{}/{}", self.npm_package, self.name),
    ];
    args.extend(context.args);
    let executable_command =
      deno_task_shell::ExecutableCommand::new("deno".to_string());
    executable_command.execute(ShellCommandContext { args, ..context })
  }
}

fn resolve_npm_commands(
  ps: &ProcState,
) -> Result<HashMap<String, Rc<dyn ShellCommand>>, AnyError> {
  let mut result = HashMap::new();
  let snapshot = ps.npm_resolver.snapshot();
  let mut package_reqs_with_id =
    snapshot.package_reqs_to_id().iter().collect::<Vec<_>>();
  package_reqs_with_id.sort_by(|a, b| a.1.cmp(&b.1));
  for (package_req, id) in package_reqs_with_id {
    let bin_commands = crate::node::node_resolve_binary_commands(
      &package_req,
      &ps.npm_resolver,
    )?;
    for bin_command in bin_commands {
      result.insert(
        bin_command.to_string(),
        Rc::new(NpmPackageBinCommand {
          name: bin_command,
          npm_package: format!("{}@{}", id.name, id.version),
        }) as Rc<dyn ShellCommand>,
      );
    }
  }
  if !result.contains_key("npx") {
    result.insert("npx".to_string(), Rc::new(NpxCommand));
  }
  Ok(result)
}
